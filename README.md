# TPT Kinetix

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

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT License ([LICENSE-MIT](LICENSE-MIT))

at your option.
