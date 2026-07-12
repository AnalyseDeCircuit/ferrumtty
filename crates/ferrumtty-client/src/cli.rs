// SPDX-License-Identifier: GPL-3.0-only

use ferrumtty_predict::PredictionDisplay;

const DEFAULT_ESCAPE_BYTE: u8 = 0x1e;
const MAX_VERBOSITY: u8 = 3;
const TRUE_COLOR_COUNT: u32 = 16_777_216;

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Command {
    Help { program: String },
    Version,
    Colors,
    Connect(ClientConfig),
}

/// Holds CLI and environment settings at the client/runtime boundary.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ClientConfig {
    pub(crate) endpoint: String,
    pub(crate) verbosity: u8,
    pub(crate) escape_byte: u8,
    pub(crate) title_no_prefix: bool,
    pub(crate) prediction_display: PredictionDisplay,
    pub(crate) prediction_overwrite: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OutputTarget {
    Stdout,
    Stderr,
}

/// Describes a command-line failure without writing it to a terminal.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct CliError {
    message: String,
    target: OutputTarget,
    exit_code: u8,
}

impl CliError {
    fn failure(message: String) -> Self {
        Self {
            message,
            target: OutputTarget::Stderr,
            exit_code: 1,
        }
    }

    pub(crate) fn message(&self) -> &str {
        &self.message
    }

    pub(crate) const fn target(&self) -> OutputTarget {
        self.target
    }

    pub(crate) const fn exit_code(&self) -> u8 {
        self.exit_code
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PlatformColorCapability {
    Unknown,
    Ansi8,
    Ansi16,
    Indexed256,
    TrueColor,
}

pub(crate) fn parse_command<I, F>(arguments: I, environment: F) -> Result<Command, CliError>
where
    I: IntoIterator<Item = String>,
    F: Fn(&str) -> Option<String>,
{
    let mut arguments = arguments.into_iter();
    let program = arguments.next().unwrap_or_else(|| "ferrumtty".to_owned());
    let remaining = arguments.collect::<Vec<_>>();

    // Match mosh-client's long-option precedence, even when another argument is malformed.
    if remaining.iter().any(|argument| argument == "--help") {
        return Ok(Command::Help { program });
    }
    if remaining.iter().any(|argument| argument == "--version") {
        return Ok(Command::Version);
    }

    let mut verbosity = 0_u8;
    let mut positionals = Vec::new();
    let mut position = 0_usize;
    let mut options_finished = false;
    while position < remaining.len() {
        let argument = &remaining[position];
        if !options_finished && argument == "--" {
            options_finished = true;
            position += 1;
            continue;
        }
        if !options_finished && argument.starts_with('-') && argument != "-" {
            let option_bytes = argument.as_bytes();
            let mut option_position = 1_usize;
            while option_position < option_bytes.len() {
                match option_bytes[option_position] {
                    b'v' => {
                        verbosity = verbosity.saturating_add(1).min(MAX_VERBOSITY);
                        option_position += 1;
                    }
                    b'c' => return Ok(Command::Colors),
                    b'#' => {
                        // The wrapper description is intentionally accepted but never logged.
                        if option_position + 1 == option_bytes.len() {
                            position += 1;
                            if position == remaining.len() {
                                return Err(CliError::failure(usage(&program)));
                            }
                        }
                        option_position = option_bytes.len();
                    }
                    _ => return Err(CliError::failure(usage(&program))),
                }
            }
        } else {
            positionals.push(argument.as_str());
        }
        position += 1;
    }

    if positionals.len() != 2 {
        return Err(CliError::failure(usage(&program)));
    }
    let host = positionals[0];
    let port_text = positionals[1];
    let port = port_text.parse::<u16>().ok().filter(|port| *port != 0);
    let Some(port) = port else {
        return Err(CliError::failure(format!(
            "{program}: Bad UDP port ({port_text})\n\n{}",
            usage(&program)
        )));
    };

    let escape_byte = environment("MOSH_ESCAPE_KEY")
        .map_or(Ok(DEFAULT_ESCAPE_BYTE), |value| parse_escape_key(&value))?;
    Ok(Command::Connect(ClientConfig {
        endpoint: format_endpoint(host, port),
        verbosity,
        escape_byte,
        title_no_prefix: environment("MOSH_TITLE_NOPREFIX").is_some(),
        prediction_display: parse_prediction_display(
            environment("MOSH_PREDICTION_DISPLAY").as_deref(),
        )?,
        prediction_overwrite: environment("MOSH_PREDICTION_OVERWRITE").as_deref() == Some("yes"),
    }))
}

fn format_endpoint(host: &str, port: u16) -> String {
    if host.contains(':') && !(host.starts_with('[') && host.ends_with(']')) {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

fn parse_prediction_display(value: Option<&str>) -> Result<PredictionDisplay, CliError> {
    match value.unwrap_or("adaptive") {
        "adaptive" => Ok(PredictionDisplay::Adaptive),
        "always" => Ok(PredictionDisplay::Always),
        "never" => Ok(PredictionDisplay::Never),
        _ => Err(CliError::failure(
            "MOSH_PREDICTION_DISPLAY must be adaptive, always, or never".to_owned(),
        )),
    }
}

fn parse_escape_key(value: &str) -> Result<u8, CliError> {
    let bytes = value.as_bytes();
    if bytes.len() == 1 && matches!(bytes[0], 1..=127) {
        return Ok(bytes[0]);
    }
    Err(CliError::failure(
        "MOSH_ESCAPE_KEY must be one literal ASCII character in the range 1-127".to_owned(),
    ))
}

pub(crate) fn usage(program: &str) -> String {
    format!(
        "{}\n\nUsage: {program} [-# 'ARGS'] [-v...] HOST PORT\n       {program} -c",
        version()
    )
}

pub(crate) fn version() -> String {
    format!("ferrumtty-client {}", env!("CARGO_PKG_VERSION"))
}

pub(crate) fn missing_key_error() -> CliError {
    CliError::failure("MOSH_KEY environment variable not found.".to_owned())
}

pub(crate) fn invalid_key_error() -> CliError {
    CliError::failure("Crypto exception: MOSH_KEY is invalid.".to_owned())
}

/// Infers color capacity without claiming terminfo-equivalent accuracy.
pub(crate) fn infer_color_count(
    term: Option<&str>,
    colorterm: Option<&str>,
    platform: PlatformColorCapability,
) -> u32 {
    let term = term.map(str::trim).filter(|value| !value.is_empty());
    let colorterm = colorterm.map(str::trim).filter(|value| !value.is_empty());
    if term.is_some_and(|value| value.eq_ignore_ascii_case("dumb")) {
        return 0;
    }
    if colorterm.is_some_and(is_true_color_marker)
        || term.is_some_and(|value| {
            let value = value.to_ascii_lowercase();
            value.contains("truecolor") || value.contains("24bit") || value.contains("direct")
        })
    {
        return TRUE_COLOR_COUNT;
    }
    if term.is_some_and(|value| value.to_ascii_lowercase().contains("256color")) {
        return 256;
    }
    if term.is_some_and(is_known_ansi_terminal) {
        return match platform {
            PlatformColorCapability::TrueColor => TRUE_COLOR_COUNT,
            PlatformColorCapability::Indexed256 => 256,
            PlatformColorCapability::Ansi16 => 16,
            PlatformColorCapability::Ansi8 | PlatformColorCapability::Unknown => 8,
        };
    }
    if cfg!(windows) && term.is_none() {
        return platform_color_count(platform);
    }
    0
}

fn is_true_color_marker(value: &str) -> bool {
    value.eq_ignore_ascii_case("truecolor") || value.eq_ignore_ascii_case("24bit")
}

fn is_known_ansi_terminal(value: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "ansi",
        "alacritty",
        "cygwin",
        "eterm",
        "foot",
        "kitty",
        "linux",
        "kterm",
        "rxvt",
        "screen",
        "tmux",
        "vt100",
        "wezterm",
        "xterm",
    ];
    let value = value.to_ascii_lowercase();
    PREFIXES.iter().any(|prefix| value.starts_with(prefix))
}

const fn platform_color_count(capability: PlatformColorCapability) -> u32 {
    match capability {
        PlatformColorCapability::Unknown => 0,
        PlatformColorCapability::Ansi8 => 8,
        PlatformColorCapability::Ansi16 => 16,
        PlatformColorCapability::Indexed256 => 256,
        PlatformColorCapability::TrueColor => TRUE_COLOR_COUNT,
    }
}

pub(crate) fn platform_color_capability(color_count: u16) -> PlatformColorCapability {
    match color_count {
        0 => PlatformColorCapability::Unknown,
        u16::MAX => PlatformColorCapability::TrueColor,
        1..=8 => PlatformColorCapability::Ansi8,
        9..=255 => PlatformColorCapability::Ansi16,
        256..=65_534 => PlatformColorCapability::Indexed256,
    }
}

/// Validates the effective character-type locale using POSIX precedence.
pub(crate) fn validate_utf8_locale<F>(environment: F, windows: bool) -> Result<(), CliError>
where
    F: Fn(&str) -> Option<String>,
{
    // Modern Windows terminal APIs carry Unicode independently of POSIX locale variables.
    if windows {
        return Ok(());
    }
    let selected = ["LC_ALL", "LC_CTYPE", "LANG"].into_iter().find_map(|name| {
        environment(name)
            .filter(|value| !value.is_empty())
            .map(|value| (name, value))
    });
    let Some((name, value)) = selected else {
        return Err(CliError::failure(
            "mosh-client needs a UTF-8 native locale to run.\n\nThe client's environment has no LC_ALL, LC_CTYPE, or LANG value.".to_owned(),
        ));
    };
    if locale_is_utf8(&value) {
        return Ok(());
    }
    Err(CliError::failure(format!(
        "mosh-client needs a UTF-8 native locale to run.\n\nUnfortunately, the client's environment ({name}={value}) does not specify UTF-8."
    )))
}

fn locale_is_utf8(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase().replace('_', "-");
    if normalized == "utf8" || normalized == "utf-8" {
        return true;
    }
    let locale_without_modifier = normalized.split('@').next().unwrap_or(&normalized);
    locale_without_modifier
        .rsplit_once('.')
        .is_some_and(|(_, codeset)| matches!(codeset, "utf8" | "utf-8"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn parse(arguments: &[&str], environment: &[(&str, &str)]) -> Result<Command, CliError> {
        let arguments = arguments.iter().map(|value| (*value).to_owned());
        let environment = environment
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect::<HashMap<_, _>>();
        parse_command(arguments, |name| environment.get(name).cloned())
    }

    #[test]
    fn help_and_version_take_precedence() {
        assert!(matches!(
            parse(&["client", "bad", "--help"], &[]),
            Ok(Command::Help { .. })
        ));
        assert_eq!(
            parse(&["client", "--version", "unexpected"], &[]),
            Ok(Command::Version)
        );
    }

    #[test]
    fn accepts_wrapper_argument_and_saturates_verbosity() {
        let command = parse(
            &[
                "client",
                "-#",
                "wrapper secret",
                "-vvvvv",
                "example.test",
                "60001",
            ],
            &[],
        );
        let Ok(Command::Connect(config)) = command else {
            panic!("connection arguments should parse")
        };
        assert_eq!(config.verbosity, MAX_VERBOSITY);
        assert_eq!(config.endpoint, "example.test:60001");
    }

    #[test]
    fn reads_supported_environment_preferences() {
        let Ok(Command::Connect(config)) = parse(
            &["client", "host", "60001"],
            &[
                ("MOSH_ESCAPE_KEY", "\u{2}"),
                ("MOSH_TITLE_NOPREFIX", ""),
                ("MOSH_PREDICTION_DISPLAY", "always"),
                ("MOSH_PREDICTION_OVERWRITE", "yes"),
            ],
        ) else {
            panic!("environment preferences should parse")
        };
        assert_eq!(config.escape_byte, 2);
        assert!(config.title_no_prefix);
        assert_eq!(config.prediction_display, PredictionDisplay::Always);
        assert!(config.prediction_overwrite);
        assert!(parse(&["client", "host", "60001"], &[("MOSH_ESCAPE_KEY", "two")]).is_err());
    }

    #[test]
    fn parses_color_mode_immediately() {
        assert_eq!(
            parse(&["client", "-vc", "ignored"], &[]),
            Ok(Command::Colors)
        );
    }

    #[test]
    fn rejects_ports_outside_the_udp_range() {
        for port in ["", "0", "65536", "12x"] {
            let error = parse(&["client", "host", port], &[]).expect_err("port should fail");
            assert!(
                error
                    .message()
                    .starts_with(&format!("client: Bad UDP port ({port})"))
            );
            assert_eq!(error.exit_code(), 1);
            assert_eq!(error.target(), OutputTarget::Stderr);
        }
        assert!(parse(&["client", "host", "65535"], &[]).is_ok());
    }

    #[test]
    fn brackets_bare_ipv6_hosts() {
        let Ok(Command::Connect(config)) = parse(&["client", "2001:db8::1", "60001"], &[]) else {
            panic!("IPv6 endpoint should parse")
        };
        assert_eq!(config.endpoint, "[2001:db8::1]:60001");
    }

    #[test]
    fn infers_colors_conservatively() {
        assert_eq!(
            infer_color_count(
                Some("dumb"),
                Some("truecolor"),
                PlatformColorCapability::TrueColor
            ),
            0
        );
        assert_eq!(
            infer_color_count(
                Some("xterm-256color"),
                None,
                PlatformColorCapability::Unknown
            ),
            256
        );
        assert_eq!(
            infer_color_count(Some("xterm"), None, PlatformColorCapability::Unknown),
            8
        );
        assert_eq!(
            infer_color_count(
                Some("xterm"),
                Some("truecolor"),
                PlatformColorCapability::Ansi16
            ),
            TRUE_COLOR_COUNT
        );
        assert_eq!(
            infer_color_count(
                Some("private-terminal"),
                None,
                PlatformColorCapability::TrueColor
            ),
            0
        );
        assert_eq!(
            infer_color_count(None, None, PlatformColorCapability::TrueColor),
            if cfg!(windows) { TRUE_COLOR_COUNT } else { 0 }
        );
        assert_eq!(platform_color_capability(8), PlatformColorCapability::Ansi8);
        assert_eq!(
            platform_color_capability(16),
            PlatformColorCapability::Ansi16
        );
        assert_eq!(
            platform_color_capability(256),
            PlatformColorCapability::Indexed256
        );
        assert_eq!(
            platform_color_capability(u16::MAX),
            PlatformColorCapability::TrueColor
        );
    }

    #[test]
    fn validates_locale_with_posix_precedence() {
        let values = HashMap::from([
            ("LANG", "en_US.UTF-8".to_owned()),
            ("LC_CTYPE", "C".to_owned()),
        ]);
        assert!(validate_utf8_locale(|name| values.get(name).cloned(), false).is_err());
        let values = HashMap::from([
            ("LANG", "C".to_owned()),
            ("LC_ALL", "zh_CN.utf8".to_owned()),
        ]);
        assert!(validate_utf8_locale(|name| values.get(name).cloned(), false).is_ok());
        let values = HashMap::from([("LANG", "en_US.UTF-8@calendar=gregorian".to_owned())]);
        assert!(validate_utf8_locale(|name| values.get(name).cloned(), false).is_ok());
        assert!(validate_utf8_locale(|_| None, false).is_err());
    }

    #[test]
    fn windows_does_not_require_posix_locale_variables() {
        assert!(validate_utf8_locale(|_| None, true).is_ok());
    }

    #[test]
    fn missing_key_message_is_exact_and_secret_free() {
        let error = missing_key_error();
        assert_eq!(error.message(), "MOSH_KEY environment variable not found.");
        assert_eq!(error.exit_code(), 1);
        assert_eq!(error.target(), OutputTarget::Stderr);
    }

    #[test]
    fn invalid_key_message_never_contains_the_supplied_value() {
        let error = invalid_key_error();
        assert_eq!(error.message(), "Crypto exception: MOSH_KEY is invalid.");
        assert!(!error.message().contains("secret-key-sentinel"));
        assert_eq!(error.exit_code(), 1);
    }
}
