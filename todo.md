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
- [ ] (Stretch) Implement basic MKV/WebM (EBML) parsing support
- [x] Document demux crate's public API with doc examples

## Phase 3 — H.264 Decoder (via KG pipeline)

- [ ] Run Phase 1 KG tooling against FFmpeg's H.264 decoder source to generate initial Rust scaffolding into `kinetix-h264`
- [ ] Hand-complete NAL unit parsing (SPS/PPS/slice header parsing) via `nom`
- [ ] Hand-complete entropy decoding (CAVLC and/or CABAC) logic
- [ ] Hand-complete macroblock reconstruction (intra/inter prediction, transform, deblocking)
- [ ] Wire in `rayon` parallel iterators for slice-level concurrent decode per the KG-identified independence points
- [ ] Build a pixel-exact comparison harness: decode a test corpus with both real `ffmpeg`/`ffprobe` and `kinetix-h264`, diff raw decoded frames
- [ ] Run comparison harness across a range of real-world H.264 sample files (baseline/main/high profiles)
- [ ] Set up `cargo-fuzz` target for the H.264 bitstream/NAL parser
- [ ] Add benchmark (via `criterion`) comparing single-threaded vs `rayon`-parallel decode throughput
- [ ] Document known limitations/unsupported H.264 features for the initial release

## Phase 4 — AV1 Support

- [ ] Design/generate native Rust AV1 decoder scaffolding in `kinetix-av1` (KG-assisted where applicable)
- [ ] Implement AV1 bitstream parsing (OBU parsing) via `nom`
- [ ] Implement AV1 decode logic, validated incrementally against `dav1d`'s reference decoded output
- [ ] Build pixel-diff harness comparing `kinetix-av1` decode output to `dav1d` output
- [ ] Set up `cargo-fuzz` target for the AV1 bitstream/OBU parser
- [ ] Integrate `rav1e` as the AV1 encoder backend (dependency wiring, safe Rust API wrapper in `kinetix-av1`)
- [ ] Implement encode configuration mapping (bitrate/quality/speed presets) through `kinetix-core` types
- [ ] Add end-to-end test: decode H.264 sample → encode to AV1 via `rav1e` → verify playable output

## Phase 5 — Pipeline Architecture (parallel demux/decode/filter)

- [ ] Design staged pipeline architecture: demux stage → decode stage → filter stage as concurrent producer/consumer streams
- [ ] Implement inter-stage channels/queues (`crossbeam-channel` or similar) with backpressure handling
- [ ] Implement a basic filter stage (e.g. scale/format conversion) as a pluggable pipeline stage
- [ ] Wire `kinetix-demux`, `kinetix-h264`/`kinetix-av1`, and filter stage together through `kinetix-pipeline`
- [ ] Add pipeline-level error propagation and graceful shutdown handling
- [ ] Build benchmark harness comparing end-to-end `kinetix-pipeline` transcode throughput/latency vs. real `ffmpeg` CLI on multi-core hardware
- [ ] Document pipeline architecture with a diagram in README

## Phase 6 — Streaming Engine (RTMP ingest + HLS output)

- [ ] Implement RTMP handshake and chunk stream parsing in `kinetix-stream`
- [ ] Implement RTMP ingest server accepting a live push (e.g. from OBS) and feeding packets into `kinetix-pipeline`
- [ ] Implement HLS packaging: segment transcoded output into fMP4 or MPEG-TS segments
- [ ] Implement `.m3u8` playlist generation (live playlist, sliding window)
- [ ] Implement minimal HTTP server to serve HLS segments + playlist
- [ ] Add end-to-end test: push live RTMP stream → transcode through pipeline → verify playable HLS output in a player (e.g. `ffplay`/hls.js)
- [ ] Add reconnect/error-handling behavior for dropped RTMP connections
- [ ] Document streaming crate's public API and a quickstart example

## Phase 7 — Testing & Validation Infrastructure

- [ ] Consolidate pixel-diff comparison harness (vs. real FFmpeg / dav1d) into a reusable internal test crate
- [ ] Build/maintain a shared corpus of malformed and malicious sample files for fuzz regression across all parsers
- [ ] Wire `cargo-fuzz` jobs into CI (scheduled runs, not just on-demand)
- [ ] Add `proptest`-based property tests for parser edge cases (demux, H.264, AV1)
- [ ] Build a cross-codec conformance test suite runnable via `cargo test --workspace`
- [ ] Add code coverage reporting (e.g. `cargo-llvm-cov`) wired into CI
- [ ] Document the full testing strategy in `CONTRIBUTING.md`

## Phase 8 — crates.io Publishing Optimization

- [ ] Fill in `description`, `keywords` (≤5), `categories`, `readme`, `license`, `repository`, `documentation` fields for every crate's `Cargo.toml`
- [ ] Ensure every public crate has a crate-level doc comment (`//!`) explaining its purpose and usage
- [ ] Add runnable doc examples (`///` with `# Examples`) to key public APIs across crates
- [ ] Run `cargo doc --workspace --no-deps` and review generated docs for gaps
- [ ] Run `cargo package --list` per crate to verify no unwanted files are included in the published package
- [ ] Run `cargo publish --dry-run` per crate and fix any warnings/errors
- [ ] Define and document the required publish order (respecting inter-crate dependency graph: core → demux/codecs → pipeline → stream → cli)
- [ ] Adopt and document a semantic-versioning policy across the workspace (shared version vs. independent versions)
- [ ] Add CI badge, crates.io version badge, and docs.rs badge to root README
- [ ] Reserve crate names on crates.io for all planned crates
- [ ] Publish v0.1.0 of each crate in dependency order

## Phase 9 — Stretch / Future Codec Expansion

- [x] Document the repeatable process for adding a new codec via the `kinetix-kg` tooling (ingest → graph → codegen → hand-complete → validate → fuzz)
- [x] Evaluate adding AAC audio decode/encode support using the KG process
- [x] Evaluate adding HEVC/H.265 decode support using the KG process
- [x] Maintain a backlog note: the full ~400-codec FFmpeg surface is explicitly out of scope for the phases above; track candidate codecs here as they're prioritized
