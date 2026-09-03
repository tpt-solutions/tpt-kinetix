# TPT Kinetix

[![CI](https://github.com/tpt-solutions/tpt-kinetix/actions/workflows/ci.yml/badge.svg)](https://github.com/tpt-solutions/tpt-kinetix/actions/workflows/ci.yml)
[![Coverage](https://codecov.io/gh/tpt-solutions/tpt-kinetix/branch/master/graph/badge.svg)](https://codecov.io/gh/tpt-solutions/tpt-kinetix)
[![crates.io](https://img.shields.io/crates/v/tpt-kinetix-core.svg)](https://crates.io/crates/tpt-kinetix-core)
[![docs.rs](https://img.shields.io/docsrs/tpt-kinetix-core)](https://docs.rs/tpt-kinetix-core)
[![license](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

A memory-safe, hyper-concurrent media processing engine written in Rust — designed as a long-term
successor to FFmpeg for production transcoding and streaming pipelines.

---

## Current status

TPT Kinetix is **early-stage and pre-1.0**. This table summarizes what works
end-to-end today versus what is scaffolded or in progress. Each crate's README
has a more detailed LIMITATIONS section, and decoders expose their state
programmatically via `DecoderCapabilities` (`capabilities()`).

| Area | Status | Notes |
| --- | --- | --- |
| MP4 / ISO-BMFF demux | ✅ Works | Track discovery, sample tables, packet extraction (`tpt-kinetix-demux`) |
| MKV / WebM demux | 🟡 Basic | EBML parsing; subset of elements |
| MP4 mux | ✅ Works | Single H.264 track, round-trips through the demuxer (`tpt-kinetix-mux`) |
| H.264 decode | ✅ Pixel-exact | CAVLC and CABAC I/P/B (progressive 4:2:0, any display dimensions, deblocking, High-profile 8×8 transform); PAFF field pictures (I/P/B) and MBAFF I/P/B frames bit-exact vs ffmpeg — `capabilities().pixel_exact == true`; strict mode returns `NotPixelExact` only for still-unsupported features (multi-slice pictures, non-4:2:0, >8-bit) |
| AV1 decode | 🟡 Not pixel-exact | OBU + sequence header parsing; CDF-based entropy/symbol decoder implemented (`entropy.rs`/`entropy_cdf.rs`), but `decode_tile_group` is not yet rewired onto it |
| AV1 encode | ✅ Works | `rav1e` backend with preset mapping (`tpt-kinetix-av1`) |
| Pipeline | ✅ Works | Concurrent demux→decode→filter→encode stages |
| RTMP ingest | ✅ Works | Handshake, chunk reassembly, AMF connect/publish, FLV depacketization |
| HLS output | ✅ Works | MPEG-TS segment muxing + sliding-window `.m3u8` + HTTP serving |
| AAC audio | ✅ AAC-LC decode | ADTS / AudioSpecificConfig parsing + fully native AAC-LC PCM decode (Huffman/IMDCT/TNS/PNS/stereo, no third-party codec dependency); HE-AAC (SBR/PS) unsupported (`tpt-kinetix-aac`) |
| CLI `probe` | ✅ Works | Inspect containers today; `transcode`/`stream` still stubs |

> ⚠️ **Decode correctness:** The H.264 decoder reports `pixel_exact: true`
> for CAVLC/CABAC I/P/B progressive and interlaced (PAFF/MBAFF) streams,
> including the High-profile 8×8 transform. Strict mode still returns
> `KinetixError::NotPixelExact` for slices that hit an unsupported feature
> (multi-slice pictures, non-4:2:0 chroma, >8-bit depth). The AV1 and AAC
> decoders are not yet pixel-exact. Call `capabilities()` (or `tpt-kinetix
> probe`) to check at runtime; in strict mode decoders return
> `KinetixError::NotPixelExact` instead of emitting approximate frames.

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

`tpt-kinetix-kg` is a companion crate that ingests the FFmpeg source tree, codec specifications, and
related RFCs into a structured knowledge graph. This graph drives:

1. **Code generation** — boilerplate codec tables and dispatch glue.
2. **Correctness analysis** — cross-referencing spec clauses with implementation paths.
3. **Regression triage** — mapping failing test vectors back to spec sections.

The knowledge graph is stored as a set of JSON-LD documents and queried at build time via the
`tpt-kinetix-kg` CLI.

---

## Crate Architecture

```
tpt-kinetix (workspace)
│
├── tpt-kinetix-core        — shared types: Frame, Packet, Timestamp, PixelFormat, Error
│
├── tpt-kinetix-demux       — container demuxers (MP4 first; MKV / TS planned)
│
├── tpt-kinetix-mux         — container muxers (progressive MP4 for H.264)
│
├── tpt-kinetix-h264        — H.264 / AVC decoder (NAL-unit parser + slice decoder)
│
├── tpt-kinetix-av1         — AV1 decoder + encoder (OBU parser, tile threading)
│
├── tpt-kinetix-aac         — AAC audio parsing (ADTS / AudioSpecificConfig) + AAC-LC PCM decode
│
├── tpt-kinetix-kg          — knowledge-graph ingestion, analysis, and codegen tooling
│
├── tpt-kinetix-pipeline    — lock-free multi-stage processing pipeline
│
├── tpt-kinetix-stream      — async streaming output: RTMP push, HLS packaging
│
└── tpt-kinetix-cli         — `tpt-kinetix` binary: probe / transcode / stream subcommands
```

### Architecture Diagram (ASCII)

```
 ┌──────────────────────────────────────────────────────────────┐
 │                        tpt-kinetix-cli                           │
 │          transcode subcommand │ stream subcommand            │
 └───────────────────┬──────────────────────┬───────────────────┘
                     │                      │
          ┌──────────▼──────────┐  ┌────────▼────────┐
          │  tpt-kinetix-pipeline   │  │ tpt-kinetix-stream  │
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
                 │ tpt-kinetix-core │
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
cargo run -p tpt-kinetix-cli -- --help
cargo run -p tpt-kinetix-cli -- transcode --help
cargo run -p tpt-kinetix-cli -- stream --help
```

### See it work: a 30-second demo

The fastest way to see TPT Kinetix actually do something is the pipeline example — it's
self-contained (no sample media file required): it generates synthetic YUV420p frames, scales
them through the pipeline's filter stage, and encodes them to AV1 with `rav1e`.

```bash
cargo run -p tpt-kinetix-pipeline --example basic_transcode
```

If you have an MP4 file handy, you can also probe it directly with the CLI or the demux crate:

```bash
cargo run -p tpt-kinetix-cli -- probe path/to/video.mp4
```

### Examples

Every functional crate ships at least one runnable, self-contained example under its
`examples/` directory:

| Example | What it shows |
| --- | --- |
| `cargo run -p tpt-kinetix-demux --example probe_mp4 -- path/to/video.mp4` | Probe an MP4 file and print its tracks |
| `cargo run -p tpt-kinetix-mux --example write_mp4 -- out.mp4` | Write a minimal single-track H.264 MP4 |
| `cargo run -p tpt-kinetix-pipeline --example basic_transcode` | Synthetic frames → filter → AV1 encode |
| `cargo run -p tpt-kinetix-stream --example hls_segment` | Generate an HLS TS segment + `.m3u8` playlist |
| `cargo run -p tpt-kinetix-aac --example parse_aac` | Parse an AudioSpecificConfig and ADTS frames |
| `cargo run -p tpt-kinetix-kg --example ingest_ffmpeg_h264 -- path/to/h264dec.c` | Ingest C source into a knowledge graph |

### Try it in your browser

`tpt-kinetix-demux` also builds for `wasm32-unknown-unknown`. See
[`web-demo/`](web-demo/) for a small, dependency-free page that probes an MP4 file entirely
client-side — drag a file in and see its tracks, no upload, no server. Build and serve it with:

```bash
just wasm-demo
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

1. `tpt-kinetix-core`
2. `tpt-kinetix-demux`, `tpt-kinetix-mux`, `tpt-kinetix-h264`, `tpt-kinetix-av1`, `tpt-kinetix-aac`, `tpt-kinetix-kg` *(depend only on `tpt-kinetix-core`)*
3. `tpt-kinetix-pipeline` *(depends on the codec/demux crates above)*
4. `tpt-kinetix-stream` *(independent of pipeline, but published after for consistency)*
5. `tpt-kinetix-cli` *(depends on `tpt-kinetix-pipeline` and `tpt-kinetix-stream`)*

### crates.io name reservation

Before running `cargo publish` for the first time, **manually reserve each crate name** on
crates.io by publishing a minimal `0.0.1` placeholder, or by logging in and creating the crate
entry. This prevents name squatting. The names to reserve are:

`tpt-kinetix-core`, `tpt-kinetix-demux`, `tpt-kinetix-mux`, `tpt-kinetix-h264`, `tpt-kinetix-av1`, `tpt-kinetix-aac`, `tpt-kinetix-kg`,
`tpt-kinetix-pipeline`, `tpt-kinetix-stream`, `tpt-kinetix-cli`

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
