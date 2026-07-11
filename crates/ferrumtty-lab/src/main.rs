// SPDX-License-Identifier: GPL-3.0-only

use ferrumtty_crypto::PeerRole;
use std::env;
use std::fmt;
use std::io::{self, Read};
use std::process::ExitCode;
use zeroize::{Zeroize, Zeroizing};

mod channel_probe;
mod connect_smoke;
mod ocb_probe;
mod protobuf_probe;

const CONNECT_PREFIX: &str = "MOSH CONNECT";
const SESSION_KEY_LENGTH: usize = 22;

#[derive(Debug, Eq, PartialEq)]
struct ConnectAnnouncement {
    udp_port: u16,
    session_key: SecretText,
}

#[derive(Eq, PartialEq)]
struct SecretText(String);

impl SecretText {
    fn parse(value: &str) -> Result<Self, AnnouncementError> {
        let has_expected_length = value.len() == SESSION_KEY_LENGTH;
        let uses_base64_alphabet = value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/'));

        if !has_expected_length || !uses_base64_alphabet {
            return Err(AnnouncementError::SessionKey);
        }

        Ok(Self(value.to_owned()))
    }

    fn exposed_length(&self) -> usize {
        self.0.len()
    }
}

impl Drop for SecretText {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for SecretText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretText([REDACTED])")
    }
}

#[derive(Debug, Eq, PartialEq)]
enum AnnouncementError {
    Shape,
    Prefix,
    Port,
    SessionKey,
}

impl fmt::Display for AnnouncementError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Shape => "startup announcement must contain four fields",
            Self::Prefix => "startup announcement has an unexpected prefix",
            Self::Port => "startup announcement has an invalid UDP port",
            Self::SessionKey => "startup announcement has an invalid session key",
        };
        formatter.write_str(message)
    }
}

fn parse_connect_announcement(line: &str) -> Result<ConnectAnnouncement, AnnouncementError> {
    let fields: Vec<_> = line.split_ascii_whitespace().collect();
    if fields.len() != 4 {
        return Err(AnnouncementError::Shape);
    }
    if fields[..2] != CONNECT_PREFIX.split_ascii_whitespace().collect::<Vec<_>>() {
        return Err(AnnouncementError::Prefix);
    }

    let udp_port = fields[2]
        .parse::<u16>()
        .map_err(|_| AnnouncementError::Port)?;
    if udp_port == 0 {
        return Err(AnnouncementError::Port);
    }

    Ok(ConnectAnnouncement {
        udp_port,
        session_key: SecretText::parse(fields[3])?,
    })
}

fn print_usage() {
    eprintln!("Usage:");
    eprintln!("  printf '%s\\n' '<startup announcement>' | ferrumtty-lab verify-connect");
    eprintln!("  printf '%s\\n%s\\n' '<key>' '<packet-hex>' | ferrumtty-lab probe-ocb");
    eprintln!("  printf '%s\\n%s\\n' '<key>' '<packet-hex>' | ferrumtty-lab verify-client-packet");
    eprintln!("  printf '%s\\n%s\\n' '<key>' '<packet-hex>' | ferrumtty-lab verify-server-packet");
}

fn run() -> Result<(), String> {
    let mut arguments = env::args().skip(1);
    let Some(command) = arguments.next() else {
        print_usage();
        return Err("missing command".to_owned());
    };
    if arguments.next().is_some() {
        print_usage();
        return Err("unexpected extra argument".to_owned());
    }
    match command.as_str() {
        "verify-connect" => verify_connect_from_stdin(),
        "probe-ocb" => ocb_probe::run_from_stdin(),
        "verify-client-packet" => channel_probe::run_from_stdin(PeerRole::Server),
        "verify-server-packet" => channel_probe::run_from_stdin(PeerRole::Client),
        "verify-server-fragment" => channel_probe::run_fragment_from_stdin(PeerRole::Client),
        "connect-standard-server" => connect_smoke::run(),
        _ => {
            print_usage();
            Err("unknown command".to_owned())
        }
    }
}

fn verify_connect_from_stdin() -> Result<(), String> {
    let mut line = Zeroizing::new(String::new());
    io::stdin()
        .read_to_string(&mut line)
        .map_err(|error| format!("failed to read startup announcement: {error}"))?;
    let announcement =
        parse_connect_announcement(line.trim()).map_err(|error| error.to_string())?;
    println!(
        "valid startup announcement: udp_port={}, session_key_bytes={} (redacted)",
        announcement.udp_port,
        announcement.session_key.exposed_length()
    );
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ferrumtty-lab: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AnnouncementError, ConnectAnnouncement, SecretText, parse_connect_announcement};

    const SYNTHETIC_KEY: &str = "AAECAwQFBgcICQoLDA0ODw";

    #[test]
    fn parses_documented_startup_shape() {
        let line = format!("MOSH CONNECT 60001 {SYNTHETIC_KEY}");

        assert_eq!(
            parse_connect_announcement(&line),
            Ok(ConnectAnnouncement {
                udp_port: 60_001,
                session_key: SecretText(SYNTHETIC_KEY.to_owned()),
            })
        );
    }

    #[test]
    fn rejects_zero_port() {
        let line = format!("MOSH CONNECT 0 {SYNTHETIC_KEY}");

        assert_eq!(
            parse_connect_announcement(&line),
            Err(AnnouncementError::Port)
        );
    }

    #[test]
    fn rejects_key_with_unexpected_shape() {
        assert_eq!(
            parse_connect_announcement("MOSH CONNECT 60001 not-a-session-key"),
            Err(AnnouncementError::SessionKey)
        );
    }

    #[test]
    fn debug_output_redacts_session_key() {
        let secret = SecretText(SYNTHETIC_KEY.to_owned());

        assert_eq!(format!("{secret:?}"), "SecretText([REDACTED])");
    }
}
