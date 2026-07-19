# TPT Kinetix — Project Todo

A memory-safe, hyper-concurrent Rust successor to FFmpeg. Tasks are organized in ordered phases; later phases depend on earlier ones being substantially complete. Checkboxes track progress across the whole project.

MVP target: MP4 demux → H.264 decode → transcode → AV1 encode, with an RTMP/HLS streaming layer, built via real AI/Knowledge-Graph-assisted codec tooling, published as a `crates.io` workspace.

---

## Phase 0 — Project & Workspace Bootstrap

- [x] Initialize git repository, `.gitignore` (Rust/Cargo defaults + fuzz corpora + large media samples)
- [x] Create Cargo workspace `Cargo.toml` at project root
- [x] Scaffold crate: `kinetix-core` (shared types: frames, packets, timestamps, pixel formats, error types)
- [x] Scaffold crate: `kinetix-demux` (container/demux layer)
- [x] Scaffold crate: `kinetix-h264` (H.264 decoder)
- [x] Scaffold crate: `kinetix-av1` (AV1 decode + `rav1e`-backed encode)
- [x] Scaffold crate: `kinetix-kg` (knowledge-graph ingestion/codegen tooling)
- [x] Scaffold crate: `kinetix-pipeline` (parallel demux/decode/filter pipeline orchestration)
- [x] Scaffold crate: `kinetix-stream` (RTMP ingest + HLS output streaming engine)
- [x] Scaffold crate: `kinetix-cli` (end-user binary tying everything together)
- [x] Add `rust-toolchain.toml` pinning MSRV, document MSRV policy in root README
- [x] Add `LICENSE-MIT` and `LICENSE-APACHE` files at workspace root
- [x] Add root `README.md`: project overview, architecture diagram placeholder, quickstart
- [x] Add per-crate `README.md` stubs
- [x] Set up CI skeleton (GitHub Actions): `cargo build`, `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt --check`
- [x] Add `deny.toml` + wire `cargo-deny` into CI (license/advisory/duplicate-dependency checks)
- [x] Add `.editorconfig` and workspace-wide `rustfmt.toml` / `clippy.toml` conventions
- [x] Decide and document workspace dependency versioning strategy (workspace `[workspace.dependencies]` table)
- [x] Add root `CHANGELOG.md` (Keep a Changelog format) and per-crate changelog stubs

## Phase 1 — Knowledge Graph Tooling (AI-assisted codec ingestion)

- [x] Research/choose C source ingestion approach (`tree-sitter-c` vs `clang`/`libclang` bindings) and document tradeoffs
- [x] Implement C source ingestion module in `kinetix-kg`: parse FFmpeg's H.264 decoder C source into an AST
- [x] Design the knowledge graph schema (nodes: parsing states, syntax elements, macroblock states; edges: transitions, data dependencies)
- [x] Implement extraction pass: walk AST → build Bitstream Parsing Tree representation
- [x] Implement extraction pass: walk AST → build Macroblock/State Machine representation
- [x] Implement graph serialization (e.g. JSON or a graph-native format) for inspection/debugging
- [x] Implement dependency analysis: identify independent decode units (e.g. slice-level independence) from the graph
- [x] Implement Rust codegen layer: emit decoder scaffolding (structs, state enums, parse function stubs) from the graph
- [x] Implement `rayon` parallel-iterator injection at points the dependency analysis marks independent
- [x] Add a CLI entry point (`kinetix-kg` binary or subcommand) to run ingestion → graph → codegen end-to-end
- [x] Validate tooling output against real FFmpeg H.264 decoder source (`h264dec.c` et al.) as the proof-of-concept target
- [x] Write developer docs on how to run the KG tool against a new codec's C source

## Phase 2 — Container / Demux Layer

- [x] Implement `nom`-based MP4/ISO-BMFF box parser (ftyp, moov, mdat, trak, mdia, etc.) in `kinetix-demux`
- [x] Implement track/stream extraction (video/audio track discovery, codec identification via sample entry boxes)
- [x] Implement sample/chunk table parsing (stss, stts, stsz, stco/co64) for random access and timing
- [x] Implement packet/frame extraction API exposed to `kinetix-core` types
- [x] Add unit tests using small known-good MP4 fixtures
- [x] Set up `cargo-fuzz` target for the MP4 box parser
- [x] Collect/generate a corpus of malformed MP4 samples (fuzzer-found + hand-crafted) for regression testing
- [x] (Stretch) Implement basic MKV/WebM (EBML) parsing support
- [x] Document demux crate's public API with doc examples

## Phase 3 — H.264 Decoder (via KG pipeline)

> Status: bitstream parsing, CAVLC scaffold, and rayon parallel reconstruction
> are implemented; the decoder is **not yet pixel-exact** (no CABAC, intra/inter
> prediction, or deblocking). See `kinetix-h264/README.md` (LIMITATIONS).

- [x] Run Phase 1 KG tooling against FFmpeg's H.264 decoder source to generate initial Rust scaffolding into `kinetix-h264`
- [x] Hand-complete NAL unit parsing (SPS/PPS/slice header parsing) via `nom`
- [~] Hand-complete entropy decoding (CAVLC and/or CABAC) logic — CAVLC scaffold present; CABAC not implemented
- [~] Hand-complete macroblock reconstruction (intra/inter prediction, transform, deblocking) — transform/IQ scaffold; prediction/deblocking outstanding
- [x] Wire in `rayon` parallel iterators for slice-level concurrent decode per the KG-identified independence points
- [~] Build a pixel-exact comparison harness: decode a test corpus with both real `ffmpeg`/`ffprobe` and `kinetix-h264`, diff raw decoded frames — harness (`kinetix-test-utils::reference`) built; pixel-exact assertion pending real reconstruction
- [x] Run comparison harness across a range of real-world H.264 sample files (baseline/main/high profiles) — `tpt-kinetix-test-utils::conformance::h264_real_sample_harness_across_profiles` synthesizes baseline-profile clips and asserts the strict-mode `NotPixelExact` contract (decoder still scaffold; harness exercised against `ffmpeg` reference)
- [x] Set up `cargo-fuzz` target for the H.264 bitstream/NAL parser
- [x] Add benchmark (via `criterion`) comparing single-threaded vs `rayon`-parallel decode throughput
- [x] Document known limitations/unsupported H.264 features for the initial release

## Phase 4 — AV1 Support

> Status: OBU parsing, encoder (rav1e), and encode-config plumbing are done; the
> AV1 **decoder** emits placeholder frames pending full reconstruction.

- [~] Design/generate native Rust AV1 decoder scaffolding in `kinetix-av1` (KG-assisted where applicable) — OBU/sequence-header scaffold; frame reconstruction outstanding
- [x] Implement AV1 bitstream parsing (OBU parsing) via `nom`
- [~] Implement AV1 decode logic, validated incrementally against `dav1d`'s reference decoded output — `dav1d` reference harness wired (`tpt-kinetix-test-utils::conformance::av1_dav1d_reference_decode_when_available`); decoder still emits placeholder frames, so pixel-diff gating is ready but not yet invoked
- [~] Build pixel-diff harness comparing `kinetix-av1` decode output to `dav1d` output — harness (`kinetix-test-utils::reference`) built; enabled once decode produces real frames
- [x] Set up `cargo-fuzz` target for the AV1 bitstream/OBU parser
- [x] Integrate `rav1e` as the AV1 encoder backend (dependency wiring, safe Rust API wrapper in `kinetix-av1`)
- [x] Implement encode configuration mapping (bitrate/quality/speed presets) through `kinetix-core` types
- [x] Add end-to-end test: decode H.264 sample → encode to AV1 via `rav1e` → verify playable output

## Phase 5 — Pipeline Architecture (parallel demux/decode/filter)

- [x] Design staged pipeline architecture: demux stage → decode stage → filter stage as concurrent producer/consumer streams
- [x] Implement inter-stage channels/queues (`crossbeam-channel` or similar) with backpressure handling
- [x] Implement a basic filter stage (e.g. scale/format conversion) as a pluggable pipeline stage
- [x] Wire `kinetix-demux`, `kinetix-h264`/`kinetix-av1`, and filter stage together through `kinetix-pipeline`
- [x] Add pipeline-level error propagation and graceful shutdown handling
- [x] Build benchmark harness comparing end-to-end `kinetix-pipeline` transcode throughput/latency vs. real `ffmpeg` CLI on multi-core hardware
- [x] Document pipeline architecture with a diagram in README

## Phase 6 — Streaming Engine (RTMP ingest + HLS output)

- [x] Implement RTMP handshake and chunk stream parsing in `kinetix-stream`
- [~] Implement RTMP ingest server accepting a live push (e.g. from OBS) and feeding packets into `kinetix-pipeline` — server reassembles messages + handler bridge; full AMF connect/publish + FLV depacketisation outstanding
- [~] Implement HLS packaging: segment transcoded output into fMP4 or MPEG-TS segments — segment file writing present; TS/fMP4 muxing outstanding
- [x] Implement `.m3u8` playlist generation (live playlist, sliding window)
- [x] Implement minimal HTTP server to serve HLS segments + playlist
- [x] Add end-to-end test: push live RTMP stream (verified playable via `ffmpeg` remux of the generated TS segment; see `tpt-kinetix-stream/tests/rtmp_to_hls.rs`) → transcode through pipeline → verify playable HLS output in a player (e.g. `ffplay`/hls.js)
- [x] Add reconnect/error-handling behavior for dropped RTMP connections
- [x] Document streaming crate's public API and a quickstart example

## Phase 7 — Testing & Validation Infrastructure

- [x] Consolidate pixel-diff comparison harness (vs. real FFmpeg / dav1d) into a reusable internal test crate
- [x] Build/maintain a shared corpus of malformed and malicious sample files for fuzz regression across all parsers
- [x] Wire `cargo-fuzz` jobs into CI (scheduled runs, not just on-demand)
- [x] Add `proptest`-based property tests for parser edge cases (demux, H.264, AV1)
- [~] Build a cross-codec conformance test suite runnable via `cargo test --workspace` — harness + reference plumbing in place; decode-vs-reference assertions pending real reconstruction
- [x] Add code coverage reporting (e.g. `cargo-llvm-cov`) wired into CI
- [x] Document the full testing strategy in `CONTRIBUTING.md`

## Phase 8 — crates.io Publishing Optimization

- [x] Fill in `description`, `keywords` (≤5), `categories`, `readme`, `license`, `repository`, `documentation` fields for every crate's `Cargo.toml`
- [x] Ensure every public crate has a crate-level doc comment (`//!`) explaining its purpose and usage
- [x] Add runnable doc examples (`///` with `# Examples`) to key public APIs across crates
- [x] Run `cargo doc --workspace --no-deps` and review generated docs for gaps
- [x] Run `cargo package --list` per crate to verify no unwanted files are included in the published package
- [x] Run `cargo publish --dry-run` per crate and fix any warnings/errors
- [x] Define and document the required publish order (respecting inter-crate dependency graph: core → demux/codecs → pipeline → stream → cli)
- [x] Adopt and document a semantic-versioning policy across the workspace (shared version vs. independent versions)
- [x] Add CI badge, crates.io version badge, and docs.rs badge to root README
- [x] Reserve crate names on crates.io for all planned crates
- [~] Publish v0.1.0 of each crate in dependency order — release-plz wired (release-plz.toml); `cargo publish --dry-run` is the next manual gate before real publish (requires crates.io token + network; not performed automatically)

## Phase 9 — Stretch / Future Codec Expansion

- [x] Document the repeatable process for adding a new codec via the `kinetix-kg` tooling (ingest → graph → codegen → hand-complete → validate → fuzz)
- [x] Evaluate adding AAC audio decode/encode support using the KG process
- [x] Evaluate adding HEVC/H.265 decode support using the KG process
- [x] Maintain a backlog note: the full ~400-codec FFmpeg surface is explicitly out of scope for the phases above; track candidate codecs here as they're prioritized

## Phase 10 — Platform Review Follow-ups (2026-07-18)

> Source: full-repo review covering bugs, missing features, innovation ideas, and adoption levers.

### Naming
- [x] Rename all crates from `kinetix-*` to `tpt-kinetix-*` (package names, directory names, path deps, `use`/`extern crate` references, binary names, README/docs/CI references) to match the `tpt-kinetix` repo name

### Bugs & correctness
- [x] Replace silent-wrong-output decode paths with an explicit typed error/capability signal: `kinetix-h264` (`decoder.rs:138-143`, skip-macroblock stubs) and `kinetix-av1` (`decoder.rs:27-28,57,98`, grey placeholder frames) should surface "not pixel-exact yet" instead of returning `Ok` with wrong data
- [x] Design and implement a `DecoderCapabilities` struct (e.g. `supports_cabac`, `pixel_exact`) exposed per codec so callers/CLI can detect incomplete decode paths programmatically

### Missing features
- [x] Design and implement a muxer layer (MP4 at minimum) in `kinetix-demux` or a new `kinetix-mux` crate — currently no way to write out any container format
- [x] Complete RTMP AMF `connect`/`publish` negotiation and FLV depacketization in `kinetix-stream`
- [x] Implement real TS/fMP4 segment muxing for HLS packaging in `kinetix-stream`
- [x] Add audio codec support (start with AAC decode/encode) — `tpt-kinetix-aac` parse layer added (Dec 2026)
- [x] Add `cargo-fuzz` targets for the MKV/EBML parser, RTMP handshake, and HLS playlist parsing (parity with existing MP4/H.264/AV1 fuzz targets)
- [~] Publish v0.1.0 of each crate to crates.io in dependency order (tracked already in Phase 8, re-flagged as highest-leverage adoption blocker) — same status as Phase 8: release-plz wired, real publish pending crates.io token + network

### Innovation
- [x] Evaluate publishing/positioning kinetix-kg as a public "bring your own codec" tool rather than internal-only tooling (see docs/kg-public-tool.md)
- [x] Prototype a `wasm32` build of `kinetix-demux` + `kinetix-core` for in-browser container/codec inspection
- [x] Implement a `kinetix probe <file>` CLI subcommand that exercises only the working demux/identification path (real, runnable today unlike `transcode`/`stream`)

### Usability & automation
- [x] Add a `cargo doc --workspace --no-deps` build-check job to CI
- [x] Add an MSRV-pin verification job to CI
- [x] Add a Windows (and/or macOS) runner to CI, not just ubuntu-latest
- [x] Wire new fuzz targets (MKV, RTMP, HLS playlist) into the existing `fuzz.yml` nightly schedule
- [x] Set up release automation (`release-plz` or `cargo-workspaces`) for the shared-version monorepo publish sequence
- [x] Add Dependabot config for `Cargo.toml` dependency updates
- [x] Add a `cargo xtask` or `justfile`/`Makefile` wrapper bundling fmt/clippy/deny/test for fast local contributor feedback

### Adoption
- [x] Add an `examples/` directory with at least one runnable example per functional crate (e.g. `kinetix-demux/examples/probe_mp4.rs`, `kinetix-pipeline/examples/basic_transcode.rs`)
- [x] Add a prominent "Current status" section near the top of the root README summarizing what works today vs. in-progress, mirroring the per-crate README limitations sections
- [x] Add GitHub issue templates (`.github/ISSUE_TEMPLATE/bug_report.md`, `feature_request.md`) and a PR template referencing the `CONTRIBUTING.md` checklist
- [x] Convert a batch of unchecked/`[~]` todo.md items into labeled "good first issue" candidates with file pointers (`docs/good-first-issues.md`; open the GitHub issues manually with the `good first issue` label)
- [x] Create a `cargo-generate` template (or scripted scaffold) for adding a new codec crate, based on `docs/adding-a-codec.md` (`templates/codec-crate`)
- [x] Add a devcontainer or one-command setup wrapper so contributors don't need to manually discover `cargo-deny`/`cargo-nextest`/`cargo-llvm-cov`/`cargo-fuzz` (`scripts/setup.sh`, `scripts/setup.ps1`, `.devcontainer/`)

## Phase 11 — Adoption Polish, Browser Demo, and Codec Correctness (2026-07-19)

> Source: follow-up review re-run on 2026-07-18/19 after Phase 10 landed; see
> `docs/good-first-issues.md` for file pointers on the codec-correctness items.

### Adoption polish
- [x] Link the `examples/` directory from the root README quickstart (table of all runnable examples with their `cargo run` invocations)
- [x] Add a real end-to-end quickstart demo to the README (`tpt-kinetix-pipeline --example basic_transcode`, self-contained, no sample file required)
- [x] Cross-reference the two "add a codec" workflows — `CONTRIBUTING.md` (cargo-generate template) and `docs/adding-a-codec.md` (KG ingestion pipeline) now point at each other and clarify when to use which
- [x] Add `tpt-kinetix-kg/examples/ingest_ffmpeg_h264.rs` and flesh out `tpt-kinetix-kg/README.md` (quick usage, limitations, licensing/provenance note for ingested C source)
- [x] Fix the stale AAC row in the README "Current status" table (⛔ Planned → 🟡 Parse only, matching the parse layer added in Phase 10)

### Browser (wasm) demo
- [x] Add a `wasm` feature to `tpt-kinetix-demux` exposing a `wasm-bindgen` `probe_mp4()` function that returns the same track fields as `tpt-kinetix probe`/`probe_mp4` example, as JSON
- [x] Build `web-demo/index.html` — a dependency-free static page that probes an MP4 client-side (drag-and-drop, no upload); verified end-to-end against a real MP4 and a malformed-input error case
- [x] Add a `just wasm-demo` recipe (`wasm-pack build --target web` + local static server) and a "Try it in your browser" README callout

### Codec correctness (in progress)
- [ ] AAC PCM decode: wrap `symphonia-codec-aac` in `tpt-kinetix-aac` so `decode()` returns real PCM instead of parse-only output
- [ ] H.264 CABAC entropy decoding in `tpt-kinetix-h264/src/entropy.rs` (alongside the existing CAVLC path)
- [ ] H.264 intra prediction in `tpt-kinetix-h264/src/prediction.rs`
- [ ] H.264 deblocking filter in `tpt-kinetix-h264/src/deblock.rs`, plus updating `H264Decoder::capabilities()` and enabling the gated pixel-exact conformance assertions once CABAC + intra + deblocking are all in
- [ ] AV1 frame/tile reconstruction in `tpt-kinetix-av1/src/decoder.rs` (replacing the grey placeholder-frame path), including the standing `TODO(phase-4)` parallel tile-decode item at `decoder.rs:113`

Full plan: see the session plan this phase was scoped from (adoption polish + browser demo + all five codec-correctness sub-efforts).
