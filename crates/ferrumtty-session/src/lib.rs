// SPDX-License-Identifier: GPL-3.0-only

//! Client-side protocol state independent of sockets and terminal adapters.

use ferrumtty_crypto::{OpenError, PeerRole, SealError, SecureChannel, SessionKey};
use ferrumtty_wire::{
    Fragment, FragmentAccumulator, FragmentError, InstructionBatch, MessageError, StateUpdate,
    decode_compressed_update, encode_compressed_update,
};
use std::collections::BTreeMap;

const NO_ECHO_TIMESTAMP: u16 = u16::MAX;
const MAXIMUM_REASSEMBLED_BYTES: usize = 16 * 1024 * 1024;
const MAXIMUM_ACTIVE_ASSEMBLIES: usize = 64;
const REPLAY_WINDOW_BITS: u64 = 128;

/// Produces authenticated client updates and accepts authenticated server states.
pub struct ClientProtocol {
    secure_channel: SecureChannel,
    next_local_state: u64,
    latest_remote_state: u64,
    latest_peer_timestamp: Option<u16>,
    assemblies: BTreeMap<u64, FragmentAccumulator>,
    pending_update: Option<PendingUpdate>,
    replay_window: ReplayWindow,
}

impl ClientProtocol {
    #[must_use]
    pub fn new(key: SessionKey) -> Self {
        Self {
            secure_channel: SecureChannel::new(PeerRole::Client, key),
            next_local_state: 1,
            latest_remote_state: 0,
            latest_peer_timestamp: None,
            assemblies: BTreeMap::new(),
            pending_update: None,
            replay_window: ReplayWindow::default(),
        }
    }

    /// Encodes one client state update and returns one or more UDP payloads.
    ///
    /// # Errors
    ///
    /// Returns an error for state-number exhaustion, compression, excessive
    /// fragmentation, or authenticated-encryption failure.
    pub fn build_update(
        &mut self,
        sent_timestamp: u16,
        instructions: &InstructionBatch,
    ) -> Result<Vec<Vec<u8>>, SessionError> {
        if self.pending_update.is_some() {
            return Err(SessionError::UpdateAwaitingAcknowledgement);
        }
        let target_state = self.next_local_state;
        let base_state = target_state
            .checked_sub(1)
            .ok_or(SessionError::StateExhausted)?;
        let mut update = StateUpdate::new(base_state, target_state, self.latest_remote_state);
        update.discard_before = base_state;
        update.delta = instructions.encode_bytes();
        let compressed = encode_compressed_update(&update).map_err(SessionError::Message)?;
        let packets = seal_fragments(
            &mut self.secure_channel,
            &compressed,
            sent_timestamp,
            self.latest_peer_timestamp,
            target_state,
        )?;
        self.pending_update = Some(PendingUpdate {
            state_id: target_state,
            compressed,
        });
        self.next_local_state = target_state
            .checked_add(1)
            .ok_or(SessionError::StateExhausted)?;
        Ok(packets)
    }

    /// Re-encrypts the pending state with fresh packet counters after a timeout.
    ///
    /// # Errors
    ///
    /// Returns an error when no state awaits acknowledgement or when fragment
    /// encoding or authenticated encryption fails.
    pub fn retransmit_pending(
        &mut self,
        sent_timestamp: u16,
    ) -> Result<Vec<Vec<u8>>, SessionError> {
        let pending = self
            .pending_update
            .as_ref()
            .ok_or(SessionError::NoPendingUpdate)?;
        seal_fragments(
            &mut self.secure_channel,
            &pending.compressed,
            sent_timestamp,
            self.latest_peer_timestamp,
            pending.state_id,
        )
    }

    /// Accepts one UDP payload and returns a complete server state when ready.
    ///
    /// # Errors
    ///
    /// Returns an error for failed authentication, invalid fragments, bounded
    /// reassembly failures, invalid messages, or mismatched state identifiers.
    pub fn ingest(&mut self, packet: &[u8]) -> Result<Option<ReceivedState>, SessionError> {
        let authenticated = self
            .secure_channel
            .open(packet)
            .map_err(SessionError::Open)?;
        if !self.replay_window.accept(authenticated.counter) {
            return Err(SessionError::ReplayDetected);
        }
        let fragment = Fragment::parse(&authenticated.plaintext).map_err(SessionError::Fragment)?;
        let header = fragment.header;
        self.latest_peer_timestamp = Some(header.sent_timestamp);
        if !self.assemblies.contains_key(&header.state_id)
            && self.assemblies.len() >= MAXIMUM_ACTIVE_ASSEMBLIES
        {
            return Err(SessionError::TooManyActiveAssemblies);
        }
        let assembly = self.assemblies.entry(header.state_id).or_insert_with(|| {
            FragmentAccumulator::new(header.state_id, MAXIMUM_REASSEMBLED_BYTES)
        });
        let Some(compressed) = assembly.push(fragment).map_err(SessionError::Fragment)? else {
            return Ok(None);
        };
        self.assemblies.remove(&header.state_id);
        let update = decode_compressed_update(&compressed).map_err(SessionError::Message)?;
        if self
            .pending_update
            .as_ref()
            .is_some_and(|pending| update.acknowledged_state >= pending.state_id)
        {
            self.pending_update = None;
        }
        let advances_remote_state = update.target_state > self.latest_remote_state
            && update.base_state == self.latest_remote_state;
        if advances_remote_state {
            self.latest_remote_state = update.target_state;
        }
        Ok(Some(ReceivedState {
            packet_counter: authenticated.counter,
            sent_timestamp: header.sent_timestamp,
            echoed_timestamp: header.echoed_timestamp,
            advances_remote_state,
            update,
        }))
    }

    #[must_use]
    pub fn latest_remote_state(&self) -> u64 {
        self.latest_remote_state
    }

    #[must_use]
    pub fn has_pending_update(&self) -> bool {
        self.pending_update.is_some()
    }
}

struct PendingUpdate {
    state_id: u64,
    compressed: Vec<u8>,
}

#[derive(Default)]
struct ReplayWindow {
    highest: Option<u64>,
    seen: u128,
}

impl ReplayWindow {
    fn accept(&mut self, counter: u64) -> bool {
        let Some(highest) = self.highest else {
            self.highest = Some(counter);
            self.seen = 1;
            return true;
        };
        if counter > highest {
            let advance = counter - highest;
            self.seen = if advance >= REPLAY_WINDOW_BITS {
                1
            } else {
                (self.seen << advance) | 1
            };
            self.highest = Some(counter);
            return true;
        }

        let age = highest - counter;
        if age >= REPLAY_WINDOW_BITS {
            return false;
        }
        let bit = 1_u128 << age;
        if self.seen & bit != 0 {
            return false;
        }
        self.seen |= bit;
        true
    }
}

fn seal_fragments(
    channel: &mut SecureChannel,
    compressed: &[u8],
    sent_timestamp: u16,
    latest_peer_timestamp: Option<u16>,
    state_id: u64,
) -> Result<Vec<Vec<u8>>, SessionError> {
    let fragments = Fragment::split(
        compressed,
        sent_timestamp,
        latest_peer_timestamp.unwrap_or(NO_ECHO_TIMESTAMP),
        state_id,
    )
    .map_err(SessionError::Fragment)?;
    fragments
        .into_iter()
        .map(|fragment| {
            channel
                .seal_next(&fragment.encode())
                .map_err(SessionError::Seal)
        })
        .collect()
}

#[derive(Debug, PartialEq)]
pub struct ReceivedState {
    pub packet_counter: u64,
    pub sent_timestamp: u16,
    pub echoed_timestamp: u16,
    pub advances_remote_state: bool,
    pub update: StateUpdate,
}

#[derive(Debug)]
pub enum SessionError {
    StateExhausted,
    UpdateAwaitingAcknowledgement,
    NoPendingUpdate,
    ReplayDetected,
    TooManyActiveAssemblies,
    Seal(SealError),
    Open(OpenError),
    Fragment(FragmentError),
    Message(MessageError),
}

#[cfg(test)]
mod tests {
    use super::{ClientProtocol, ReplayWindow};
    use ferrumtty_crypto::{PeerRole, SecureChannel, SessionKey};
    use ferrumtty_wire::{
        ByteRun, Fragment, Instruction, InstructionBatch, StateUpdate, decode_compressed_update,
        encode_compressed_update,
    };

    const SYNTHETIC_KEY: &str = "AAECAwQFBgcICQoLDA0ODw";

    #[test]
    fn initial_update_matches_observed_state_shape() {
        let mut client = ClientProtocol::new(
            SessionKey::decode(SYNTHETIC_KEY).expect("synthetic key must decode"),
        );
        let instructions = InstructionBatch {
            instructions: vec![Instruction {
                bytes: Some(ByteRun {
                    value: b"exit\n".to_vec(),
                }),
                viewport: None,
                marker: None,
            }],
        };
        let packets = client
            .build_update(0x1234, &instructions)
            .expect("initial update must encode");
        assert_eq!(packets.len(), 1);

        let server_channel = SecureChannel::new(
            PeerRole::Server,
            SessionKey::decode(SYNTHETIC_KEY).expect("synthetic key must decode"),
        );
        let plaintext = server_channel
            .open(&packets[0])
            .expect("server side must authenticate")
            .plaintext;
        let fragment = Fragment::parse(&plaintext).expect("fragment must parse");
        let update = decode_compressed_update(&fragment.body).expect("state must decode");

        assert_eq!((update.base_state, update.target_state), (0, 1));
        assert_eq!(update.acknowledged_state, 0);
        assert_eq!(
            update
                .decode_instructions()
                .expect("instructions must decode")
                .instructions[0]
                .bytes
                .as_ref()
                .map(|bytes| bytes.value.as_slice()),
            Some(b"exit\n".as_slice())
        );
    }

    #[test]
    fn retransmission_uses_new_packet_counter_and_clears_on_acknowledgement() {
        let mut client = ClientProtocol::new(
            SessionKey::decode(SYNTHETIC_KEY).expect("synthetic key must decode"),
        );
        let empty = InstructionBatch {
            instructions: Vec::new(),
        };
        let first = client
            .build_update(1, &empty)
            .expect("initial update must encode");
        let retransmitted = client
            .retransmit_pending(2)
            .expect("pending update must retransmit");
        assert_eq!(&first[0][..8], &[0; 8]);
        assert_eq!(&retransmitted[0][..8], &[0, 0, 0, 0, 0, 0, 0, 1]);

        let mut server_channel = SecureChannel::new(
            PeerRole::Server,
            SessionKey::decode(SYNTHETIC_KEY).expect("synthetic key must decode"),
        );
        let acknowledgement = StateUpdate::new(0, 1, 1);
        let compressed =
            encode_compressed_update(&acknowledgement).expect("acknowledgement must compress");
        let fragment = Fragment::split(&compressed, 3, 2, 1)
            .expect("acknowledgement must fragment")
            .remove(0);
        let packet = server_channel
            .seal_next(&fragment.encode())
            .expect("server packet must seal");

        let received = client
            .ingest(&packet)
            .expect("acknowledgement must open")
            .expect("acknowledgement must complete");
        assert!(received.advances_remote_state);
        assert!(!client.has_pending_update());

        let retransmitted_packet = server_channel
            .seal_next(&fragment.encode())
            .expect("server retransmission must seal under a fresh counter");
        let retransmitted = client
            .ingest(&retransmitted_packet)
            .expect("server retransmission must authenticate")
            .expect("server retransmission must complete");
        assert!(!retransmitted.advances_remote_state);
        assert_eq!(client.latest_remote_state(), 1);
    }

    #[test]
    fn replay_window_accepts_reordering_once() {
        let mut window = ReplayWindow::default();

        assert!(window.accept(10));
        assert!(window.accept(12));
        assert!(window.accept(11));
        assert!(!window.accept(11));
        assert!(!window.accept(10));
        assert!(window.accept(140));
        assert!(!window.accept(12));
    }

    #[test]
    fn fragmented_server_state_converges_after_loss_and_reordering() {
        let mut client = ClientProtocol::new(
            SessionKey::decode(SYNTHETIC_KEY).expect("synthetic key must decode"),
        );
        let empty = InstructionBatch {
            instructions: Vec::new(),
        };
        let first_client_packets = client
            .build_update(1, &empty)
            .expect("initial update must encode");
        assert_eq!(first_client_packets.len(), 1);

        // Dropping the initial datagram must leave the exact logical update
        // available for resealing under a fresh packet counter.
        let retransmitted = client
            .retransmit_pending(2)
            .expect("dropped update must remain pending");
        let server_reader = SecureChannel::new(
            PeerRole::Server,
            SessionKey::decode(SYNTHETIC_KEY).expect("synthetic key must decode"),
        );
        let retransmitted_plaintext = server_reader
            .open(&retransmitted[0])
            .expect("retransmission must authenticate")
            .plaintext;
        let retransmitted_update = decode_compressed_update(
            &Fragment::parse(&retransmitted_plaintext)
                .expect("retransmission must contain a fragment")
                .body,
        )
        .expect("retransmission must preserve the update");
        assert_eq!(
            (
                retransmitted_update.base_state,
                retransmitted_update.target_state
            ),
            (0, 1)
        );

        let mut pseudo_random_state = 0x1234_5678_u32;
        let terminal_bytes = (0..3_000)
            .map(|_| {
                pseudo_random_state ^= pseudo_random_state << 13;
                pseudo_random_state ^= pseudo_random_state >> 17;
                pseudo_random_state ^= pseudo_random_state << 5;
                pseudo_random_state.to_le_bytes()[0]
            })
            .collect::<Vec<_>>();
        let instructions = InstructionBatch {
            instructions: vec![Instruction {
                bytes: Some(ByteRun {
                    value: terminal_bytes.clone(),
                }),
                viewport: None,
                marker: None,
            }],
        };
        let mut server_update = StateUpdate::new(0, 2, 1);
        server_update.delta = instructions.encode_bytes();
        let compressed =
            encode_compressed_update(&server_update).expect("server state must compress");
        let fragments = Fragment::split(&compressed, 3, 2, 77).expect("server state must fragment");
        assert!(fragments.len() > 1);

        let mut server_writer = SecureChannel::new(
            PeerRole::Server,
            SessionKey::decode(SYNTHETIC_KEY).expect("synthetic key must decode"),
        );
        let mut packets = fragments
            .into_iter()
            .map(|fragment| {
                server_writer
                    .seal_next(&fragment.encode())
                    .expect("server fragment must seal")
            })
            .collect::<Vec<_>>();
        packets.reverse();

        let mut received = None;
        for packet in &packets {
            if let Some(state) = client.ingest(packet).expect("reordered packet must open") {
                received = Some(state);
            }
        }
        let received = received.expect("all reordered fragments must converge");
        assert_eq!(received.update.target_state, 2);
        assert_eq!(
            received
                .update
                .decode_instructions()
                .expect("instructions must decode")
                .instructions[0]
                .bytes
                .as_ref()
                .expect("terminal bytes must exist")
                .value,
            terminal_bytes
        );
        assert!(!client.has_pending_update());
        assert!(matches!(
            client.ingest(&packets[0]),
            Err(super::SessionError::ReplayDetected)
        ));
    }
}
