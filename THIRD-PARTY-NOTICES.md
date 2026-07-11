# Third-party notices

FerrumTTY depends on the Rust packages below. The exact resolved versions and
checksums are authoritative in `Cargo.lock`. This inventory was generated from
the locked production workspace on 2026-07-11. No dependency is licensed under
GPL or AGPL.

| Packages | Declared license choices |
| --- | --- |
| `aead`, `aes`, `arrayvec`, `base64`, `bitflags`, `cfg-if`, `cipher`, `cpufeatures`, `crc32fast`, `crypto-common`, `ctr`, `document-features`, `either`, `errno`, `flate2`, `inout`, `itertools`, `itoa`, `libc`, `litrs`, `lock_api`, `log`, `miniz_oxide`, `ocb3`, `parking_lot`, `parking_lot_core`, `proc-macro2`, `quote`, `scopeguard`, `signal-hook`, `signal-hook-mio`, `signal-hook-registry`, `smallvec`, `syn`, `unicode-width`, `version_check`, `vte`, `windows-link`, `windows-sys`, `zeroize` | MIT and/or Apache-2.0; some packages additionally offer Zlib |
| `adler2` | 0BSD, MIT, or Apache-2.0 |
| `anyhow`, `generic-array`, `mio`, `prost`, `prost-derive`, `rustix` | MIT and/or Apache-2.0 |
| `bytes`, `crossterm`, `crossterm_winapi`, `redox_syscall`, `simd-adler32`, `vt100` | MIT |
| `linux-raw-sys`, `wasi` | Apache-2.0 with LLVM exception, Apache-2.0, or MIT |
| `memchr` | Unlicense or MIT |
| `subtle` | BSD-3-Clause |
| `unicode-ident` | MIT or Apache-2.0, and Unicode-3.0 |
| `winapi`, `winapi-i686-pc-windows-gnu`, `winapi-x86_64-pc-windows-gnu` | MIT or Apache-2.0 |

The packages `typenum`, `vte`, `prost`, and their transitive build-time macro
dependencies are included in the inventory even when they do not contribute a
separately linked runtime library. License texts and package source links are
available from each package's crates.io release and repository metadata.
