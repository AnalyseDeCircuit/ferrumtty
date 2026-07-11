// SPDX-License-Identifier: GPL-3.0-only

//! Local terminal lifecycle and input translation.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use ferrumtty_wire::{ByteRun, Instruction, ViewportSize};
use std::env;
use std::io::{self, Write};

const DEFAULT_ESCAPE_BYTE: u8 = 0x1e;
const COLUMNS_ENVIRONMENT_VARIABLE: &str = "COLUMNS";
const LINES_ENVIRONMENT_VARIABLE: &str = "LINES";
const SKIP_TERMINAL_INITIALIZATION_VARIABLE: &str = "MOSH_NO_TERM_INIT";

#[derive(Debug, Eq, PartialEq)]
pub enum EscapeAction {
    Hold,
    Forward(Vec<u8>),
    Quit,
}

/// Interprets the documented local two-key escape without forwarding the
/// prefix until its meaning is known.
#[derive(Default)]
pub struct EscapeInterpreter {
    prefix_held: bool,
}

impl EscapeInterpreter {
    #[must_use]
    pub fn input(&mut self, bytes: Vec<u8>) -> EscapeAction {
        if !self.prefix_held {
            if bytes == [DEFAULT_ESCAPE_BYTE] {
                self.prefix_held = true;
                return EscapeAction::Hold;
            }
            return EscapeAction::Forward(bytes);
        }
        self.prefix_held = false;
        if bytes == b"." {
            return EscapeAction::Quit;
        }
        if bytes == b"^" {
            return EscapeAction::Forward(vec![DEFAULT_ESCAPE_BYTE]);
        }
        let mut forwarded = Vec::with_capacity(bytes.len() + 1);
        forwarded.push(DEFAULT_ESCAPE_BYTE);
        forwarded.extend(bytes);
        EscapeAction::Forward(forwarded)
    }

    #[must_use]
    pub fn flush(&mut self) -> Option<Vec<u8>> {
        self.prefix_held.then(|| {
            self.prefix_held = false;
            vec![DEFAULT_ESCAPE_BYTE]
        })
    }
}

/// Restores the local terminal mode when the client exits through normal
/// returns or unwinding.
pub struct TerminalGuard {
    output: io::Stdout,
    authoritative: AuthoritativeScreen,
    alternate_screen_active: bool,
}

impl TerminalGuard {
    /// Enables raw input and bracketed-paste reporting.
    ///
    /// # Errors
    ///
    /// Returns an operating-system error if the terminal cannot be changed.
    pub fn enter() -> io::Result<Self> {
        let (columns, rows) = terminal_size()?;
        enable_raw_mode()?;
        let mut output = io::stdout();
        let alternate_screen_active = env::var_os(SKIP_TERMINAL_INITIALIZATION_VARIABLE).is_none();
        let setup_result = if alternate_screen_active {
            crossterm::execute!(
                output,
                crossterm::terminal::EnterAlternateScreen,
                crossterm::event::EnableBracketedPaste
            )
            .and_then(|()| output.write_all(b"\x1b[?1h"))
            .and_then(|()| output.flush())
        } else {
            crossterm::execute!(output, crossterm::event::EnableBracketedPaste)
        };
        if let Err(error) = setup_result {
            if alternate_screen_active {
                let _ = crossterm::execute!(output, crossterm::terminal::LeaveAlternateScreen);
            }
            let _ = disable_raw_mode();
            return Err(error);
        }
        Ok(Self {
            output,
            authoritative: AuthoritativeScreen::new(columns, rows),
            alternate_screen_active,
        })
    }

    /// Writes authoritative terminal bytes received from the server.
    ///
    /// # Errors
    ///
    /// Returns an error if standard output cannot be written or flushed.
    pub fn write_server_bytes(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.authoritative.process(bytes);
        self.write_bytes(bytes)
    }

    /// Writes a temporary local overlay without changing authoritative state.
    ///
    /// # Errors
    ///
    /// Returns an error if standard output cannot be written or flushed.
    pub fn write_overlay_bytes(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.write_bytes(bytes)
    }

    /// Clears temporary output and reconstructs the authoritative modeled
    /// screen, including cursor position and rendition.
    ///
    /// # Errors
    ///
    /// Returns an error if standard output cannot be written or flushed.
    pub fn redraw_authoritative(&mut self) -> io::Result<()> {
        let formatted = self.authoritative.formatted();
        self.write_bytes(&formatted)
    }

    pub fn resize_model(&mut self, columns: u16, rows: u16) {
        self.authoritative.resize(columns, rows);
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.output.write_all(bytes)?;
        self.output.flush()
    }
}

/// Reads a nonzero terminal viewport suitable for terminal models and the
/// wire-level viewport instruction.
///
/// # Errors
///
/// Returns an operating-system error when the console size is unavailable.
pub fn terminal_size() -> io::Result<(u16, u16)> {
    match crossterm::terminal::size() {
        Ok((columns, rows)) => Ok((columns.max(1), rows.max(1))),
        Err(console_error) => environment_terminal_size().ok_or(console_error),
    }
}

fn environment_terminal_size() -> Option<(u16, u16)> {
    let columns = env::var(COLUMNS_ENVIRONMENT_VARIABLE)
        .ok()?
        .parse::<u16>()
        .ok()?
        .max(1);
    let rows = env::var(LINES_ENVIRONMENT_VARIABLE)
        .ok()?
        .parse::<u16>()
        .ok()?
        .max(1);
    Some((columns, rows))
}

struct AuthoritativeScreen {
    parser: vt100::Parser,
}

impl AuthoritativeScreen {
    fn new(columns: u16, rows: u16) -> Self {
        Self {
            parser: vt100::Parser::new(rows, columns, 0),
        }
    }

    fn process(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
    }

    fn resize(&mut self, columns: u16, rows: u16) {
        self.parser.screen_mut().set_size(rows, columns);
    }

    fn formatted(&self) -> Vec<u8> {
        self.parser.screen().contents_formatted()
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = crossterm::execute!(
            self.output,
            crossterm::event::DisableBracketedPaste,
            crossterm::event::DisableMouseCapture,
            crossterm::event::DisableFocusChange
        );
        if self.alternate_screen_active {
            let _ = self.output.write_all(b"\x1b[?1l");
            let _ = crossterm::execute!(self.output, crossterm::terminal::LeaveAlternateScreen);
        }
        let _ = disable_raw_mode();
    }
}

/// Converts a local key event to the byte sequence commonly emitted by an
/// xterm-compatible terminal.
#[must_use]
pub fn encode_key(event: KeyEvent) -> Option<Vec<u8>> {
    let modifiers = event.modifiers;
    let bytes = match event.code {
        KeyCode::Char(character) if modifiers.contains(KeyModifiers::CONTROL) => {
            encode_control(character).map(|byte| vec![byte])?
        }
        KeyCode::Char(character) => {
            let mut bytes = Vec::with_capacity(5);
            if modifiers.contains(KeyModifiers::ALT) {
                bytes.push(0x1b);
            }
            let mut encoded = [0_u8; 4];
            bytes.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
            bytes
        }
        KeyCode::Enter => b"\r".to_vec(),
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Tab => b"\t".to_vec(),
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::F(number) => encode_function_key(number)?.to_vec(),
        _ => return None,
    };
    Some(bytes)
}

#[must_use]
pub fn bytes_instruction(value: Vec<u8>) -> Instruction {
    Instruction {
        bytes: Some(ByteRun { value }),
        viewport: None,
        marker: None,
    }
}

#[must_use]
pub fn viewport_instruction(columns: u16, rows: u16) -> Instruction {
    Instruction {
        bytes: None,
        viewport: Some(ViewportSize {
            columns: u64::from(columns),
            rows: u64::from(rows),
        }),
        marker: None,
    }
}

/// Reconstructs the bracketed-paste delimiters consumed by the local event
/// parser so the remote application receives paste as a single transaction.
#[must_use]
pub fn encode_paste(contents: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(contents.len() + 12);
    bytes.extend_from_slice(b"\x1b[200~");
    bytes.extend_from_slice(contents.as_bytes());
    bytes.extend_from_slice(b"\x1b[201~");
    bytes
}

/// Converts a parsed mouse event back to the SGR mouse sequence expected by
/// remote terminal applications.
#[must_use]
pub fn encode_mouse(event: MouseEvent) -> Option<Vec<u8>> {
    let (base_code, released) = match event.kind {
        MouseEventKind::Down(button) => (mouse_button_code(button), false),
        MouseEventKind::Up(button) => (mouse_button_code(button), true),
        MouseEventKind::Drag(button) => (mouse_button_code(button) + 32, false),
        MouseEventKind::Moved => (35, false),
        MouseEventKind::ScrollUp => (64, false),
        MouseEventKind::ScrollDown => (65, false),
        MouseEventKind::ScrollLeft => (66, false),
        MouseEventKind::ScrollRight => (67, false),
    };
    let modifier_code = u16::from(event.modifiers.contains(KeyModifiers::SHIFT)) * 4
        + u16::from(event.modifiers.contains(KeyModifiers::ALT)) * 8
        + u16::from(event.modifiers.contains(KeyModifiers::CONTROL)) * 16;
    let code = base_code + modifier_code;
    let terminator = if released { 'm' } else { 'M' };
    Some(
        format!(
            "\x1b[<{};{};{}{}",
            code,
            u32::from(event.column) + 1,
            u32::from(event.row) + 1,
            terminator
        )
        .into_bytes(),
    )
}

#[must_use]
pub fn encode_focus(focused: bool) -> Vec<u8> {
    if focused {
        b"\x1b[I".to_vec()
    } else {
        b"\x1b[O".to_vec()
    }
}

fn encode_control(character: char) -> Option<u8> {
    match character {
        '@'..='_' => Some(character as u8 - b'@'),
        'a'..='z' => Some(character as u8 - b'a' + 1),
        '2' => Some(0x00),
        '3' => Some(0x1b),
        '4' => Some(0x1c),
        '5' => Some(0x1d),
        '6' => Some(0x1e),
        '7' => Some(0x1f),
        '8' | '?' => Some(0x7f),
        _ => None,
    }
}

fn encode_function_key(number: u8) -> Option<&'static [u8]> {
    match number {
        1 => Some(b"\x1bOP"),
        2 => Some(b"\x1bOQ"),
        3 => Some(b"\x1bOR"),
        4 => Some(b"\x1bOS"),
        5 => Some(b"\x1b[15~"),
        6 => Some(b"\x1b[17~"),
        7 => Some(b"\x1b[18~"),
        8 => Some(b"\x1b[19~"),
        9 => Some(b"\x1b[20~"),
        10 => Some(b"\x1b[21~"),
        11 => Some(b"\x1b[23~"),
        12 => Some(b"\x1b[24~"),
        _ => None,
    }
}

fn mouse_button_code(button: MouseButton) -> u16 {
    match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AuthoritativeScreen, EscapeAction, EscapeInterpreter, encode_focus, encode_key,
        encode_mouse, encode_paste, viewport_instruction,
    };
    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };

    #[test]
    fn encodes_text_control_and_navigation_keys() {
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Char('界'), KeyModifiers::NONE)),
            Some("界".as_bytes().to_vec())
        );
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(vec![3])
        );
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)),
            Some(b"\x1b[D".to_vec())
        );
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Char('6'), KeyModifiers::CONTROL)),
            Some(vec![0x1e])
        );
    }

    #[test]
    fn resize_uses_observed_viewport_fields() {
        let instruction = viewport_instruction(120, 40);
        let viewport = instruction.viewport.expect("viewport must be present");
        assert_eq!((viewport.columns, viewport.rows), (120, 40));
    }

    #[test]
    fn paste_preserves_transaction_boundaries() {
        assert_eq!(
            encode_paste("first\nsecond"),
            b"\x1b[200~first\nsecond\x1b[201~"
        );
    }

    #[test]
    fn mouse_and_focus_events_use_standard_terminal_sequences() {
        assert_eq!(
            encode_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 4,
                row: 2,
                modifiers: KeyModifiers::CONTROL,
            }),
            Some(b"\x1b[<16;5;3M".to_vec())
        );
        assert_eq!(encode_focus(true), b"\x1b[I");
        assert_eq!(encode_focus(false), b"\x1b[O");
    }

    #[test]
    fn authoritative_model_restores_utf8_color_and_cursor() {
        let mut model = AuthoritativeScreen::new(20, 4);
        model.process(b"plain \x1b[31mred\x1b[0m ");
        model.process("界e\u{301}".as_bytes());
        let formatted = model.formatted();

        let mut restored = vt100::Parser::new(4, 20, 0);
        restored.process(&formatted);
        assert_eq!(
            restored.screen().contents(),
            model.parser.screen().contents()
        );
        assert_eq!(
            restored.screen().cursor_position(),
            model.parser.screen().cursor_position()
        );
    }

    #[test]
    fn malformed_utf8_and_unknown_control_input_are_safe() {
        let mut model = AuthoritativeScreen::new(10, 2);
        model.process(&[0xff, 0xfe, 0x1b, b'[', b'?', b'9', b'9', b'9', b'9', b'h']);
        let formatted = model.formatted();
        let mut restored = vt100::Parser::new(2, 10, 0);
        restored.process(&formatted);
        assert_eq!(restored.screen().size(), (2, 10));
    }

    #[test]
    fn local_escape_quits_or_forwards_literal_prefix() {
        let mut interpreter = EscapeInterpreter::default();
        assert_eq!(interpreter.input(vec![0x1e]), EscapeAction::Hold);
        assert_eq!(interpreter.input(b".".to_vec()), EscapeAction::Quit);
        assert_eq!(interpreter.input(vec![0x1e]), EscapeAction::Hold);
        assert_eq!(
            interpreter.input(b"^".to_vec()),
            EscapeAction::Forward(vec![0x1e])
        );
        assert_eq!(interpreter.input(vec![0x1e]), EscapeAction::Hold);
        assert_eq!(
            interpreter.input(b"x".to_vec()),
            EscapeAction::Forward(vec![0x1e, b'x'])
        );
    }
}
