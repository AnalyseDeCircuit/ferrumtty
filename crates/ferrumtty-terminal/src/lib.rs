// SPDX-License-Identifier: GPL-3.0-only

//! Local terminal lifecycle and input translation.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use ferrumtty_wire::{ByteRun, Instruction, ViewportSize};
use std::env;
use std::fmt;
use std::io::{self, Write};

#[cfg(unix)]
use rustix::termios::{InputModes, OptionalActions, tcgetattr, tcsetattr};

const DEFAULT_ESCAPE_BYTE: u8 = 0x1e;
const COLUMNS_ENVIRONMENT_VARIABLE: &str = "COLUMNS";
const LINES_ENVIRONMENT_VARIABLE: &str = "LINES";
const SKIP_TERMINAL_INITIALIZATION_VARIABLE: &str = "MOSH_NO_TERM_INIT";
const MOSH_TITLE_PREFIX: &[u8] = b"[mosh] ";
const MAX_OSC_COMMAND_BYTES: usize = 16;
const MAX_REMOTE_TITLE_BYTES: usize = 1_024;
const CSI_MODIFIER_BASE: u8 = 1;
const SHIFT_MODIFIER_VALUE: u8 = 1;
const ALT_MODIFIER_VALUE: u8 = 2;
const CONTROL_MODIFIER_VALUE: u8 = 4;

/// Selects how unmodified cursor keys are encoded for the remote application.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CursorKeyMode {
    /// Uses ANSI CSI cursor sequences such as `ESC [ A`.
    #[default]
    Normal,
    /// Uses application cursor sequences such as `ESC O A`.
    Application,
}

/// Selects which remote mouse events should be reported.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MouseTrackingMode {
    /// Does not report mouse events.
    #[default]
    Disabled,
    /// Reports button presses and wheel events.
    Press,
    /// Reports button presses, releases, and wheel events.
    PressRelease,
    /// Also reports motion while a button is held.
    ButtonMotion,
    /// Reports all mouse motion.
    AnyMotion,
}

/// Selects the wire encoding for remote mouse events.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MouseEncoding {
    /// Uses the original single-byte X10 encoding.
    #[default]
    Default,
    /// Uses the extended UTF-8 coordinate encoding.
    Utf8,
    /// Uses the SGR decimal coordinate encoding.
    Sgr,
}

/// Selects the cursor appearance requested by the remote application.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CursorShape {
    #[default]
    Default,
    BlinkingBlock,
    SteadyBlock,
    BlinkingUnderline,
    SteadyUnderline,
    BlinkingBar,
    SteadyBar,
}

/// Represents a terminal mode with explicit enabled and disabled states.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TerminalMode {
    #[default]
    Disabled,
    Enabled,
}

/// Selects how remote terminal-title controls are presented locally.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TitlePolicy {
    /// Preserves remote output byte-for-byte.
    #[default]
    PreserveRemote,
    /// Adds the standard Mosh marker to the remote window title.
    MoshPrefix,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OscTitleCommand {
    IconAndWindow,
    Icon,
    Window,
}

#[derive(Debug)]
enum TitleFilterState {
    Ground,
    Escape,
    OscHeader {
        bytes: Vec<u8>,
        escape_pending: bool,
    },
    Title {
        command: OscTitleCommand,
        bytes: Vec<u8>,
        escape_pending: bool,
    },
    PassthroughOsc {
        escape_pending: bool,
    },
    DiscardTitle {
        escape_pending: bool,
    },
}

#[derive(Debug, Default)]
struct RemoteTitles {
    initialized: bool,
    icon: Vec<u8>,
    window: Vec<u8>,
}

/// Rewrites title OSC controls without forwarding their unprefixed form first.
///
/// The filter retains incomplete OSC input across calls. Other OSC commands are
/// passed through unchanged as soon as their command field is known.
pub struct TitleOutputFilter {
    policy: TitlePolicy,
    state: TitleFilterState,
    remote: RemoteTitles,
    prefix_active: bool,
    displayed: Option<(Vec<u8>, Vec<u8>)>,
}

impl fmt::Debug for TitleOutputFilter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TitleOutputFilter")
            .field("policy", &self.policy)
            .field("remote_title_initialized", &self.remote.initialized)
            .field("prefix_active", &self.prefix_active)
            .finish_non_exhaustive()
    }
}

impl TitleOutputFilter {
    #[must_use]
    pub fn new(policy: TitlePolicy) -> Self {
        Self {
            policy,
            state: TitleFilterState::Ground,
            remote: RemoteTitles::default(),
            prefix_active: policy == TitlePolicy::MoshPrefix,
            displayed: None,
        }
    }

    /// Filters one remote-output fragment and returns bytes safe to write once.
    #[must_use]
    pub fn process(&mut self, bytes: &[u8]) -> Vec<u8> {
        if self.policy == TitlePolicy::PreserveRemote {
            return bytes.to_vec();
        }
        let mut output = Vec::with_capacity(bytes.len());
        for &byte in bytes {
            self.process_byte(byte, &mut output);
        }
        output
    }

    /// Restores the latest unprefixed remote title before local suspension or exit.
    #[must_use]
    pub fn restore_remote_title(&mut self) -> Vec<u8> {
        if self.policy != TitlePolicy::MoshPrefix || !self.prefix_active {
            return Vec::new();
        }
        self.prefix_active = false;
        self.emit_titles()
    }

    /// Reapplies the configured title prefix after local resume.
    #[must_use]
    pub fn reapply_policy(&mut self) -> Vec<u8> {
        if self.policy != TitlePolicy::MoshPrefix || self.prefix_active {
            return Vec::new();
        }
        self.prefix_active = true;
        self.emit_titles()
    }

    // Keeping every byte-state transition together makes split OSC handling auditable.
    #[allow(clippy::too_many_lines)]
    fn process_byte(&mut self, byte: u8, output: &mut Vec<u8>) {
        let state = std::mem::replace(&mut self.state, TitleFilterState::Ground);
        self.state = match state {
            TitleFilterState::Ground => match byte {
                0x1b => TitleFilterState::Escape,
                0x9d => TitleFilterState::OscHeader {
                    bytes: vec![byte],
                    escape_pending: false,
                },
                _ => {
                    output.push(byte);
                    TitleFilterState::Ground
                }
            },
            TitleFilterState::Escape => {
                if byte == b']' {
                    TitleFilterState::OscHeader {
                        bytes: vec![0x1b, b']'],
                        escape_pending: false,
                    }
                } else {
                    output.extend_from_slice(&[0x1b, byte]);
                    TitleFilterState::Ground
                }
            }
            TitleFilterState::OscHeader {
                mut bytes,
                mut escape_pending,
            } => {
                if escape_pending {
                    escape_pending = false;
                    if byte == b'\\' {
                        bytes.extend_from_slice(b"\x1b\\");
                        output.extend_from_slice(&bytes);
                        return;
                    }
                    bytes.push(0x1b);
                }
                match byte {
                    0x07 | 0x9c | 0x18 | 0x1a => {
                        bytes.push(byte);
                        output.extend_from_slice(&bytes);
                        TitleFilterState::Ground
                    }
                    0x1b => TitleFilterState::OscHeader {
                        bytes,
                        escape_pending: true,
                    },
                    b';' => {
                        let command = title_command(&bytes);
                        bytes.push(byte);
                        if let Some(command) = command {
                            TitleFilterState::Title {
                                command,
                                bytes: Vec::new(),
                                escape_pending: false,
                            }
                        } else {
                            output.extend_from_slice(&bytes);
                            TitleFilterState::PassthroughOsc {
                                escape_pending: false,
                            }
                        }
                    }
                    _ => {
                        bytes.push(byte);
                        if osc_command_length(&bytes) > MAX_OSC_COMMAND_BYTES {
                            output.extend_from_slice(&bytes);
                            TitleFilterState::PassthroughOsc {
                                escape_pending: false,
                            }
                        } else {
                            TitleFilterState::OscHeader {
                                bytes,
                                escape_pending,
                            }
                        }
                    }
                }
            }
            TitleFilterState::Title {
                command,
                mut bytes,
                mut escape_pending,
            } => {
                let mut capacity_reached = false;
                if escape_pending {
                    escape_pending = false;
                    if byte == b'\\' {
                        self.set_remote_title(command, bytes);
                        output.extend_from_slice(&self.emit_titles());
                        return;
                    }
                    if bytes.len() < MAX_REMOTE_TITLE_BYTES {
                        bytes.push(0x1b);
                    } else {
                        capacity_reached = true;
                    }
                }
                match byte {
                    0x07 | 0x9c => {
                        self.set_remote_title(command, bytes);
                        output.extend_from_slice(&self.emit_titles());
                        TitleFilterState::Ground
                    }
                    0x18 | 0x1a => TitleFilterState::Ground,
                    0x1b => TitleFilterState::Title {
                        command,
                        bytes,
                        escape_pending: true,
                    },
                    _ if capacity_reached || bytes.len() == MAX_REMOTE_TITLE_BYTES => {
                        TitleFilterState::DiscardTitle {
                            escape_pending: false,
                        }
                    }
                    _ => {
                        bytes.push(byte);
                        TitleFilterState::Title {
                            command,
                            bytes,
                            escape_pending,
                        }
                    }
                }
            }
            TitleFilterState::PassthroughOsc { mut escape_pending } => {
                output.push(byte);
                if escape_pending {
                    escape_pending = false;
                    if byte == b'\\' {
                        return;
                    }
                }
                match byte {
                    0x07 | 0x9c | 0x18 | 0x1a => TitleFilterState::Ground,
                    0x1b => TitleFilterState::PassthroughOsc {
                        escape_pending: true,
                    },
                    _ => TitleFilterState::PassthroughOsc { escape_pending },
                }
            }
            TitleFilterState::DiscardTitle { mut escape_pending } => {
                if escape_pending {
                    escape_pending = false;
                    if byte == b'\\' {
                        return;
                    }
                }
                match byte {
                    0x07 | 0x9c | 0x18 | 0x1a => TitleFilterState::Ground,
                    0x1b => TitleFilterState::DiscardTitle {
                        escape_pending: true,
                    },
                    _ => TitleFilterState::DiscardTitle { escape_pending },
                }
            }
        };
    }

    fn set_remote_title(&mut self, command: OscTitleCommand, bytes: Vec<u8>) {
        self.remote.initialized = true;
        match command {
            OscTitleCommand::IconAndWindow => {
                self.remote.icon.clone_from(&bytes);
                self.remote.window = bytes;
            }
            OscTitleCommand::Icon => self.remote.icon = bytes,
            OscTitleCommand::Window => self.remote.window = bytes,
        }
    }

    fn emit_titles(&mut self) -> Vec<u8> {
        if !self.remote.initialized {
            return Vec::new();
        }
        let mut window = Vec::new();
        if self.prefix_active {
            window.extend_from_slice(MOSH_TITLE_PREFIX);
        }
        window.extend_from_slice(&self.remote.window);
        let icon = if self.prefix_active && self.remote.icon == self.remote.window {
            window.clone()
        } else {
            self.remote.icon.clone()
        };
        if self.displayed.as_ref() == Some(&(icon.clone(), window.clone())) {
            return Vec::new();
        }
        self.displayed = Some((icon.clone(), window.clone()));
        encode_titles(&icon, &window)
    }
}

fn osc_command_length(bytes: &[u8]) -> usize {
    bytes
        .len()
        .saturating_sub(if bytes.starts_with(b"\x1b]") { 2 } else { 1 })
}

fn title_command(bytes: &[u8]) -> Option<OscTitleCommand> {
    let offset = if bytes.starts_with(b"\x1b]") { 2 } else { 1 };
    match bytes.get(offset..) {
        Some(b"" | b"0") => Some(OscTitleCommand::IconAndWindow),
        Some(b"1") => Some(OscTitleCommand::Icon),
        Some(b"2") => Some(OscTitleCommand::Window),
        _ => None,
    }
}

fn encode_titles(icon: &[u8], window: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(icon.len() + window.len() + 10);
    if icon == window {
        output.extend_from_slice(b"\x1b]0;");
        output.extend_from_slice(window);
        output.push(0x07);
    } else {
        output.extend_from_slice(b"\x1b]1;");
        output.extend_from_slice(icon);
        output.extend_from_slice(b"\x07\x1b]2;");
        output.extend_from_slice(window);
        output.push(0x07);
    }
    output
}

impl From<bool> for TerminalMode {
    fn from(enabled: bool) -> Self {
        if enabled {
            Self::Enabled
        } else {
            Self::Disabled
        }
    }
}

/// Terminal modes selected by authoritative remote output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RemoteTerminalModes {
    pub cursor_key: CursorKeyMode,
    pub application_keypad: bool,
    pub bracketed_paste: bool,
    pub mouse_tracking: MouseTrackingMode,
    pub mouse_encoding: MouseEncoding,
    pub focus_reporting: bool,
    pub insert_mode: TerminalMode,
    pub origin_mode: TerminalMode,
    pub auto_wrap: TerminalMode,
    pub cursor_visible: TerminalMode,
    pub cursor_shape: CursorShape,
    pub alternate_screen: TerminalMode,
    /// Stores the one-based inclusive top and bottom margins.
    pub scroll_region: Option<(u16, u16)>,
}

impl Default for RemoteTerminalModes {
    fn default() -> Self {
        Self {
            cursor_key: CursorKeyMode::Normal,
            application_keypad: false,
            bracketed_paste: false,
            mouse_tracking: MouseTrackingMode::Disabled,
            mouse_encoding: MouseEncoding::Default,
            focus_reporting: false,
            insert_mode: TerminalMode::Disabled,
            origin_mode: TerminalMode::Disabled,
            auto_wrap: TerminalMode::Enabled,
            cursor_visible: TerminalMode::Enabled,
            cursor_shape: CursorShape::Default,
            alternate_screen: TerminalMode::Disabled,
            scroll_region: None,
        }
    }
}

#[derive(Eq, PartialEq)]
pub enum EscapeAction {
    Hold,
    Forward(Vec<u8>),
    Help,
    Quit,
    Suspend,
}

impl fmt::Debug for EscapeAction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hold => formatter.write_str("Hold"),
            Self::Forward(bytes) => formatter
                .debug_struct("Forward")
                .field("bytes", &bytes.len())
                .finish(),
            Self::Help => formatter.write_str("Help"),
            Self::Quit => formatter.write_str("Quit"),
            Self::Suspend => formatter.write_str("Suspend"),
        }
    }
}

/// Interprets the configured Mosh local-command prefix.
pub struct EscapeInterpreter {
    escape_byte: u8,
    prefix_held: bool,
    at_line_start: bool,
}

impl Default for EscapeInterpreter {
    fn default() -> Self {
        Self::new(DEFAULT_ESCAPE_BYTE)
    }
}

impl EscapeInterpreter {
    #[must_use]
    pub const fn new(escape_byte: u8) -> Self {
        Self {
            escape_byte,
            prefix_held: false,
            at_line_start: true,
        }
    }

    #[must_use]
    pub fn input(&mut self, bytes: Vec<u8>) -> EscapeAction {
        if !self.prefix_held {
            let printable_escape = self.escape_byte.is_ascii_graphic() || self.escape_byte == b' ';
            if bytes == [self.escape_byte] && (!printable_escape || self.at_line_start) {
                self.prefix_held = true;
                return EscapeAction::Hold;
            }
            return self.forward(bytes);
        }
        self.prefix_held = false;
        if bytes == b"." {
            return EscapeAction::Quit;
        }
        if bytes == b"?" {
            return EscapeAction::Help;
        }
        if bytes == [0x1a] {
            return EscapeAction::Suspend;
        }
        if bytes == [self.literal_escape_suffix()] {
            return self.forward(vec![self.escape_byte]);
        }
        let mut forwarded = Vec::with_capacity(bytes.len() + 1);
        forwarded.push(self.escape_byte);
        forwarded.extend(bytes);
        self.forward(forwarded)
    }

    #[must_use]
    pub fn flush(&mut self) -> Option<Vec<u8>> {
        self.prefix_held.then(|| {
            self.prefix_held = false;
            self.at_line_start = false;
            vec![self.escape_byte]
        })
    }

    fn literal_escape_suffix(&self) -> u8 {
        match self.escape_byte {
            1..=31 => self.escape_byte + 64,
            127 => b'?',
            printable => printable,
        }
    }

    fn forward(&mut self, bytes: Vec<u8>) -> EscapeAction {
        self.at_line_start = bytes
            .last()
            .is_some_and(|byte| matches!(byte, b'\r' | b'\n'));
        EscapeAction::Forward(bytes)
    }
}

/// Restores the local terminal mode when the client exits through normal
/// returns or unwinding.
pub struct TerminalGuard {
    output: io::Stdout,
    authoritative: AuthoritativeScreen,
    title_output: TitleOutputFilter,
    alternate_screen_active: bool,
    session_terminal_active: bool,
    local_notice_active: bool,
}

/// Authoritative cursor information used by a host-side prediction overlay.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PredictionCursorContext {
    pub row: u16,
    pub column: u16,
    pub columns: u16,
    pub attributes: Vec<u8>,
    pub cursor_state: Vec<u8>,
}

impl TerminalGuard {
    /// Enables raw input and bracketed-paste reporting.
    ///
    /// # Errors
    ///
    /// Returns an operating-system error if the terminal cannot be changed.
    pub fn enter() -> io::Result<Self> {
        Self::enter_with_title_policy(TitlePolicy::PreserveRemote)
    }

    /// Enables terminal input and applies an explicit remote-title policy.
    ///
    /// # Errors
    ///
    /// Returns an operating-system error if the terminal cannot be changed.
    pub fn enter_with_title_policy(title_policy: TitlePolicy) -> io::Result<Self> {
        let (columns, rows) = terminal_size()?;
        enable_session_raw_mode()?;
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
            title_output: TitleOutputFilter::new(title_policy),
            alternate_screen_active,
            session_terminal_active: true,
            local_notice_active: false,
        })
    }

    /// Writes authoritative terminal bytes received from the server.
    ///
    /// # Errors
    ///
    /// Returns an error if standard output cannot be written or flushed.
    pub fn write_server_bytes(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.clear_local_notice()?;
        self.authoritative.process(bytes);
        let filtered = self.title_output.process(bytes);
        self.write_bytes(&filtered)
    }

    /// Writes a temporary local overlay without changing authoritative state.
    ///
    /// # Errors
    ///
    /// Returns an error if standard output cannot be written or flushed.
    pub fn write_overlay_bytes(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.write_bytes(bytes)
    }

    /// Writes a local notice that will be removed before subsequent activity.
    ///
    /// # Errors
    ///
    /// Returns an error if standard output cannot be written or flushed.
    pub fn write_local_notice(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.clear_local_notice()?;
        self.write_bytes(bytes)?;
        self.local_notice_active = true;
        Ok(())
    }

    /// Removes a local notice by reconstructing authoritative remote state.
    ///
    /// # Errors
    ///
    /// Returns an error if standard output cannot be written or flushed.
    pub fn clear_local_notice(&mut self) -> io::Result<()> {
        if !self.local_notice_active {
            return Ok(());
        }
        self.local_notice_active = false;
        self.redraw_authoritative()
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

    /// Restores the local terminal before the process enters job-control suspension.
    ///
    /// # Errors
    ///
    /// Returns an operating-system error if terminal modes cannot be restored.
    pub fn leave_for_suspend(&mut self) -> io::Result<()> {
        self.local_notice_active = false;
        self.deactivate_terminal()
    }

    /// Re-enters session terminal modes and redraws authoritative state after resume.
    ///
    /// # Errors
    ///
    /// Returns an operating-system error if terminal modes cannot be enabled or redrawn.
    pub fn resume_after_suspend(&mut self) -> io::Result<()> {
        if self.session_terminal_active {
            return Ok(());
        }
        enable_session_raw_mode()?;
        let setup_result = if self.alternate_screen_active {
            crossterm::execute!(
                self.output,
                crossterm::terminal::EnterAlternateScreen,
                crossterm::event::EnableBracketedPaste
            )
            .and_then(|()| self.output.write_all(b"\x1b[?1h"))
            .and_then(|()| self.output.flush())
        } else {
            crossterm::execute!(self.output, crossterm::event::EnableBracketedPaste)
        };
        if let Err(error) = setup_result {
            let _ = disable_raw_mode();
            return Err(error);
        }
        self.session_terminal_active = true;
        self.local_notice_active = false;
        self.redraw_authoritative()?;
        let title = self.title_output.reapply_policy();
        self.write_bytes(&title)
    }

    /// Returns the input modes selected by authoritative remote output.
    #[must_use]
    pub fn remote_modes(&self) -> RemoteTerminalModes {
        self.authoritative.remote_modes()
    }

    /// Returns the latest title selected by authoritative remote output.
    #[must_use]
    pub fn remote_title(&self) -> Option<&str> {
        self.authoritative.remote_title()
    }

    /// Captures the modeled remote cursor without observing the temporary
    /// local overlay written directly to the terminal.
    #[must_use]
    pub fn prediction_cursor_context(&self) -> PredictionCursorContext {
        self.authoritative.prediction_cursor_context()
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.output.write_all(bytes)?;
        self.output.flush()
    }

    fn deactivate_terminal(&mut self) -> io::Result<()> {
        if !self.session_terminal_active {
            return Ok(());
        }
        let mut first_error = crossterm::execute!(
            self.output,
            crossterm::event::DisableBracketedPaste,
            crossterm::event::DisableMouseCapture,
            crossterm::event::DisableFocusChange
        )
        .err();
        let remote_title = self.title_output.restore_remote_title();
        if let Err(error) = self.write_bytes(&remote_title) {
            first_error.get_or_insert(error);
        }
        if self.alternate_screen_active {
            if let Err(error) = self.output.write_all(b"\x1b[?1l") {
                first_error.get_or_insert(error);
            }
            if let Err(error) =
                crossterm::execute!(self.output, crossterm::terminal::LeaveAlternateScreen)
            {
                first_error.get_or_insert(error);
            }
        }
        if let Err(error) = disable_raw_mode() {
            first_error.get_or_insert(error);
        }
        self.session_terminal_active = false;
        first_error.map_or(Ok(()), Err)
    }
}

fn enable_session_raw_mode() -> io::Result<()> {
    enable_raw_mode()?;
    if let Err(error) = enable_utf8_input_mode() {
        let _ = disable_raw_mode();
        return Err(error);
    }
    Ok(())
}

#[cfg(all(
    unix,
    not(any(
        target_os = "aix",
        target_os = "emscripten",
        target_os = "freebsd",
        target_os = "haiku",
        target_os = "hurd",
        target_os = "illumos",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "redox",
        target_os = "solaris"
    ))
))]
fn enable_utf8_input_mode() -> io::Result<()> {
    let input = io::stdin();
    let mut attributes = tcgetattr(&input).map_err(io::Error::from)?;
    attributes.input_modes.insert(InputModes::IUTF8);
    tcsetattr(&input, OptionalActions::Now, &attributes).map_err(io::Error::from)
}

#[cfg(any(
    windows,
    all(
        unix,
        any(
            target_os = "aix",
            target_os = "emscripten",
            target_os = "freebsd",
            target_os = "haiku",
            target_os = "hurd",
            target_os = "illumos",
            target_os = "netbsd",
            target_os = "openbsd",
            target_os = "redox",
            target_os = "solaris"
        )
    )
))]
const fn enable_utf8_input_mode() -> io::Result<()> {
    Ok(())
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
    focus_parser: vte::Parser,
    focus_state: FocusReportingState,
}

impl AuthoritativeScreen {
    fn new(columns: u16, rows: u16) -> Self {
        Self {
            parser: vt100::Parser::new(rows, columns, 0),
            focus_parser: vte::Parser::new(),
            focus_state: FocusReportingState::default(),
        }
    }

    fn process(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
        self.focus_parser.advance(&mut self.focus_state, bytes);
    }

    fn resize(&mut self, columns: u16, rows: u16) {
        self.parser.screen_mut().set_size(rows, columns);
        self.focus_state.scroll_region = None;
    }

    fn formatted(&self) -> Vec<u8> {
        self.parser.screen().contents_formatted()
    }

    fn remote_modes(&self) -> RemoteTerminalModes {
        let screen = self.parser.screen();
        RemoteTerminalModes {
            cursor_key: if screen.application_cursor() {
                CursorKeyMode::Application
            } else {
                CursorKeyMode::Normal
            },
            application_keypad: screen.application_keypad(),
            bracketed_paste: screen.bracketed_paste(),
            mouse_tracking: match screen.mouse_protocol_mode() {
                vt100::MouseProtocolMode::None => MouseTrackingMode::Disabled,
                vt100::MouseProtocolMode::Press => MouseTrackingMode::Press,
                vt100::MouseProtocolMode::PressRelease => MouseTrackingMode::PressRelease,
                vt100::MouseProtocolMode::ButtonMotion => MouseTrackingMode::ButtonMotion,
                vt100::MouseProtocolMode::AnyMotion => MouseTrackingMode::AnyMotion,
            },
            mouse_encoding: match screen.mouse_protocol_encoding() {
                vt100::MouseProtocolEncoding::Default => MouseEncoding::Default,
                vt100::MouseProtocolEncoding::Utf8 => MouseEncoding::Utf8,
                vt100::MouseProtocolEncoding::Sgr => MouseEncoding::Sgr,
            },
            focus_reporting: self.focus_state.enabled,
            insert_mode: self.focus_state.insert_mode,
            origin_mode: self.focus_state.origin_mode,
            auto_wrap: self.focus_state.auto_wrap,
            cursor_visible: TerminalMode::from(!screen.hide_cursor()),
            cursor_shape: self.focus_state.cursor_shape,
            alternate_screen: TerminalMode::from(screen.alternate_screen()),
            scroll_region: self.focus_state.scroll_region,
        }
    }

    fn remote_title(&self) -> Option<&str> {
        self.focus_state.title.as_deref()
    }

    fn prediction_cursor_context(&self) -> PredictionCursorContext {
        let screen = self.parser.screen();
        let (row, column) = screen.cursor_position();
        let (_, columns) = screen.size();
        PredictionCursorContext {
            row,
            column,
            columns,
            attributes: screen.attributes_formatted(),
            cursor_state: screen.cursor_state_formatted(),
        }
    }
}

struct FocusReportingState {
    enabled: bool,
    insert_mode: TerminalMode,
    origin_mode: TerminalMode,
    auto_wrap: TerminalMode,
    cursor_shape: CursorShape,
    scroll_region: Option<(u16, u16)>,
    title: Option<String>,
}

impl Default for FocusReportingState {
    fn default() -> Self {
        Self {
            enabled: false,
            insert_mode: TerminalMode::Disabled,
            origin_mode: TerminalMode::Disabled,
            auto_wrap: TerminalMode::Enabled,
            cursor_shape: CursorShape::Default,
            scroll_region: None,
            title: None,
        }
    }
}

impl vte::Perform for FocusReportingState {
    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        intermediates: &[u8],
        ignore: bool,
        action: char,
    ) {
        if ignore {
            return;
        }
        if intermediates == b"?" && matches!(action, 'h' | 'l') {
            let enabled = action == 'h';
            for parameter in params {
                match parameter {
                    [6] => self.origin_mode = TerminalMode::from(enabled),
                    [7] => self.auto_wrap = TerminalMode::from(enabled),
                    [1004] => self.enabled = enabled,
                    _ => {}
                }
            }
            return;
        }
        if intermediates.is_empty() && matches!(action, 'h' | 'l') {
            if params.iter().any(|parameter| parameter == [4]) {
                self.insert_mode = TerminalMode::from(action == 'h');
            }
            return;
        }
        if intermediates.is_empty() && action == 'r' {
            let mut margins = params
                .iter()
                .filter_map(|parameter| parameter.first().copied());
            self.scroll_region = match (margins.next(), margins.next()) {
                (Some(top), Some(bottom)) if top > 0 && bottom >= top => Some((top, bottom)),
                _ => None,
            };
            return;
        }
        if intermediates == b" " && action == 'q' {
            let shape = params
                .iter()
                .next()
                .and_then(|parameter| parameter.first())
                .copied()
                .unwrap_or(0);
            self.cursor_shape = match shape {
                1 => CursorShape::BlinkingBlock,
                2 => CursorShape::SteadyBlock,
                3 => CursorShape::BlinkingUnderline,
                4 => CursorShape::SteadyUnderline,
                5 => CursorShape::BlinkingBar,
                6 => CursorShape::SteadyBar,
                _ => CursorShape::Default,
            };
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        let Some(command) = params.first() else {
            return;
        };
        if !matches!(*command, b"" | b"0" | b"2") {
            return;
        }
        // VTE separates every semicolon-delimited OSC parameter, including
        // semicolons that belong to the title itself.
        let title_bytes = params.get(1..).unwrap_or_default().join(&b';');
        if let Ok(title) = String::from_utf8(title_bytes) {
            self.title = Some(title);
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        if !ignore && intermediates.is_empty() && byte == b'c' {
            *self = Self::default();
        }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = self.deactivate_terminal();
    }
}

/// Converts a local key event to the byte sequence commonly emitted by an
/// xterm-compatible terminal.
#[must_use]
pub fn encode_key(event: KeyEvent) -> Option<Vec<u8>> {
    encode_key_with_mode(event, CursorKeyMode::Normal)
}

/// Converts a local key event using the selected remote cursor-key mode.
#[must_use]
pub fn encode_key_with_mode(event: KeyEvent, cursor_key_mode: CursorKeyMode) -> Option<Vec<u8>> {
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
        KeyCode::Up => encode_cursor_key(b'A', modifiers, cursor_key_mode),
        KeyCode::Down => encode_cursor_key(b'B', modifiers, cursor_key_mode),
        KeyCode::Right => encode_cursor_key(b'C', modifiers, cursor_key_mode),
        KeyCode::Left => encode_cursor_key(b'D', modifiers, cursor_key_mode),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::F(number) => encode_function_key(number, modifiers)?,
        _ => return None,
    };
    Some(bytes)
}

fn encode_cursor_key(
    final_byte: u8,
    modifiers: KeyModifiers,
    cursor_key_mode: CursorKeyMode,
) -> Vec<u8> {
    if let Some(modifier_parameter) = key_modifier_parameter(modifiers) {
        return format!("\x1b[1;{modifier_parameter}{}", char::from(final_byte)).into_bytes();
    }

    let introducer = match cursor_key_mode {
        CursorKeyMode::Normal => b'[',
        CursorKeyMode::Application => b'O',
    };
    vec![0x1b, introducer, final_byte]
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
    encode_paste_with_mode(contents, true)
}

/// Encodes paste contents according to the remote bracketed-paste mode.
#[must_use]
pub fn encode_paste_with_mode(contents: &str, bracketed_paste: bool) -> Vec<u8> {
    if !bracketed_paste {
        return contents.as_bytes().to_vec();
    }
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
    encode_mouse_with_mode(event, MouseTrackingMode::AnyMotion, MouseEncoding::Sgr)
}

/// Encodes a mouse event according to the remote tracking and encoding modes.
#[must_use]
pub fn encode_mouse_with_mode(
    event: MouseEvent,
    tracking: MouseTrackingMode,
    encoding: MouseEncoding,
) -> Option<Vec<u8>> {
    if !mouse_event_is_reportable(event.kind, tracking) {
        return None;
    }
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
    match encoding {
        MouseEncoding::Sgr => {
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
        MouseEncoding::Default => encode_legacy_mouse(event, code, released, false),
        MouseEncoding::Utf8 => encode_legacy_mouse(event, code, released, true),
    }
}

fn mouse_event_is_reportable(kind: MouseEventKind, tracking: MouseTrackingMode) -> bool {
    match tracking {
        MouseTrackingMode::Disabled => false,
        MouseTrackingMode::Press => matches!(
            kind,
            MouseEventKind::Down(_)
                | MouseEventKind::ScrollUp
                | MouseEventKind::ScrollDown
                | MouseEventKind::ScrollLeft
                | MouseEventKind::ScrollRight
        ),
        MouseTrackingMode::PressRelease => {
            !matches!(kind, MouseEventKind::Drag(_) | MouseEventKind::Moved)
        }
        MouseTrackingMode::ButtonMotion => !matches!(kind, MouseEventKind::Moved),
        MouseTrackingMode::AnyMotion => true,
    }
}

fn encode_legacy_mouse(
    event: MouseEvent,
    code: u16,
    released: bool,
    utf8: bool,
) -> Option<Vec<u8>> {
    let legacy_code = if released { 3 } else { code } + 32;
    let column = u32::from(event.column) + 33;
    let row = u32::from(event.row) + 33;
    let mut bytes = b"\x1b[M".to_vec();
    if utf8 {
        for value in [u32::from(legacy_code), column, row] {
            let character = char::from_u32(value)?;
            let mut encoded = [0_u8; 4];
            bytes.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
        }
    } else {
        bytes.extend([
            u8::try_from(legacy_code).ok()?,
            u8::try_from(column).ok()?,
            u8::try_from(row).ok()?,
        ]);
    }
    Some(bytes)
}

#[must_use]
pub fn encode_focus(focused: bool) -> Vec<u8> {
    if focused {
        b"\x1b[I".to_vec()
    } else {
        b"\x1b[O".to_vec()
    }
}

/// Encodes focus changes only when requested by the remote application.
#[must_use]
pub fn encode_focus_with_mode(focused: bool, focus_reporting: bool) -> Option<Vec<u8>> {
    focus_reporting.then(|| encode_focus(focused))
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

fn encode_function_key(number: u8, modifiers: KeyModifiers) -> Option<Vec<u8>> {
    let modifier_parameter = key_modifier_parameter(modifiers);
    match (number, modifier_parameter) {
        (1..=4, None) => Some(vec![0x1b, b'O', b'P' + number - 1]),
        (1..=4, Some(parameter)) => {
            Some(format!("\x1b[1;{parameter}{}", char::from(b'P' + number - 1)).into_bytes())
        }
        (5..=12, parameter) => {
            let key_parameter = match number {
                5 => 15,
                6 => 17,
                7 => 18,
                8 => 19,
                9 => 20,
                10 => 21,
                11 => 23,
                12 => 24,
                _ => unreachable!(),
            };
            Some(match parameter {
                Some(parameter) => format!("\x1b[{key_parameter};{parameter}~").into_bytes(),
                None => format!("\x1b[{key_parameter}~").into_bytes(),
            })
        }
        _ => None,
    }
}

fn key_modifier_parameter(modifiers: KeyModifiers) -> Option<u8> {
    let modifier_value = u8::from(modifiers.contains(KeyModifiers::SHIFT)) * SHIFT_MODIFIER_VALUE
        + u8::from(modifiers.contains(KeyModifiers::ALT)) * ALT_MODIFIER_VALUE
        + u8::from(modifiers.contains(KeyModifiers::CONTROL)) * CONTROL_MODIFIER_VALUE;
    (modifier_value != 0).then_some(CSI_MODIFIER_BASE + modifier_value)
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
        AuthoritativeScreen, CursorKeyMode, CursorShape, EscapeAction, EscapeInterpreter,
        MAX_REMOTE_TITLE_BYTES, MouseEncoding, MouseTrackingMode, RemoteTerminalModes,
        TerminalMode, TitleOutputFilter, TitlePolicy, encode_focus, encode_focus_with_mode,
        encode_key, encode_key_with_mode, encode_mouse, encode_mouse_with_mode, encode_paste,
        encode_paste_with_mode, viewport_instruction,
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
    fn cursor_mode_changes_unmodified_direction_keys() {
        let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(
            encode_key_with_mode(up, CursorKeyMode::Normal),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            encode_key_with_mode(up, CursorKeyMode::Application),
            Some(b"\x1bOA".to_vec())
        );
    }

    #[test]
    fn authoritative_modes_follow_fragmented_remote_sequences() {
        let mut model = AuthoritativeScreen::new(20, 4);
        model.process(b"\x1b[?");
        assert_eq!(model.remote_modes(), RemoteTerminalModes::default());

        model.process(b"1;1004;2004h\x1b=\x1b[?1002;1006h");
        assert_eq!(
            model.remote_modes(),
            RemoteTerminalModes {
                cursor_key: CursorKeyMode::Application,
                application_keypad: true,
                bracketed_paste: true,
                mouse_tracking: MouseTrackingMode::ButtonMotion,
                mouse_encoding: MouseEncoding::Sgr,
                focus_reporting: true,
                ..RemoteTerminalModes::default()
            }
        );

        model.process(b"\x1b[?1;1004;2004l\x1b>\x1b[?1002;1006l");
        assert_eq!(model.remote_modes(), RemoteTerminalModes::default());
    }

    #[test]
    fn terminal_reset_clears_focus_reporting() {
        let mut model = AuthoritativeScreen::new(20, 4);
        model.process(b"\x1b[?1004h");
        assert!(model.remote_modes().focus_reporting);
        model.process(b"\x1bc");
        assert_eq!(model.remote_modes(), RemoteTerminalModes::default());
    }

    #[test]
    fn tracks_extended_terminal_modes_and_title() {
        let mut model = AuthoritativeScreen::new(20, 4);
        model.process(b"\x1b[4h\x1b[?6;7l\x1b[2;4r\x1b[5 q\x1b[?25l\x1b[?1049h");
        model.process(b"\x1b]2;remote title\x07");

        assert_eq!(
            model.remote_modes(),
            RemoteTerminalModes {
                insert_mode: TerminalMode::Enabled,
                origin_mode: TerminalMode::Disabled,
                auto_wrap: TerminalMode::Disabled,
                cursor_visible: TerminalMode::Disabled,
                cursor_shape: CursorShape::BlinkingBar,
                alternate_screen: TerminalMode::Enabled,
                scroll_region: Some((2, 4)),
                ..RemoteTerminalModes::default()
            }
        );
        assert_eq!(model.remote_title(), Some("remote title"));
    }

    #[test]
    fn prediction_context_uses_authoritative_cursor_and_rendition() {
        let mut model = AuthoritativeScreen::new(20, 4);
        model.process(b"\x1b[3;5H\x1b[1m");

        let context = model.prediction_cursor_context();
        assert_eq!((context.row, context.column, context.columns), (2, 4, 20));
        assert!(!context.attributes.is_empty());
        assert!(!context.cursor_state.is_empty());
    }

    #[test]
    fn title_tracking_preserves_semicolons_and_ignores_icon_only_updates() {
        let mut model = AuthoritativeScreen::new(20, 4);
        model.process(b"\x1b];first;second\x07");
        assert_eq!(model.remote_title(), Some("first;second"));
        model.process(b"\x1b]1;icon only\x07");
        assert_eq!(model.remote_title(), Some("first;second"));
    }

    #[test]
    fn title_filter_rewrites_title_controls_once() {
        let mut filter = TitleOutputFilter::new(TitlePolicy::MoshPrefix);
        assert_eq!(
            filter.process(b"before\x1b]0;remote\x07after"),
            b"before\x1b]0;[mosh] remote\x07after"
        );
        assert_eq!(
            filter.process(b"\x1b]1;remote icon\x1b\\"),
            b"\x1b]1;remote icon\x07\x1b]2;[mosh] remote\x07"
        );
    }

    #[test]
    fn title_filter_preserves_other_osc_commands_exactly() {
        let sequences = [
            b"\x1b]52;c;c2VjcmV0\x07".as_slice(),
            b"\x1b]8;;https://example.test\x1b\\label".as_slice(),
            b"\x1b]10;rgb:00/00/00\x1b\\".as_slice(),
        ];
        let mut filter = TitleOutputFilter::new(TitlePolicy::MoshPrefix);
        for sequence in sequences {
            assert_eq!(filter.process(sequence), sequence);
        }
    }

    #[test]
    fn title_filter_is_stable_across_every_fragment_boundary() {
        let sequence = b"a\x1b]2;one;two\x1b\\b\x1b]52;c;payload\x07c";
        let expected = TitleOutputFilter::new(TitlePolicy::MoshPrefix).process(sequence);
        for split in 0..=sequence.len() {
            let mut filter = TitleOutputFilter::new(TitlePolicy::MoshPrefix);
            let mut fragmented = filter.process(&sequence[..split]);
            fragmented.extend_from_slice(&filter.process(&sequence[split..]));
            assert_eq!(fragmented, expected, "split at byte {split}");
        }
    }

    #[test]
    fn title_filter_supports_c1_controls_and_cancelled_titles() {
        let mut filter = TitleOutputFilter::new(TitlePolicy::MoshPrefix);
        assert_eq!(
            filter.process(b"\x9d0;c1 title\x9ctail"),
            b"\x1b]0;[mosh] c1 title\x07tail"
        );
        assert_eq!(filter.process(b"\x1b]2;cancelled\x18tail"), b"tail");
        assert_eq!(filter.process(b"\x1b]2;cancelled\x1atail"), b"tail");
    }

    #[test]
    fn title_filter_bounds_incomplete_title_storage_and_recovers() {
        let mut oversized = b"\x1b]2;".to_vec();
        oversized.extend(std::iter::repeat_n(b'x', MAX_REMOTE_TITLE_BYTES + 1));
        oversized.extend_from_slice(b"\x07visible");
        let mut filter = TitleOutputFilter::new(TitlePolicy::MoshPrefix);
        assert_eq!(filter.process(&oversized), b"visible");
        assert_eq!(
            filter.process(b"\x1b]0;recovered\x07"),
            b"\x1b]0;[mosh] recovered\x07"
        );

        let mut escaped_overflow = b"\x1b]2;".to_vec();
        escaped_overflow.extend(std::iter::repeat_n(b'x', MAX_REMOTE_TITLE_BYTES));
        escaped_overflow.extend_from_slice(b"\x1bX\x07visible");
        assert_eq!(filter.process(&escaped_overflow), b"visible");
    }

    #[test]
    fn title_filter_restores_and_reapplies_remote_title() {
        let mut filter = TitleOutputFilter::new(TitlePolicy::MoshPrefix);
        assert_eq!(
            filter.process(b"\x1b]0;remote\x07"),
            b"\x1b]0;[mosh] remote\x07"
        );
        assert_eq!(filter.restore_remote_title(), b"\x1b]0;remote\x07");
        assert_eq!(filter.reapply_policy(), b"\x1b]0;[mosh] remote\x07");
    }

    #[test]
    fn title_filter_debug_redacts_remote_titles() {
        let mut filter = TitleOutputFilter::new(TitlePolicy::MoshPrefix);
        let _ = filter.process(b"\x1b]2;remote-title-sentinel\x07");
        let output = format!("{filter:?}");
        assert!(!output.contains("remote-title-sentinel"));
        assert!(output.contains("remote_title_initialized: true"));
    }

    #[test]
    fn preserve_title_policy_is_byte_exact() {
        let sequence = b"\x1b]0;remote;title\x1b\\";
        let mut filter = TitleOutputFilter::new(TitlePolicy::PreserveRemote);
        assert_eq!(filter.process(sequence), sequence);
        assert!(filter.restore_remote_title().is_empty());
        assert!(filter.reapply_policy().is_empty());
    }

    #[test]
    fn direction_keys_encode_common_modifier_combinations() {
        let modifiers = [
            (KeyModifiers::SHIFT, b"\x1b[1;2D".as_slice()),
            (KeyModifiers::ALT, b"\x1b[1;3D".as_slice()),
            (KeyModifiers::CONTROL, b"\x1b[1;5D".as_slice()),
            (
                KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL,
                b"\x1b[1;8D".as_slice(),
            ),
        ];

        for (modifier, expected) in modifiers {
            let left = KeyEvent::new(KeyCode::Left, modifier);
            assert_eq!(
                encode_key_with_mode(left, CursorKeyMode::Application),
                Some(expected.to_vec())
            );
        }
    }

    #[test]
    fn function_keys_encode_common_modifiers() {
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::F(1), KeyModifiers::SHIFT)),
            Some(b"\x1b[1;2P".to_vec())
        );
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::F(5), KeyModifiers::ALT)),
            Some(b"\x1b[15;3~".to_vec())
        );
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::F(12), KeyModifiers::CONTROL)),
            Some(b"\x1b[24;5~".to_vec())
        );
        assert_eq!(
            encode_key(KeyEvent::new(
                KeyCode::F(4),
                KeyModifiers::SHIFT | KeyModifiers::CONTROL,
            )),
            Some(b"\x1b[1;6S".to_vec())
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
        assert_eq!(
            encode_paste_with_mode("first\nsecond", false),
            b"first\nsecond"
        );
    }

    #[test]
    fn mouse_and_focus_events_use_standard_terminal_sequences() {
        let press = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 4,
            row: 2,
            modifiers: KeyModifiers::CONTROL,
        };
        assert_eq!(encode_mouse(press), Some(b"\x1b[<16;5;3M".to_vec()));
        assert_eq!(
            encode_mouse_with_mode(press, MouseTrackingMode::Press, MouseEncoding::Default),
            Some(b"\x1b[M0%#".to_vec())
        );
        assert_eq!(
            encode_mouse_with_mode(press, MouseTrackingMode::Press, MouseEncoding::Utf8),
            Some(b"\x1b[M0%#".to_vec())
        );
        assert_eq!(
            encode_mouse_with_mode(
                MouseEvent {
                    kind: MouseEventKind::Moved,
                    ..press
                },
                MouseTrackingMode::ButtonMotion,
                MouseEncoding::Sgr,
            ),
            None
        );
        assert_eq!(
            encode_mouse_with_mode(press, MouseTrackingMode::Disabled, MouseEncoding::Sgr),
            None
        );
        assert_eq!(encode_focus(true), b"\x1b[I");
        assert_eq!(encode_focus(false), b"\x1b[O");
        assert_eq!(encode_focus_with_mode(true, true), Some(b"\x1b[I".to_vec()));
        assert_eq!(encode_focus_with_mode(true, false), None);
    }

    #[test]
    fn legacy_mouse_encoding_rejects_unrepresentable_coordinates() {
        let event = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: u16::MAX,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(
            encode_mouse_with_mode(event, MouseTrackingMode::Press, MouseEncoding::Default),
            None
        );
        assert!(
            encode_mouse_with_mode(event, MouseTrackingMode::Press, MouseEncoding::Utf8).is_some()
        );
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
    fn utf8_and_control_sequences_are_stable_across_hostbyte_boundaries() {
        let bytes = "界e\u{301}🙂".as_bytes();
        let mut sequence = b"\x1b[38;2;12;34;56m".to_vec();
        sequence.extend_from_slice(bytes);
        sequence.extend_from_slice(b"\x1b[?1;1004h");

        let mut whole = AuthoritativeScreen::new(20, 4);
        whole.process(&sequence);
        for split in 0..=sequence.len() {
            let mut fragmented = AuthoritativeScreen::new(20, 4);
            fragmented.process(&sequence[..split]);
            fragmented.process(&sequence[split..]);
            assert_eq!(
                fragmented.parser.screen().contents(),
                whole.parser.screen().contents()
            );
            assert_eq!(fragmented.remote_modes(), whole.remote_modes());
        }
    }

    #[test]
    fn local_escape_quits_or_forwards_literal_prefix() {
        let mut interpreter = EscapeInterpreter::default();
        assert_eq!(interpreter.input(vec![0x1e]), EscapeAction::Hold);
        assert_eq!(interpreter.input(b".".to_vec()), EscapeAction::Quit);
        assert_eq!(interpreter.input(vec![0x1e]), EscapeAction::Hold);
        assert_eq!(interpreter.input(b"?".to_vec()), EscapeAction::Help);
        assert_eq!(interpreter.input(vec![0x1e]), EscapeAction::Hold);
        assert_eq!(interpreter.input(vec![0x1a]), EscapeAction::Suspend);
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
        assert!(
            !format!("{:?}", EscapeAction::Forward(b"input-sentinel".to_vec()))
                .contains("input-sentinel")
        );
    }

    #[test]
    fn configurable_control_escape_uses_its_ascii_suffix() {
        let mut interpreter = EscapeInterpreter::new(2);
        assert_eq!(
            interpreter.input(b"x".to_vec()),
            EscapeAction::Forward(b"x".to_vec())
        );
        assert_eq!(interpreter.input(vec![2]), EscapeAction::Hold);
        assert_eq!(
            interpreter.input(b"B".to_vec()),
            EscapeAction::Forward(vec![2])
        );
    }

    #[test]
    fn printable_escape_requires_line_start_and_repeats_literally() {
        let mut interpreter = EscapeInterpreter::new(b'~');
        assert_eq!(
            interpreter.input(b"x".to_vec()),
            EscapeAction::Forward(b"x".to_vec())
        );
        assert_eq!(
            interpreter.input(b"~".to_vec()),
            EscapeAction::Forward(b"~".to_vec())
        );
        assert_eq!(
            interpreter.input(b"\r".to_vec()),
            EscapeAction::Forward(b"\r".to_vec())
        );
        assert_eq!(interpreter.input(b"~".to_vec()), EscapeAction::Hold);
        assert_eq!(
            interpreter.input(b"~".to_vec()),
            EscapeAction::Forward(b"~".to_vec())
        );
    }
}
