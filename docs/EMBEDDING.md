# Embedding contract

`ferrumtty-runtime` is the platform-independent host boundary. It owns session
convergence, acknowledgements, retransmission timing, heartbeat timing, input
bounds, and the ephemeral key after construction. It does not open sockets,
read clocks, inspect terminal devices, or start SSH.

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
5. queue input and the latest viewport, and call `poll` no later than
   `milliseconds_until_next_poll`;
6. call `resume` after system sleep or any interval where the host could not
   observe network progress, preventing that interval from becoming a false
   server timeout;
7. discard the runtime and restore local terminal state on every exit path.

`SessionAction::AcknowledgePrediction` carries the newest server `EchoAck` to
the host prediction layer. It is not terminal output and must not be written to
the screen. `SessionRuntime::is_server_responsive` reports prolonged network
silence without terminating the session; the host may display connection state
while continuing to poll and send heartbeats so Mosh can recover later.

Remote state updates are applied only when their base matches the latest
applied server state. Retransmitted or stale states may still acknowledge local
state, but their terminal instructions are not applied twice.

The host must not persist the session key, terminal content, plaintext packet
payloads, or generated datagrams. A host may change its local UDP address while
retaining the same authenticated remote endpoint. Changing the remote endpoint
requires a new authenticated bootstrap.

The API is pre-release (`0.0.0`) and is not yet covered by semantic-versioning
stability guarantees. It is not an SSH protocol or credential-storage
interface.
