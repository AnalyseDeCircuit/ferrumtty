# FerrumTTY

FerrumTTY is an independent, pure-Rust remote terminal client compatible with
the standard `mosh-server` wire protocol. It is licensed under GPL-3.0-only.

The current compatibility baseline is `mosh-server` 1.4.0. FerrumTTY includes
authenticated UDP state synchronization, retransmission and roaming, terminal
input and resize handling, conservative local prediction, and terminal-state
restoration on exit.

## Run

An external SSH or host-application bootstrap supplies the UDP endpoint and
ephemeral session key:

```sh
MOSH_KEY='REDACTED' cargo run --package ferrumtty-client -- HOST PORT
```

The primary release executable is `ferrumtty`. Release archives also contain a
`mosh-client` compatibility copy for existing bootstrap integrations.

The local escape `Ctrl-^ .` ends a session. `Ctrl-^ ^` sends a literal
`Ctrl-^`.

## Build and test

```sh
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
./lab/verify-ferrumtty-to-standard-server.sh
./lab/verify-terminal-restoration.exp
```

Build a release archive with:

```sh
./scripts/package-release.sh 0.1.0 aarch64-apple-darwin
```

## Documentation

- [Compatibility](docs/COMPATIBILITY.md)
- [Clean-room policy](docs/CLEAN_ROOM.md)
- [Embedding API](docs/EMBEDDING.md)
- [Prediction behavior](docs/PREDICTION.md)

FerrumTTY is not affiliated with or endorsed by the Mosh project. Mosh is a
registered trademark of its respective owner.
