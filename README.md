# TPT Kinetix

[![CI](https://github.com/tpt-solutions/tpt-kinetix/actions/workflows/ci.yml/badge.svg)](https://github.com/tpt-solutions/tpt-kinetix/actions/workflows/ci.yml)
[![Coverage](https://codecov.io/gh/tpt-solutions/tpt-kinetix/branch/master/graph/badge.svg)](https://codecov.io/gh/tpt-solutions/tpt-kinetix)
[![crates.io](https://img.shields.io/crates/v/kinetix-core.svg)](https://crates.io/crates/kinetix-core)
[![docs.rs](https://img.shields.io/docsrs/kinetix-core)](https://docs.rs/kinetix-core)
[![license](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

A memory-safe, hyper-concurrent media processing engine written in Rust — designed as a long-term
successor to FFmpeg for production transcoding and streaming pipelines.

---

## Why TPT Kinetix?

**FFmpeg** is the de facto standard for media processing. It is battle-tested and feature-complete,
but it carries decades of technical debt:

- **Memory safety**: the C codebase has an extensive CVE history rooted in buffer overflows,
  use-after-free, and integer truncation bugs. Rust eliminates these classes of bug at compile time.
- **Concurrency model**: FFmpeg's internal threading is coarse-grained and difficult to scale across
  modern many-core CPUs. TPT Kinetix is designed from the ground up with a lock-free pipeline model
  using `rayon` work-stealing and `crossbeam` channels.
- **Composability**: monolithic ffmpeg CLI makes embedding and customisation hard. Every codec and
  mux format in Kinetix is an independent crate with a stable public API.

---

## AI / Knowledge-Graph Strategy

`kinetix-kg` is a companion crate that ingests the FFmpeg source tree, codec specifications, and
related RFCs into a structured knowledge graph. This graph drives:

1. **Code generation** — boilerplate codec tables and dispatch glue.
2. **Correctness analysis** — cross-referencing spec clauses with implementation paths.
3. **Regression triage** — mapping failing test vectors back to spec sections.

The knowledge graph is stored as a set of JSON-LD documents and queried at build time via the
`kinetix-kg` CLI.

---

## Crate Architecture

```
tpt-kinetix (workspace)
│
├── kinetix-core        — shared types: Frame, Packet, Timestamp, PixelFormat, Error
│
├── kinetix-demux       — container demuxers (MP4 first; MKV / TS planned)
│
├── kinetix-h264        — H.264 / AVC decoder (NAL-unit parser + slice decoder)
│
├── kinetix-av1         — AV1 decoder + encoder (OBU parser, tile threading)
│
├── kinetix-kg          — knowledge-graph ingestion, analysis, and codegen tooling
│
├── kinetix-pipeline    — lock-free multi-stage processing pipeline
│
├── kinetix-stream      — async streaming output: RTMP push, HLS packaging
│
└── kinetix-cli         — `kinetix` binary: transcode / stream subcommands
```

### Architecture Diagram (ASCII)

```
 ┌──────────────────────────────────────────────────────────────┐
 │                        kinetix-cli                           │
 │          transcode subcommand │ stream subcommand            │
 └───────────────────┬──────────────────────┬───────────────────┘
                     │                      │
          ┌──────────▼──────────┐  ┌────────▼────────┐
          │  kinetix-pipeline   │  │ kinetix-stream  │
          │  Stage graph /      │  │  RTMP / HLS     │
          │  crossbeam channels │  └────────┬────────┘
          └──────┬──────────────┘           │
                 │                          │
    ┌────────────┼────────────┐             │
    │            │            │             │
┌───▼───┐  ┌────▼────┐  ┌────▼────┐        │
│demux  │  │ h264    │  │  av1    │        │
│(MP4…) │  │ decoder │  │dec/enc  │        │
└───┬───┘  └────┬────┘  └────┬────┘        │
    │            │            │             │
    └────────────┴────────────┴─────────────┘
                         │
                 ┌───────▼──────┐
                 │ kinetix-core │
                 │ Frame/Packet │
                 │ Timestamp    │
                 └──────────────┘
```

---

## Quickstart

### Prerequisites

- Rust 1.82 or later (`rustup update stable`)
- `cargo-deny` for supply-chain checks (`cargo install cargo-deny`)

### Build

```bash
git clone https://github.com/tpt-solutions/tpt-kinetix
cd tpt-kinetix
cargo build --workspace
```

### Test

```bash
cargo test --workspace
```

### Lint

```bash
cargo clippy --workspace -- -D warnings
cargo fmt --check
cargo deny check
```

### Run the CLI

```bash
cargo run -p kinetix-cli -- --help
cargo run -p kinetix-cli -- transcode --help
cargo run -p kinetix-cli -- stream --help
```

---

## Release & Versioning

### Semver policy

All crates in this workspace share the same version number (monorepo style). When a breaking change
is made to any public API, **all** crates are bumped together. This keeps the dependency graph
coherent and avoids mixed-version combinations.

`v0.1.0` is the initial development release. API stability is **not guaranteed** until `v1.0.0`.

### Publish order

Crates must be published to crates.io in dependency order to satisfy the registry resolver:

1. `kinetix-core`
2. `kinetix-demux`, `kinetix-h264`, `kinetix-av1`, `kinetix-kg` *(depend only on `kinetix-core`)*
3. `kinetix-pipeline` *(depends on the four above)*
4. `kinetix-stream` *(independent of pipeline, but published after for consistency)*
5. `kinetix-cli` *(depends on `kinetix-pipeline` and `kinetix-stream`)*

### crates.io name reservation

Before running `cargo publish` for the first time, **manually reserve each crate name** on
crates.io by publishing a minimal `0.0.1` placeholder, or by logging in and creating the crate
entry. This prevents name squatting. The names to reserve are:

`kinetix-core`, `kinetix-demux`, `kinetix-h264`, `kinetix-av1`, `kinetix-kg`,
`kinetix-pipeline`, `kinetix-stream`, `kinetix-cli`

---

## Roadmap

- **Phase 9 (stretch)**: See [`docs/adding-a-codec.md`](docs/adding-a-codec.md) for the process of adding new codecs via the KG pipeline.
- **Future codecs**: See [`docs/codec-backlog.md`](docs/codec-backlog.md) for the prioritised list.
- **Codec evaluations**: [`docs/codec-evaluations/aac.md`](docs/codec-evaluations/aac.md), [`docs/codec-evaluations/hevc.md`](docs/codec-evaluations/hevc.md)

---

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT License ([LICENSE-MIT](LICENSE-MIT))

at your option.
