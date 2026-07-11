<div align="center">
  <img src="assets/ferrumtty-icon.png" width="168" alt="FerrumTTY icon">
  <h1>FerrumTTY</h1>
  <p><strong>A pure-Rust terminal client that keeps working when the network does not.</strong></p>

  <p>
    <img alt="Rust 1.85+" src="https://img.shields.io/badge/Rust-1.85%2B-b7410e?logo=rust">
    <img alt="GPL-3.0-only" src="https://img.shields.io/badge/license-GPL--3.0--only-blue">
    <img alt="macOS, Linux, Windows" src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-333">
    <img alt="mosh-server 1.4.0" src="https://img.shields.io/badge/tested%20with-mosh--server%201.4.0-orange">
  </p>

  <p>English · <a href="README.zh-CN.md">简体中文</a></p>
</div>

FerrumTTY is an independent client for the standard `mosh-server` wire
protocol. It combines authenticated UDP state synchronization, network roaming,
terminal handling, and conservative local prediction in a Rust-only codebase.

It is designed to be useful in two places:

- as a standalone network client launched after an SSH bootstrap;
- as an embeddable runtime inside another terminal or host application.

> **Project status:** the protocol path interoperates with the unmodified Debian
> `mosh-server` 1.4.0 package. The API is still pre-release and compatibility
> claims are deliberately limited to exact versions tested in the lab.

## Why FerrumTTY?

Remote shells tend to feel worst exactly when they matter most: on mobile
networks, unstable Wi-Fi, VPN transitions, and machines waking from sleep.
FerrumTTY synchronizes terminal state over authenticated UDP instead of treating
the session as one fragile byte stream.

- **Roaming:** continue after the client address or UDP source port changes.
- **Loss recovery:** retransmit logical state without reusing packet nonces.
- **Fast feedback:** predict only safe printable input and roll it back against
  the authoritative server screen.
- **Native integration:** embed the protocol runtime without giving it control
  of SSH, sockets, clocks, or credentials.
- **Portable Rust:** no bundled C or C++ protocol runtime.

## Quick start

FerrumTTY starts after a standard server has supplied a UDP port and ephemeral
key. An SSH command, terminal manager, or another host application can perform
that bootstrap.

```console
$ cargo build --release --package ferrumtty-client
$ MOSH_KEY='SESSION_KEY' ./target/release/ferrumtty SERVER_IP UDP_PORT
```

For example, a bootstrap typically obtains a line shaped like:

```text
MOSH CONNECT 60001 SESSION_KEY
```

Pass the port and key directly to FerrumTTY without writing the key to disk or
placing it in command-line arguments.

### Local escape

| Keys | Action |
| --- | --- |
| `Ctrl-^ .` | End the local session |
| `Ctrl-^ ^` | Send a literal `Ctrl-^` |

## What works

- AES-128 OCB3 authenticated datagrams
- Bounded packet replay window and fragment reassembly
- Acknowledgements, retransmission backoff, heartbeat, and timeout
- IPv4 and IPv6 endpoints
- Client UDP rebinding and suspend/resume recovery
- UTF-8 terminal output and authoritative VT screen tracking
- Keyboard, function keys, mouse, focus, bracketed paste, and resize
- Conservative local prediction with full authoritative rollback
- Terminal restoration after exit, error, panic unwinding, and supported signals
- English and Simplified Chinese command-line diagnostics
- Native source checks for macOS, Linux, and Windows targets

## Architecture

The workspace keeps protocol concerns separate from operating-system concerns:

| Crate | Responsibility |
| --- | --- |
| `ferrumtty-wire` | Fragment framing, bounded Protobuf decoding, and compression |
| `ferrumtty-crypto` | Session-key ownership and OCB3 packet envelopes |
| `ferrumtty-session` | State numbers, acknowledgements, replay handling, and reassembly |
| `ferrumtty-runtime` | Deterministic timers, queues, retransmission, and host actions |
| `ferrumtty-terminal` | Terminal lifecycle and input encoding |
| `ferrumtty-predict` | Non-authoritative local prediction overlay |
| `ferrumtty-client` | UDP and local-console command-line application |
| `ferrumtty-lab` | Black-box compatibility probes and synthetic fixtures |

The embeddable API is described in [docs/EMBEDDING.md](docs/EMBEDDING.md).

## Compatibility

The checked-in laboratory verifies FerrumTTY against Debian
`mosh-server 1.4.0-1+b1` on arm64. It exercises bidirectional state exchange,
packet loss, retransmission, reordering, and UDP rebinding.

```console
$ ./lab/verify-ferrumtty-to-standard-server.sh
standard server exchanged FerrumTTY state: ... roamed=true
```

See [docs/COMPATIBILITY.md](docs/COMPATIBILITY.md) for the pinned artifact and
the exact scope of the compatibility claim.

## Development

Rust 1.85 or later is required.

```console
$ cargo build --workspace --locked
$ cargo test --workspace --locked
$ cargo clippy --workspace --all-targets --locked -- -D warnings
$ cargo deny check
```

Run the real PTY lifecycle check:

```console
$ cargo build --package ferrumtty-client
$ ./lab/verify-terminal-restoration.exp
```

Create a self-contained release archive:

```console
$ ./scripts/package-release.sh 0.1.0 aarch64-apple-darwin
```

The archive contains `ferrumtty`, a `mosh-client` compatibility copy, the
license, copyright notice, third-party notices, and a SHA-256 checksum.

## Documentation

- [Compatibility and tested artifact](docs/COMPATIBILITY.md)
- [Clean-room policy](docs/CLEAN_ROOM.md)
- [Embedding contract](docs/EMBEDDING.md)
- [Prediction policy](docs/PREDICTION.md)
- [Third-party notices](THIRD-PARTY-NOTICES.md)

## License and independence

FerrumTTY is licensed under [GPL-3.0-only](LICENSE). It is an independent
clean-room implementation and is not affiliated with or endorsed by the Mosh
project. Mosh is a registered trademark of its respective owner.
