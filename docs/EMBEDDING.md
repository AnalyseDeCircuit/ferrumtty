# Embedding contract

`ferrumtty-runtime` is the platform-independent host boundary. It owns session
convergence, acknowledgements, retransmission timing, heartbeat timing, input
bounds, and the ephemeral key after construction. It does not open sockets,
read clocks, inspect terminal devices, or start SSH.

`EMBEDDING_API_VERSION` identifies the current host-action contract. The API
remains pre-release, so hosts should check this value instead of assuming that
all `0.0.0` builds expose identical events.

The host must:

1. obtain an authenticated UDP host, port, and `MOSH_KEY` through its own
   bootstrap mechanism;
2. construct `SessionRuntime` and provide monotonic milliseconds beginning at
   any value;
3. apply every `SessionAction` in order, sending datagrams only to the
   authenticated bootstrap endpoint and writing terminal bytes without logging
   them;
4. pass received datagrams to `receive_datagram`, then call `poll` immediately
   so acknowledgements are not delayed;
5. queue `TerminalInputEvent` values or use the lower-level byte and viewport
   methods, and call `poll` no later than `milliseconds_until_next_poll`;
6. call `resume` after system sleep or any interval where the host could not
   observe network progress, preventing that interval from becoming a false
   server timeout;
7. call `request_shutdown` for a normal local exit, continue applying actions
   until `ShutdownComplete`, and treat `TimedOut` as an unclean exit;
8. discard the runtime and restore local terminal state on every exit path.

`SessionAction::AcknowledgePrediction` carries the newest server `EchoAck` to
the host prediction layer. It is not terminal output and must not be written to
the screen. `SessionAction::RemoteStateAdvanced` reports the committed SSP
state separately from any following `WriteTerminal` effects.
`SessionAction::ConnectionStateChanged` distinguishes a session
that has never connected from a connected session that is temporarily
interrupted. `SessionAction::RoundTripEstimate` reports changed RTT estimates.
`SessionAction::SessionLifecycleChanged` reports explicit host pause, resume,
and cancellation transitions. `SessionAction::UdpBindingChanged` reports the
monotonic generation returned after a host-managed local UDP rebind; it does
not authorize changing the authenticated remote endpoint.
`SessionAction::ShutdownComplete` reports an acknowledged local shutdown, an
acknowledged peer request, or the bounded local acknowledgement timeout. A peer
shutdown is not complete until its `u64::MAX` acknowledgement datagram has been
returned to the host for sending.
These are structured host events and must not be injected into remote terminal
content.

An embedding host can call `pause` before suspending its event loop and
`resume_with_actions` after it resumes. A paused runtime rejects new input and
datagrams and does not advance timers. `cancel` rejects all subsequent work and
clears unsent input; the host must then drop the runtime so the in-memory
session key is released. After replacing only its local UDP socket, the host
calls `notify_udp_rebound` so subscribers can observe the rebind without
changing protocol state.

`SessionAction::Diagnostic` contains only closed, content-free metadata:
state numbers, counters, lengths, timing values, booleans, and safe enums. It
never contains keys, user input, HostBytes, authenticated plaintext, or
datagram contents. Hosts must preserve that boundary instead of formatting
protocol payload types directly.

`SessionRuntime::is_server_responsive` and
`milliseconds_since_server_response` report prolonged network silence without
terminating the session; the host may display connection state while continuing
to poll and send heartbeats so Mosh can recover later.

Local input can advance through multiple unacknowledged SSP states. A timeout
rebuilds the latest logical input suffix from the highest exactly acknowledged
state and reseals it with fresh packet counters. Retransmitted or stale server
states may still acknowledge local state, but their terminal instructions are
not applied twice.

The host must not persist the session key, terminal content, plaintext packet
payloads, or generated datagrams. A host may change its local UDP address while
retaining the same authenticated remote endpoint. Changing the remote endpoint
requires a new authenticated bootstrap.

The API is pre-release (`0.0.0`) and is not yet covered by semantic-versioning
stability guarantees. It is not an SSH protocol or credential-storage
interface.
