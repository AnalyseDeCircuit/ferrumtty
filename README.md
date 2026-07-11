<div align="center">
  <img src="assets/ferrumtty-icon.png" width="180" alt="FerrumTTY icon">
  <h1>FerrumTTY</h1>
  <p><strong>A pure-Rust client for resilient remote terminal sessions.</strong></p>
  <p>English · <a href="README.zh-CN.md">简体中文</a></p>
</div>

FerrumTTY is an independent, cross-platform remote terminal client compatible
with the standard `mosh-server` wire protocol. It is written in pure Rust and
licensed under GPL-3.0-only.

The current verified compatibility baseline is `mosh-server` 1.4.0. FerrumTTY
accepts an endpoint and ephemeral session key from an external SSH or
host-application bootstrap, then runs the authenticated UDP session.

## Features

- Pure-Rust protocol, cryptography integration, compression, and terminal path
- AES-128 OCB3 authenticated datagrams
- Acknowledgements, retransmission, heartbeat, and bounded replay handling
- Session continuity across client address and UDP source-port changes
- IPv4 and IPv6 endpoint support
- UTF-8 output, keyboard, mouse, focus, bracketed paste, and resize handling
- Conservative local prediction with authoritative-screen rollback
- Terminal restoration after normal exit, local escape, errors, and supported
  termination signals
- Embeddable runtime that does not own SSH, sockets, clocks, or credentials
- Source checks for macOS, Linux, and Windows targets

## Status and compatibility

FerrumTTY has completed bidirectional interoperability tests against the
unmodified Debian `mosh-server` package `1.4.0-1+b1` on arm64. Tests cover
authenticated state exchange, injected packet loss, retransmission, reordering,
and client UDP rebinding.

Compatibility claims are limited to exact server versions tested by the
checked-in laboratory. See [Compatibility](docs/COMPATIBILITY.md) for the
artifact identity and current platform limits.

## Run from source

FerrumTTY expects the conventional `MOSH_KEY` environment variable plus the
server host and UDP port:

```sh
MOSH_KEY='REDACTED' cargo run --release --package ferrumtty-client -- HOST PORT
```

The primary executable is `ferrumtty`. Release archives also include a
`mosh-client` compatibility copy for existing external bootstrap integrations.

## Local escape

- `Ctrl-^ .` ends the local session.
- `Ctrl-^ ^` sends a literal `Ctrl-^` to the remote application.

## Build and test

The workspace requires Rust 1.85 or later.

```sh
cargo build --workspace --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Run the standard-server and terminal lifecycle checks:

```sh
./lab/verify-ferrumtty-to-standard-server.sh
./lab/verify-terminal-restoration.exp
```

Build a release archive containing the executable, compatibility command,
license, notices, and checksum:

```sh
./scripts/package-release.sh 0.1.0 aarch64-apple-darwin
```

## Embedding

`ferrumtty-runtime` exposes deterministic input, resize, datagram, timer, and
terminal-output actions. A host application remains responsible for SSH
bootstrap, UDP transport, monotonic time, terminal presentation, and credential
ownership. See the [embedding contract](docs/EMBEDDING.md).

## Documentation

- [Compatibility](docs/COMPATIBILITY.md)
- [Clean-room policy](docs/CLEAN_ROOM.md)
- [Embedding API](docs/EMBEDDING.md)
- [Prediction behavior](docs/PREDICTION.md)
- [Third-party notices](THIRD-PARTY-NOTICES.md)

## Independence and trademark notice

FerrumTTY is an independent clean-room implementation. It is not affiliated
with or endorsed by the Mosh project. Mosh is a registered trademark of its
respective owner.
