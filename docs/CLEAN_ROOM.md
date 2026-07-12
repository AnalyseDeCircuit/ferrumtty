# Independent implementation policy

FerrumTTY is an independent Rust implementation. Compatibility facts may come
from published papers, RFCs, manuals, general Protobuf documentation,
unmodified standard-server binaries, synthetic packet captures produced by
FerrumTTY's own laboratory, and review of the official Mosh source when a wire
behavior is not specified precisely elsewhere.

The project does not copy, translate, mechanically rewrite, or adapt source
code, tests, comments, identifiers, module structures, fixtures, or distinctive
implementation expression from Mosh or another client implementation. No
third-party source is incorporated into FerrumTTY unless it is recorded as a
dependency or vendored component with compatible licensing and attribution.

Protocol tests use synthetic terminal content and ephemeral keys. Session keys,
credentials, production addresses, user transcripts, and unsanitized packet
captures must never be committed.

Production diagnostics and laboratory probe output are metadata-only. They may
report counters, state numbers, field numbers, lengths, and aggregate counts,
but must not print authenticated plaintext, terminal bytes, input bytes,
session keys, or complete datagrams.

The standalone client decodes `MOSH_KEY` into the runtime and zeroizes its
temporary Rust string. It does not call Rust 2024's unsafe process-environment
mutation APIs, because this workspace forbids unsafe code. Embedding hosts can
avoid an environment copy entirely by constructing `SessionRuntime` from a
`SessionKey`; launchers that provide `MOSH_KEY` remain responsible for limiting
the inherited environment and process-inspection exposure.

The implementation uses project-native naming and independently constructed
tests. Public compatibility claims are limited to exact server versions tested
by the checked-in laboratory.
