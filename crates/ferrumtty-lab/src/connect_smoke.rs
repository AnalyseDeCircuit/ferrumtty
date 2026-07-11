// SPDX-License-Identifier: GPL-3.0-only

use ferrumtty_crypto::SessionKey;
use ferrumtty_session::ClientProtocol;
use ferrumtty_wire::{ByteRun, Instruction, InstructionBatch, ViewportSize};
use std::io::{self, BufRead, ErrorKind};
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use zeroize::{Zeroize, Zeroizing};

const RECEIVE_BUFFER_BYTES: usize = 65_535;
const RECEIVE_TIMEOUT: Duration = Duration::from_millis(250);
const SYNTHETIC_COLUMNS: u64 = 80;
const SYNTHETIC_ROWS: u64 = 24;
const MAXIMUM_SMOKE_EVENTS: usize = 40;

/// Sends one independently encoded state and authenticates the server response.
pub(crate) fn run() -> Result<(), String> {
    let (port, key) = read_announcement()?;
    let mut socket = connect_socket(port)?;
    let mut protocol = ClientProtocol::new(key);
    let initial_packets = protocol
        .build_update(current_timestamp(), &initial_instructions())
        .map_err(|error| format!("failed to build initial update: {error:?}"))?;
    send_packets(&socket, &initial_packets, "initial update")?;
    exchange_until_terminal_output(&mut socket, &mut protocol)
}

fn read_announcement() -> Result<(u16, SessionKey), String> {
    let mut announcement = Zeroizing::new(String::new());
    io::stdin()
        .lock()
        .read_line(&mut announcement)
        .map_err(|error| format!("failed to read startup announcement: {error}"))?;
    let fields: Vec<_> = announcement.split_ascii_whitespace().collect();
    if fields.len() != 4 || fields[..2] != ["MOSH", "CONNECT"] {
        return Err("invalid startup announcement".to_owned());
    }
    let port = fields[2]
        .parse::<u16>()
        .map_err(|_| "invalid startup UDP port".to_owned())?;
    if port == 0 {
        return Err("invalid startup UDP port".to_owned());
    }
    let key =
        SessionKey::decode(fields[3]).map_err(|_| "invalid startup session key".to_owned())?;
    announcement.zeroize();
    Ok((port, key))
}

fn connect_socket(port: u16) -> Result<UdpSocket, String> {
    let socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
        .map_err(|error| format!("failed to bind UDP socket: {error}"))?;
    socket
        .connect(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))
        .map_err(|error| format!("failed to connect UDP socket: {error}"))?;
    socket
        .set_read_timeout(Some(RECEIVE_TIMEOUT))
        .map_err(|error| format!("failed to set UDP timeout: {error}"))?;
    Ok(socket)
}

fn initial_instructions() -> InstructionBatch {
    InstructionBatch {
        instructions: vec![
            Instruction {
                bytes: None,
                viewport: Some(ViewportSize {
                    columns: SYNTHETIC_COLUMNS,
                    rows: SYNTHETIC_ROWS,
                }),
                marker: None,
            },
            Instruction {
                bytes: Some(ByteRun {
                    value: b"exit\n".to_vec(),
                }),
                viewport: None,
                marker: None,
            },
        ],
    }
}

fn send_packets(socket: &UdpSocket, packets: &[Vec<u8>], purpose: &str) -> Result<(), String> {
    for packet in packets {
        socket
            .send(packet)
            .map_err(|error| format!("failed to send {purpose}: {error}"))?;
    }
    Ok(())
}

fn exchange_until_terminal_output(
    socket: &mut UdpSocket,
    protocol: &mut ClientProtocol,
) -> Result<(), String> {
    let mut receive_buffer = vec![0_u8; RECEIVE_BUFFER_BYTES];
    let mut roamed = false;
    for _ in 0..MAXIMUM_SMOKE_EVENTS {
        let received = match socket.recv(&mut receive_buffer) {
            Ok(received) => received,
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                if protocol.has_pending_update() {
                    let packets =
                        protocol
                            .retransmit_pending(current_timestamp())
                            .map_err(|error| {
                                format!("failed to retransmit pending state: {error:?}")
                            })?;
                    send_packets(socket, &packets, "retransmitted state")?;
                }
                continue;
            }
            Err(error) => {
                return Err(format!(
                    "failed to receive standard-server response: {error}"
                ));
            }
        };
        if let Some(state) = protocol
            .ingest(&receive_buffer[..received])
            .map_err(|error| {
                format!("failed to authenticate standard-server response: {error:?}")
            })?
        {
            let instructions = state
                .update
                .decode_instructions()
                .map_err(|error| format!("failed to decode server instructions: {error:?}"))?;
            let terminal_bytes = instructions
                .instructions
                .iter()
                .filter_map(|instruction| instruction.bytes.as_ref())
                .map(|bytes| bytes.value.len())
                .sum::<usize>();
            if terminal_bytes > 0 {
                println!(
                    "standard server exchanged FerrumTTY state: target_state={}, instructions={}, terminal_bytes={terminal_bytes}, roamed={roamed}",
                    state.update.target_state,
                    instructions.instructions.len()
                );
                return Ok(());
            }

            let acknowledgement = InstructionBatch {
                instructions: Vec::new(),
            };
            if !roamed {
                *socket = rebind_socket(socket)?;
                roamed = true;
            }
            let packets = protocol
                .build_update(current_timestamp(), &acknowledgement)
                .map_err(|error| format!("failed to build acknowledgement: {error:?}"))?;
            send_packets(socket, &packets, "acknowledgement")?;
        }
    }
    Err("standard server did not produce terminal output within the smoke-event bound".to_owned())
}

fn rebind_socket(current: &UdpSocket) -> Result<UdpSocket, String> {
    let peer = current
        .peer_addr()
        .map_err(|error| format!("failed to read current UDP peer: {error}"))?;
    let replacement = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
        .map_err(|error| format!("failed to bind roaming UDP socket: {error}"))?;
    replacement
        .connect(peer)
        .map_err(|error| format!("failed to connect roaming UDP socket: {error}"))?;
    replacement
        .set_read_timeout(Some(RECEIVE_TIMEOUT))
        .map_err(|error| format!("failed to set roaming UDP timeout: {error}"))?;
    Ok(replacement)
}

fn current_timestamp() -> u16 {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    let low_bits = elapsed.as_millis() & u128::from(u16::MAX);
    u16::try_from(low_bits).expect("timestamp is masked to 16 bits")
}
