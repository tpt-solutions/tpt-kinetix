# Patent posture

TPT Kinetix is distributed as **source code only**. This repository contains no
pre-compiled binaries, no vendored codec libraries, and no hosted service. The
`LICENSE-MIT` / `LICENSE-APACHE` files grant **copyright** permissions only —
they do not grant, and cannot grant, any rights under third-party **patents**.

## What this means

Some media codecs are covered by patents in some jurisdictions. Patent rights
attach to **making, using, selling, offering for sale, or importing** an
implementation of the claimed invention — *not* to the act of compiling source
code. Distributing the `.rs` source in this repository is the same posture taken
by FFmpeg, x264, and dav1d: **no patent licenses are obtained by this project**,
and responsibility for obtaining any that apply passes to whoever builds, runs,
redistributes binaries of, or operates a service using this software.

If you compile and run the patent-encumbered decoders — including via
`cargo test`, `cargo bench`, the crates' `examples/`, `just conformance`, or
`just corpus-check`, all of which execute the decoders on your machine — you are
responsible for any patent licenses required in your jurisdiction and use case.

Note that Apache-2.0 §3 does include an **express patent grant from the project's
own contributors** for their contributions; the dual MIT/Apache-2.0 license is
retained partly to preserve that grant. It does not extend to patents held by
third parties who are not contributors.

## Codec encumbrance status

| Crate | Codec | Encumbrance | Notes |
| --- | --- | --- | --- |
| `tpt-kinetix-h264` | H.264 / AVC decode | **Encumbered** | Via LA (ex-MPEG-LA) AVC patent pool; core patents expected to run to ~2027–2030 |
| `tpt-kinetix-aac` | AAC-LC decode | **Encumbered** | Via LA AAC patent pool |
| `tpt-kinetix-av1` | AV1 decode / encode | Royalty-free | AOMedia patent license; designed to be royalty-free |
| `tpt-kinetix-demux` / `-mux` | MP4 / MKV / WebM containers | Unencumbered | Container formats, no known active essential patents |
| `tpt-kinetix-stream` | RTMP, HLS, MPEG-TS | Unencumbered / expired | MPEG-TS systems-layer patents have expired |
| *planned* | VP9, Opus | Royalty-free | Both designed and licensed as royalty-free |
| *planned* | MP3 | Expired | Last MP3 patents expired in 2017 |

The project's codec roadmap targets **royalty-free formats only** going forward
(VP9, Opus, MP3, MPEG-TS). New codec crates must be assessed and added to this
table as part of the change that introduces them — see `CONTRIBUTING.md`.

## Producing a royalty-free-only build

The `tpt-kinetix-pipeline`, `tpt-kinetix-stream`, and `tpt-kinetix-cli` crates
enable the encumbered codecs through **default-on Cargo features** (`codec-h264`,
`codec-aac`). To build the CLI without them:

```sh
just build-royalty-free
# = cargo build -p tpt-kinetix-pipeline -p tpt-kinetix-cli --no-default-features
```

This drops `tpt-kinetix-h264` and `tpt-kinetix-aac` from the CLI dependency graph
entirely (verify with `cargo tree -p tpt-kinetix-cli --no-default-features`); the
resulting engine handles only the royalty-free paths — AV1 plus the
container/streaming layers.

The codec crates themselves (`tpt-kinetix-h264`, `tpt-kinetix-aac`) and the
test-only helper crate (`tpt-kinetix-test-utils`, `publish = false`) still build
and run their own test suites directly, matching how FFmpeg keeps its
conformance coverage (FATE) running for encumbered decoders. CI is not gated:
stripping encumbered codecs for redistribution is a decision for a downstream
packager shipping binaries into a specific jurisdiction, not for this project.
