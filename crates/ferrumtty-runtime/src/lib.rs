// SPDX-License-Identifier: GPL-3.0-only

//! Deterministic host-facing session runtime without socket or terminal access.

use ferrumtty_crypto::SessionKey;
use ferrumtty_session::{ClientProtocol, SessionError};
use ferrumtty_wire::{ByteRun, Instruction, InstructionBatch, MessageError, ViewportSize};
use std::collections::VecDeque;
use std::fmt;

const INITIAL_RETRANSMIT_MILLISECONDS: u64 = 250;
const MAXIMUM_RETRANSMIT_MILLISECONDS: u64 = 2_000;
const HEARTBEAT_MILLISECONDS: u64 = 3_000;
const SERVER_UNRESPONSIVE_MILLISECONDS: u64 = 30_000;
const MAXIMUM_QUEUED_INPUT_BYTES: usize = 1024 * 1024;
const SHUTDOWN_RETRY_LIMIT: u8 = 16;
const SHUTDOWN_TIMEOUT_MILLISECONDS: u64 = 10_000;

/// Version of the host-facing runtime contract documented by this crate.
pub const EMBEDDING_API_VERSION: u16 = 4;

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
#[derive(Eq, PartialEq)]
pub enum SessionAction {
    SendDatagram(Vec<u8>),
    WriteTerminal(Vec<u8>),
    AcknowledgePrediction(u64),
    RemoteStateAdvanced(u64),
    ConnectionStateChanged(ConnectionState),
    RoundTripEstimate(u16),
    SessionLifecycleChanged(SessionLifecycle),
    ShutdownComplete(ShutdownOutcome),
    UdpBindingChanged(u64),
    /// Reports content-free protocol metadata to an embedding host.
    Diagnostic(DiagnosticEvent),
}

impl fmt::Debug for SessionAction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SendDatagram(bytes) => formatter
                .debug_struct("SendDatagram")
                .field("bytes", &bytes.len())
                .finish(),
            Self::WriteTerminal(bytes) => formatter
                .debug_struct("WriteTerminal")
                .field("bytes", &bytes.len())
                .finish(),
            Self::AcknowledgePrediction(value) => formatter
                .debug_tuple("AcknowledgePrediction")
                .field(value)
                .finish(),
            Self::RemoteStateAdvanced(value) => formatter
                .debug_tuple("RemoteStateAdvanced")
                .field(value)
                .finish(),
            Self::ConnectionStateChanged(value) => formatter
                .debug_tuple("ConnectionStateChanged")
                .field(value)
                .finish(),
            Self::RoundTripEstimate(value) => formatter
                .debug_tuple("RoundTripEstimate")
                .field(value)
                .finish(),
            Self::SessionLifecycleChanged(value) => formatter
                .debug_tuple("SessionLifecycleChanged")
                .field(value)
                .finish(),
            Self::ShutdownComplete(value) => formatter
                .debug_tuple("ShutdownComplete")
                .field(value)
                .finish(),
            Self::UdpBindingChanged(value) => formatter
                .debug_tuple("UdpBindingChanged")
                .field(value)
                .finish(),
            Self::Diagnostic(value) => formatter.debug_tuple("Diagnostic").field(value).finish(),
        }
    }
}

/// Content-free protocol metadata suitable for host-controlled diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticEvent {
    ConnectionStateChanged {
        state: ConnectionState,
    },
    FreshUpdatePrepared {
        state_id: u64,
        datagram_count: u64,
        datagram_bytes: u64,
        instruction_count: u64,
        input_bytes: u64,
    },
    RetransmissionPrepared {
        state_id: u64,
        datagram_count: u64,
        datagram_bytes: u64,
        retransmit_delay_milliseconds: u64,
    },
    InboundUpdateAccepted {
        packet_counter: u64,
        base_state: u64,
        target_state: u64,
        acknowledged_state: u64,
        discard_before: u64,
        delta_bytes: u64,
        advances_remote_state: bool,
    },
    RoundTripUpdated {
        milliseconds: u16,
    },
    SessionLifecycleChanged {
        state: SessionLifecycle,
    },
    UdpBindingChanged {
        generation: u64,
    },
    ShutdownStarted,
    ShutdownComplete {
        outcome: ShutdownOutcome,
    },
}

/// Describes why the bounded SSP shutdown handshake ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShutdownOutcome {
    Acknowledged,
    PeerRequested,
    TimedOut,
}

/// Describes whether an embedding host is actively driving the session.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SessionLifecycle {
    #[default]
    Running,
    Paused,
    Cancelled,
}

/// A terminal input event supplied by an embedding host without using stdio.
#[derive(Eq, PartialEq)]
pub enum TerminalInputEvent {
    Bytes(Vec<u8>),
    Resize { columns: u16, rows: u16 },
}

impl fmt::Debug for TerminalInputEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bytes(bytes) => formatter
                .debug_struct("Bytes")
                .field("length", &bytes.len())
                .finish(),
            Self::Resize { columns, rows } => formatter
                .debug_struct("Resize")
                .field("columns", columns)
                .field("rows", rows)
                .finish(),
        }
    }
}

/// Describes whether an authenticated server session has made recent progress.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ConnectionState {
    /// No authenticated server state has been received yet.
    #[default]
    Connecting,
    /// Authenticated server traffic is arriving normally.
    Connected,
    /// A previously connected session has stopped responding temporarily.
    Interrupted,
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
    connection_state: ConnectionState,
    lifecycle: SessionLifecycle,
    udp_binding_generation: u64,
    shutdown: ShutdownState,
    peer_shutdown_acknowledgement_due: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ShutdownState {
    Open,
    LocalRequested {
        started_at: MonotonicTime,
        transmissions: u8,
    },
    PeerRequested,
    Complete(ShutdownOutcome),
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
            connection_state: ConnectionState::Connecting,
            lifecycle: SessionLifecycle::Running,
            udp_binding_generation: 0,
            shutdown: ShutdownState::Open,
            peer_shutdown_acknowledgement_due: false,
        }
    }

    /// Queues a host-provided terminal event without reading process stdio.
    ///
    /// # Errors
    ///
    /// Returns an error if the session is paused, cancelled, shutting down, or
    /// the input queue would exceed its fixed memory bound.
    pub fn queue_terminal_event(&mut self, event: TerminalInputEvent) -> Result<(), RuntimeError> {
        self.ensure_accepting_input()?;
        match event {
            TerminalInputEvent::Bytes(bytes) => self.queue_input(bytes),
            TerminalInputEvent::Resize { columns, rows } => {
                self.queue_resize(columns, rows);
                Ok(())
            }
        }
    }

    /// Queues terminal input under a fixed memory bound.
    ///
    /// # Errors
    ///
    /// Returns an error if the runtime cannot accept input or if total unsent
    /// input would exceed the bound.
    pub fn queue_input(&mut self, bytes: Vec<u8>) -> Result<(), RuntimeError> {
        self.ensure_accepting_input()?;
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
        if self.lifecycle != SessionLifecycle::Running || self.shutdown != ShutdownState::Open {
            return;
        }
        self.protocol.set_initial_remote_viewport(columns, rows);
        self.pending_resize = Some(ViewportSize {
            columns: u64::from(columns),
            rows: u64::from(rows),
        });
    }

    /// Pauses timers and rejects new host input until `resume` is called.
    #[must_use]
    pub fn pause(&mut self) -> Vec<SessionAction> {
        if self.lifecycle != SessionLifecycle::Running {
            return Vec::new();
        }
        self.lifecycle = SessionLifecycle::Paused;
        lifecycle_actions(SessionLifecycle::Paused)
    }

    /// Cancels further protocol work and clears queued, unsent host input.
    ///
    /// The embedding host should drop the runtime after applying the returned
    /// event so its in-memory session key is released immediately.
    #[must_use]
    pub fn cancel(&mut self) -> Vec<SessionAction> {
        if self.lifecycle == SessionLifecycle::Cancelled {
            return Vec::new();
        }
        self.lifecycle = SessionLifecycle::Cancelled;
        self.queued_instructions.clear();
        self.pending_resize = None;
        self.queued_input_bytes = 0;
        lifecycle_actions(SessionLifecycle::Cancelled)
    }

    /// Starts the bounded SSP clean-shutdown handshake.
    ///
    /// # Errors
    ///
    /// Returns an error if the runtime is paused or cancelled.
    pub fn request_shutdown(
        &mut self,
        now: MonotonicTime,
    ) -> Result<Vec<SessionAction>, RuntimeError> {
        self.ensure_running()?;
        if self.shutdown == ShutdownState::Open {
            self.shutdown = ShutdownState::LocalRequested {
                started_at: now,
                transmissions: 0,
            };
            return Ok(vec![SessionAction::Diagnostic(
                DiagnosticEvent::ShutdownStarted,
            )]);
        }
        Ok(Vec::new())
    }

    /// Reports a host-managed local UDP rebind without changing the trusted peer.
    #[must_use]
    pub fn notify_udp_rebound(&mut self) -> Vec<SessionAction> {
        if self.lifecycle == SessionLifecycle::Cancelled {
            return Vec::new();
        }
        self.udp_binding_generation = self.udp_binding_generation.saturating_add(1);
        vec![
            SessionAction::UdpBindingChanged(self.udp_binding_generation),
            SessionAction::Diagnostic(DiagnosticEvent::UdpBindingChanged {
                generation: self.udp_binding_generation,
            }),
        ]
    }

    /// Re-arms liveness after the host reports a system resume or another
    /// interval in which network progress could not have been observed.
    pub fn resume(&mut self, now: MonotonicTime) {
        self.resume_state(now);
    }

    /// Resumes a paused session and returns the structured lifecycle event.
    #[must_use]
    pub fn resume_with_actions(&mut self, now: MonotonicTime) -> Vec<SessionAction> {
        if self.lifecycle == SessionLifecycle::Cancelled {
            return Vec::new();
        }
        let was_paused = self.lifecycle == SessionLifecycle::Paused;
        self.resume_state(now);
        if was_paused {
            lifecycle_actions(SessionLifecycle::Running)
        } else {
            Vec::new()
        }
    }

    fn resume_state(&mut self, now: MonotonicTime) {
        if self.lifecycle == SessionLifecycle::Cancelled {
            return;
        }
        self.lifecycle = SessionLifecycle::Running;
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
        self.ensure_running()?;
        let state = self
            .protocol
            .ingest(packet)
            .map_err(RuntimeError::Session)?;
        self.last_receive = now;
        let mut actions = Vec::new();
        if self.connection_state != ConnectionState::Connected {
            self.connection_state = ConnectionState::Connected;
            actions.push(SessionAction::ConnectionStateChanged(
                ConnectionState::Connected,
            ));
            actions.push(SessionAction::Diagnostic(
                DiagnosticEvent::ConnectionStateChanged {
                    state: ConnectionState::Connected,
                },
            ));
        }
        let Some(state) = state else {
            return Ok(actions);
        };
        if state.echoed_timestamp != u16::MAX {
            let elapsed = timestamp(now).wrapping_sub(state.echoed_timestamp);
            if i16::try_from(elapsed).is_ok() && self.round_trip_milliseconds != Some(elapsed) {
                self.round_trip_milliseconds = Some(elapsed);
                actions.push(SessionAction::RoundTripEstimate(elapsed));
                actions.push(SessionAction::Diagnostic(
                    DiagnosticEvent::RoundTripUpdated {
                        milliseconds: elapsed,
                    },
                ));
            }
        }
        self.acknowledgement_due = true;
        self.received_server_state = true;
        if state.update.target_state == u64::MAX {
            self.peer_shutdown_acknowledgement_due = true;
            if self.shutdown == ShutdownState::Open {
                self.shutdown = ShutdownState::PeerRequested;
            }
        }
        actions.push(SessionAction::Diagnostic(
            DiagnosticEvent::InboundUpdateAccepted {
                packet_counter: state.packet_counter,
                base_state: state.update.base_state,
                target_state: state.update.target_state,
                acknowledged_state: state.update.acknowledged_state,
                discard_before: state.update.discard_before,
                delta_bytes: usize_as_u64(state.update.delta.len()),
                advances_remote_state: state.advances_remote_state,
            },
        ));
        if !state.advances_remote_state {
            return Ok(actions);
        }
        actions.push(SessionAction::RemoteStateAdvanced(
            state.update.target_state,
        ));
        let instructions = state
            .update
            .decode_instructions()
            .map_err(RuntimeError::Message)?;
        actions.extend(
            instructions
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
                }),
        );
        Ok(actions)
    }

    /// Advances timers and returns datagrams that should be sent now.
    ///
    /// # Errors
    ///
    /// Returns an error for protocol encoding failure.
    pub fn poll(&mut self, now: MonotonicTime) -> Result<Vec<SessionAction>, RuntimeError> {
        if self.lifecycle != SessionLifecycle::Running {
            return Ok(Vec::new());
        }
        if matches!(self.shutdown, ShutdownState::Complete(_)) {
            return Ok(Vec::new());
        }
        if self.protocol.local_shutdown_acknowledged() && !self.peer_shutdown_acknowledgement_due {
            return Ok(self.complete_shutdown(ShutdownOutcome::Acknowledged));
        }
        if !self.peer_shutdown_acknowledgement_due && self.shutdown_timed_out(now) {
            return Ok(self.complete_shutdown(ShutdownOutcome::TimedOut));
        }
        let mut actions = self
            .connection_state_action(now)
            .into_iter()
            .flat_map(|state| {
                [
                    SessionAction::ConnectionStateChanged(state),
                    SessionAction::Diagnostic(DiagnosticEvent::ConnectionStateChanged { state }),
                ]
            })
            .collect::<Vec<_>>();
        let has_queued_state =
            !self.queued_instructions.is_empty() || self.pending_resize.is_some();
        let local_shutdown_unsent = matches!(self.shutdown, ShutdownState::LocalRequested { .. })
            && !self.protocol.local_shutdown_sent();
        if has_queued_state || local_shutdown_unsent {
            let (packets, diagnostic) = self.build_queued_update(now)?;
            actions.extend(send_actions(packets));
            actions.push(SessionAction::Diagnostic(diagnostic));
            self.record_shutdown_transmission();
            self.complete_peer_shutdown_after_ack(&mut actions);
            return Ok(actions);
        }

        if self.protocol.has_pending_update() {
            let last_send = self.last_send.unwrap_or(now);
            if !self.acknowledgement_due
                && now.elapsed_since(last_send) < self.retransmit_milliseconds
            {
                return Ok(actions);
            }
            let packets = self
                .protocol
                .retransmit_pending(timestamp(now))
                .map_err(RuntimeError::Session)?;
            let diagnostic = retransmission_diagnostic(
                self.protocol.latest_local_state(),
                &packets,
                self.retransmit_milliseconds,
            );
            self.last_send = Some(now);
            self.retransmit_milliseconds = self
                .retransmit_milliseconds
                .saturating_mul(2)
                .min(MAXIMUM_RETRANSMIT_MILLISECONDS);
            self.acknowledgement_due = false;
            actions.extend(send_actions(packets));
            actions.push(SessionAction::Diagnostic(diagnostic));
            self.record_shutdown_transmission();
            self.complete_peer_shutdown_after_ack(&mut actions);
            return Ok(actions);
        }

        let heartbeat_due = self
            .last_send
            .is_none_or(|last_send| now.elapsed_since(last_send) >= HEARTBEAT_MILLISECONDS);
        if !self.acknowledgement_due && !heartbeat_due {
            return Ok(actions);
        }
        let (packets, diagnostic) = self.build_queued_update(now)?;
        actions.extend(send_actions(packets));
        actions.push(SessionAction::Diagnostic(diagnostic));
        self.complete_peer_shutdown_after_ack(&mut actions);
        Ok(actions)
    }

    fn build_queued_update(
        &mut self,
        now: MonotonicTime,
    ) -> Result<(Vec<Vec<u8>>, DiagnosticEvent), RuntimeError> {
        let state_id = if (matches!(self.shutdown, ShutdownState::LocalRequested { .. })
            && !self.protocol.local_shutdown_sent())
            || (self.peer_shutdown_acknowledgement_due
                && self.protocol.local_shutdown_acknowledged())
        {
            u64::MAX
        } else {
            self.protocol.next_local_state()
        };
        let input_bytes = self.queued_input_bytes;
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
        let instruction_count = instructions.instructions.len();
        self.queued_input_bytes = 0;
        self.acknowledgement_due = false;
        let packets = if self.peer_shutdown_acknowledgement_due
            && self.protocol.local_shutdown_acknowledged()
        {
            self.protocol
                .build_shutdown_ack(timestamp(now))
                .map_err(RuntimeError::Session)?
        } else if matches!(self.shutdown, ShutdownState::LocalRequested { .. })
            && !self.protocol.local_shutdown_sent()
        {
            self.protocol
                .build_shutdown(timestamp(now), &instructions)
                .map_err(RuntimeError::Session)?
        } else {
            self.protocol
                .build_update(timestamp(now), &instructions)
                .map_err(RuntimeError::Session)?
        };
        self.last_send = Some(now);
        self.retransmit_milliseconds = INITIAL_RETRANSMIT_MILLISECONDS;
        let diagnostic =
            fresh_update_diagnostic(state_id, &packets, instruction_count, input_bytes);
        Ok((packets, diagnostic))
    }

    #[must_use]
    pub fn milliseconds_until_next_poll(&self, now: MonotonicTime) -> u64 {
        if self.lifecycle != SessionLifecycle::Running
            || matches!(self.shutdown, ShutdownState::Complete(_))
        {
            return u64::MAX;
        }
        let protocol_wait = if self.acknowledgement_due
            || !self.queued_instructions.is_empty()
            || self.pending_resize.is_some()
            || (matches!(self.shutdown, ShutdownState::LocalRequested { .. })
                && !self.protocol.local_shutdown_sent())
        {
            0
        } else if self.protocol.has_pending_update() {
            self.last_send.map_or(0, |last_send| {
                self.retransmit_milliseconds
                    .saturating_sub(now.elapsed_since(last_send))
            })
        } else {
            self.last_send.map_or(0, |last_send| {
                HEARTBEAT_MILLISECONDS.saturating_sub(now.elapsed_since(last_send))
            })
        };
        match self.shutdown {
            ShutdownState::LocalRequested {
                started_at,
                transmissions,
            } => {
                let timeout_wait = if transmissions >= SHUTDOWN_RETRY_LIMIT {
                    0
                } else {
                    SHUTDOWN_TIMEOUT_MILLISECONDS.saturating_sub(now.elapsed_since(started_at))
                };
                protocol_wait.min(timeout_wait)
            }
            _ => protocol_wait,
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

    /// Returns the SSP frame that will contain input queued before the next poll.
    #[must_use]
    pub fn prediction_frame_id(&self) -> u64 {
        self.protocol.next_local_state()
    }

    #[must_use]
    pub const fn connection_state(&self) -> ConnectionState {
        self.connection_state
    }

    #[must_use]
    pub const fn lifecycle(&self) -> SessionLifecycle {
        self.lifecycle
    }

    #[must_use]
    pub const fn shutdown_outcome(&self) -> Option<ShutdownOutcome> {
        match self.shutdown {
            ShutdownState::Complete(outcome) => Some(outcome),
            _ => None,
        }
    }

    #[must_use]
    pub const fn shutdown_in_progress(&self) -> bool {
        !matches!(
            self.shutdown,
            ShutdownState::Open | ShutdownState::Complete(_)
        )
    }

    /// Returns the time since the most recent authenticated server datagram.
    #[must_use]
    pub fn milliseconds_since_server_response(&self, now: MonotonicTime) -> u64 {
        now.elapsed_since(self.last_receive)
    }

    fn connection_state_action(&mut self, now: MonotonicTime) -> Option<ConnectionState> {
        if self.connection_state == ConnectionState::Connected && !self.is_server_responsive(now) {
            self.connection_state = ConnectionState::Interrupted;
            return Some(ConnectionState::Interrupted);
        }
        None
    }

    fn ensure_running(&self) -> Result<(), RuntimeError> {
        match self.lifecycle {
            SessionLifecycle::Running => Ok(()),
            SessionLifecycle::Paused => Err(RuntimeError::SessionPaused),
            SessionLifecycle::Cancelled => Err(RuntimeError::SessionCancelled),
        }
    }

    fn ensure_accepting_input(&self) -> Result<(), RuntimeError> {
        self.ensure_running()?;
        if self.shutdown != ShutdownState::Open {
            return Err(RuntimeError::ShutdownInProgress);
        }
        Ok(())
    }

    fn shutdown_timed_out(&self, now: MonotonicTime) -> bool {
        let ShutdownState::LocalRequested {
            started_at,
            transmissions,
        } = self.shutdown
        else {
            return false;
        };
        transmissions >= SHUTDOWN_RETRY_LIMIT
            || now.elapsed_since(started_at) >= SHUTDOWN_TIMEOUT_MILLISECONDS
    }

    fn record_shutdown_transmission(&mut self) {
        if let ShutdownState::LocalRequested { transmissions, .. } = &mut self.shutdown {
            *transmissions = transmissions.saturating_add(1);
        }
    }

    fn complete_peer_shutdown_after_ack(&mut self, actions: &mut Vec<SessionAction>) {
        if self.peer_shutdown_acknowledgement_due {
            self.peer_shutdown_acknowledgement_due = false;
            actions.extend(self.complete_shutdown(ShutdownOutcome::PeerRequested));
        }
    }

    fn complete_shutdown(&mut self, outcome: ShutdownOutcome) -> Vec<SessionAction> {
        self.shutdown = ShutdownState::Complete(outcome);
        vec![
            SessionAction::ShutdownComplete(outcome),
            SessionAction::Diagnostic(DiagnosticEvent::ShutdownComplete { outcome }),
        ]
    }
}

fn lifecycle_actions(state: SessionLifecycle) -> Vec<SessionAction> {
    vec![
        SessionAction::SessionLifecycleChanged(state),
        SessionAction::Diagnostic(DiagnosticEvent::SessionLifecycleChanged { state }),
    ]
}

fn fresh_update_diagnostic(
    state_id: u64,
    packets: &[Vec<u8>],
    instruction_count: usize,
    input_bytes: usize,
) -> DiagnosticEvent {
    DiagnosticEvent::FreshUpdatePrepared {
        state_id,
        datagram_count: usize_as_u64(packets.len()),
        datagram_bytes: total_packet_bytes(packets),
        instruction_count: usize_as_u64(instruction_count),
        input_bytes: usize_as_u64(input_bytes),
    }
}

fn retransmission_diagnostic(
    state_id: u64,
    packets: &[Vec<u8>],
    retransmit_delay_milliseconds: u64,
) -> DiagnosticEvent {
    DiagnosticEvent::RetransmissionPrepared {
        state_id,
        datagram_count: usize_as_u64(packets.len()),
        datagram_bytes: total_packet_bytes(packets),
        retransmit_delay_milliseconds,
    }
}

fn total_packet_bytes(packets: &[Vec<u8>]) -> u64 {
    packets.iter().fold(0_u64, |total, packet| {
        total.saturating_add(usize_as_u64(packet.len()))
    })
}

fn usize_as_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
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
    SessionPaused,
    SessionCancelled,
    ShutdownInProgress,
    Session(SessionError),
    Message(MessageError),
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InputQueueFull => formatter.write_str("terminal input queue is full"),
            Self::SessionPaused => formatter.write_str("session is paused"),
            Self::SessionCancelled => formatter.write_str("session is cancelled"),
            Self::ShutdownInProgress => formatter.write_str("session shutdown is in progress"),
            Self::Session(error) => write!(formatter, "session protocol failed: {error:?}"),
            Self::Message(error) => write!(formatter, "session message failed: {error:?}"),
        }
    }
}

impl std::error::Error for RuntimeError {}

#[cfg(test)]
mod tests {
    use super::{
        ConnectionState, DiagnosticEvent, MonotonicTime, RuntimeError, SessionAction,
        SessionLifecycle, SessionRuntime, ShutdownOutcome, TerminalInputEvent,
    };
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
        assert_eq!(non_diagnostic_actions(first).len(), 1);
        assert!(
            runtime
                .poll(time(249))
                .expect("early poll must work")
                .is_empty()
        );
        assert_eq!(
            non_diagnostic_actions(runtime.poll(time(250)).expect("retry must send")).len(),
            1
        );
        assert_eq!(runtime.milliseconds_until_next_poll(time(250)), 500);

        let server_packet = server_packet(1, b"ready");
        assert_eq!(
            non_diagnostic_actions(
                runtime
                    .receive_datagram(&server_packet, time(300))
                    .expect("server state must open")
            ),
            vec![
                SessionAction::ConnectionStateChanged(ConnectionState::Connected),
                SessionAction::RoundTripEstimate(300),
                SessionAction::RemoteStateAdvanced(1),
                SessionAction::WriteTerminal(b"ready".to_vec()),
            ]
        );
        assert!(runtime.has_received_server_state());
        assert_eq!(runtime.milliseconds_until_next_poll(time(300)), 0);
        assert_eq!(
            non_diagnostic_actions(runtime.poll(time(300)).expect("ack must send")).len(),
            1
        );
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
    fn new_input_advances_while_older_states_await_acknowledgement() {
        let mut runtime = SessionRuntime::new(key(), time(0));
        let mut server_channel = SecureChannel::new(PeerRole::Server, key());

        runtime
            .queue_input(b"a".to_vec())
            .expect("input must queue");
        let first = runtime.poll(time(0)).expect("first input must send");
        let first_update = decode_sent_update(&first, &mut server_channel);
        assert_eq!((first_update.base_state, first_update.target_state), (0, 1));

        runtime
            .queue_input(b"b".to_vec())
            .expect("input must queue");
        let second = runtime.poll(time(1)).expect("second input must send");
        let second_update = decode_sent_update(&second, &mut server_channel);
        assert_eq!(
            (second_update.base_state, second_update.target_state),
            (1, 2)
        );

        let retransmitted = runtime.poll(time(251)).expect("latest state must retry");
        let retransmitted_update = decode_sent_update(&retransmitted, &mut server_channel);
        assert_eq!(
            (
                retransmitted_update.base_state,
                retransmitted_update.target_state
            ),
            (0, 2)
        );
        let instructions = retransmitted_update
            .decode_instructions()
            .expect("retransmitted input must decode");
        assert_eq!(instructions.instructions.len(), 2);
    }

    #[test]
    fn resume_prevents_sleep_interval_from_becoming_server_timeout() {
        let mut runtime = SessionRuntime::new(key(), time(0));
        runtime.queue_resize(80, 24);
        assert_eq!(
            non_diagnostic_actions(runtime.poll(time(0)).expect("initial state must send")).len(),
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
            non_diagnostic_actions(
                runtime
                    .receive_datagram(&packet, time(1))
                    .expect("echo acknowledgement must decode")
            ),
            vec![
                SessionAction::ConnectionStateChanged(ConnectionState::Connected),
                SessionAction::RoundTripEstimate(1),
                SessionAction::RemoteStateAdvanced(1),
                SessionAction::AcknowledgePrediction(17),
            ]
        );
    }

    #[test]
    fn connection_events_distinguish_initial_and_interrupted_sessions() {
        let mut runtime = SessionRuntime::new(key(), time(0));
        assert_eq!(runtime.connection_state(), ConnectionState::Connecting);
        assert_eq!(runtime.milliseconds_since_server_response(time(12)), 12);
        assert!(
            runtime
                .poll(time(30_000))
                .expect("poll must work")
                .iter()
                .all(|action| !matches!(action, SessionAction::ConnectionStateChanged(_)))
        );

        let packet = server_packet(0, b"connected");
        let connected = runtime
            .receive_datagram(&packet, time(30_001))
            .expect("server state must open");
        assert!(connected.contains(&SessionAction::ConnectionStateChanged(
            ConnectionState::Connected
        )));

        let interrupted = runtime.poll(time(60_001)).expect("poll must work");
        assert!(interrupted.contains(&SessionAction::ConnectionStateChanged(
            ConnectionState::Interrupted
        )));
        assert_eq!(runtime.connection_state(), ConnectionState::Interrupted);
    }

    #[test]
    fn diagnostics_report_only_content_free_protocol_metadata() {
        let mut runtime = SessionRuntime::new(key(), time(0));
        runtime
            .queue_input(b"CLIENT_INPUT_SENTINEL".to_vec())
            .expect("sentinel input must queue");
        let outbound = runtime.poll(time(0)).expect("input must produce an update");
        assert!(outbound.iter().any(|action| matches!(
            action,
            SessionAction::Diagnostic(DiagnosticEvent::FreshUpdatePrepared {
                state_id: 1,
                datagram_count: 1,
                instruction_count: 1,
                input_bytes: 21,
                ..
            })
        )));
        let retransmission = runtime.poll(time(250)).expect("update must retransmit");
        assert!(retransmission.iter().any(|action| matches!(
            action,
            SessionAction::Diagnostic(DiagnosticEvent::RetransmissionPrepared {
                state_id: 1,
                datagram_count: 1,
                retransmit_delay_milliseconds: 250,
                ..
            })
        )));

        let inbound_packet = server_packet(1, b"SERVER_OUTPUT_SENTINEL");
        let inbound = runtime
            .receive_datagram(&inbound_packet, time(300))
            .expect("server update must open");
        assert!(inbound.iter().any(|action| matches!(
            action,
            SessionAction::Diagnostic(DiagnosticEvent::InboundUpdateAccepted {
                packet_counter: 0,
                base_state: 0,
                target_state: 1,
                acknowledged_state: 1,
                discard_before: 0,
                advances_remote_state: true,
                ..
            })
        )));
        let rendered = format!("{outbound:?}{retransmission:?}{inbound:?}");
        assert!(!rendered.contains("CLIENT_INPUT_SENTINEL"));
        assert!(!rendered.contains("SERVER_OUTPUT_SENTINEL"));
        assert!(!rendered.contains(SYNTHETIC_KEY));
    }

    #[test]
    fn action_debug_redacts_datagram_and_terminal_bytes() {
        let datagram = SessionAction::SendDatagram(b"DATAGRAM_SENTINEL".to_vec());
        let terminal = SessionAction::WriteTerminal(b"TERMINAL_SENTINEL".to_vec());
        let rendered = format!("{datagram:?} {terminal:?}");

        assert_eq!(
            rendered,
            "SendDatagram { bytes: 17 } WriteTerminal { bytes: 17 }"
        );
        assert!(!rendered.contains("SENTINEL"));
    }

    #[test]
    fn embedding_lifecycle_and_rebind_events_are_structured() {
        let mut runtime = SessionRuntime::new(key(), time(0));
        assert_eq!(runtime.lifecycle(), SessionLifecycle::Running);
        assert_eq!(
            runtime.pause(),
            vec![
                SessionAction::SessionLifecycleChanged(SessionLifecycle::Paused),
                SessionAction::Diagnostic(DiagnosticEvent::SessionLifecycleChanged {
                    state: SessionLifecycle::Paused,
                }),
            ]
        );
        assert!(matches!(
            runtime.queue_terminal_event(TerminalInputEvent::Bytes(b"secret input".to_vec())),
            Err(RuntimeError::SessionPaused)
        ));
        assert_eq!(runtime.milliseconds_until_next_poll(time(1)), u64::MAX);
        assert_eq!(
            runtime.resume_with_actions(time(2)),
            vec![
                SessionAction::SessionLifecycleChanged(SessionLifecycle::Running),
                SessionAction::Diagnostic(DiagnosticEvent::SessionLifecycleChanged {
                    state: SessionLifecycle::Running,
                }),
            ]
        );
        assert_eq!(
            runtime.notify_udp_rebound(),
            vec![
                SessionAction::UdpBindingChanged(1),
                SessionAction::Diagnostic(DiagnosticEvent::UdpBindingChanged { generation: 1 }),
            ]
        );
        assert_eq!(
            runtime.notify_udp_rebound()[0],
            SessionAction::UdpBindingChanged(2)
        );
    }

    #[test]
    fn cancellation_rejects_work_and_terminal_event_debug_redacts_bytes() {
        let event = TerminalInputEvent::Bytes(b"INPUT_EVENT_SENTINEL".to_vec());
        assert!(!format!("{event:?}").contains("INPUT_EVENT_SENTINEL"));

        let mut runtime = SessionRuntime::new(key(), time(0));
        runtime
            .queue_terminal_event(event)
            .expect("running session must accept host input");
        assert!(
            runtime
                .cancel()
                .contains(&SessionAction::SessionLifecycleChanged(
                    SessionLifecycle::Cancelled
                ))
        );
        assert!(matches!(
            runtime.poll(time(1)),
            Ok(actions) if actions.is_empty()
        ));
        assert!(matches!(
            runtime.queue_input(Vec::new()),
            Err(RuntimeError::SessionCancelled)
        ));
        assert!(matches!(
            runtime.receive_datagram(&[], time(1)),
            Err(RuntimeError::SessionCancelled)
        ));
        runtime.queue_resize(132, 43);
        assert!(runtime.pending_resize.is_none());
        assert!(runtime.notify_udp_rebound().is_empty());
    }

    #[test]
    fn local_shutdown_uses_reserved_state_and_completes_on_acknowledgement() {
        let mut runtime = SessionRuntime::new(key(), time(0));
        let mut server_channel = SecureChannel::new(PeerRole::Server, key());
        runtime
            .queue_input(b"final".to_vec())
            .expect("final input must queue");
        assert!(
            runtime
                .request_shutdown(time(0))
                .expect("shutdown must start")
                .contains(&SessionAction::Diagnostic(DiagnosticEvent::ShutdownStarted))
        );
        assert!(matches!(
            runtime.queue_input(b"late".to_vec()),
            Err(RuntimeError::ShutdownInProgress)
        ));

        let outbound = runtime.poll(time(0)).expect("shutdown state must send");
        let update = decode_sent_update(&outbound, &mut server_channel);
        assert_eq!(update.target_state, u64::MAX);
        assert!(outbound.iter().any(|action| matches!(
            action,
            SessionAction::Diagnostic(DiagnosticEvent::FreshUpdatePrepared {
                state_id: u64::MAX,
                ..
            })
        )));

        let acknowledgement = server_state_packet(0, 1, u64::MAX);
        runtime
            .receive_datagram(&acknowledgement, time(1))
            .expect("shutdown acknowledgement must open");
        let completion = runtime.poll(time(1)).expect("shutdown must complete");
        assert!(completion.contains(&SessionAction::ShutdownComplete(
            ShutdownOutcome::Acknowledged
        )));
        assert_eq!(
            runtime.shutdown_outcome(),
            Some(ShutdownOutcome::Acknowledged)
        );
    }

    #[test]
    fn peer_shutdown_is_acknowledged_before_completion() {
        let mut runtime = SessionRuntime::new(key(), time(0));
        let mut server_channel = SecureChannel::new(PeerRole::Server, key());
        let request = server_state_packet(0, u64::MAX, 0);
        runtime
            .receive_datagram(&request, time(1))
            .expect("peer shutdown request must open");

        let actions = runtime.poll(time(1)).expect("peer shutdown ack must send");
        let acknowledgement = decode_sent_update(&actions, &mut server_channel);
        assert_eq!(acknowledgement.acknowledged_state, u64::MAX);
        assert!(actions.contains(&SessionAction::ShutdownComplete(
            ShutdownOutcome::PeerRequested
        )));
        assert_eq!(
            runtime.shutdown_outcome(),
            Some(ShutdownOutcome::PeerRequested)
        );
    }

    #[test]
    fn simultaneous_shutdown_still_sends_the_peer_acknowledgement() {
        let mut runtime = SessionRuntime::new(key(), time(0));
        let mut server_channel = SecureChannel::new(PeerRole::Server, key());
        runtime
            .request_shutdown(time(0))
            .expect("local shutdown must start");
        let initial = runtime.poll(time(0)).expect("local shutdown must send");
        assert_eq!(
            decode_sent_update(&initial, &mut server_channel).target_state,
            u64::MAX
        );

        let peer_request = server_state_packet(0, u64::MAX, u64::MAX);
        runtime
            .receive_datagram(&peer_request, time(1))
            .expect("simultaneous peer shutdown must open");
        let completion = runtime.poll(time(1)).expect("peer ack must send");
        let acknowledgement = decode_sent_update(&completion, &mut server_channel);
        assert_eq!(
            (
                acknowledgement.base_state,
                acknowledgement.target_state,
                acknowledgement.acknowledged_state,
            ),
            (u64::MAX, u64::MAX, u64::MAX)
        );
        assert!(completion.contains(&SessionAction::ShutdownComplete(
            ShutdownOutcome::PeerRequested
        )));
    }

    #[test]
    fn local_shutdown_has_a_bounded_acknowledgement_timeout() {
        let mut runtime = SessionRuntime::new(key(), time(0));
        runtime
            .request_shutdown(time(0))
            .expect("shutdown must start");
        runtime.poll(time(0)).expect("shutdown state must send");

        let completion = runtime
            .poll(time(super::SHUTDOWN_TIMEOUT_MILLISECONDS))
            .expect("shutdown timeout must be deterministic");
        assert!(completion.contains(&SessionAction::ShutdownComplete(ShutdownOutcome::TimedOut)));
        assert_eq!(runtime.milliseconds_until_next_poll(time(20_000)), u64::MAX);
    }

    fn non_diagnostic_actions(actions: Vec<SessionAction>) -> Vec<SessionAction> {
        actions
            .into_iter()
            .filter(|action| !matches!(action, SessionAction::Diagnostic(_)))
            .collect()
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

    fn server_state_packet(base_state: u64, target_state: u64, acknowledged_state: u64) -> Vec<u8> {
        let update = StateUpdate::new(base_state, target_state, acknowledged_state);
        let compressed = encode_compressed_update(&update).expect("state must compress");
        let fragment = Fragment::split(&compressed, 1, 0, target_state)
            .expect("state must fragment")
            .remove(0);
        let mut channel = SecureChannel::new(PeerRole::Server, key());
        channel
            .seal_next(&fragment.encode())
            .expect("server packet must seal")
    }

    fn decode_sent_update(
        actions: &[SessionAction],
        server_channel: &mut SecureChannel,
    ) -> StateUpdate {
        let packet = actions
            .iter()
            .find_map(|action| match action {
                SessionAction::SendDatagram(packet) => Some(packet),
                _ => None,
            })
            .expect("actions must contain a datagram");
        let plaintext = server_channel
            .open(packet)
            .expect("client datagram must authenticate")
            .plaintext;
        let fragment = Fragment::parse(&plaintext).expect("datagram must contain a fragment");
        decode_compressed_update(&fragment.body).expect("state update must decode")
    }

    fn key() -> SessionKey {
        SessionKey::decode(SYNTHETIC_KEY).expect("synthetic key must decode")
    }

    const fn time(value: u64) -> MonotonicTime {
        MonotonicTime::from_milliseconds(value)
    }
}
