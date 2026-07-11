// SPDX-License-Identifier: GPL-3.0-only

use crossterm::event::{self, Event, KeyEventKind};
use ferrumtty_crypto::SessionKey;
use ferrumtty_predict::{InputKind, PredictionOverlay};
use ferrumtty_runtime::{MonotonicTime, RuntimeError, SessionAction, SessionRuntime};
use ferrumtty_terminal::{
    EscapeAction, EscapeInterpreter, TerminalGuard, encode_focus, encode_key, encode_mouse,
    encode_paste, terminal_size,
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
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(text(Text::TerminalRequired).to_owned());
    }
    let endpoint = parse_endpoint()?;
    let key = read_session_key()?;
    let socket = connect_socket(&endpoint)?;
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
    )
}

fn parse_endpoint() -> Result<String, String> {
    let mut arguments = env::args();
    let program = arguments.next().unwrap_or_else(|| "ferrumtty".to_owned());
    let host = arguments.next().ok_or_else(|| usage(&program))?;
    let port = arguments.next().ok_or_else(|| usage(&program))?;
    let port = port.parse::<u16>().map_err(|_| usage(&program))?;
    if arguments.next().is_some() || port == 0 {
        return Err(usage(&program));
    }
    Ok(format!("{host}:{port}"))
}

fn usage(program: &str) -> String {
    format!("{}: {program} HOST PORT", text(Text::Usage))
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
) -> Result<(), String> {
    let mut receive_buffer = vec![0_u8; RECEIVE_BUFFER_BYTES];
    let mut predictor = PredictionOverlay::default();
    let mut escape = EscapeInterpreter::default();
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
                if let Some(bytes) = encode_key(key) {
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
        RuntimeError::ServerTimeout => text(Text::ServerTimeout).to_owned(),
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
    MissingKey,
    NoAddress,
    ProtocolFailed,
    ReceiveFailed,
    ResolveFailed,
    SendFailed,
    ServerTimeout,
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
        Text::MissingKey => "MOSH_KEY is not set",
        Text::NoAddress => "no address was resolved",
        Text::ProtocolFailed => "protocol operation failed",
        Text::ReceiveFailed => "UDP receive failed",
        Text::ResolveFailed => "host lookup failed",
        Text::SendFailed => "UDP send failed",
        Text::ServerTimeout => "server did not respond before the timeout",
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
        Text::MissingKey => "未设置 MOSH_KEY",
        Text::NoAddress => "未解析到可用地址",
        Text::ProtocolFailed => "协议操作失败",
        Text::ReceiveFailed => "UDP 接收失败",
        Text::ResolveFailed => "主机名解析失败",
        Text::SendFailed => "UDP 发送失败",
        Text::ServerTimeout => "服务器未在超时前响应",
        Text::SignalSetupFailed => "无法安装终止信号处理",
        Text::SocketFailed => "无法配置 UDP 套接字",
        Text::TerminalRequired => "标准输入和输出必须连接终端",
        Text::TerminalSetupFailed => "无法进入终端原始模式",
        Text::TerminalSizeFailed => "无法读取终端尺寸",
        Text::TerminalWriteFailed => "无法写入终端输出",
        Text::Usage => "用法",
    }
}
