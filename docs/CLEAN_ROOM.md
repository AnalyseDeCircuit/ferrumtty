# Clean-room policy

FerrumTTY is independently implemented from public specifications and
black-box behavior. Compatibility facts may come from published papers, RFCs,
manuals, general Protobuf documentation, unmodified standard-server binaries,
and synthetic packet captures produced by FerrumTTY's own laboratory.

The project does not copy, translate, mechanically rewrite, or adapt source
code, tests, comments, identifiers, module structures, fixtures, or distinctive
implementation expression from Mosh or another client implementation. No GPL
or AGPL third-party source is incorporated into FerrumTTY.

Protocol tests use synthetic terminal content and ephemeral keys. Session keys,
credentials, production addresses, user transcripts, and unsanitized packet
captures must never be committed.

The implementation uses project-native naming and independently constructed
tests. Public compatibility claims are limited to exact server versions tested
by the checked-in laboratory.
