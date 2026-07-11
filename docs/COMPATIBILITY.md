# Compatibility

FerrumTTY has been tested against the unmodified Debian `mosh-server` package
`1.4.0-1+b1` on arm64. The laboratory pins the Debian image, snapshot date, and
package installation in `lab/mosh-server-1.4.0/Containerfile`. The tested
server executable has SHA-256 digest
`93ea256dbeca783c39f0b7c678cc1bc12b076551092411a1dc9f685fd240d262`.

The implementation was derived from RFC 7253, the published Mosh paper,
general Protobuf rules, and independent black-box experiments. No upstream
implementation source or test suite was used.

Verified behavior includes:

- AES-128 OCB3 authentication in both packet directions;
- encrypted UDP framing, bounded fragmentation, and Protobuf state updates;
- acknowledgements, fresh-counter retransmission, heartbeat, and timeout;
- packet loss, duplicate rejection, bounded reordering, and client UDP
  rebinding;
- UTF-8 terminal output, resize, keyboard, mouse, focus, and bracketed paste;
- conservative local prediction with authoritative-screen rollback;
- terminal restoration after local escape and termination signals.

The source tree is checked for macOS arm64/x86_64, Linux arm64/x86_64, and
Windows x86_64 targets. A platform build check is not a substitute for a native
runtime test, so release claims should identify the platform actually tested.

Compatibility with later `mosh-server` 1.4.x releases must be verified against
each exact release before being claimed.
