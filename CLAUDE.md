# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

TPT Kinetix is a memory-safe, hyper-concurrent media processing engine in Rust — a long-term successor
to FFmpeg for transcoding/streaming pipelines. It is **early-stage and pre-1.0**; the H.264 and AV1
decoders are the active, unfinished work: they parse bitstreams correctly but are **not yet pixel-exact**
against reference decoders. `DecoderCapabilities` (`capabilities()`) reports this at runtime, and strict
mode returns `KinetixError::NotPixelExact` instead of a placeholder frame. See README.md's status table
for the current state of every crate before assuming something works.

## Commands

Prefer `just <recipe>` (see `justfile`) — it mirrors CI exactly. Direct cargo equivalents also work.

```sh
just build                 # cargo build --workspace
just test                  # cargo nextest run --workspace --lib --bins --tests (falls back to cargo test)
just test-doc               # cargo test --workspace --doc (nextest doesn't run doctests)
just clippy                 # cargo clippy --workspace --all-targets -- -D warnings
just fmt / just fmt-check   # cargo fmt --all / --check
just deny                   # cargo deny check (license/advisory/duplicate-dep gate)
just doc                    # cargo doc, denies rustdoc warnings
just check                  # fmt-check + clippy + build + test — run this before opening a PR
```

Single-crate / single-test:

```sh
cargo test -p tpt-kinetix-h264
cargo test -p tpt-kinetix-h264 --test b_frame_conformance
cargo test -p tpt-kinetix-h264 some_test_name -- --exact
```

Decoder conformance and corpus tooling (H.264-focused, the crate closest to correctness right now):

```sh
just conformance      # prints each decoder's DecoderCapabilities; second run asserts pixel-exact (--strict)
just corpus-check     # regenerates the ad-hoc test-src corpus, then decodes/diffs every file in it
```

Fuzzing (nightly + `cargo-fuzz` required):

```sh
just fuzz-build                              # compiles all fuzz targets across crates
just fuzz tpt-kinetix-h264 fuzz_h264_nal 60   # run one target for 60s
```
A crash reproducer lands in `fuzz/artifacts/<target>/crash-*`; commit it into `fuzz/corpus/<target>/`
so it becomes a permanent regression case. Run the relevant fuzz target for ≥60s after touching any
parser before considering the change done.

Benches: `just bench` (Criterion, h264/av1/aac/pipeline crates), `just bench-report` (release + consolidated
timing table via `tpt-kinetix-test-utils`).

Wasm demo: `just wasm-demo` (builds `tpt-kinetix-demux` for `wasm32-unknown-unknown`, serves `web-demo/`).

## Architecture

Cargo workspace, one crate per concern, monorepo-versioned (all crates share one version number; a
breaking change in any public API bumps all of them together):

- `tpt-kinetix-core` — shared types everything else depends on: `Frame`, `Packet`, `Timestamp`,
  `PixelFormat`, `KinetixError`, `DecoderCapabilities`.
- `tpt-kinetix-demux` — container demuxers (MP4/ISO-BMFF works; MKV/WebM is a basic EBML subset).
- `tpt-kinetix-mux` — container muxers (progressive MP4, single H.264 track).
- `tpt-kinetix-h264` — H.264/AVC decoder: NAL parsing, SPS/PPS, slice header/data, CAVLC + CABAC
  entropy decode, intra/inter prediction, motion compensation, deblocking, reference-picture
  management (MMCO/dec_ref_pic_marking, POC-based B-slice list ordering). This is the crate under
  heaviest active development — check `todo.md` and the README LIMITATIONS section for current gaps.
- `tpt-kinetix-av1` — AV1 OBU parser + `rav1e`-backed encoder (encode works); decoder does entropy
  decoding (CDF-based symbol decoder in `entropy.rs`/`entropy_cdf.rs`) but is not yet pixel-exact.
- `tpt-kinetix-aac` — ADTS/AudioSpecificConfig parsing + native AAC-LC PCM decode (fully native Huffman/IMDCT/TNS/PNS/stereo pipeline, no third-party codec dependency); HE-AAC (SBR/PS) unsupported.
- `tpt-kinetix-kg` — knowledge-graph tooling that ingests FFmpeg C source / codec specs and generates
  Rust scaffolding for new codecs (`ingest` → `graph` → `analyze` → `codegen`, or `run` for all four).
  Used when starting a new codec crate; see CONTRIBUTING.md's "Adding a new codec" section.
- `tpt-kinetix-pipeline` — lock-free multi-stage demux→decode→filter→encode pipeline (`rayon` +
  `crossbeam-channel`).
- `tpt-kinetix-stream` — async streaming output: RTMP ingest (handshake/chunk/AMF/FLV) and HLS output
  (MPEG-TS segmenting + sliding-window `.m3u8` + HTTP serving).
- `tpt-kinetix-cli` — the `tpt-kinetix` binary (`probe` works today; `transcode`/`stream` are stubs).
- `tpt-kinetix-test-utils` — shared conformance/corpus/bench-report helpers used across crates' test
  suites, notably `codec_status` (prints `DecoderCapabilities` for every decoder).
- `tpt-kinetix-lean`, `tpt-kinetix-vision` — newer/experimental crates; check their own README before
  relying on them.

Data flow: `tpt-kinetix-cli` drives either `tpt-kinetix-pipeline` (transcode) or `tpt-kinetix-stream`
(RTMP/HLS). The pipeline fans out to demux → per-codec decoder → filter → encoder stages, all built on
`tpt-kinetix-core` types.

### Testing layers (see CONTRIBUTING.md for full detail)

1. **Unit tests** — inline `#[cfg(test)]` modules.
2. **Integration tests** — `<crate>/tests/*.rs`, one binary per file, exercises public API.
3. **Proptest** — `<crate>/tests/proptest_*.rs`; asserts parsers never panic on random input.
   Failing inputs shrink and persist to `.proptest-regressions/` — **commit these**.
4. **Fuzz** — `cargo-fuzz` targets per crate under `fuzz/`, ASan/UBSan enabled.
5. **Conformance** — `tpt-kinetix-test-utils/tests/conformance.rs` plus the H.264
   `corpus-check`/`conformance` recipes; decoder output is compared against reference decode
   (e.g. ffmpeg) for bit-exactness, not just "doesn't panic."

New parsing code needs a corresponding `*_never_panics` proptest. Public API needs `///` doc comments.

### Adding a new codec crate

Use the `cargo-generate` template for the crate skeleton, optionally combined with the `tpt-kinetix-kg`
pipeline to derive scaffolding from an existing FFmpeg C decoder — full walkthrough in
`docs/adding-a-codec.md` and CONTRIBUTING.md. Register the new crate in root `Cargo.toml`'s
`[workspace] members`.

## Known correctness state (read before claiming a fix)

H.264 CABAC I-slice decode currently desyncs on some inputs; root cause is still open (see
`tpt-kinetix-h264/src/entropy.rs`, `cabac_tables.rs`). P-frame (CAVLC path) and B-slice bi-prediction
are bit-exact against ffmpeg as of the last conformance pass. AV1 decode has the entropy coder wired up
in isolation but `decode_tile_group` is not yet rewired onto it. Don't assume a decoder path is correct
without running `just conformance` — `capabilities()` is the source of truth, not README prose.
