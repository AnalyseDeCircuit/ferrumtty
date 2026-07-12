// SPDX-License-Identifier: GPL-3.0-only

mod cli;
mod diagnostics;

use cli::{CliError, ClientConfig, Command, OutputTarget};
use crossterm::event::{self, Event, KeyEventKind};
use diagnostics::DiagnosticLogger;
use ferrumtty_crypto::SessionKey;
use ferrumtty_predict::{
    PredictionAction, PredictionContext, PredictionOverlay, PredictionReconciliation,
};
use ferrumtty_runtime::{MonotonicTime, RuntimeError, SessionAction, SessionRuntime};
use ferrumtty_terminal::{
    EscapeAction, EscapeInterpreter, TerminalGuard, TitlePolicy, encode_focus_with_mode,
    encode_key_with_mode, encode_mouse_with_mode, encode_paste_with_mode, terminal_size,
};
use std::env;
use std::io::{self, IsTerminal};
use std::net::{ToSocketAddrs, UdpSocket};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use zeroize::{Zeroize, Zeroizing};

const RECEIVE_BUFFER_BYTES: usize = 65_535;
const LOOP_INTERVAL: Duration = Duration::from_millis(20);
const SUSPEND_DETECTION_MILLISECONDS: u64 = 5_000;

enum AppError {
    Cli(CliError),
    Runtime(String),
    UncleanShutdown,
}

impl From<CliError> for AppError {
    fn from(error: CliError) -> Self {
        Self::Cli(error)
    }
}

impl From<String> for AppError {
    fn from(error: String) -> Self {
        Self::Runtime(error)
    }
}

fn main() -> ExitCode {
    match dispatch() {
        Ok(exit_code) => exit_code,
        Err(AppError::Cli(error)) => {
            write_cli_output(error.target(), error.message());
            ExitCode::from(error.exit_code())
        }
        Err(AppError::Runtime(error)) => {
            eprintln!("{}: {error}", text(Text::ErrorPrefix));
            ExitCode::FAILURE
        }
        Err(AppError::UncleanShutdown) => {
            eprintln!("{}", text(Text::UncleanShutdown));
            ExitCode::FAILURE
        }
    }
}

fn write_cli_output(target: OutputTarget, message: &str) {
    match target {
        OutputTarget::Stdout => println!("{message}"),
        OutputTarget::Stderr => eprintln!("{message}"),
    }
}

fn dispatch() -> Result<ExitCode, AppError> {
    let command = cli::parse_command(env::args(), |name| env::var(name).ok())?;
    match command {
        Command::Help { program } => {
            write_cli_output(OutputTarget::Stdout, &cli::usage(&program));
            Ok(ExitCode::SUCCESS)
        }
        Command::Version => {
            write_cli_output(OutputTarget::Stdout, &cli::version());
            Ok(ExitCode::SUCCESS)
        }
        Command::Colors => {
            let platform =
                cli::platform_color_capability(crossterm::style::available_color_count());
            let color_count = cli::infer_color_count(
                env::var("TERM").ok().as_deref(),
                env::var("COLORTERM").ok().as_deref(),
                platform,
            );
            println!("{color_count}");
            Ok(ExitCode::SUCCESS)
        }
        Command::Connect(config) => {
            run(&config)?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn run(config: &ClientConfig) -> Result<(), AppError> {
    let key = read_session_key()?;
    cli::validate_utf8_locale(|name| env::var(name).ok(), cfg!(windows))?;
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(text(Text::TerminalRequired).to_owned().into());
    }
    let logger = DiagnosticLogger::new(config.verbosity);
    logger.write_startup(config);
    let socket = connect_socket(&config.endpoint)?;
    let termination_requested = install_termination_flag()?;
    let title_policy = if config.title_no_prefix {
        TitlePolicy::PreserveRemote
    } else {
        TitlePolicy::MoshPrefix
    };
    let mut terminal = TerminalGuard::enter_with_title_policy(title_policy)
        .map_err(|error| format!("{}: {error}", text(Text::TerminalSetupFailed)))?;
    let started_at = Instant::now();
    let mut runtime = SessionRuntime::new(key, monotonic_time(started_at));
    let (columns, rows) =
        terminal_size().map_err(|error| format!("{}: {error}", text(Text::TerminalSizeFailed)))?;
    runtime.queue_resize(columns, rows);
    let clean_shutdown = event_loop(
        &socket,
        &mut runtime,
        &mut terminal,
        started_at,
        &termination_requested,
        config,
        &logger,
    )?;
    if clean_shutdown {
        Ok(())
    } else {
        Err(AppError::UncleanShutdown)
    }
}

fn read_session_key() -> Result<SessionKey, AppError> {
    let mut encoded = Zeroizing::new(env::var("MOSH_KEY").map_err(|_| cli::missing_key_error())?);
    let key = SessionKey::decode(&encoded).map_err(|_| cli::invalid_key_error())?;
    encoded.zeroize();
    Ok(key)
}

fn connect_socket(endpoint: &str) -> Result<UdpSocket, String> {
    let addresses = endpoint
        .to_socket_addrs()
        .map_err(|error| format!("{}: {error}", text(Text::ResolveFailed)))?;
    let mut last_error = None;
    for address in addresses {
        let bind_address = if address.is_ipv4() {
            "0.0.0.0:0"
        } else {
            "[::]:0"
        };
        let socket = match UdpSocket::bind(bind_address) {
            Ok(socket) => socket,
            Err(error) => {
                last_error = Some(error);
                continue;
            }
        };
        match socket.connect(address) {
            Ok(()) => {
                socket
                    .set_nonblocking(true)
                    .map_err(|error| format!("{}: {error}", text(Text::SocketFailed)))?;
                return Ok(socket);
            }
            Err(error) => last_error = Some(error),
        }
    }
    Err(format!(
        "{}: {}",
        text(Text::ConnectFailed),
        last_error.map_or_else(
            || text(Text::NoAddress).to_owned(),
            |error| error.to_string()
        )
    ))
}

fn event_loop(
    socket: &UdpSocket,
    runtime: &mut SessionRuntime,
    terminal: &mut TerminalGuard,
    started_at: Instant,
    termination_requested: &AtomicBool,
    config: &ClientConfig,
    logger: &DiagnosticLogger,
) -> Result<bool, String> {
    let mut receive_buffer = vec![0_u8; RECEIVE_BUFFER_BYTES];
    let mut predictor =
        PredictionOverlay::new(config.prediction_display, config.prediction_overwrite);
    let mut escape = EscapeInterpreter::new(config.escape_byte);
    let mut previous_poll = monotonic_time(started_at);
    let result = loop {
        let now = monotonic_time(started_at);
        if termination_requested.load(Ordering::Relaxed) {
            let shutdown_result = runtime
                .request_shutdown(now)
                .map_err(format_runtime_error)
                .and_then(|actions| {
                    apply_actions(socket, terminal, &mut predictor, actions, logger)
                });
            if let Err(error) = shutdown_result {
                break Err(error);
            }
        }
        if now
            .milliseconds()
            .saturating_sub(previous_poll.milliseconds())
            >= SUSPEND_DETECTION_MILLISECONDS
        {
            runtime.resume(now);
        }
        previous_poll = now;
        match drain_network(
            socket,
            runtime,
            terminal,
            &mut predictor,
            &mut receive_buffer,
            now,
            logger,
        ) {
            Ok(true) => break Ok(true),
            Ok(false) => {}
            Err(error) => break Err(error),
        }
        predictor.set_round_trip_milliseconds(runtime.round_trip_milliseconds());
        if !runtime.shutdown_in_progress() {
            match drain_terminal(runtime, terminal, &mut predictor, &mut escape, started_at) {
                Ok(true) => {
                    let shutdown_result = runtime
                        .request_shutdown(monotonic_time(started_at))
                        .map_err(format_runtime_error)
                        .and_then(|actions| {
                            apply_actions(socket, terminal, &mut predictor, actions, logger)
                        });
                    if let Err(error) = shutdown_result {
                        break Err(error);
                    }
                }
                Ok(false) => {}
                Err(error) => break Err(error),
            }
        }
        let actions = match runtime.poll(now).map_err(format_runtime_error) {
            Ok(actions) => actions,
            Err(error) => break Err(error),
        };
        if let Err(error) = apply_actions(socket, terminal, &mut predictor, actions, logger) {
            break Err(error);
        }
        if let Some(outcome) = runtime.shutdown_outcome() {
            break Ok(outcome != ferrumtty_runtime::ShutdownOutcome::TimedOut);
        }
        let wait_milliseconds = runtime
            .milliseconds_until_next_poll(monotonic_time(started_at))
            .max(1);
        std::thread::sleep(LOOP_INTERVAL.min(Duration::from_millis(wait_milliseconds)));
    };
    reconcile_prediction(terminal, &mut predictor)?;
    result
}

#[cfg(not(windows))]
fn install_termination_flag() -> Result<Arc<AtomicBool>, String> {
    let requested = Arc::new(AtomicBool::new(false));
    for signal in signal_hook::consts::TERM_SIGNALS {
        signal_hook::flag::register(*signal, Arc::clone(&requested))
            .map_err(|error| format!("{}: {error}", text(Text::SignalSetupFailed)))?;
    }
    Ok(requested)
}

#[cfg(windows)]
fn install_termination_flag() -> Result<Arc<AtomicBool>, String> {
    // ConPTY can raise a control event while also delivering the key as input.
    // Ignoring the process-level event lets Ctrl+C reach the remote session.
    ctrlc::set_handler(|| {})
        .map_err(|error| format!("{}: {error}", text(Text::SignalSetupFailed)))?;
    Ok(Arc::new(AtomicBool::new(false)))
}

fn drain_network(
    socket: &UdpSocket,
    runtime: &mut SessionRuntime,
    terminal: &mut TerminalGuard,
    predictor: &mut PredictionOverlay,
    receive_buffer: &mut [u8],
    now: MonotonicTime,
    logger: &DiagnosticLogger,
) -> Result<bool, String> {
    loop {
        let received = match socket.recv(receive_buffer) {
            Ok(received) => received,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(error)
                if error.kind() == io::ErrorKind::ConnectionRefused
                    && runtime.has_received_server_state() =>
            {
                return Ok(true);
            }
            Err(error) => return Err(format!("{}: {error}", text(Text::ReceiveFailed))),
        };
        let actions = runtime
            .receive_datagram(&receive_buffer[..received], now)
            .map_err(format_runtime_error)?;
        apply_actions(socket, terminal, predictor, actions, logger)?;
    }
}

fn drain_terminal(
    runtime: &mut SessionRuntime,
    terminal: &mut TerminalGuard,
    predictor: &mut PredictionOverlay,
    escape: &mut EscapeInterpreter,
    started_at: Instant,
) -> Result<bool, String> {
    while event::poll(Duration::ZERO)
        .map_err(|error| format!("{}: {error}", text(Text::InputFailed)))?
    {
        let terminal_event =
            event::read().map_err(|error| format!("{}: {error}", text(Text::InputFailed)))?;
        terminal
            .clear_local_notice()
            .map_err(|error| format!("{}: {error}", text(Text::TerminalWriteFailed)))?;
        match terminal_event {
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                let remote_modes = terminal.remote_modes();
                if let Some(bytes) = encode_key_with_mode(key, remote_modes.cursor_key) {
                    match escape.input(bytes) {
                        EscapeAction::Hold => {}
                        EscapeAction::Help => terminal
                            .write_local_notice(text(Text::LocalCommandHelp).as_bytes())
                            .map_err(|error| {
                                format!("{}: {error}", text(Text::TerminalWriteFailed))
                            })?,
                        EscapeAction::Quit => return Ok(true),
                        EscapeAction::Suspend => {
                            suspend_client(terminal)?;
                            runtime.resume(monotonic_time(started_at));
                        }
                        EscapeAction::Forward(bytes) => {
                            let prediction_frame = runtime.prediction_frame_id();
                            let prediction_action = prediction_action(key, &bytes);
                            let terminal_context = terminal.prediction_cursor_context();
                            let prediction_context = PredictionContext {
                                row: terminal_context.row,
                                column: terminal_context.column,
                                columns: terminal_context.columns,
                                attributes: terminal_context.attributes,
                                cursor_state: terminal_context.cursor_state,
                            };
                            queue_input(runtime, bytes.clone())?;
                            if let Some(prediction) = predictor.offer_for_frame_with_context(
                                prediction_frame,
                                prediction_action,
                                Some(&prediction_context),
                            ) {
                                terminal.write_overlay_bytes(&prediction).map_err(|error| {
                                    format!("{}: {error}", text(Text::TerminalWriteFailed))
                                })?;
                            }
                        }
                    }
                }
            }
            Event::Paste(contents) => {
                flush_escape(runtime, escape)?;
                let bytes =
                    encode_paste_with_mode(&contents, terminal.remote_modes().bracketed_paste);
                let prediction_frame = runtime.prediction_frame_id();
                queue_input(runtime, bytes)?;
                let _ = predictor.offer_for_frame(prediction_frame, PredictionAction::Barrier);
            }
            Event::Mouse(mouse) => {
                flush_escape(runtime, escape)?;
                let remote_modes = terminal.remote_modes();
                if let Some(bytes) = encode_mouse_with_mode(
                    mouse,
                    remote_modes.mouse_tracking,
                    remote_modes.mouse_encoding,
                ) {
                    let prediction_frame = runtime.prediction_frame_id();
                    queue_input(runtime, bytes)?;
                    let _ = predictor.offer_for_frame(prediction_frame, PredictionAction::Barrier);
                }
            }
            Event::FocusGained => {
                flush_escape(runtime, escape)?;
                if let Some(bytes) =
                    encode_focus_with_mode(true, terminal.remote_modes().focus_reporting)
                {
                    let prediction_frame = runtime.prediction_frame_id();
                    queue_input(runtime, bytes)?;
                    let _ = predictor.offer_for_frame(prediction_frame, PredictionAction::Barrier);
                }
            }
            Event::FocusLost => {
                flush_escape(runtime, escape)?;
                if let Some(bytes) =
                    encode_focus_with_mode(false, terminal.remote_modes().focus_reporting)
                {
                    let prediction_frame = runtime.prediction_frame_id();
                    queue_input(runtime, bytes)?;
                    let _ = predictor.offer_for_frame(prediction_frame, PredictionAction::Barrier);
                }
            }
            Event::Resize(columns, rows) => {
                flush_escape(runtime, escape)?;
                reconcile_prediction(terminal, predictor)?;
                terminal.resize_model(columns, rows);
                runtime.queue_resize(columns, rows);
            }
            Event::Key(_) => {}
        }
    }
    Ok(false)
}

fn flush_escape(
    runtime: &mut SessionRuntime,
    escape: &mut EscapeInterpreter,
) -> Result<(), String> {
    if let Some(prefix) = escape.flush() {
        queue_input(runtime, prefix)?;
    }
    Ok(())
}

fn queue_input(runtime: &mut SessionRuntime, bytes: Vec<u8>) -> Result<(), String> {
    runtime.queue_input(bytes).map_err(format_runtime_error)
}

#[cfg(unix)]
fn suspend_client(terminal: &mut TerminalGuard) -> Result<(), String> {
    terminal
        .leave_for_suspend()
        .map_err(|error| format!("{}: {error}", text(Text::TerminalSetupFailed)))?;
    let suspend_result = signal_hook::low_level::raise(signal_hook::consts::SIGTSTP)
        .map_err(|error| format!("{}: {error}", text(Text::SignalSetupFailed)));
    let resume_result = terminal
        .resume_after_suspend()
        .map_err(|error| format!("{}: {error}", text(Text::TerminalSetupFailed)));
    suspend_result.and(resume_result)
}

#[cfg(windows)]
fn suspend_client(terminal: &mut TerminalGuard) -> Result<(), String> {
    terminal
        .write_local_notice(text(Text::SuspendUnsupported).as_bytes())
        .map_err(|error| format!("{}: {error}", text(Text::TerminalWriteFailed)))
}

fn prediction_action(key: crossterm::event::KeyEvent, forwarded: &[u8]) -> PredictionAction {
    if !key
        .modifiers
        .intersects(crossterm::event::KeyModifiers::ALT | crossterm::event::KeyModifiers::CONTROL)
    {
        match key.code {
            crossterm::event::KeyCode::Char(_) if forwarded.len() == 1 => {
                return PredictionAction::PrintableAscii(forwarded[0]);
            }
            crossterm::event::KeyCode::Backspace => return PredictionAction::Backspace,
            crossterm::event::KeyCode::Left => return PredictionAction::Left,
            crossterm::event::KeyCode::Right => return PredictionAction::Right,
            _ => {}
        }
    }
    PredictionAction::Barrier
}

fn apply_actions(
    socket: &UdpSocket,
    terminal: &mut TerminalGuard,
    predictor: &mut PredictionOverlay,
    actions: Vec<SessionAction>,
    logger: &DiagnosticLogger,
) -> Result<(), String> {
    for action in actions {
        match action {
            SessionAction::SendDatagram(packet) => socket
                .send(&packet)
                .map(|_| ())
                .map_err(|error| format!("{}: {error}", text(Text::SendFailed)))?,
            SessionAction::WriteTerminal(bytes) => {
                reconcile_prediction(terminal, predictor)?;
                terminal
                    .write_server_bytes(&bytes)
                    .map_err(|error| format!("{}: {error}", text(Text::TerminalWriteFailed)))?;
            }
            SessionAction::AcknowledgePrediction(value) => predictor.acknowledge(value),
            SessionAction::RemoteStateAdvanced(_)
            | SessionAction::ConnectionStateChanged(_)
            | SessionAction::RoundTripEstimate(_)
            | SessionAction::SessionLifecycleChanged(_)
            | SessionAction::UdpBindingChanged(_)
            | SessionAction::ShutdownComplete(_) => {}
            SessionAction::Diagnostic(event) => logger.write_event(event),
        }
    }
    Ok(())
}

fn reconcile_prediction(
    terminal: &mut TerminalGuard,
    predictor: &mut PredictionOverlay,
) -> Result<(), String> {
    match predictor.take_reconciliation() {
        PredictionReconciliation::None => {}
        PredictionReconciliation::Local(bytes) => terminal
            .write_overlay_bytes(&bytes)
            .map_err(|error| format!("{}: {error}", text(Text::TerminalWriteFailed)))?,
        PredictionReconciliation::Redraw => terminal
            .redraw_authoritative()
            .map_err(|error| format!("{}: {error}", text(Text::TerminalWriteFailed)))?,
    }
    Ok(())
}

fn monotonic_time(started_at: Instant) -> MonotonicTime {
    let milliseconds = Instant::now().duration_since(started_at).as_millis();
    MonotonicTime::from_milliseconds(u64::try_from(milliseconds).unwrap_or(u64::MAX))
}

fn format_runtime_error(error: RuntimeError) -> String {
    match error {
        RuntimeError::InputQueueFull => text(Text::InputQueueFull).to_owned(),
        other => format!("{}: {other}", text(Text::ProtocolFailed)),
    }
}

#[derive(Clone, Copy)]
enum Text {
    ConnectFailed,
    ErrorPrefix,
    InputFailed,
    InputQueueFull,
    LocalCommandHelp,
    NoAddress,
    ProtocolFailed,
    ReceiveFailed,
    ResolveFailed,
    SendFailed,
    SignalSetupFailed,
    SocketFailed,
    #[cfg(windows)]
    SuspendUnsupported,
    TerminalRequired,
    TerminalSetupFailed,
    TerminalSizeFailed,
    TerminalWriteFailed,
    UncleanShutdown,
}

fn text(message: Text) -> &'static str {
    let language = env::var("LANG").unwrap_or_default();
    if language.starts_with("zh") {
        return text_zh(message);
    }
    text_en(message)
}

fn text_en(message: Text) -> &'static str {
    match message {
        Text::ConnectFailed => "could not connect UDP socket",
        Text::ErrorPrefix => "FerrumTTY error",
        Text::InputFailed => "could not read terminal input",
        Text::InputQueueFull => "local input queue limit reached",
        Text::LocalCommandHelp => {
            "\r\nFerrumTTY local commands: . quit, Ctrl-Z suspend, ? help, prefix prefix sends a literal prefix\r\n"
        }
        Text::NoAddress => "no address was resolved",
        Text::ProtocolFailed => "protocol operation failed",
        Text::ReceiveFailed => "UDP receive failed",
        Text::ResolveFailed => "host lookup failed",
        Text::SendFailed => "UDP send failed",
        Text::SignalSetupFailed => "could not install termination handling",
        Text::SocketFailed => "could not configure UDP socket",
        #[cfg(windows)]
        Text::SuspendUnsupported => "\r\nLocal suspend is not supported on this platform.\r\n",
        Text::TerminalRequired => "standard input and output must be terminals",
        Text::TerminalSetupFailed => "could not enter terminal raw mode",
        Text::TerminalSizeFailed => "could not read terminal size",
        Text::TerminalWriteFailed => "could not write terminal output",
        Text::UncleanShutdown => {
            "\nmosh did not shut down cleanly. Please note that the\nmosh-server process may still be running on the server."
        }
    }
}

fn text_zh(message: Text) -> &'static str {
    match message {
        Text::ConnectFailed => "无法连接 UDP 套接字",
        Text::ErrorPrefix => "FerrumTTY 错误",
        Text::InputFailed => "无法读取终端输入",
        Text::InputQueueFull => "本地输入队列已达到上限",
        Text::LocalCommandHelp => {
            "\r\nFerrumTTY 本地命令：. 退出，Ctrl-Z 暂停，? 帮助，连续输入两次前缀可发送字面前缀\r\n"
        }
        Text::NoAddress => "未解析到可用地址",
        Text::ProtocolFailed => "协议操作失败",
        Text::ReceiveFailed => "UDP 接收失败",
        Text::ResolveFailed => "主机名解析失败",
        Text::SendFailed => "UDP 发送失败",
        Text::SignalSetupFailed => "无法安装终止信号处理",
        Text::SocketFailed => "无法配置 UDP 套接字",
        #[cfg(windows)]
        Text::SuspendUnsupported => "\r\n当前平台不支持本地暂停。\r\n",
        Text::TerminalRequired => "标准输入和输出必须连接终端",
        Text::TerminalSetupFailed => "无法进入终端原始模式",
        Text::TerminalSizeFailed => "无法读取终端尺寸",
        Text::TerminalWriteFailed => "无法写入终端输出",
        Text::UncleanShutdown => {
            "\nmosh 未能干净关闭。请注意，服务器上的 mosh-server 进程可能仍在运行。"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::prediction_action;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ferrumtty_predict::PredictionAction;

    #[test]
    fn maps_key_semantics_without_parsing_terminal_escape_bytes() {
        assert_eq!(
            prediction_action(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE), b"a"),
            PredictionAction::PrintableAscii(b'a')
        );
        assert_eq!(
            prediction_action(
                KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
                b"\x7f"
            ),
            PredictionAction::Backspace
        );
        assert_eq!(
            prediction_action(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE), b"\x1bOD"),
            PredictionAction::Left
        );
        assert_eq!(
            prediction_action(
                KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL),
                b"\x1b[1;5D"
            ),
            PredictionAction::Barrier
        );
    }
}
