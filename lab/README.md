# Compatibility laboratory

The laboratory runs unmodified distribution binaries as external black-box
oracles. It never copies their source, tests, fixtures, or implementation
structure into FerrumTTY.

## Standard server image

The current arm64 baseline uses:

- Debian Bookworm slim image from Debian's public Amazon ECR mirror;
- immutable image digest recorded in the Containerfile;
- Debian Snapshot archive dated 2025-06-01;
- exact package version `mosh 1.4.0-1+b1`.

Build the image:

```sh
docker build \
  --file lab/mosh-server-1.4.0/Containerfile \
  --tag ferrumtty-lab/mosh-server:1.4.0-arm64 \
  lab/mosh-server-1.4.0
```

Inspect the installed package and executable identity:

```sh
docker run --rm --entrypoint /usr/local/bin/ferrumtty-server-identity \
  ferrumtty-lab/mosh-server:1.4.0-arm64
```

The identity command does not launch a session or expose a session key.

Validate a startup announcement by piping it over standard input. Never pass
the announcement as a command argument because process listings may expose it:

```sh
printf '%s\n' "$EPHEMERAL_ANNOUNCEMENT" \
  | cargo run --quiet --package ferrumtty-lab -- verify-connect
```

Run the checked-in startup and fault-injection smoke tests:

```sh
sh lab/run-startup-baseline.sh
sh lab/verify-netem.sh
sh lab/verify-network-controls.sh
sh lab/verify-standard-client-packet.sh
sh lab/verify-standard-server-packet.sh
sh lab/verify-ferrumtty-to-standard-server.sh
expect lab/verify-terminal-restoration.exp
```

The terminal restoration fixture uses synthetic keys and local UDP sink sockets.
It verifies exact `stty` restoration after both `Ctrl-^ .` and an external
termination signal. It does not contact a server or retain terminal content.

## Secret handling

The server startup line contains an ephemeral session key. Pipe it directly
into a process that keeps the key in memory and emits only redacted metadata.
Never store the raw line in a file, CI log, shell trace, terminal recording, or
committed artifact.
