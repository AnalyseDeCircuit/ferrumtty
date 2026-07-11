// SPDX-License-Identifier: GPL-3.0-only

//! Deterministic host-facing session runtime without socket or terminal access.

use ferrumtty_crypto::SessionKey;
use ferrumtty_session::{ClientProtocol, SessionError};
use ferrumtty_wire::{ByteRun, Instruction, InstructionBatch, MessageError, ViewportSize};
use std::collections::VecDeque;

const INITIAL_RETRANSMIT_MILLISECONDS: u64 = 250;
const MAXIMUM_RETRANSMIT_MILLISECONDS: u64 = 2_000;
const HEARTBEAT_MILLISECONDS: u64 = 3_000;
const SERVER_UNRESPONSIVE_MILLISECONDS: u64 = 30_000;
const MAXIMUM_QUEUED_INPUT_BYTES: usize = 1024 * 1024;

/// A monotonic millisecond value supplied by the embedding host.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct MonotonicTime(u64);

impl MonotonicTime {
    #[must_use]
    pub const fn from_milliseconds(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn milliseconds(self) -> u64 {
        self.0
    }

    fn elapsed_since(self, earlier: Self) -> u64 {
        self.0.saturating_sub(earlier.0)
    }
}

/// An effect the host must apply in order.
#[derive(Debug, Eq, PartialEq)]
pub enum SessionAction {
    SendDatagram(Vec<u8>),
    WriteTerminal(Vec<u8>),
    AcknowledgePrediction(u64),
}

/// Owns synchronization, timers, and bounded local input pending transport.
pub struct SessionRuntime {
    protocol: ClientProtocol,
    queued_instructions: VecDeque<Instruction>,
    pending_resize: Option<ViewportSize>,
    queued_input_bytes: usize,
    last_send: Option<MonotonicTime>,
    last_receive: MonotonicTime,
    retransmit_milliseconds: u64,
    acknowledgement_due: bool,
    received_server_state: bool,
    round_trip_milliseconds: Option<u16>,
}

impl SessionRuntime {
    #[must_use]
    pub fn new(key: SessionKey, now: MonotonicTime) -> Self {
        Self {
            protocol: ClientProtocol::new(key),
            queued_instructions: VecDeque::new(),
            pending_resize: None,
            queued_input_bytes: 0,
            last_send: None,
            last_receive: now,
            retransmit_milliseconds: INITIAL_RETRANSMIT_MILLISECONDS,
            acknowledgement_due: false,
            received_server_state: false,
            round_trip_milliseconds: None,
        }
    }

    /// Queues terminal input under a fixed memory bound.
    ///
    /// # Errors
    ///
    /// Returns an error if the total unsent input would exceed the bound.
    pub fn queue_input(&mut self, bytes: Vec<u8>) -> Result<(), RuntimeError> {
        let new_size = self
            .queued_input_bytes
            .checked_add(bytes.len())
            .ok_or(RuntimeError::InputQueueFull)?;
        if new_size > MAXIMUM_QUEUED_INPUT_BYTES {
            return Err(RuntimeError::InputQueueFull);
        }
        self.queued_input_bytes = new_size;
        self.queued_instructions.push_back(Instruction {
            bytes: Some(ByteRun { value: bytes }),
            viewport: None,
            marker: None,
        });
        Ok(())
    }

    pub fn queue_resize(&mut self, columns: u16, rows: u16) {
        self.pending_resize = Some(ViewportSize {
            columns: u64::from(columns),
            rows: u64::from(rows),
        });
    }

    /// Re-arms liveness after the host reports a system resume or another
    /// interval in which network progress could not have been observed.
    pub fn resume(&mut self, now: MonotonicTime) {
        self.last_receive = now;
        self.last_send = self.protocol.has_pending_update().then_some(now);
        self.retransmit_milliseconds = INITIAL_RETRANSMIT_MILLISECONDS;
    }

    /// Accepts an authenticated server datagram and returns terminal effects.
    ///
    /// # Errors
    ///
    /// Returns an error for authentication, replay, framing, decompression, or
    /// unsupported message failures.
    pub fn receive_datagram(
        &mut self,
        packet: &[u8],
        now: MonotonicTime,
    ) -> Result<Vec<SessionAction>, RuntimeError> {
        let Some(state) = self
            .protocol
            .ingest(packet)
            .map_err(RuntimeError::Session)?
        else {
            return Ok(Vec::new());
        };
        self.last_receive = now;
        if state.echoed_timestamp != u16::MAX {
            let elapsed = timestamp(now).wrapping_sub(state.echoed_timestamp);
            if i16::try_from(elapsed).is_ok() {
                self.round_trip_milliseconds = Some(elapsed);
            }
        }
        self.acknowledgement_due = true;
        self.received_server_state = true;
        if !state.advances_remote_state {
            return Ok(Vec::new());
        }
        let instructions = state
            .update
            .decode_instructions()
            .map_err(RuntimeError::Message)?;
        Ok(instructions
            .instructions
            .into_iter()
            .flat_map(|instruction| {
                let mut actions = Vec::with_capacity(2);
                if let Some(bytes) = instruction.bytes {
                    actions.push(SessionAction::WriteTerminal(bytes.value));
                }
                if let Some(marker) = instruction.marker {
                    actions.push(SessionAction::AcknowledgePrediction(marker.value));
                }
                actions
            })
            .collect())
    }

    /// Advances timers and returns datagrams that should be sent now.
    ///
    /// # Errors
    ///
    /// Returns an error for protocol encoding failure.
    pub fn poll(&mut self, now: MonotonicTime) -> Result<Vec<SessionAction>, RuntimeError> {
        if self.protocol.has_pending_update() {
            let last_send = self.last_send.unwrap_or(now);
            if now.elapsed_since(last_send) < self.retransmit_milliseconds {
                return Ok(Vec::new());
            }
            let packets = self
                .protocol
                .retransmit_pending(timestamp(now))
                .map_err(RuntimeError::Session)?;
            self.last_send = Some(now);
            self.retransmit_milliseconds = self
                .retransmit_milliseconds
                .saturating_mul(2)
                .min(MAXIMUM_RETRANSMIT_MILLISECONDS);
            return Ok(send_actions(packets));
        }

        let heartbeat_due = self
            .last_send
            .is_none_or(|last_send| now.elapsed_since(last_send) >= HEARTBEAT_MILLISECONDS);
        if self.queued_instructions.is_empty()
            && self.pending_resize.is_none()
            && !self.acknowledgement_due
            && !heartbeat_due
        {
            return Ok(Vec::new());
        }
        let viewport = self.pending_resize.take().map(|viewport| Instruction {
            bytes: None,
            viewport: Some(viewport),
            marker: None,
        });
        let instructions = InstructionBatch {
            instructions: viewport
                .into_iter()
                .chain(self.queued_instructions.drain(..))
                .collect(),
        };
        self.queued_input_bytes = 0;
        self.acknowledgement_due = false;
        let packets = self
            .protocol
            .build_update(timestamp(now), &instructions)
            .map_err(RuntimeError::Session)?;
        self.last_send = Some(now);
        self.retransmit_milliseconds = INITIAL_RETRANSMIT_MILLISECONDS;
        Ok(send_actions(packets))
    }

    #[must_use]
    pub fn milliseconds_until_next_poll(&self, now: MonotonicTime) -> u64 {
        if self.protocol.has_pending_update() {
            self.last_send.map_or(0, |last_send| {
                self.retransmit_milliseconds
                    .saturating_sub(now.elapsed_since(last_send))
            })
        } else if self.acknowledgement_due
            || !self.queued_instructions.is_empty()
            || self.pending_resize.is_some()
        {
            0
        } else {
            self.last_send.map_or(0, |last_send| {
                HEARTBEAT_MILLISECONDS.saturating_sub(now.elapsed_since(last_send))
            })
        }
    }

    #[must_use]
    pub const fn has_received_server_state(&self) -> bool {
        self.received_server_state
    }

    /// Reports whether the server has responded recently without ending an
    /// otherwise recoverable Mosh session when connectivity is intermittent.
    #[must_use]
    pub fn is_server_responsive(&self, now: MonotonicTime) -> bool {
        now.elapsed_since(self.last_receive) < SERVER_UNRESPONSIVE_MILLISECONDS
    }

    #[must_use]
    pub const fn round_trip_milliseconds(&self) -> Option<u16> {
        self.round_trip_milliseconds
    }
}

fn timestamp(now: MonotonicTime) -> u16 {
    let low_bits = now.milliseconds() & u64::from(u16::MAX);
    u16::try_from(low_bits).expect("timestamp is masked to 16 bits")
}

fn send_actions(packets: Vec<Vec<u8>>) -> Vec<SessionAction> {
    packets
        .into_iter()
        .map(SessionAction::SendDatagram)
        .collect()
}

#[derive(Debug)]
pub enum RuntimeError {
    InputQueueFull,
    Session(SessionError),
    Message(MessageError),
}

#[cfg(test)]
mod tests {
    use super::{MonotonicTime, RuntimeError, SessionAction, SessionRuntime};
    use ferrumtty_crypto::{PeerRole, SecureChannel, SessionKey};
    use ferrumtty_wire::{
        ByteRun, Fragment, Instruction, InstructionBatch, StateUpdate, ViewportSize,
        decode_compressed_update, encode_compressed_update,
    };

    const SYNTHETIC_KEY: &str = "AAECAwQFBgcICQoLDA0ODw";

    #[test]
    fn immediate_acknowledgement_and_retransmission_are_deterministic() {
        let mut runtime = SessionRuntime::new(key(), time(0));
        assert!(!runtime.has_received_server_state());
        runtime.queue_resize(80, 24);
        let first = runtime.poll(time(0)).expect("initial poll must send");
        assert_eq!(first.len(), 1);
        assert!(
            runtime
                .poll(time(249))
                .expect("early poll must work")
                .is_empty()
        );
        assert_eq!(runtime.poll(time(250)).expect("retry must send").len(), 1);
        assert_eq!(runtime.milliseconds_until_next_poll(time(250)), 500);

        let server_packet = server_packet(1, b"ready");
        assert_eq!(
            runtime
                .receive_datagram(&server_packet, time(300))
                .expect("server state must open"),
            vec![SessionAction::WriteTerminal(b"ready".to_vec())]
        );
        assert!(runtime.has_received_server_state());
        assert_eq!(runtime.milliseconds_until_next_poll(time(300)), 0);
        assert_eq!(runtime.poll(time(300)).expect("ack must send").len(), 1);
    }

    #[test]
    fn input_is_bounded_and_network_silence_is_recoverable() {
        let mut runtime = SessionRuntime::new(key(), time(10));
        assert!(matches!(
            runtime.queue_input(vec![0; 1024 * 1024 + 1]),
            Err(RuntimeError::InputQueueFull)
        ));
        assert!(!runtime.is_server_responsive(time(30_010)));
        assert!(runtime.poll(time(30_010)).is_ok());
    }

    #[test]
    fn rapid_resize_sends_only_the_latest_dimensions() {
        let mut runtime = SessionRuntime::new(key(), time(0));
        runtime.queue_resize(80, 24);
        runtime.queue_resize(100, 30);
        runtime.queue_resize(132, 43);
        let actions = runtime.poll(time(0)).expect("resize must send");
        let SessionAction::SendDatagram(packet) = &actions[0] else {
            panic!("first action must be a datagram");
        };
        let server_channel = SecureChannel::new(PeerRole::Server, key());
        let plaintext = server_channel
            .open(packet)
            .expect("resize packet must authenticate")
            .plaintext;
        let fragment = Fragment::parse(&plaintext).expect("resize fragment must parse");
        let update = decode_compressed_update(&fragment.body).expect("resize state must decode");
        assert_eq!(
            update
                .decode_instructions()
                .expect("resize instructions must decode")
                .instructions[0]
                .viewport,
            Some(ViewportSize {
                columns: 132,
                rows: 43,
            })
        );
    }

    #[test]
    fn resume_prevents_sleep_interval_from_becoming_server_timeout() {
        let mut runtime = SessionRuntime::new(key(), time(0));
        runtime.queue_resize(80, 24);
        assert_eq!(
            runtime
                .poll(time(0))
                .expect("initial state must send")
                .len(),
            1
        );

        runtime.resume(time(120_000));
        assert!(
            runtime
                .poll(time(120_000))
                .expect("resume poll must work")
                .is_empty()
        );
        assert_eq!(runtime.milliseconds_until_next_poll(time(120_000)), 250);
    }

    #[test]
    fn server_echo_ack_is_exposed_to_the_prediction_host() {
        let mut runtime = SessionRuntime::new(key(), time(0));
        let packet = server_instruction_packet(
            0,
            Instruction {
                bytes: None,
                viewport: None,
                marker: Some(ferrumtty_wire::Marker { value: 17 }),
            },
        );
        assert_eq!(
            runtime
                .receive_datagram(&packet, time(1))
                .expect("echo acknowledgement must decode"),
            vec![SessionAction::AcknowledgePrediction(17)]
        );
    }

    fn server_packet(acknowledged_state: u64, output: &[u8]) -> Vec<u8> {
        server_instruction_packet(
            acknowledged_state,
            Instruction {
                bytes: Some(ByteRun {
                    value: output.to_vec(),
                }),
                viewport: None,
                marker: None,
            },
        )
    }

    fn server_instruction_packet(acknowledged_state: u64, instruction: Instruction) -> Vec<u8> {
        let batch = InstructionBatch {
            instructions: vec![instruction],
        };
        let mut update = StateUpdate::new(0, 1, acknowledged_state);
        update.delta = batch.encode_bytes();
        let compressed = encode_compressed_update(&update).expect("state must compress");
        let fragment = Fragment::split(&compressed, 1, 0, 1)
            .expect("state must fragment")
            .remove(0);
        let mut channel = SecureChannel::new(PeerRole::Server, key());
        channel
            .seal_next(&fragment.encode())
            .expect("server packet must seal")
    }

    fn key() -> SessionKey {
        SessionKey::decode(SYNTHETIC_KEY).expect("synthetic key must decode")
    }

    const fn time(value: u64) -> MonotonicTime {
        MonotonicTime::from_milliseconds(value)
    }
}
