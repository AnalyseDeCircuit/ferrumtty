# Compatibility

FerrumTTY has been tested against the unmodified Debian `mosh-server` package
`1.4.0-1+b1` on arm64. The laboratory pins the Debian image, snapshot date, and
package installation in `lab/mosh-server-1.4.0/Containerfile`. The tested
server executable has SHA-256 digest
`93ea256dbeca783c39f0b7c678cc1bc12b076551092411a1dc9f685fd240d262`.

The implementation was derived from RFC 7253, the published Mosh paper,
general Protobuf rules, independent black-box experiments, and targeted review
of the official Mosh 1.4.0 source for underspecified SSP behavior. No upstream
source code or test fixture is incorporated into this repository.

Verified behavior includes:

- AES-128 OCB3 authentication in both packet directions;
- encrypted UDP framing, bounded fragmentation, and Protobuf state updates;
- acknowledgements, fresh-counter retransmission, heartbeat, and recoverable
  liveness tracking;
- packet loss, duplicate rejection, bounded reordering, and client UDP
  rebinding;
- UTF-8 terminal output, resize, keyboard, mouse, focus, and bracketed paste;
- conservative local prediction with authoritative-screen rollback;
- terminal restoration after local escape and termination signals.

Synthetic compatibility checks additionally cover:

- suppression of a completed server state retransmitted under a fresh packet
  counter;
- exact acknowledgement matching for retained local states;
- one-unacknowledged-state input batching and retransmission from the confirmed
  baseline;
- retained remote-state reconstruction, `throwaway_num` pruning, and
  suppression of repeated HostBytes when the reconstructed history extends the
  delivered history;
- convergence of plain-text branches through a screen diff when both divergent
  tails end at a parser ground boundary;
- bounded sender history plus local, peer-initiated, and simultaneous
  `u64::MAX` shutdown handshakes with bounded acknowledgement timeout;
- forward-compatible skipping of unknown Protobuf fields while retaining wire
  type validation for known fields;
- delivery of server `EchoAck` markers to the prediction boundary;
- frame-associated prediction acknowledgement, conservative Backspace, and
  bounded left/right cursor prediction;
- fragmented UTF-8 and terminal-mode sequences across HostBytes boundaries;
- command-line parsing, strict UDP port validation, UTF-8 locale precedence,
  heuristic color capability detection, and title OSC rewriting across every
  fragment boundary;
- Unix raw-mode setup enables the termios `IUTF8` flag where the platform
  exposes it, with restoration delegated to the existing terminal guard;
- content-redacted diagnostics and `Debug` formatting for authenticated
  plaintext, datagrams, fragments, terminal output, and prediction input;
- continued polling and heartbeat scheduling after prolonged network silence.
- mosh-go state-zero association, adaptive 250 ms to 10 s retransmission
  timing from every authenticated packet, timestamp-only heartbeat handling,
  ordered resize coalescing, 1300-byte peer fragments, acceptance of fragment
  identifiers independent from SSP state identifiers, and optional
  capability/session-control extension fields.

The new sender-history, prediction, and dynamic terminal-mode behavior added
after the recorded live-server run is covered only by these synthetic checks.
It must not be described as live interoperability validation until the lab is
run again against an exact server artifact.

Remote terminal histories retain at most 16 MiB of HostBytes and 65,536 parser
operations per logical branch. Completed ground-state histories compact to a
formatted terminal snapshot before reaching that bound. A divergent branch
uses a screen diff when both parsers end at a ground boundary; incomplete UTF-8
or control sequences remain conservatively undelivered.

The mosh-go comparison baseline is commit
`8dca5c67ec8e09f71a4dc8eda9216f2f4ee7ec0f` from 2026-04-05. FerrumTTY keeps
its bounded replay window, conservative 1214-byte outbound fragmentation,
structured errors, pause/resume events, and graceful SSP shutdown because
those are robustness or host-contract extensions. Its receiver accepts the
larger mosh-go fragment shape. Compatibility here means client-visible
function and protocol interoperability, not byte-for-byte reproduction of
every mosh-go implementation choice.

These checks use constructed protocol states and do not constitute a native
terminal or live-server interoperability test.

The source tree is checked for macOS arm64/x86_64, Linux arm64/x86_64, and
Windows x86_64 targets. A platform build check is not a substitute for a native
runtime test, so release claims should identify the platform actually tested.

Compatibility with later `mosh-server` 1.4.x releases must be verified against
each exact release before being claimed.
