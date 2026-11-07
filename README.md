# TPT Kinetix

[![CI](https://github.com/tpt-solutions/tpt-kinetix/actions/workflows/ci.yml/badge.svg)](https://github.com/tpt-solutions/tpt-kinetix/actions/workflows/ci.yml)
[![Coverage](https://codecov.io/gh/tpt-solutions/tpt-kinetix/branch/master/graph/badge.svg)](https://codecov.io/gh/tpt-solutions/tpt-kinetix)
[![crates.io](https://img.shields.io/crates/v/tpt-kinetix-core.svg)](https://crates.io/crates/tpt-kinetix-core)
[![docs.rs](https://img.shields.io/docsrs/tpt-kinetix-core)](https://docs.rs/tpt-kinetix-core)
[![license](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

A memory-safe, hyper-concurrent media processing engine written in Rust — designed as a long-term
successor to FFmpeg for production transcoding and streaming pipelines.

---

## Quickstart

Install the CLI (see [Install the CLI](#install-the-cli) for the options), then try it on any MP4 or WebM file:

```bash
tpt-kinetix probe in.mp4                       # list tracks, codecs, duration
tpt-kinetix remux in.webm out.mp4 --faststart  # copy streams into MP4, no re-encode
tpt-kinetix package in.mp4 out/                # HLS + DASH with fMP4 segments
tpt-kinetix live --port 8080                   # live server; publish AV1/VP9 + Opus to /ingest/<key>
```

To try it without a file of your own, make a six-second H.264/AAC sample with ffmpeg and probe it:

```bash
ffmpeg -f lavfi -i testsrc=size=320x180:rate=25 -f lavfi -i sine=frequency=440 -t 6 \
  -c:v libx264 -pix_fmt yuv420p -c:a aac -movflags +faststart sample.mp4
tpt-kinetix probe sample.mp4
```

Expected output for that sample (checked on 2026-11-08): two tracks, `codec=H264` 320x180 with 150 samples
and `codec=Aac` 1 channel at 44100 Hz. `remux` and `package` copy the H.264 and AAC unchanged. Kinetix
does not decode H.264 or AAC; see [what works with my files](docs/what-works-with-my-files.md) for the
full matrix, and [the ffmpeg migration guide](docs/migrating-from-ffmpeg.md) for command equivalents.

## Current status

TPT Kinetix is **early-stage and pre-1.0**. This table summarizes what works
end-to-end today versus what is scaffolded or in progress. Each crate's README
has more detail on its limitations, and decoders expose their state
programmatically via `DecoderCapabilities` (`capabilities()`).

| Area | Status | Notes |
| --- | --- | --- |
| MP4 / ISO-BMFF demux | ✅ Works | Track discovery, sample tables, packet extraction (`tpt-kinetix-demux`) |
| MKV / WebM demux | 🟡 Basic (whole buffer) + ✅ live | Whole-buffer reader is a subset; `MkvStream` is a streaming parser for **AV1 / VP9 / Opus** WebM (unknown-size clusters, any chunking, `av1C`/`vpcC`/`dOps` built for MP4) used by live ingest |
| IVF demux | ✅ Works | Raw AV1/VP9 elementary streams (`.ivf`, `DKIF`): fourcc, geometry, per-frame ranges, key-frame-free seeking, truncated-tail tolerant. `probe` reads it, values match `ffprobe` |
| MPEG-TS demux | ✅ Works | ⚖️ Royalty-free. PAT/PMT parsing, PES depacketization with PTS/DTS, PCR tracking; unlocks HLS/broadcast input. H.264 comes out Annex-B framed; round-trips through the HLS `TsMuxer` and matches `ffprobe` on real clips (`tpt-kinetix-demux`) |
| MP4 mux / remux | ✅ Works | Multi-track passthrough writer (AV1, VP9, Opus, AAC, H.264/H.265, FLAC, AC-3...), faststart, fragmented MP4 / CMAF writer, CENC (`cenc` and `cbcs`, ClearKey); `tpt-kinetix remux` to MP4, WebM, IVF (one AV1/VP9 track) or Ogg/Opus (`tpt-kinetix-mux`). The old single-track `Mp4Muxer` remains |
| WebM / MP4 metadata and subtitles | ✅ Works | WebVTT subtitle tracks through demux, mux and remux (MP4 `wvtt`, WebM `S_TEXT/WEBVTT`); track language and default/forced disposition in `probe`; MP4 text metadata, chapters and Matroska tags carry into WebM output |
| HLS / DASH packaging | ✅ Works | `tpt-kinetix-package`: just-in-time HLS (fMP4) + DASH from an MP4 or WebM index over ranged reads (file, HTTP, or WASM `fetch`); WebVTT subtitle renditions, I-frame trick-play playlists, CENC ClearKey (`cenc`/`cbcs`), `LANGUAGE` on renditions; `package` and `serve` commands; Worker example in `examples/edge-worker`. Verified frame-exact in ffmpeg and playing in hls.js/dash.js, and CENC ClearKey plays in headless Chrome |
| Live (AV1/VP9 + Opus) | ✅ Works | `tpt-kinetix live`: ingest by HTTP POST, WebSocket (browser `MediaRecorder`, page at `/publish`), WHIP (WebRTC, AV1 or VP9, `--features whip`) or Enhanced RTMP (`--rtmp-port`, incl. multitrack video/audio, HDR `colorInfo`, v2 capability exchange); plays as live fMP4 HLS (low-latency parts, LL-HLS subtitle parts, blocking reload) and dynamic DASH, with WebVTT subtitles from WebM ingest and optional CENC. Ingest hardening (tokens, per-key limits, idle/size/bitrate cut-offs, `/metrics`), DVR recording, HTTPS / `wss://` / RTMPS (`--tls-cert`, build with `--features tls`). See [tpt-kinetix-stream](tpt-kinetix-stream/README.md) and [todo-io.md](todo-io.md) |
| Language bindings | ✅ Works | C ABI (`tpt-kinetix-ffi`, header `include/tpt_kinetix.h`), Python (`tpt-kinetix-py`, maturin) and Node.js (`tpt-kinetix-node`, napi-rs) for probe, faststart, remux and package, all calling the shared `tpt-kinetix-ops` crate. Tested in CI. None is published yet; the WASM packager's npm package is `private` |
| H.264 decode | ✅ Pixel-exact | ⚖️ Patent-encumbered. CAVLC and CABAC I/P/B (progressive 4:2:0, any display dimensions, deblocking, High-profile 8×8 transform); PAFF field pictures (I/P/B) and MBAFF I/P/B frames bit-exact vs ffmpeg — `capabilities().pixel_exact == true`; strict mode returns `NotPixelExact` only for still-unsupported features (multi-slice pictures, non-4:2:0, >8-bit). Now also gated by the official ITU-T H.264.1 conformance suite (`just fetch-h264-conformance`): 34 curated clips decode byte-exact vs the standard's reference YUV (0 failures; see [docs/CONFORMANCE.md](docs/CONFORMANCE.md)); remaining gaps (multi-slice reconstruction, real MBAFF-CABAC-I, some hierarchical-B GOPs, 4:2:2/4:4:4) are tracked, not yet fixed. **Not published** — kept in-repo for reference only (`out-kinetix-h264`, `publish = false`; see [PATENTS.md](PATENTS.md)) |
| AV1 decode | ✅ Pixel-exact | ⚖️ Royalty-free. Intra and inter reconstruction, all transforms, deblock/CDEF/loop restoration, film grain, 4:2:0/4:2:2/4:4:4 and 8/10/12-bit. The official FFmpeg FATE AV1 set decodes 204/204 frames byte-exact vs libdav1d (`cargo run --release -p tpt-kinetix-av1 --example av1_fate_score`), and a libaom-encode → Kinetix-vs-dav1d crosscheck (`tpt-kinetix-av1/tests/libaom_crosscheck.rs`) covers far more than FATE does. `capabilities().pixel_exact == true`; results are tabulated in [docs/CONFORMANCE.md](docs/CONFORMANCE.md). Frames above the AV1 level 6.3 picture-size limit are refused before any plane is allocated (found by fuzzing). |
| AV1 encode | ✅ Works | `rav1e` backend with preset mapping (`tpt-kinetix-av1`) |
| VP9 decode | ✅ Pixel-exact | ⚖️ Royalty-free. Profile-0 (8-bit 4:2:0) decode implemented end-to-end (reconstruction, reference management, loop filtering, superframes). The whole ffmpeg conformance corpus — 13 clips covering lossless/lossy, content, intra-only, inter, odd size 125x67, and multitile — decodes byte-exact vs `ffmpeg -c:v vp9` on every plane, and the test asserts it (`capabilities().pixel_exact == true`; `tpt-kinetix-vp9`, see `todo-vp9.md`). Wired into the pipeline (`Vp9DecodeStage`) and the CLI (`probe` reports decoder status; VP9-input transcode to AV1 takes this royalty-free path). |
| Pipeline | ✅ Works | Concurrent demux→decode→filter→encode stages |
| RTMP ingest | ✅ Works | Handshake, chunk reassembly, AMF connect/publish, FLV depacketization; **Enhanced RTMP** AV1/VP9 + Opus straight into live HLS (`live --rtmp-port`) |
| HLS output | ✅ Works | MPEG-TS segment muxing + sliding-window `.m3u8` + HTTP serving |
| Original codecs | 🟡 Experimental | `lean`, `screen`, `realtime`, `vision`, `face`, `volumetric` and `lossless`: Kinetix-designed formats with no external reference decoder, pinned by golden vectors instead (see the note below). `lossless` is bit-exact and round-trips; `face` is synthesized by design; `volumetric` is not yet validated against the TMC13 oracle |
| CLI | ✅ Works / 🟡 Partial | `probe` (MP4, MPEG-TS, Matroska/WebM, IVF; `--json` is ffprobe-shaped), `remux`, `sanitize` (keeps only royalty-free streams from untrusted files), `doctor` (will it play in a browser?), `thumbnails` (keyframe PNGs and a contact sheet), `package`, `serve`, `live`, and `stream` (older RTMP → TS HLS server) all work. `transcode --vcodec av1` runs the full demux → decode → encode pipeline, but VP9 is the only supported input (royalty-free `codec-vp9` path). `vision` decodes Vision bitstreams. See [tpt-kinetix-cli](tpt-kinetix-cli/README.md) |

> ⚠️ **Decode correctness:** The H.264, VP9 and AV1 decoders report
> `pixel_exact: true` for their supported subsets (H.264: CAVLC/CABAC
> I/P/B progressive and interlaced (PAFF/MBAFF) including the High-profile
> 8×8 transform; VP9: profile 0 8-bit 4:2:0; AV1: all profiles in the FATE set). Strict mode still returns
> `KinetixError::NotPixelExact` when a stream hits an unsupported feature
> (multi-slice or non-4:2:0/>8-bit H.264; non-profile-0 VP9). Call `capabilities()` (or
> `tpt-kinetix probe`) to check at runtime.
>
> The original-format codecs (`lean`, `vision`, `screen`, `realtime`, `volumetric`, `lossless`)
> have no independent reference decoder, so all except `lossless` report `pixel_exact: false`.
> They instead report `deterministic: true`: exact encoded bytes and decoded output are pinned
> by committed golden vectors (a `golden_vector_pins_bitstream_and_output` test in each crate).
> `face` is synthesized by design and reports neither. `lossless` is `pixel_exact` because it
> is integer-reversible and round-trips exactly.

> ⚖️ **We will not be releasing H.264.** H.264/AVC is patent-encumbered for
> both encode and decode, and this project ships source only and obtains no
> patent licenses. The decoder (`out-kinetix-h264`) exists in this repository
> for local and reference work, but it is `publish = false`, is not on
> crates.io, and is not used by the CLI or the pipeline. Everything that is
> published is royalty-free (AV1, VP9, and the containers). If you need H.264,
> use a codec library you have licensed separately. See
> [PATENTS.md](PATENTS.md) for the full posture.

> 🔊 **Audio lives elsewhere.** Kinetix handles video and containers only. Audio
> codecs (AAC, Opus, MP3, and others) are developed in our sister project,
> [**tpt-cadence**](https://github.com/tpt-solutions/tpt-cadence), which is
> the place to look for audio decode/encode. Please don't open audio-codec
> requests against this repo.

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
├── tpt-kinetix-demux       — container demuxers (MP4, MKV/WebM incl. live, MPEG-TS, IVF)
│
├── tpt-kinetix-mux         — container muxers (multi-track MP4, faststart, fMP4/CMAF, WebM)
│
├── tpt-kinetix-package     — just-in-time HLS/DASH packaging + live packager (wasm-capable)
│
├── tpt-kinetix-stream      — live server (WebM/WebSocket/WHIP/Enhanced RTMP ingest → LL-HLS/DASH)
│
├── tpt-kinetix-av1         — AV1 decoder (pixel-exact) + rav1e-backed encoder
│
├── tpt-kinetix-vp9         — VP9 decoder (pixel-exact, profile 0)
│
├── out-kinetix-h264        — H.264 / AVC decoder (unpublished, patent-encumbered)
│
├── tpt-kinetix-pipeline    — lock-free multi-stage processing pipeline
│
├── tpt-kinetix-cli         — `tpt-kinetix` binary: probe / remux / sanitize / doctor / thumbnails / package /
│                             serve / live / transcode / stream
│
├── tpt-kinetix-ops         — the container operations (probe JSON, remux, package) as a library, shared by
│                             the CLI and the bindings (unpublished)
│
├── Language bindings       — tpt-kinetix-ffi (C ABI), tpt-kinetix-py (Python), tpt-kinetix-node (Node.js);
│                             all unpublished for now
│
├── Original codecs         — tpt-kinetix-lean, -screen, -realtime, -vision, -face, -volumetric, -lossless,
│                             sharing tpt-kinetix-bitstream (BitReader, rANS)
│
└── Tooling                 — tpt-kinetix-kg (knowledge-graph codegen), tpt-kinetix-test-utils (unpublished)
```

### Architecture Diagram (ASCII)

```
 ┌──────────────────────────────────────────────────────────────┐
 │                       tpt-kinetix-cli                        │
 │  probe │ remux │ package │ serve │ live │ transcode │ stream │
 └──────┬──────────────────┬──────────────────────┬─────────────┘
        │                  │                      │
┌───────▼─────────┐ ┌──────▼──────────┐  ┌────────▼────────────┐
│ tpt-kinetix-    │ │ tpt-kinetix-    │  │ tpt-kinetix-stream  │
│ pipeline        │ │ package         │◀─│ live ingest, RTMP,  │
│ (transcode)     │ │ HLS / DASH      │  │ LL-HLS / DASH       │
└──┬────────┬─────┘ └──────┬──────────┘  └────────┬────────────┘
   │        │              │                      │
┌──▼───┐ ┌──▼───┐   ┌──────▼──────┐        ┌──────▼──────┐
│ vp9  │ │ av1  │   │ demux / mux │        │ demux / mux │
│ dec  │ │ enc  │   │ MP4 WebM TS │        │             │
└──┬───┘ └──┬───┘   └──────┬──────┘        └──────┬──────┘
   │        │              │                      │
   └────────┴──────────────┴──────────────────────┘
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

### Install the CLI

Prebuilt `tpt-kinetix` binaries (Linux x86_64, macOS x86_64 and arm64, Windows x86_64)
are attached to each GitHub Release, with SHA-256 sums. Or build from source:

```bash
cargo binstall tpt-kinetix-cli        # prebuilt, from the GitHub Release
cargo install tpt-kinetix-cli         # from source, once published to crates.io
docker run --rm ghcr.io/tpt-solutions/tpt-kinetix:<version> --help   # container image
```

The container image is built from the [Dockerfile](Dockerfile) and published by the
Binaries workflow under the version tag only (there is no `latest` tag). To work on a
local file, mount it: `docker run --rm -v "$PWD:/data" ghcr.io/tpt-solutions/tpt-kinetix:<version> probe in.mp4`.

### Build

```bash
git clone https://github.com/tpt-solutions/tpt-kinetix
cd tpt-kinetix
cargo build --workspace
```

### Test

```bash
just test        # or: cargo test --workspace
just check       # fmt-check + clippy + build + test: run before opening a PR
```

See [docs/CONFORMANCE.md](docs/CONFORMANCE.md) for decoder conformance results and
[docs/PERFORMANCE.md](docs/PERFORMANCE.md) for benchmarks against ffmpeg/libaom/libvpx
(`just bench-ffmpeg`, `just bench-io`).

### Lint

```bash
cargo clippy --workspace -- -D warnings
cargo fmt --check
cargo deny check
```

### Run the CLI

```bash
cargo run -p tpt-kinetix-cli -- --help
cargo run -p tpt-kinetix-cli -- remux in.mp4 out.mp4 --faststart
cargo run -p tpt-kinetix-cli -- serve in.mp4          # HLS + DASH on http://127.0.0.1:8080
cargo run -p tpt-kinetix-cli -- live --help           # live ingest -> HLS/DASH
cargo run -p tpt-kinetix-cli -- doctor clip.webm      # will it play in a browser?
cargo run -p tpt-kinetix-cli -- thumbnails clip.webm stills/ --count 6 --sprite-columns 3
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
# or machine-readable, ffprobe-shaped:
cargo run -p tpt-kinetix-cli -- probe path/to/video.webm --json
```

### Examples

Every functional crate ships at least one runnable, self-contained example under its
`examples/` directory:

| Example | What it shows |
| --- | --- |
| `cargo run -p tpt-kinetix-demux --example probe_mp4 -- path/to/video.mp4` | Probe an MP4 file and print its tracks |
| `cargo run -p tpt-kinetix-mux --example write_mp4 -- out.mp4` | Write a minimal single-track H.264 MP4 with the legacy `Mp4Muxer` (multi-track output: `Mp4Writer`, or `tpt-kinetix remux`) |
| `cargo run -p tpt-kinetix-pipeline --example basic_transcode` | Synthetic frames → filter → AV1 encode |
| `cargo run -p tpt-kinetix-stream --example hls_segment` | Generate an HLS TS segment + `.m3u8` playlist |
| `cargo run -p tpt-kinetix-stream --example live_server` | Run a live server: publish AV1/VP9 + Opus WebM, play as live HLS |
| `cargo run -p tpt-kinetix-kg --example ingest_ffmpeg_h264 -- path/to/h264dec.c` | Ingest C source into a knowledge graph |
| `bash examples/tasks/run-all.sh` | Six ffmpeg-replacement tasks: probe, remote probe, remux, package, sanitize an upload, live ingest ([examples/tasks](examples/tasks/README.md)) |

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

The current release is `v0.2.0`. Changes are listed in [CHANGELOG.md](CHANGELOG.md). API stability is **not guaranteed** until `v1.0.0`.

### Publish order

Crates must be published to crates.io in dependency order to satisfy the registry resolver:

Publishing is automated by release-plz (`release-plz.toml`), which derives this order from the
dependency graph. In outline:

1. `tpt-kinetix-core`
2. `tpt-kinetix-demux`, `tpt-kinetix-mux`, `tpt-kinetix-av1`, `tpt-kinetix-vp9`, `tpt-kinetix-bitstream`, `tpt-kinetix-kg` *(depend only on `tpt-kinetix-core`)*
3. `tpt-kinetix-package`, `tpt-kinetix-pipeline`, `tpt-kinetix-lean`, `-screen`, `-realtime`, `-vision`, `-face`, `-volumetric`, `-lossless` *(depend on the crates above)*
4. `tpt-kinetix-stream` *(depends on `tpt-kinetix-package`)*
5. `tpt-kinetix-cli` *(depends on pipeline, stream, package and the rest)*

`out-kinetix-h264` and `tpt-kinetix-test-utils` are never published. `tpt-kinetix-ops`, `tpt-kinetix-ffi`,
`tpt-kinetix-py` and `tpt-kinetix-node` are `publish = false` for now.

### crates.io name reservation

Before running `cargo publish` for the first time, **manually reserve each crate name** on
crates.io by publishing a minimal `0.0.1` placeholder, or by logging in and creating the crate
entry. This prevents name squatting. The names to reserve are:

`tpt-kinetix-core`, `tpt-kinetix-demux`, `tpt-kinetix-mux`, `tpt-kinetix-package`, `tpt-kinetix-av1`,
`tpt-kinetix-vp9`, `tpt-kinetix-bitstream`, `tpt-kinetix-kg`, `tpt-kinetix-pipeline`, `tpt-kinetix-stream`,
`tpt-kinetix-cli`, and the original-codec crates (`-lean`, `-screen`, `-realtime`, `-vision`, `-face`,
`-volumetric`, `-lossless`)

---

## Roadmap

- **Open I/O tasks**: [`todo-io.md`](todo-io.md) is the task list. The product is the memory-safe I/O layer (demux, mux, packaging, live ingest, edge/WASM), prioritising royalty-free AV1, VP9 and Opus; Kinetix does not try to out-run ffmpeg/dav1d at codec decode speed (see [docs/PERFORMANCE.md](docs/PERFORMANCE.md)).
- **Adding codecs**: See [`docs/adding-a-codec.md`](docs/adding-a-codec.md) for the process of adding new codecs via the KG pipeline.
- **Future codecs**: See [`docs/codec-backlog.md`](docs/codec-backlog.md) for the prioritised list.
- **Codec evaluations**: [`docs/codec-evaluations/hevc.md`](docs/codec-evaluations/hevc.md) (HEVC is out of scope: patent-encumbered, like H.264). Audio codecs are handled by [tpt-cadence](https://github.com/tpt-solutions/tpt-cadence), not here.

---

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT License ([LICENSE-MIT](LICENSE-MIT))

at your option.
