<div align="center">
  <img src="assets/ferrumtty-icon.png" width="168" alt="FerrumTTY icon">
  <h1>FerrumTTY</h1>
  <p><strong>A pure-Rust Mosh client for stock mosh-server.</strong></p>

  <p>
    <img alt="Rust 1.85+" src="https://img.shields.io/badge/Rust-1.85%2B-b7410e?logo=rust">
    <img alt="GPL-3.0-only" src="https://img.shields.io/badge/license-GPL--3.0--only-blue">
    <img alt="macOS, Linux, Windows" src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-333">
    <img alt="mosh-server 1.4.0" src="https://img.shields.io/badge/tested%20with-mosh--server%201.4.0-orange">
  </p>

  <p>English · <a href="README.zh-CN.md">简体中文</a></p>
</div>

**FerrumTTY is an independent, pure-Rust [Mosh](https://mosh.org/) client.** It
speaks the standard Mosh wire protocol directly to an unmodified `mosh-server`,
including AES-128-OCB3 authenticated datagrams, state synchronization,
fragmentation, acknowledgements, roaming, and terminal paint instructions.
It does not contain an SSH client or a Mosh server.

It is designed to be useful in two places:

- as a standalone network client launched after an SSH bootstrap;
- as an embeddable runtime inside another terminal or host application.

> **Project status:** the protocol path interoperates with the unmodified Debian
> `mosh-server` 1.4.0 package. The API is still pre-release and compatibility
> claims are deliberately limited to exact versions tested in the lab.

## Mosh compatibility

FerrumTTY occupies the same network-client position as the conventional
`mosh-client` executable. SSH is used only to start the remote server and obtain
the `MOSH CONNECT` port and session key; FerrumTTY then runs the Mosh session
over authenticated UDP.

```text
SSH or host application
        │  MOSH CONNECT <port> <key>
        ▼
FerrumTTY / mosh-client
        │  Mosh protocol over authenticated UDP
        ▼
stock mosh-server 1.4.x
```

The current compatibility target is the standard Mosh 1.4.x protocol family.
Automated black-box interoperability is presently pinned to the unmodified
Debian `mosh-server` 1.4.0 package; other 1.4.x releases remain compatibility
targets until individually recorded in the lab.

Release archives include both `ferrumtty` and a `mosh-client`-named copy for
SSH bootstrap tools and terminal applications that expect that executable
name. The reusable runtime can also be embedded directly into applications
such as terminal emulators.

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

Set `MOSH_ESCAPE_KEY` to one literal ASCII character to choose another local
command prefix. Printable prefixes follow the conventional newline-prefix
behavior; control-character prefixes are recognized directly.

FerrumTTY uses the local alternate screen by default so the original terminal
contents return after exit. Set `MOSH_NO_TERM_INIT=1` to keep the current screen.
If the operating system cannot report the viewport, positive `COLUMNS` and
`LINES` environment variables are used as a fallback.

| Environment | Effect |
| --- | --- |
| `MOSH_KEY` | Required 128-bit session key from `MOSH CONNECT` |
| `MOSH_ESCAPE_KEY` | One literal ASCII local-command prefix |
| `MOSH_PREDICTION_DISPLAY` | `adaptive`, `always`, or `never` |
| `MOSH_PREDICTION_OVERWRITE=yes` | Overwrite instead of insert predicted cells |
| `MOSH_TITLE_NOPREFIX` | Accepted for standard-client compatibility; FerrumTTY adds no title prefix |
| `MOSH_NO_TERM_INIT=1` | Skip local alternate-screen initialization |

## What works

- AES-128 OCB3 authenticated datagrams
- Bounded packet replay window and fragment reassembly
- Acknowledgements, retransmission backoff, heartbeat, and liveness tracking
- Stale-state suppression and recoverable indefinite network interruption
- IPv4 and IPv6 endpoints
- Client UDP rebinding and suspend/resume recovery
- UTF-8 terminal output and authoritative VT screen tracking
- Keyboard, function keys, mouse, focus, bracketed paste, and resize
- Conservative local prediction with full authoritative rollback
- Terminal restoration after exit, error, panic unwinding, and supported signals
- English and Simplified Chinese command-line diagnostics
- Native source checks for macOS, Linux, and Windows targets
- Static MSVC runtime in Windows release binaries
- `mosh-client -c`, `-v`, and standard Mosh client environment parsing

On Windows, console control events for Ctrl+C and Ctrl+Break are ignored by the
local process so their input can be forwarded to the remote session. This path
is compile-checked in CI but has not yet been claimed as end-to-end validated
under every Windows terminal host.

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

### GitHub releases

Pushing a semantic version tag builds native archives for Linux x86_64/arm64,
macOS x86_64/arm64, and Windows x86_64, then publishes them to a GitHub Release:

```console
$ git tag -a v0.1.0 -m "FerrumTTY 0.1.0"
$ git push origin v0.1.0
```

The release workflow runs the test and Clippy gates before packaging. A failed
platform build prevents the GitHub Release from being created.

## Documentation

- [Compatibility and tested artifact](docs/COMPATIBILITY.md)
- [Clean-room policy](docs/CLEAN_ROOM.md)
- [Embedding contract](docs/EMBEDDING.md)
- [Prediction policy](docs/PREDICTION.md)
- [Project governance and additional licensing](GOVERNANCE.md)
- [Third-party notices](THIRD-PARTY-NOTICES.md)

## License and independence

FerrumTTY is licensed under [GPL-3.0-only](LICENSE). It is an independent
clean-room implementation and is not affiliated with or endorsed by the Mosh
project. The copyright holder's additional-licensing policy is described in
[the project governance document](GOVERNANCE.md). Mosh is a registered
trademark of its respective owner.
