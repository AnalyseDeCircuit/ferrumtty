// SPDX-License-Identifier: GPL-3.0-only

use crossterm::event::{self, Event, KeyEventKind};
use ferrumtty_crypto::SessionKey;
use ferrumtty_predict::{InputKind, PredictionDisplay, PredictionOverlay};
use ferrumtty_runtime::{MonotonicTime, RuntimeError, SessionAction, SessionRuntime};
use ferrumtty_terminal::{
    CursorKeyMode, EscapeAction, EscapeInterpreter, TerminalGuard, encode_focus,
    encode_key_with_mode, encode_mouse, encode_paste, terminal_size,
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
const DEFAULT_ESCAPE_BYTE: u8 = 0x1e;

#[derive(Debug, Eq, PartialEq)]
enum Command {
    Colors,
    Connect(ClientConfig),
}

/// Holds CLI and environment settings at the client/runtime boundary.
#[derive(Debug, Eq, PartialEq)]
struct ClientConfig {
    endpoint: String,
    verbosity: u8,
    escape_byte: u8,
    title_no_prefix: bool,
    prediction_display: PredictionDisplay,
    prediction_overwrite: bool,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{}: {error}", text(Text::ErrorPrefix));
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let command = parse_command(env::args(), |name| env::var(name).ok())?;
    if command == Command::Colors {
        println!("{}", crossterm::style::available_color_count());
        return Ok(());
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(text(Text::TerminalRequired).to_owned());
    }
    let Command::Connect(config) = command else {
        unreachable!("color mode returned before terminal setup")
    };
    write_debug_configuration(&config);
    let key = read_session_key()?;
    let socket = connect_socket(&config.endpoint)?;
    let termination_requested = install_termination_flag()?;
    let mut terminal = TerminalGuard::enter()
        .map_err(|error| format!("{}: {error}", text(Text::TerminalSetupFailed)))?;
    let started_at = Instant::now();
    let mut runtime = SessionRuntime::new(key, monotonic_time(started_at));
    let (columns, rows) =
        terminal_size().map_err(|error| format!("{}: {error}", text(Text::TerminalSizeFailed)))?;
    runtime.queue_resize(columns, rows);
    event_loop(
        &socket,
        &mut runtime,
        &mut terminal,
        started_at,
        &termination_requested,
        &config,
    )
}

fn parse_command<I, F>(arguments: I, environment: F) -> Result<Command, String>
where
    I: IntoIterator<Item = String>,
    F: Fn(&str) -> Option<String>,
{
    let mut arguments = arguments.into_iter();
    let program = arguments.next().unwrap_or_else(|| "ferrumtty".to_owned());
    let remaining = arguments.collect::<Vec<_>>();
    if remaining == ["-c"] {
        return Ok(Command::Colors);
    }
    let mut verbosity = 0_u8;
    let mut position = 0;
    while let Some(argument) = remaining.get(position) {
        if argument.len() < 2 || !argument.as_bytes()[1..].iter().all(|byte| *byte == b'v') {
            break;
        }
        verbosity = verbosity.saturating_add(u8::try_from(argument.len() - 1).unwrap_or(u8::MAX));
        position += 1;
    }
    let host = remaining.get(position).ok_or_else(|| usage(&program))?;
    position += 1;
    let port = remaining
        .get(position)
        .ok_or_else(|| usage(&program))?
        .parse::<u16>()
        .map_err(|_| usage(&program))?;
    position += 1;
    if position != remaining.len() || port == 0 {
        return Err(usage(&program));
    }
    let escape_byte = environment("MOSH_ESCAPE_KEY")
        .map_or(Ok(DEFAULT_ESCAPE_BYTE), |value| parse_escape_key(&value))?;
    Ok(Command::Connect(ClientConfig {
        endpoint: format!("{host}:{port}"),
        verbosity,
        escape_byte,
        title_no_prefix: environment("MOSH_TITLE_NOPREFIX").is_some(),
        prediction_display: parse_prediction_display(
            environment("MOSH_PREDICTION_DISPLAY").as_deref(),
        )?,
        prediction_overwrite: environment("MOSH_PREDICTION_OVERWRITE").as_deref() == Some("yes"),
    }))
}

fn parse_prediction_display(value: Option<&str>) -> Result<PredictionDisplay, String> {
    match value.unwrap_or("adaptive") {
        "adaptive" => Ok(PredictionDisplay::Adaptive),
        "always" => Ok(PredictionDisplay::Always),
        "never" => Ok(PredictionDisplay::Never),
        _ => Err(text(Text::InvalidPredictionDisplay).to_owned()),
    }
}

fn parse_escape_key(value: &str) -> Result<u8, String> {
    let bytes = value.as_bytes();
    if bytes.len() == 1 && matches!(bytes[0], 1..=127) {
        return Ok(bytes[0]);
    }
    Err(text(Text::InvalidEscapeKey).to_owned())
}

fn write_debug_configuration(config: &ClientConfig) {
    if config.verbosity == 0 {
        return;
    }
    eprintln!("FerrumTTY: connecting to {}", config.endpoint);
    if config.verbosity > 1 {
        eprintln!(
            "FerrumTTY: escape={} title_no_prefix={} prediction_display={:?} prediction_overwrite={:?}",
            config.escape_byte,
            config.title_no_prefix,
            config.prediction_display,
            config.prediction_overwrite
        );
    }
}

fn usage(program: &str) -> String {
    format!(
        "{}: {program} [-v...] HOST PORT\n       {program} -c",
        text(Text::Usage)
    )
}

fn read_session_key() -> Result<SessionKey, String> {
    let mut encoded =
        Zeroizing::new(env::var("MOSH_KEY").map_err(|_| text(Text::MissingKey).to_owned())?);
    let key = SessionKey::decode(&encoded).map_err(|_| text(Text::InvalidKey).to_owned())?;
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
) -> Result<(), String> {
    let mut receive_buffer = vec![0_u8; RECEIVE_BUFFER_BYTES];
    let mut predictor =
        PredictionOverlay::new(config.prediction_display, config.prediction_overwrite);
    let mut escape = EscapeInterpreter::new(config.escape_byte);
    let mut previous_poll = monotonic_time(started_at);
    let result = loop {
        if termination_requested.load(Ordering::Relaxed) {
            break Ok(());
        }
        let now = monotonic_time(started_at);
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
        ) {
            Ok(true) => break Ok(()),
            Ok(false) => {}
            Err(error) => break Err(error),
        }
        predictor.set_round_trip_milliseconds(runtime.round_trip_milliseconds());
        match drain_terminal(runtime, terminal, &mut predictor, &mut escape) {
            Ok(true) => break Ok(()),
            Ok(false) => {}
            Err(error) => break Err(error),
        }
        let actions = match runtime.poll(now).map_err(format_runtime_error) {
            Ok(actions) => actions,
            Err(error) => break Err(error),
        };
        if let Err(error) = apply_actions(socket, terminal, &mut predictor, actions) {
            break Err(error);
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
        apply_actions(socket, terminal, predictor, actions)?;
    }
}

fn drain_terminal(
    runtime: &mut SessionRuntime,
    terminal: &mut TerminalGuard,
    predictor: &mut PredictionOverlay,
    escape: &mut EscapeInterpreter,
) -> Result<bool, String> {
    while event::poll(Duration::ZERO)
        .map_err(|error| format!("{}: {error}", text(Text::InputFailed)))?
    {
        match event::read().map_err(|error| format!("{}: {error}", text(Text::InputFailed)))? {
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                if let Some(bytes) = encode_key_with_mode(key, CursorKeyMode::Application) {
                    match escape.input(bytes) {
                        EscapeAction::Hold => {}
                        EscapeAction::Quit => return Ok(true),
                        EscapeAction::Forward(bytes) => {
                            queue_input(runtime, bytes.clone())?;
                            if let Some(prediction) = predictor.offer(InputKind::Key, &bytes) {
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
                let bytes = encode_paste(&contents);
                let _ = predictor.offer(InputKind::Paste, &bytes);
                queue_input(runtime, bytes)?;
            }
            Event::Mouse(mouse) => {
                flush_escape(runtime, escape)?;
                if let Some(bytes) = encode_mouse(mouse) {
                    let _ = predictor.offer(InputKind::Mouse, &bytes);
                    queue_input(runtime, bytes)?;
                }
            }
            Event::FocusGained => {
                flush_escape(runtime, escape)?;
                let bytes = encode_focus(true);
                let _ = predictor.offer(InputKind::Focus, &bytes);
                queue_input(runtime, bytes)?;
            }
            Event::FocusLost => {
                flush_escape(runtime, escape)?;
                let bytes = encode_focus(false);
                let _ = predictor.offer(InputKind::Focus, &bytes);
                queue_input(runtime, bytes)?;
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

fn apply_actions(
    socket: &UdpSocket,
    terminal: &mut TerminalGuard,
    predictor: &mut PredictionOverlay,
    actions: Vec<SessionAction>,
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
        }
    }
    Ok(())
}

fn reconcile_prediction(
    terminal: &mut TerminalGuard,
    predictor: &mut PredictionOverlay,
) -> Result<(), String> {
    if predictor.reconcile() {
        terminal
            .redraw_authoritative()
            .map_err(|error| format!("{}: {error}", text(Text::TerminalWriteFailed)))?;
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
        other => format!("{}: {other:?}", text(Text::ProtocolFailed)),
    }
}

#[derive(Clone, Copy)]
enum Text {
    ConnectFailed,
    ErrorPrefix,
    InputFailed,
    InputQueueFull,
    InvalidKey,
    InvalidEscapeKey,
    InvalidPredictionDisplay,
    MissingKey,
    NoAddress,
    ProtocolFailed,
    ReceiveFailed,
    ResolveFailed,
    SendFailed,
    SignalSetupFailed,
    SocketFailed,
    TerminalRequired,
    TerminalSetupFailed,
    TerminalSizeFailed,
    TerminalWriteFailed,
    Usage,
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
        Text::InvalidKey => "MOSH_KEY is invalid",
        Text::InvalidEscapeKey => {
            "MOSH_ESCAPE_KEY must be one literal ASCII character in the range 1-127"
        }
        Text::InvalidPredictionDisplay => {
            "MOSH_PREDICTION_DISPLAY must be adaptive, always, or never"
        }
        Text::MissingKey => "MOSH_KEY is not set",
        Text::NoAddress => "no address was resolved",
        Text::ProtocolFailed => "protocol operation failed",
        Text::ReceiveFailed => "UDP receive failed",
        Text::ResolveFailed => "host lookup failed",
        Text::SendFailed => "UDP send failed",
        Text::SignalSetupFailed => "could not install termination handling",
        Text::SocketFailed => "could not configure UDP socket",
        Text::TerminalRequired => "standard input and output must be terminals",
        Text::TerminalSetupFailed => "could not enter terminal raw mode",
        Text::TerminalSizeFailed => "could not read terminal size",
        Text::TerminalWriteFailed => "could not write terminal output",
        Text::Usage => "usage",
    }
}

fn text_zh(message: Text) -> &'static str {
    match message {
        Text::ConnectFailed => "无法连接 UDP 套接字",
        Text::ErrorPrefix => "FerrumTTY 错误",
        Text::InputFailed => "无法读取终端输入",
        Text::InputQueueFull => "本地输入队列已达到上限",
        Text::InvalidKey => "MOSH_KEY 无效",
        Text::InvalidEscapeKey => "MOSH_ESCAPE_KEY 必须是一个取值为 1-127 的字面 ASCII 字符",
        Text::InvalidPredictionDisplay => {
            "MOSH_PREDICTION_DISPLAY 必须是 adaptive、always 或 never"
        }
        Text::MissingKey => "未设置 MOSH_KEY",
        Text::NoAddress => "未解析到可用地址",
        Text::ProtocolFailed => "协议操作失败",
        Text::ReceiveFailed => "UDP 接收失败",
        Text::ResolveFailed => "主机名解析失败",
        Text::SendFailed => "UDP 发送失败",
        Text::SignalSetupFailed => "无法安装终止信号处理",
        Text::SocketFailed => "无法配置 UDP 套接字",
        Text::TerminalRequired => "标准输入和输出必须连接终端",
        Text::TerminalSetupFailed => "无法进入终端原始模式",
        Text::TerminalSizeFailed => "无法读取终端尺寸",
        Text::TerminalWriteFailed => "无法写入终端输出",
        Text::Usage => "用法",
    }
}

#[cfg(test)]
mod tests {
    use super::{ClientConfig, Command, DEFAULT_ESCAPE_BYTE, parse_command, parse_escape_key};
    use ferrumtty_predict::PredictionDisplay;
    use std::collections::HashMap;

    fn parse(arguments: &[&str], environment: &[(&str, &str)]) -> Result<Command, String> {
        let arguments = arguments.iter().map(|value| (*value).to_owned());
        let environment = environment
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect::<HashMap<_, _>>();
        parse_command(arguments, |name| environment.get(name).cloned())
    }

    #[test]
    fn parses_color_mode_without_connection_arguments() {
        assert_eq!(parse(&["mosh-client", "-c"], &[]), Ok(Command::Colors));
        assert!(parse(&["mosh-client", "-c", "host", "60001"], &[]).is_err());
    }

    #[test]
    fn parses_repeated_verbose_flags_and_environment_boundary() {
        assert_eq!(
            parse(
                &["mosh-client", "-vv", "-v", "example.test", "60001"],
                &[
                    ("MOSH_ESCAPE_KEY", "\u{2}"),
                    ("MOSH_TITLE_NOPREFIX", "1"),
                    ("MOSH_PREDICTION_DISPLAY", "always"),
                    ("MOSH_PREDICTION_OVERWRITE", "yes"),
                ],
            ),
            Ok(Command::Connect(ClientConfig {
                endpoint: "example.test:60001".to_owned(),
                verbosity: 3,
                escape_byte: 2,
                title_no_prefix: true,
                prediction_display: PredictionDisplay::Always,
                prediction_overwrite: true,
            }))
        );
        assert!(
            parse(
                &["mosh-client", "example.test", "60001"],
                &[("MOSH_PREDICTION_DISPLAY", "experimental")],
            )
            .is_err()
        );
    }

    #[test]
    fn uses_default_escape_and_rejects_invalid_escape_values() {
        let Ok(Command::Connect(config)) = parse(&["mosh-client", "127.0.0.1", "60001"], &[])
        else {
            panic!("connection arguments should parse")
        };
        assert_eq!(config.escape_byte, DEFAULT_ESCAPE_BYTE);
        assert!(parse_escape_key("").is_err());
        assert!(parse_escape_key("ab").is_err());
        assert!(parse_escape_key("\u{80}").is_err());
        assert!(parse_escape_key("\0").is_err());
    }
}
