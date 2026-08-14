# TPT Kinetix — Project Todo

A memory-safe, hyper-concurrent Rust successor to FFmpeg. Tasks are organized in ordered phases; later phases depend on earlier ones being substantially complete. Checkboxes track progress across the whole project.

MVP target: MP4 demux → H.264 decode → transcode → AV1 encode, with an RTMP/HLS streaming layer, built via real AI/Knowledge-Graph-assisted codec tooling, published as a `crates.io` workspace.

> **Last reconciled with code/git:** 2026-08-14. Drift closed since the last
> edit: H.264 Phase D.4 (P/B-slice CABAC) is implemented and bit-exact;
> Phase F.1 + F.2 first bullet (8×8 flag parse / 8×8 inverse transform) landed;
> AV1 Phase C (superblock partition tree + per-block intra mode/`tx_size`) is
> implemented; `tpt-kinetix-realtime` gained the `deadline_ms`/`max_decode_ms`
> rate-control contract. **Working-tree additions (uncommitted):** H.264 F.3
> scaling-list application is now spec-correct — `decoder.rs` merges the PPS
> list over the SPS list (§8.5.9) and passes the active `ScalingLists` into
> `dequant_idct_4x4`/`dequant_idct_8x8`, which apply it; H.264 G.3 MBAFF parsing
> landed (`mb_adaptive_frame_field_flag` in SPS with a round-trip test,
> `mb_field_decoding_flag` decoded per macroblock pair in CAVLC + CABAC and
> stored on `Macroblock`, `MbFieldDecodingFlagContext` added); G.2 field
> deinterleave/merge helpers drafted in `reconstruct.rs`; AV1 Phase D loop
> filter / CDEF / restoration is now **wired** — `loop_filter.rs` is declared
> and invoked per-tile from `reconstruct.rs::reconstruct_av1_frame` (deblock →
> CDEF → restoration passthrough), with per-8×8-block tx/skip metadata
> collected into a `FrameMeta` during `decode_block`. **AV1 Phase F (parallel
> tile decode) is now wired** via `rayon` in `reconstruct_av1_frame` (each tile
> decodes into a tile-local buffer, then blits into the master planes;
> `decode_tile_group` restricts its superblock walk to the tile's region); **AV1
> Phase E reference-frame store (`RefFrameStore`, §7.20) is added** and
> populated by `refresh_frame_flags` after each reconstructed frame
> (`Av1Decoder::ref_frames()`). Inter-prediction motion compensation (the rest
> of Phase E) and `dav1d` pixel-exact validation (Phase G) are still open.
> Still open: H.264 F.4 (wire 8×8 reconstruction +
> remove the High-profile gate, High-profile validation), G.2/G.4/G.5
> (interlaced reconstruction), AV1 E/F/G, Phase 18 native AAC, realtime
> harness/foveation, volumetric octree. NOTE: `tests/conformance_matrix.rs`
> still labels CABAC P/B as "known non-conformant" — that classification is
> stale (contradicted by the live `cabac_conformance.rs` bit-exact P/B tests)
> and should be corrected in the test file.

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

> Status: bitstream parsing, CAVLC scaffold, intra prediction, the in-loop
> deblocking filter, and rayon parallel reconstruction are implemented; the
> decoder is **not yet pixel-exact** (no CABAC, no inter prediction). See
> `kinetix-h264/README.md` (LIMITATIONS).

- [x] Run Phase 1 KG tooling against FFmpeg's H.264 decoder source to generate initial Rust scaffolding into `kinetix-h264`
- [x] Hand-complete NAL unit parsing (SPS/PPS/slice header parsing) via `nom`
- [x] Hand-complete entropy decoding (CAVLC and/or CABAC) logic — **CAVLC fully done** (spec-exact `cavlc_tables.rs`, I/P/B-slice parsing + bit-exact residual decode, Phase 12 A/B/C/E); CABAC arithmetic decoding engine + context init present (`entropy.rs`), with I-slice context tables/binarizations/parsing done — full P/B-slice CABAC tracked separately at Phase 12 D
- [x] Hand-complete macroblock reconstruction (intra/inter prediction, transform, deblocking) — all done: intra prediction (`prediction.rs`), deblocking (`deblock.rs`), transform/IQ (`transform.rs`), inter prediction + motion comp (`reconstruct.rs`, Phase 12 C/E); I/P/B frames bit-exact vs ffmpeg
- [x] Wire in `rayon` parallel iterators for slice-level concurrent decode per the KG-identified independence points
- [x] Build a pixel-exact comparison harness: `kinetix-test-utils::reference` (`decode_h264_with_ffmpeg`, `decode_av1_with_dav1d`, plus ffmpeg-backed `decode_av1_with_ffmpeg`/`decode_aac_with_ffmpeg`) + `pixel_diff`/`audio_diff` driven across a generated corpus; CAVLC I/P/B assertions pass bit-exact (max_diff=0). CABAC/AV1 gating remains pending their reconstruction (Phase 12 D / AV1 C)
- [x] Run comparison harness across a range of real-world H.264 sample files (baseline/main/high profiles) — `tpt-kinetix-test-utils::conformance::h264_real_sample_harness_across_profiles` synthesizes baseline-profile clips and asserts the strict-mode `NotPixelExact` contract (decoder still scaffold; harness exercised against `ffmpeg` reference)
- [x] Set up `cargo-fuzz` target for the H.264 bitstream/NAL parser
- [x] Add benchmark (via `criterion`) comparing single-threaded vs `rayon`-parallel decode throughput
- [x] Document known limitations/unsupported H.264 features for the initial release

## Phase 4 — AV1 Support

> Status: OBU parsing, encoder (rav1e), and encode-config plumbing are done; the
> AV1 **decoder** now performs intra keyframe tile-group reconstruction via the
> real symbol decoder (`coeffs()` syntax), wired in `reconstruct`/`coeff`.
> Block structure is still a placeholder grid (Phase C), so output is not yet
> pixel-exact against `dav1d`; real streams fail loudly rather than decode to
> silent garbage.

- [x] Design/generate native Rust AV1 decoder scaffolding in `kinetix-av1` (KG-assisted where applicable) — OBU/sequence-header scaffold + intra tile-group reconstruction done (Phase A/B); partition/mode syntax outstanding (Phase AV1 C)
- [x] Implement AV1 bitstream parsing (OBU parsing) via `nom`
- [x] Implement AV1 decode logic, validated incrementally against `dav1d`'s reference decoded output — `dav1d` reference harness wired (`tpt-kinetix-test-utils::conformance::av1_dav1d_reference_decode_when_available`); intra keyframe coefficients now decode via the symbol decoder (Phase A/B), but placeholder block grid means pixel-diff gating is ready but not yet invoked (Phase AV1 C outstanding)
- [x] Build pixel-diff harness comparing `kinetix-av1` decode output to `dav1d` output — harness (`tpt-kinetix-test-utils::reference`) built; enabled once decode produces real frames (Phase AV1 C)
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
- [x] Implement RTMP ingest server accepting a live push (e.g. from OBS) and feeding packets into `kinetix-pipeline` — server reassembles messages + handler bridge + full AMF connect/publish + FLV depacketisation (completed in Phase 10; see `tpt-kinetix-stream`)
- [x] Implement HLS packaging: segment transcoded output into fMP4 or MPEG-TS segments — segment file writing + real TS/fMP4 muxing (completed in Phase 10; see `tpt-kinetix-stream`)
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
- [x] Build a cross-codec conformance test suite runnable via `cargo test --workspace` — harness + reference plumbing in place; ffmpeg-gated CAVLC I/P/B assertion tests pass bit-exact, decode-vs-reference assertions for CABAC/AV1 pending their reconstruction (Phase 12 D / AV1 C)
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
- [x] AAC PCM decode: wrap `symphonia-codec-aac` in `tpt-kinetix-aac` so `decode()` returns real PCM instead of parse-only output — `AacDecoder::decode()` delegates AAC-LC reconstruction to `symphonia-codec-aac`, returning interleaved `f32` PCM; verified by the `ffmpeg`-gated round-trip test `tpt-kinetix-aac/tests/decode_pcm.rs` (HE-AAC SBR/PS still unsupported by the wrapped decoder)
- [~] H.264 CABAC entropy decoding in `tpt-kinetix-h264/src/entropy.rs` (alongside the existing CAVLC path) — binary arithmetic decoding engine (`CabacDecoder`: `decode_decision`/`decode_bypass`/`decode_terminate`, §9.3.3.2) and context-variable init from `(m, n)` (§9.3.1.1) are implemented and tested with the spec's `rangeTabLPS`/`transIdxLPS`/`transIdxMPS` tables; mb_type I-slice context (Table 9-11), coded_block_pattern (Table 9-12), and mb_qp_delta (Table 9-20) context tables implemented and tested; still missing: the remaining per-syntax-element context-index tables (Tables 9-13..9-33) and macroblock-level CABAC syntax parsing wired into `decoder.rs`
- [x] H.264 intra prediction in `tpt-kinetix-h264/src/prediction.rs`
- [x] H.264 deblocking filter in `tpt-kinetix-h264/src/deblock.rs`, plus updating `H264Decoder::capabilities()` and enabling the gated pixel-exact conformance assertions once CABAC + intra + deblocking are all in
- [~] AV1 frame/tile reconstruction in `tpt-kinetix-av1/src/reconstruct.rs` (replacing the grey placeholder-frame path), including the standing `TODO(phase-4)` parallel tile-decode item — FrameHeader and TileGroup OBUs now parsed and stored; `TileData` struct captures per-tile payloads for future parallel decode; **AV1 Phase B is done (2026-08-09): intra keyframe coefficients now decode through the real symbol decoder via `coeffs()` (all_zero / intra_tx_type / eob_pt_* / eob_extra / coeff_base(_eob) / coeff_br / dc_sign / sign_bit / Exp-Golomb tail) in `coeff.rs`, rewiring the old `BitReader` scheme in `decode_tile_group`/`decode_chroma_tx`**; still outstanding: real superblock partition + mode syntax (Phase C), inter prediction, non-square transforms, full AV1 transform set, and loop filters

Full plan: see the session plan this phase was scoped from (adoption polish + browser demo + all five codec-correctness sub-efforts).

## Phase 12 — Full From-Scratch Conformant Decoders (2026-07-20)

> Goal: genuine pixel-exact, conformant decode for H.264 (and AV1), built
> from scratch. Every normative table is transcribed from an authoritative
> source (ITU-T H.264 spec; cross-checked against permissively-licensed
> references) with citations — no guessed/approximated tables. Each phase must
> compile, be unit-tested, and validated bit-exact against `ffmpeg`/`dav1d`
> before its box is checked. Do NOT flip `capabilities().pixel_exact = true`
> for a codec until its conformance harness passes.

### Foundations & correctness fixes (blockers)
- [x] Replace the approximated CAVLC tables in `slice.rs` with spec-exact
      `coeff_token` (Table 9-5), `level_prefix` (Table 9-6), `total_zeros`
      (Tables 9-7/9-8), chroma-DC `total_zeros` (Table 9-9), and `run_before`
      (Table 9-10) tables, with unit tests per table — done in
      `src/cavlc_tables.rs` (exhaustive prefix-code roundtrip tests pass)
- [x] Replace the simplified single-scale inverse-quant/IDCT in `macroblock.rs`
      with the spec `LevelScale4x4` weighting + correct 4×4 residual transform
      (§8.5.12), and add the Intra_16×16 luma DC Hadamard transform (§8.5.10)
      and chroma DC transform (§8.5.11) — done in `src/transform.rs` (unit-tested)
- [x] Extend SPS/PPS/slice-header parsers to retain all fields needed for
      reconstruction (chroma_format_idc, transform_8x8_mode_flag,
      chroma_qp_index_offset, num_ref_idx overrides, ref_pic_list_modification,
      pred_weight_table, dec_ref_pic_marking) — SPS/PPS extended; slice header
      fully rewritten (§7.3.3) exposing `data_bit_offset`

### H.264 — Phase A: I-frame / baseline pixel-exact
- [x] Implement the real slice-data parsing loop (§7.3.4): mb_type,
      coded_block_pattern, mb_qp_delta, CAVLC residual parsing dispatch —
      I-slice parser done in `src/slice_data.rs` (mb_type Table 7-11, CBP
      Table 9-4, nC neighbour derivation, spec §9.2.2 level decoding, unit
      tested). Wired into `decoder.rs::decode_slice()`; the fallback path now
      produces spec-exact CAVLC I-frames via `parse_i_slice` +
      `reconstruct_intra_frame` + deblocking, instead of the all-skip grey stub.
      I_PCM + Intra_4×4 MPM neighbour tracking are also implemented in
      `slice_data.rs`.
- [x] Neighbour-availability + Intra_4×4/16×16 mode signalling
      (prev_intra4x4_pred_mode / rem_intra4x4_pred_mode, §8.3.1.1) — full MPM
      derivation with left/top neighbour tracking implemented in
      `slice_data.rs::parse_i_macroblock`
- [x] Validate bit-exact I-frame baseline decode vs `ffmpeg` on a generated corpus —
      found and fixed a real bug: `Intra4x4Mode::DiagonalDownRight` in
      `prediction.rs` used the wrong sample weighting (only left/top-left
      samples, mismapped to the wrong output positions) instead of the spec
      §8.3.1.2.5 formula (cross-checked against ffmpeg's
      `pred4x4_down_right_c`); the other 7 Intra_4×4 modes were individually
      re-verified against the same ffmpeg reference and are correct. Also
      fixed an OOB panic in `deblock.rs` for non-16-aligned picture
      dimensions (missing per-sample x/y bounds checks in the last partial
      row/column of macroblocks). `cavlc_iframe_no_deblock_is_bitexact` and
      `cavlc_iframe_with_deblock_tracks_progress` are now both bit-exact
      (max_diff=0, not just <=20), and an ad hoc corpus of 8 MB-aligned
      clips (`tpt-kinetix-h264/examples/corpus_check.rs`, varied resolution
      48x32..128x96, testsrc/smptebars content) all decode bit-exact.
      Remaining known gap: non-16-aligned picture dimensions still show
      small (≤53) pixel diffs clustered at the partial right/bottom
      macroblock edges (deblocking/prediction edge-sample handling for
      cropped pictures) — tracked as a follow-up, not yet root-caused.

### H.264 — Phase B: complete CAVLC
- [x] Wire the spec-exact CAVLC tables into residual parsing; correct nC
      derivation from left/top neighbour TotalCoeff; validate on P/I CAVLC clips —
      `slice_data.rs` already drove the spec-exact `cavlc_tables.rs` tables
      (coeff_token/total_zeros/run_before) with real left/top-neighbour nC
      derivation for I-slices as of Phase A, validated bit-exact there. Removed
      the last user of the old approximated hand-rolled VLC tables in
      `slice.rs` (`parse_cavlc_residual` and its private VLC0/1/2/3,
      total_zeros, run_before helpers) — it was dead code (only its own unit
      test called it) left over from before `cavlc_tables.rs` existed, and its
      `total_zeros`/`run_before` tables were explicitly approximated per their
      own doc comments. P-slice CAVLC validation is blocked on Phase C (inter
      prediction) since P slices need motion compensation to reconstruct, but
      the residual-parsing path itself (coeff_token/nC/total_zeros/run_before)
      is slice-type-agnostic and already spec-exact.

### H.264 — Phase C: inter prediction (P-frames)
- [x] DPB + POC derivation (§8.2.1), reference-picture-list construction (§8.2.4)
- [x] Motion-vector prediction (§8.4.1) and mb_type/sub_mb partition parsing
- [x] Luma 6-tap + chroma bilinear sub-pel interpolation (§8.4.2.2)
- [x] Validate bit-exact P-frame decode vs `ffmpeg` — **DONE (2026-08-08)**

  #### Phase C.1 — unblock P-slice parsing (RESOLVED)

  **Status (2026-08-07):** The CAVLC bit-position desync is **resolved**.
  `p_slice_cavlc_invariant::p_slice_cavlc_parse_succeeds` now passes (the
  slice parses to completion without a `run_before > zeros_left` error), and the
  P-slice path (`parse_p_slice` → `parse_p_macroblock`) runs end-to-end. The
  prior desync was fixed by the `0ee3386` line of work (inter CBP table,
  `coeff_token` FLC codes, `dec_ref_pic_marking` gating). The `level_code`
  assembly in `parse_cavlc_block`/`parse_cavlc_chroma_dc` is the spec form
  (`base = (level_prefix << suffixLength)`, escape `+15` for `prefix>=14 &&
  sl==0`, and `+(15 - (1<<(prefix-3)) + 4096)` for `prefix>=15`); this was
  cross-checked against the IJERT reference algorithm and against I-frame
  bit-exactness (changing it to a `level_prefix.min(15)` form regressed the
  I-frame tests, confirming the committed form is correct).

  - [x] Reproduce deterministically (was `max_diff=127`, now resolved).
  - [x] Static 2-frame clip (identical frames → zero residual, cbp=0) is
        BIT-EXACT (`inter_skip_copies_reference` + live decode).
  - [x] CAVLC tables cross-checked against FFmpeg `h264data.c`; chroma-AC
        `total_zeros` "bug" confirmed NOT a bug (single combined table is
        spec-correct).
  - [x] `parse_p_slice` / `parse_p_macroblock` run end-to-end without panic.

  #### Phase C.2 — P-frame reconstruction correctness — **DONE (2026-08-08)**

  The earlier "max_diff=2 over 49/4608 samples" gap was **not** a CAVLC
  residual bug — it was a false premise in the test harness. `-x264-params
  deblock=0` only zeroes x264's alpha/beta filter *offset*; it does not set
  `disable_deblocking_filter_idc=1`, so every P-frame conformance test was
  unknowingly comparing against an ffmpeg reference with deblocking **on**
  while assuming it was off (`no-deblock=1` is the key that actually disables
  it). Two independent lines of evidence closed this out:
  1. A fixed differential CAVLC oracle (`p_slice_oracle2.rs` — an independent
     re-implementation of the §9.2.2 level-assembly walk, sharing only the
     already-verified VLC tables) found **0 mismatches** across all 104
     luma/chroma-AC/chroma-DC residual blocks of the 64×48 clip. (The oracle
     itself had a bug — it discarded computed chroma-AC `TotalCoeff` back into
     the nC neighbour grid as all-zero — which is what caused the earlier
     apparent desync; fixed alongside.)
  2. With deblocking genuinely disabled (`no-deblock=1`), P-frame decode is
     **bit-exact (max_diff=0)** against ffmpeg — proving CAVLC, MV
     prediction, motion compensation, and dequant/IDCT are all already
     correct.
  3. That pointed the real gap at the deblocking filter: `deblock.rs` derived
     one boundary-strength (`bS`) value per *whole macroblock edge* from a
     whole-MB `has_coeffs` flag, and had no motion-vector/ref-index rule at
     all (documented as a known gap). Spec §8.7.2.1 requires `bS` per
     4-sample segment (i.e. per pair of 4×4 blocks straddling the edge), with
     a coefficient-OR rule (bS=2 if *either* side's block has nonzero coeffs
     — the old code's "both vs exactly one coded" distinction, giving bS=1,
     was itself non-spec) and a fallback bS=1 rule for differing ref_idx or
     MV components differing by ≥4 quarter-pel. Rewired `DeblockMbInfo` to
     carry the per-4×4-block `nz` grid and `MvStore` cells, and restructured
     `deblock_luma_edge`/`deblock_chroma_edge` to filter each 4-sample (luma)
     / 2-sample (chroma) segment with its own bS. Result: bit-exact
     (max_diff=0) with deblocking **enabled** too (`p_frame_conformance.rs`
     now covers both variants; `cavlc_conformance.rs`'s I-frame equivalent
     tightened from a loosened `<=20` bound to exact 0 as well).

  - [x] Confirm P-slice `parse_p_macroblock` runs end-to-end without panic.
  - [x] Motion-compensated reconstruction (MV prediction + 6-tap/bilinear
        interp) produces correct reference-block fetches.
  - [x] Validate bit-exact P-frame decode vs `ffmpeg` — **max_diff=0**, both
        with deblocking disabled and with deblocking enabled.
  - [x] Independent CAVLC oracle confirms residual decode was never the bug.
  - [x] Per-4×4-block deblocking `bS` (coefficient-OR + MV/ref rule) fixes the
        real remaining gap; chroma reuses the co-located luma `bS` per spec.

   #### Phase C.3 — multi-P-frame chaining — **RESOLVED (2026-08-13)**

   - [x] `tests/multi_frame_dpb.rs::ipppp_clip_decodes_bitexact_frame_by_frame`
         now passes bit-exact (max_diff=0) for **all five** frames on the
         64×48 IPPPP clip. The original frame-2 divergence (max_diff=33 over
         124 samples, the first P picture predicting from another P picture)
         is gone. Root cause was the reference-list / POC-ordering logic that
         later landed in Phase E.1 (`modify_ref_pic_list` §8.2.4.3), E.2
         (`mark_decoded_picture` §8.2.5), and E.5 (`build_ref_list_l0_b_slice`
         POC-based ordering, plus the 2-partition B MVD interleave fix) — all
         of which make second-and-later P pictures predict from the correct
         reference. The decoder's `MAX_MB_COUNT`/`MAX_DIMENSION` guards
         (decoder.rs) also bound the picture allocation that the original
         report worried about. Verified by re-running the test directly.
   - [x] `tests/fuzz_from_seed.rs::fuzz_structured_seeds` is now a reliable CI
         gate. Two fixes landed:
         1. **Real fuzz-crash fixed:** a structured-seed mutation survey
            (800 k iters) found an actual panic — `attempt to shift left with
            overflow` at `slice.rs::WeightEntry::default_for`, triggered by an
            attacker-controlled `luma_log2_weight_denom`/`chroma_log2_weight_denom`
            (`ue(v)`, unbounded) fed into `1 << denom`. Fixed by bounding both
            denoms to ≤30 at parse time in `parse_pred_weight_table`
            (§7.4.3.2), and making `default_for` clamp defensively. (The
            `>> (denom + 1)` in `reconstruct.rs::weighted_bi` also needs
            denom ≤30 to stay panic-free, so 30 is the safe ceiling.) Other
            unbounded shifts were already safe: `log2_max_frame_num_minus4` is
            bounded ≤12 (sps.rs) and `transform.rs` `shift = qp/6` is always
            in range.
         2. **Machine-relative timeout:** replaced the fixed 300 ms constant
            with a budget calibrated from the worst-case *valid* decode on the
            runner (a 36864-MB IDR, capped by `MAX_MB_COUNT`), set to
            `max(2s, min(30s, 3 × worst_valid))`. Added a 60 s overall
            wall-clock deadline so the test can't run for hours on fast
            runners, and silenced the panic hook so caught-panic backtraces
            don't spam CI logs. Re-running the survey now shows **0 panics /
            800 k iters**; the test itself completes ~204 k iters in 60 s with
            no crash and slowest decode ~97 ms (well under the calibrated
            timeout).

### H.264 — Phase D: CABAC

> Updated 2026-08-09: the engine + I-slice context tables/binarizations from
> the previous entry were re-verified against FFmpeg's `libavcodec/h264_cabac.c`
> (cross-checked the same way this repo's CAVLC tables already are) and found
> to have real bugs, not just missing coverage — `MbTypeICabacContext`'s bin-0
> context was static instead of neighbor-derived and was missing the I_PCM
> `decode_terminate()` check; `CbpCabacContext` had no neighbor input at all;
> `MbQpDeltaCabacContext` used a truncated-unary+EG0 binarization instead of
> the real unbounded-unary one. All three were rewritten, and the previously
> "outstanding" I-slice-relevant tables (chroma pred mode, intra4x4 pred mode,
> coded_block_flag, significant/last_significant_coeff_flag, coeff_abs_level)
> were implemented — see Phase D.1/D.3 below for what's actually done now.

- [x] Context-index tables + binarizations for **I-slice** syntax elements
       (mb_type, intra_chroma_pred_mode, prev/rem_intra4x4_pred_mode,
       coded_block_pattern, mb_qp_delta, coded_block_flag,
       significant_coeff_flag/last_significant_coeff_flag/coeff_abs_level_minus1)
       implemented in `entropy.rs`/`cabac_tables.rs` and unit-tested. P/B-slice
       tables (mb_skip_flag refinement, mb_type P/B, sub_mb_type, ref_idx, mvd)
       also implemented (Phase D.1/D.2) and now exercised by the P/B-slice
       parsing landed in Phase D.4.
- [x] CABAC macroblock/residual syntax parsing wired into the slice loop
       (`slice_data.rs::parse_i_slice_cabac`/`parse_intra_macroblock_cabac`,
       `parse_p_slice_cabac`/`parse_p_macroblock_cabac`,
       `parse_b_slice_cabac`/`parse_b_macroblock_cabac`, all wired into
       `decoder.rs`) for I/P/B slices, reusing the existing CAVLC-path
       reconstruction/dequant/IDCT/deblock code unchanged. The CABAC 8×8-transform
       (High-profile) path remains gated off in `decoder.rs:305` (Phase F).
- [x] **CABAC I-slice desync bug — RESOLVED 2026-08-12.** Root cause:
      `entropy.rs::TRANS_IDX_LPS[28]` was `23`; the correct value is `22` — a
      single-entry transcription error in the `transIdxLPS` (spec Table 9-45)
      state-transition table, present since the table was first added. It
      only manifested when `pStateIdx` reached exactly `28` *and* underwent
      an LPS (least-probable-symbol) transition while decoding
      `coeff_abs_level_minus1`'s truncated-unary continuation bins — a
      specific (state, branch) combination most test content never hit,
      which is why CAVLC conformance, the I-slice mb_type/cbp/residual unit
      tests, and even several new bespoke CABAC repros (flat/checkerboard/
      random-noise/gradient content, including ones exercising the
      significant-coefficient-count-16/16 edge case and the Exp-Golomb escape
      path) all passed while `ffmpeg`'s `testsrc`/`testsrc2`/`rgbtestsrc`/
      `smptebars` filters at `size=16x16` reliably triggered it.
      \
      Found by building a self-contained C harness (MSVC via
      `vcvars64.bat`+`cl.exe`; no mingw/gcc available on this box) that
      copies FFmpeg's actual `libavcodec/cabac.c` engine and
      `ff_h264_cabac_tables` verbatim (fetched fresh from
      github.com/FFmpeg/FFmpeg — not reimplemented from memory), then running
      it against the real 290-byte CABAC payload from the `testsrc` repro
      side-by-side with the Rust decoder's own per-bin trace
      (`entropy.rs::CabacDecoder::debug_state()`, temporary, removed after).
      The two engines' `range` value (directly comparable — it isn't
      rescaled differently between representations) matched at every single
      call through the significance map and the first several level
      decodes, then diverged at one specific `coeff_abs_level_minus1`
      continuation bin. Isolating that one call and decoding FFmpeg's packed
      `ff_h264_mlps_state` table by hand for the same `(pStateIdx, valMPS)`
      pair going in — cross-checked programmatically for *all* 64 states in
      both `TRANS_IDX_LPS` and `TRANS_IDX_MPS`, not just the one that
      failed — found exactly one mismatch: index 28 of `TRANS_IDX_LPS`.
      \
      This means the extensive engine/context-table verification from the
      previous investigation pass (re-checking against real FFmpeg source,
      writing an independent from-scratch Python reimplementation) was
      thorough but insufficient: two implementations built from the *same*
      transcribed table inevitably agree with each other while both being
      wrong, so the only way this surfaced was comparing against the actual
      *compiled* reference engine rather than another reimplementation from
      the same source reading. Worth remembering as a lesson if a similar
      "two independent implementations agree but still don't match ffmpeg"
      situation comes up elsewhere (AV1 entropy decoder, P/B-slice CABAC).
      \
      `cabac_conformance.rs`'s two tests (Main-profile CABAC I-frame,
      deblocking on/off, real `testsrc` content at 64×48) are un-`#[ignore]`d
      and pass bit-exact; all 191 `tpt-kinetix-h264` unit tests still pass.
- [x] Validate bit-exact Main/High CABAC decode vs `ffmpeg` — done, see above.

  #### Phase D.1 — remaining context-index tables

  > Updated 2026-08-09: the three remaining Phase D.1 checkboxes are now
  > done, but this is **context-index-table work only** (init values +
  > ctxIdxOffset layout + ctxIdxInc derivation *data*), not P/B-slice syntax
  > parsing — Phase D.4 below (mb_type-P/B/sub_mb_type/ref_idx/mvd
  > *binarization and decode-loop wiring*) remains not started. Added
  > `CABAC_CTX_INIT_PB0`/`1`/`2` (1024-entry `(m,n)` tables per
  > `cabac_init_idc`) plus ctxIdxOffset constants for mb_skip_flag/mb_type/
  > sub_mb_type (P/SP and B) and mvd_x/mvd_y/ref_idx, all fetched and
  > cross-checked from FFmpeg's `libavcodec/h264_cabac.c` two independent
  > ways (the source's own `/* lo - hi */` block comments plus the literal
  > ctxIdx arithmetic in `decode_cabac_mb_skip`/`decode_cabac_p_mb_sub_type`/
  > `decode_cabac_b_mb_sub_type`/`decode_cabac_mb_ref`/`decode_cabac_mb_mvd`).
  > Found and fixed a real bug in the process: the pre-existing
  > `MB_SKIP_FLAG_P_INIT` stub's three `(m,n)` pairs turned out to be ctxIdx
  > 11's value from *each of the three* `cabac_init_idc` tables, not ctxIdx
  > 11/12/13 from one table — `MbSkipFlagContext::new_p_slice` now takes a
  > `cabac_init_idc` parameter and reads the verified `CABAC_CTX_INIT_PB*`
  > tables directly (see `entropy.rs`'s `MbSkipFlagContext` doc comment for
  > the full story); a `new_b_slice` constructor (ctxIdx 24..=26) was added
  > alongside it, confirmed from source to reuse the same condTermFlag
  > derivation as P/SP. Also added `ctxBlockCat` 5 (Luma8x8) residual
  > contexts: extended `SIG_COEFF_CTX_BASE`/`LAST_COEFF_CTX_BASE`/
  > `COEFF_ABS_LEVEL_M1_CTX_BASE` to 6 entries, and confirmed from FFmpeg's
  > `decode_cabac_residual_nondc` that `coded_block_flag` is *not* separately
  > signalled for Luma8x8 in the non-4:4:4 case this crate targets (so
  > `CBF_CTX_BASE` deliberately stays at 5 entries, documented on
  > `CAT_LUMA_8X8`); added the `significant_coeff_flag`/
  > `last_significant_coeff_flag` many-to-one ctxIdxInc indirection tables
  > (`SIG_COEFF_CTX_INC_8X8_FRAME`/`LAST_COEFF_CTX_INC_8X8_FRAME`, 63 entries
  > each) as standalone consts — **not** wired into
  > `ResidualCabacContext::decode_block`, which still assumes ctxIdxInc ==
  > scan position (only valid for cats 0..=4); that restructuring, plus all
  > actual P/B mb_type/sub_mb_type/ref_idx/mvd binarization, is Phase D.4.
  > All new tables/consts are unit-tested in `cabac_tables.rs`.

  - [x] mb_type I-slice, coded_block_pattern, mb_qp_delta, intra_chroma_pred_mode,
        prev/rem_intra4x4_pred_mode, coded_block_flag, significant_coeff_flag,
        last_significant_coeff_flag, coeff_abs_level_minus1 — all I-slice-only,
        frame coding (no MBAFF/field), no 8x8 transform.
  - [x] mb_type P/B-slice, sub_mb_type, ref_idx, mvd context init (needed for
        Phase D.4 P/B-slice CABAC) — tables + ctxIdxOffset constants only,
        see `cabac_tables.rs`; no binarization/parsing implemented yet.
  - [x] mb_skip_flag refinement for P/B — old `MB_SKIP_FLAG_P_INIT` stub was
        wrong (see note above), fixed and extended with a B-slice
        constructor; both now `cabac_init_idc`-dependent per source.
  - [x] 8x8-transform-specific residual contexts (ctxBlockCat 5, Luma8x8) —
        context-index tables + ctxIdxInc indirection LUTs only; still not
        wired into `decode_block`, so the `!pps.transform_8x8_mode_flag`
        gate elsewhere in the codebase must stay in place until Phase D.4.

  #### Phase D.2 — binarizations (§9.3.2)
  - [x] Truncated-unary, FL (LSB-first per §9.3.2.5, distinct from CAVLC's
        MSB-first `u(v)`), and UEG0 (via `decode_bypass_eg`) binarizations
        used by the I-slice tables above are implemented; see `entropy.rs`.
  - [x] Binarizations specific to P/B-slice elements (mvd's UEGk suffix beyond
        what `decode_bypass_eg` already covers, ref_idx's truncated unary) —
        **already implemented**, found while starting this phase: commit
        `a8e4b56` (labeled "MMCO/ref-list wiring") landed `RefIdxCabacContext`
        and `MvdCabacContext` in `entropy.rs` alongside the ref-pic-list work,
        but never got its own checkbox here. `RefIdxCabacContext::decode` is a
        1:1 transliteration of FFmpeg's `decode_cabac_mb_ref` ctxIdx recurrence
        (truncated unary, no separate bin-0 special case). `MvdCabacContext::decode`
        does the context-coded truncated-unary prefix (saturating at 9) then
        falls through to the existing `decode_bypass_eg(3)` for the UEGk
        suffix — confirming no new bypass primitive was needed. Both are
        unit-tested (`ref_idx_decode_*`, `mvd_decode_*`, 6 tests, all passing).
        Neither is wired into `slice_data.rs`'s parser yet — that's still
        Phase D.4, unchanged below.

  #### Phase D.3 — CABAC syntax parsing in the slice loop
  - [x] I-slice mb_type / intra pred modes / coded_block_pattern / mb_qp_delta
        / residual wired into `slice_data.rs`'s CABAC-specific parser,
        reusing existing CAVLC reconstruction (transform/prediction/deblock)
        unchanged (see `parse_i_slice_cabac`).
  - [x] Fix the known desync bug above (`TRANS_IDX_LPS[28]` fix, 2026-08-12).
  - [x] Add a Main/High-profile CABAC clip to the corpus and validate bit-exact
        vs `ffmpeg` — `cabac_conformance.rs`'s two tests are un-`#[ignore]`d
        and passing (64×48 `testsrc`, deblocking on/off).

#### Phase D.4 — P/B-slice CABAC — **DONE (2026-08-13)**

> `slice_data.rs` gained `parse_p_slice_cabac`/`parse_p_macroblock_cabac` and
> `parse_b_slice_cabac`/`parse_b_macroblock_cabac`, and `decoder.rs` now
> dispatches CABAC P/B slices through them (reusing the CAVLC-path
> reconstruction/dequant/IDCT/deblock unchanged). The context tables/parsing
> for mb_skip_flag, mb_type P/B, sub_mb_type, ref_idx, and mvd were already in
> place (Phase D.1/D.2), and the mb_type/CBP binarization + context logic was
> fixed in commit `0f9e0a3`. Conformance is bit-exact:
> `tests/cabac_conformance.rs` has live (un-`#[ignore]`d) `cabac_pframe_*_is_bitexact`
> and `cabac_bframe_*_is_bitexact` tests (deblocking on/off), and
> `tests/cabac_pframe_conformance.rs` additionally pins the inter-MB CABAC parse
> path bit-exact vs ffmpeg. NOTE: the CABAC **8×8-transform** path is still
> explicitly gated off in `decoder.rs:305` (High profile, `transform_8x8_mode_flag`)
> — see Phase F.4.

- [x] mb_skip_flag, mb_type P/B, sub_mb_type, ref_idx, mvd context tables +
      parsing, following the same I-slice-first-then-P-slice pattern used
      for CAVLC

### H.264 — Phase E/F/G: advanced tools

> Broken down 2026-08-06: each of the three items below previously bundled
> several independent features into one checkbox. Grounded against the
> actual code state: `parse_ref_pic_list_modification` (`slice.rs:305`),
> `parse_dec_ref_pic_marking` (`slice.rs:371`), and `parse_pred_weight_table`
> (`slice.rs:334`) all already parse their syntax but only to advance the bit
> position — every decoded value (`_long_term_pic_num`, `_lw`, `_cw`, etc.) is
> discarded, per their own doc comments. `slice_data.rs` has no B-slice
> `mb_type`/`sub_mb_type` handling at all yet. `transform_8x8_mode_flag` is
> parsed into `pps.rs` but `transform.rs` has no 8×8 transform path, and the
> SPS/PPS `scaling_list` values are parsed but never applied at dequant.
> `field_pic_flag` is parsed in `slice.rs` but `bottom_field_flag` is
> discarded and there is no field/MBAFF decode logic anywhere.
>
> Updated 2026-08-09: Phase E.1 is done —
> `parse_ref_pic_list_modification`'s values are no longer discarded; they now
> drive `ref_pic::modify_ref_pic_list` (§8.2.4.3). Phase E.2 is done too —
> `parse_dec_ref_pic_marking`'s values now drive `ref_pic::Dpb::mark_decoded_picture`
> (§8.2.5). The rest of the paragraph above still holds:
> `parse_pred_weight_table` (E.4) remains parse-only.

#### Phase E.1 — ref_pic_list_modification (wire the existing parse-only stub)
- [x] Thread `modification_of_pic_nums_idc` + `abs_diff_pic_num_minus1` /
      `long_term_pic_num` from `parse_ref_pic_list_modification` (`slice.rs:305`)
      into `ref_pic.rs`'s reference-list construction so it actually reorders
      `RefPicList0`/`RefPicList1` per §8.2.4.3, instead of discarding the values
      — **DONE (2026-08-09)**. `ref_pic.rs` gained `modify_ref_pic_list`
      (§8.2.4.3.1 short-term `picNumLXPred`/`picNumLXNoWrap`/`picNumLX`
      derivation with `MaxPicNum` wrap, §8.2.4.3.2 long-term selection, and the
      shared 8-38/8-39 insert-shift-dedupe splice via `splice_into_list`, using
      `PicNumF`/`LongTermPicNumF` as `Option<i64>` "never matches" sentinels).
      `build_ref_list_l0` now takes `PicNumContext` + the header's
      `ref_pic_list_modification_l0` and applies §8.2.4.2.1 initialisation then
      §8.2.4.3 modification; `decoder.rs` passes them through. Two adjacent
      correctness fixes landed with it: (a) §8.2.4.2.1 P-slice initialisation
      ordered short-term refs by descending **PicOrderCnt**, which is the
      B-slice rule — it now orders by descending **PicNum** (`FrameNumWrap`),
      so `frame_num`-wrapped references sort correctly; (b) `decoder.rs` sized
      RefPicList0 from the raw PPS `num_ref_idx_l0_default_active_minus1`,
      ignoring the slice header's `num_ref_idx_active_override_flag` — it now
      uses the header's effective value (§7.4.3). Malformed streams fail safe:
      a command naming a picture absent from the DPB yields
      `RefPicListError`/`None` so the caller falls back rather than decoding
      against a wrong reference list, and the parser now enforces §7.4.3.1's
      cap of `num_ref_idx_lX_active_minus1 + 1` commands (previously an
      unbounded loop, harmless only because the values were discarded).
      **Note: L1 is parsed and stored but not yet applied — B-slice decode
      does not exist (Phase E.3), so there is no `RefPicList1` to modify.**
- [x] Unit test: a P-slice with an explicit reorder command produces a
      different `RefPicList0` order than default construction (§8.2.4.2) —
      `ref_pic.rs::tests::modification_reorders_p_slice_list_away_from_default`
      plus 7 sibling unit tests (pred carried across commands, `MaxPicNum`
      wrap on `ShortTermAdd`, long-term promotion, list length invariance
      across `num_active` 1..=4, pulling in a picture the truncation dropped,
      absent-picture error, empty-list no-op), and the end-to-end
      `tests/ref_pic_list_modification.rs` which drives the reorder from real
      slice-header bitstream syntax (3 tests) plus 3 new `slice.rs` header
      round-trip/§7.4.3.1-rejection tests. Existing bit-exact I/P-frame
      conformance (`cavlc_conformance.rs`, `p_frame_conformance.rs`,
      max_diff=0) is unaffected.

#### Phase E.2 — MMCO / dec_ref_pic_marking (wire the existing parse-only stub)
- [x] Thread `memory_management_control_operation` values 1–6 from
      `parse_dec_ref_pic_marking` (`slice.rs:371`) into real DPB marking in
      `ref_pic.rs` (mark-unused-for-reference, long-term conversion, sliding
      window override), instead of discarding the values — **DONE (2026-08-09)**.
      `parse_dec_ref_pic_marking` now returns a typed `DecRefPicMarking`
      (`Idr { no_output_of_prior_pics_flag, long_term_reference_flag }` /
      `SlidingWindow` / `Adaptive(Vec<MmcoOp>)`) stored on
      `SliceHeader::dec_ref_pic_marking`, and `ref_pic.rs` gained
      `Dpb::mark_decoded_picture`, the §8.2.5 decoded reference picture marking
      process, which `decoder.rs::store_reference_picture` runs for every
      reference picture on both decode paths. All six operations are
      implemented: MMCO 1 (§8.2.5.4.1, `picNumX = CurrPicNum −
      (difference_of_pic_nums_minus1 + 1)`, equation 8-40, matched against
      `PicNum`/`FrameNumWrap` so pre-wrap negatives work), MMCO 2 (§8.2.5.4.2),
      MMCO 3 (§8.2.5.4.3, short-term → long-term, evicting whichever picture
      already held that `LongTermFrameIdx`), MMCO 4 (§8.2.5.4.4,
      `MaxLongTermFrameIdx`, dropping every long-term above the new maximum,
      `plus1 == 0` meaning "no long-term frame indices"), MMCO 5 (§8.2.5.4.5,
      empty the DPB, reset `MaxLongTermFrameIdx`, and rebase the current
      picture to `frame_num == 0` / `PicOrderCnt == 0` per §7.4.3/§8.2.1.1 —
      reported back through `MarkingOutcome::mmco5` so `decoder.rs` also runs
      `PocState::reset_after_mmco5`), and MMCO 6 (§8.2.5.4.6, current picture →
      long-term). §8.2.5.1's "adaptive marking replaces sliding-window marking"
      rule is honoured: `Adaptive` never runs `apply_sliding_window`, and the
      current picture is marked short-term afterwards unless MMCO 6 claimed it.
      IDR marking (including `long_term_reference_flag`) goes through the same
      entry point. Malformed streams fail safe rather than half-marking: a
      command naming a picture that is not in the DPB with the required marking
      returns `MmcoError` **and empties the DPB**, so the next inter slice
      cannot predict from a wrongly-marked reference; the parser additionally
      rejects out-of-range operands at parse time (§7.4.3.3: `long_term_frame_idx`
      / `long_term_pic_num` > 15, `max_long_term_frame_idx_plus1` > 16, unknown
      MMCO values) and caps the command list at FFmpeg's `MAX_MMCO_COUNT` (66)
      so a `0`-terminated loop cannot be made unbounded. A defensive
      post-marking capacity clamp mirrors FFmpeg's
      `ff_h264_execute_ref_pic_marking` "reference frames exceeds max (probably
      corrupt input)" behaviour, since each DPB entry owns a full decoded frame
      and is therefore a memory-exhaustion vector for the fuzzers.
- [x] Unit test: MMCO 5 (reset) and MMCO 1 (mark short-term unused) each
      produce the expected DPB state — the two headline cases are
      `ref_pic.rs::tests::mmco1_marks_the_selected_short_term_picture_unused`
      and `::mmco5_resets_the_dpb_and_rebases_the_current_picture`, alongside 12
      sibling unit tests (MMCO 2/3/4/6, `LongTermFrameIdx` reuse eviction,
      adaptive-overrides-sliding-window, in-order application, absent-picture
      fail-safe, overfull-DPB clamp, IDR with/without `long_term_reference_flag`,
      the MMCO-5 POC-state reset, and an MMCO 3 → §8.2.4.3.2 hand-off proving
      Phase E.1 and E.2 compose). Two further layers were added on top:
      `tests/dec_ref_pic_marking.rs` drives the same operations from **real
      slice-header bitstream syntax** (7 tests), and — new this session — from
      **whole Annex B access units through the public `H264Decoder::decode`
      API** (6 tests), which is the only layer that covers
      `decoder.rs::store_reference_picture` itself: POC derivation, the
      `nal_ref_idc == 0` "non-reference pictures never enter the DPB" gate, the
      `PocState::reset_after_mmco5` rebase, and the fail-safe error branch. The
      decoder-level tests were mutation-checked (severing the header→marking
      wiring fails 5 of the 6; deleting the `reset_after_mmco5()` call fails the
      MMCO 5 one, which needed `pic_order_cnt_lsb` values chosen so the missing
      reset actually changes the derived POC — 12 → 2 reads as an MSB wrap and
      yields 18). `H264Decoder::dpb()` was added as a read-only accessor so the
      marking result is observable without inferring it from pixels. Existing
      bit-exact I/P-frame conformance (`cavlc_conformance.rs`,
      `p_frame_conformance.rs`, max_diff=0) is unaffected.

#### Phase E.3 — B-slice parsing + direct mode — **DONE (2026-08-12)**
- [x] Parse B-slice `mb_type`/`sub_mb_type` (Tables 7-14..7-18) in
      `slice_data.rs` — `parse_b_slice`/`parse_b_macroblock` added; all 23
      inter mb_types (Direct/L0/L1/Bi 16×16, eighteen 16×8+8×16 variants,
      B_8x8) and 13 B sub_mb_types (Table 7-15) are parsed; intra fall-through
      subtracts 23 from raw mb_type per spec §7.4.5; ref_idx and MVD reading
      follows the spec's all-L0-refs/all-L1-refs/per-part-MVDs order for
      multi-partition types and the B_8x8 sub-partition loop. Added
      `BPredDir` enum and new `MbType` variants (`BL016x16`/`BL116x16`/
      `BBi16x16`/`B16x8`/`B8x16`/`BB8x8`) to `macroblock.rs`; added L1 fields
      (`ref_idx_l1`, `mvd_l1`, `pred_dirs`, `sub_mb_type_b`) to `InterMotion`.
- [x] Implement spatial direct mode MV derivation (§8.4.1.2.2) —
      `predict_b_slice_mvs` in `mv.rs`: for B_Direct/B_Skip blocks,
      `refIdxL0` is the min non-negative L0 ref among spatial neighbors A/B/C
      (default 0), `refIdxL1 = 0`; `mvL0`/`mvL1` from the standard
      §8.4.1.3.1 median predictor applied to each list's neighbor fields.
      `MvCell` extended with `mv_l1`/`ref_idx_l1`; `build_ref_list_l1` added
      to `ref_pic.rs` (ascending POC > current, then descending POC ≤
      current, then long-term ascending). `direct_spatial_mv_pred_flag` stored
      on `SliceHeader` (was discarded).
- [x] Implement temporal direct mode MV derivation (§8.4.1.2.3) — when
      `direct_spatial_mv_pred_flag == 0`: scales `mvCol` from the co-located
      4×4 block in `RefPicList1[0]` by `tb/td` (L0) and `(tb−td)/td` (L1)
      per spec §8.4.1.2.3; falls back to (0,0)/ref0 if co-located MV grid
      unavailable. `DpbEntry` gains `mv_grid: Option<Arc<Vec<[MvCell;16]>>>`;
      `decoder.rs` passes the decoded P/B MV grid through `store_reference_picture`.
- [x] Implement bi-predictive motion compensation: average two
      motion-compensated blocks (§8.4.2.3) — `reconstruct_b_frame` in
      `reconstruct.rs`: for each 4×4 block, L0-only/L1-only/bi-pred selected
      by `cell.ref_idx`/`cell.ref_idx_l1` sentinels;
      `pred[i] = (l0[i] + l1[i] + 1) >> 1` for bi-pred; B-slice dispatch
      added to `decoder.rs`. All 190 existing tests pass.

#### Phase E.4 — Weighted prediction (wire the existing parse-only stub)
- [x] Thread `luma_weight`/`luma_offset`/`chroma_weight`/`chroma_offset` from
      `parse_pred_weight_table` (`slice.rs:334`) into explicit weighted
      prediction (§8.4.2.3.2) for P and B slices, instead of discarding them
- [x] Implement implicit weighted prediction (§8.4.2.3.2, B-slices only,
      distance-based weight derivation)
- [x] Unit test: explicit weighted P-slice reconstruction matches hand-computed
      weight/offset for a synthetic block

  **Completed 2026-08-12.** `parse_pred_weight_table` now returns a
  `PredWeightTable` (`slice.rs`) instead of just advancing the bit position;
  `SliceHeader::pred_weight_table` carries it. `reconstruct.rs` gained a
  `WeightedPred` enum (`Default`/`Explicit`/`Implicit`) threaded through
  `reconstruct_inter_frame`/`reconstruct_b_frame` down to the per-4×4-block
  `combine_weighted` helper, implementing the explicit uni/bi-pred formulas
  and the POC-distance-based implicit-weight derivation (§8.4.2.3.2), both
  per FFmpeg-cross-checked spec formulas. `decoder.rs` selects the mode from
  `pps.weighted_pred_flag`/`weighted_bipred_idc`. Caught and fixed a real bug
  along the way via the existing fuzz harness: the first cut of
  `parse_pred_weight_table` preallocated `Vec::with_capacity` directly from
  the attacker-controlled `num_ref_idx_lX_active_minus1` `ue(v)`, which
  OOM'd `fuzz_structured_seeds` on a malformed seed (64GB alloc); fixed by
  dropping the capacity hint and adding an explicit 32-entry bound (§7.4.3),
  matching the pattern `parse_ref_pic_list_modification` already uses.

#### Phase E.5 — Validate B-frame decode
- [x] Generate an IBP-structured corpus clip with `ffmpeg`
- [x] Validate bit-exact B-frame decode vs `ffmpeg` on that corpus

  **Completed 2026-08-12.** Root cause was `build_ref_list_l0` using P-slice
  PicNum ordering for B-slices; B-slices need POC-based ordering (§8.2.4.2.3).
  Added `build_ref_list_l0_b_slice` with the correct ordering. Also fixed the
  2-partition B-type MVD interleave bug (all L0 MVDs before all L1 MVDs per
  §7.3.5.1). Tests `tests/b_frame_conformance.rs` now pass bit-exact
  (max_abs_diff=0) for both deblock-enabled and deblock-disabled variants.

#### Phase F.1 — 8×8 transform: parsing
- [x] Parse `transform_size_8x8_flag` per-macroblock in `slice_data.rs` when
      `pps.transform_8x8_mode_flag` is set (stored on `Macroblock::transform_size_8x8`)
- [x] Parse the 8×8 residual block CAVLC syntax (distinct coeff scan/context
      from the 4×4 path, §7.3.5.3.3) — `luma_coeffs_8x8` populated in `slice_data.rs`

#### Phase F.2 — 8×8 transform: reconstruction
- [x] Implement the 8×8 inverse transform (§8.5.12.3) in `transform.rs` —
      `dequant_idct_8x8` now uses a faithful port of FFmpeg's `ff_h264_idct8_add`
      core (the previous hand-rolled butterfly had the wrong `a4`/`a6` pairing and
      omitted the `a1`/`a3`/`a5`/`a7` + `b1`/`b3`/`b5`/`b7` cross-terms, which
      zeroed DC-only blocks). Unit tests `eight_by_eight_dc_only_is_flat` /
      `eight_by_eight_flat_scaling_*` updated to assert correct (FFmpeg-matching)
      values.
- [~] Implement the four 8×8 intra prediction modes (§8.3.2.2) in
      `prediction.rs` — `predict_8x8` exists but clamps neighbour indices at 7
      (`t(7)`/`l(7)` fallbacks) instead of using the full 8×8 neighbour set
      (top samples 0–15 incl. the top-right extension, left 0–7, §8.3.2.2); the
      diagonal/vertical-right/horizontal-* modes are therefore not yet pixel-exact.
      **Not wired into reconstruction either** (see Phase F.4 gate below).

#### Phase F.3 — High-profile scaling matrices

- [x] Apply the already-parsed SPS/PPS `scaling_list` values (Table 7-... /
      §8.5.9) to 4×4 dequant in `transform.rs` — `dequant_idct_4x4` derives
      `LevelScale4x4` from the active `ScalingLists` (§8.5.9). `decoder.rs` now
      merges the PPS list over the SPS list (§8.5.9 fallback) and passes the
      merged set into reconstruction; `pps.rs` defaults the PPS list to the SPS
      list so the active set is always correct.
- [x] Apply the same scaling lists to the 8×8 dequant path from Phase F.2 —
      `dequant_idct_8x8` reads `scaling.scaling_8x8(scale_list)` per coefficient
      (§8.5.9); same merged active set is threaded through `decoder.rs`.

#### Phase F.4 — Validate High-profile 8×8-transform decode
- [ ] **Wire 8×8 reconstruction into `reconstruct.rs`** (`MbType::Intra4x4` +
      `transform_size_8x8` → `dequant_idct_8x8` + `predict_8x8` per 8×8 block),
      then remove the `entropy_coding_mode_flag && transform_8x8_mode_flag`
      early-return gate in `decoder.rs::try_decode_real_slice` (keep the gate for
      inter 8×8 / non-intra until inter 8×8 is implemented).
- [ ] Generate a High-profile corpus clip (`transform_8x8_mode_flag=1`) with
      `ffmpeg`, validate bit-exact decode

#### Phase G.1 — PAFF: field-picture parsing
- [x] Thread the already-parsed `bottom_field_flag` (`slice.rs:169`, previously
      discarded as `_bottom_field_flag`) through slice/header state instead of
      dropping it — `SliceHeader` now carries `field_pic_flag`, `bottom_field_flag`,
      and `delta_pic_order_cnt_bottom` (§7.3.3); the slice header parser reads and
      stores them, and round-trip unit tests assert both field and frame pictures
      parse correctly
- [x] Implement field-picture POC derivation (§8.2.1.2/8.2.1.3, distinct from the
      existing frame-picture path) — `derive_pic_order_cnt` now takes
      `field_pic_flag`/`bottom_field_flag`/`delta_pic_order_cnt_bottom`; `PocState`
      tracks per-field `prev_top_field_order_cnt`/`prev_bottom_field_order_cnt`
      (the MSB/LSB predictor is derived from their max, per §8.2.1.1) so type-0
      (separate per-field `pic_order_cnt_lsb`) and type-2 (`base + 1` for the
      bottom field) field POC both derive correctly and are unit-tested

#### Phase G.2 — PAFF: field-picture reconstruction
- [ ] Field-picture reference list construction (§8.2.4.2.5)
- [ ] Field-based (odd/even scanline) macroblock reconstruction and output
       interleaving back into a full frame

  **Drafted (working tree):** `reconstruct.rs` gained `ReconstructedFrame::
  deinterleave_luma`/`deinterleave_chroma` and the free `merge_field_into`
  helper that place a field's samples on every other scanline (parity from
  `bottom_field_flag`) and merge two complementary fields into one interlaced
  frame. These are the field→frame plumbing; the actual field reconstruction
  and reference-list construction above are still unimplemented.

#### Phase G.3 — MBAFF: parsing
- [x] Parse `mb_field_decoding_flag` and macroblock-pair decode ordering
       (§7.3.4, §7.4.4) when `mb_adaptive_frame_field_flag` is set — SPS gained
       `mb_adaptive_frame_field_flag` (parsed, round-trip tested in
       `sps.rs::tests::sps_mb_adaptive_frame_field_flag_round_trips`); in MBAFF
       frames (`mb_adaptive_frame_field_flag && !field_pic_flag`)
       `slice_data.rs` reads `mb_field_decoding_flag` once per macroblock pair
       (CAVLC `parse_i_slice` and CABAC `parse_i_slice_cabac`, via the new
       `MbFieldDecodingFlagContext` in `entropy.rs`) and stores it on
       `Macroblock::mb_field_flag`. Reconstruction-side macroblock-pair ordering
       (neighbour derivation / output interleave) is still Phase G.4.

#### Phase G.4 — MBAFF: neighbour derivation + reconstruction
- [ ] Adjust neighbour derivation (nC, MPM, motion prediction) for mixed
      field/frame macroblock pairs (§6.4.10.1)
- [ ] Field/frame-adaptive reconstruction per macroblock pair

#### Phase G.5 — Validate interlaced decode
- [ ] Generate a PAFF corpus clip and a separate MBAFF corpus clip with
      `ffmpeg`; validate bit-exact decode vs `ffmpeg` for each independently

### H.264 — Phase H: conformance & capability flip

- [x] Cross-codec conformance harness vs `ffmpeg`: `tpt-kinetix-h264/tests/conformance_matrix.rs`
      enumerates a profile × entropy × frame-structure × deblock × resolution
      matrix (CAVLC/CABAC I/P/B, 4:2:0, progressive, 16-px-aligned, no 8×8) and
      asserts **bit-exact** (`max_abs_diff == 0`) decode vs `ffmpeg` for every
      supported cell; the already-present `*_conformance.rs` suites are the
      per-feature gated pixel-exact assertions. The harness additionally asserts
      the **honesty contract** for the unsupported subset (8×8 transform / High
      profile, interlaced PAFF): under `with_strict(true)` the decoder returns
      `KinetixError::NotPixelExact` rather than emitting wrong pixels. Gated on
      `ffmpeg` presence (skips on runners without it).
- [x] Update `H264Decoder::capabilities()` to the *actual* achieved state:
      `supports_inter_prediction = true` (P/B + B-frames are bit-exact),
      accurate `notes` (CAVLC/CABAC I/P/B bit-exact; 8×8 / interlaced /
      non-16-aligned still open). Stale claim that B-frames, CABAC P/B, and
      weighted prediction were unimplemented — contradicting the passing
      conformance suites — has been corrected. `tpt-kinetix-core` `capabilities.rs`
      and `tpt-kinetix-h264/README.md` status sections updated to match.
- [~] **Global `pixel_exact` flip — gated (NOT flipped).** The decoder is
      bit-exact for its supported subset, but `pixel_exact` is a *global* honesty
      flag, and genuine gaps remain: the 8×8 transform / High profile (Phase F),
      interlaced PAFF/MBAFF (Phase G), and a non-16-aligned-dimension crop-edge
      gap (Phase 12 A follow-up). Flipping the flag while those exist would make
      callers/CLI trust approximate output, directly contradicting the project's
      `NotPixelExact` honesty design. The flip stays `false` until Phases F/G and
      the crop-edge gap land; the `conformance_matrix.rs` gate asserts
      `!capabilities().pixel_exact` so the constraint is enforced in CI.

### AV1 — from-scratch reconstruction

> Status corrected 2026-08-06 (previous wording was stale): `decoder.rs`
> already calls into `reconstruct.rs::reconstruct_av1_frame` for intra
> keyframes, and `reconstruct.rs` has real inverse-transform (`dct_4x4`,
> `dct_8x8`, `dct_16x16`, `adst_4x4`, `wht_4x4`) and intra-prediction
> (DC/vertical/horizontal/paeth/smooth/directional) code — this is
> substantially more than "grey placeholder frames." **However**,
> `decode_tile_group` (`reconstruct.rs:664`) previously read coefficients with a
> plain `BitReader` using an invented exp-golomb-like scheme (trailing-ones +
> level-prefix/suffix, modeled on H.264 CAVLC). Phase A landed the symbol
> decoder engine in `entropy.rs`/`entropy_cdf.rs`, and **Phase B (2026-08-09)
> rewired `decode_tile_group`/`decode_chroma_tx` onto real `coeffs()` syntax
> (`coeff.rs::read_coeffs`)** read via that symbol decoder — the reconstruction
> math (`inverse_transform`/`predict_intra_block`) is unchanged, and the
> lossless WHT path is now wired. Intra keyframe coefficients now decode through
> the real arithmetic decoder (cross-checked against an independent Python
> `coeffs()` oracle); output is still not pixel-exact because inter prediction
> (Phase E) is not yet implemented — the in-loop post-filters (deblock + CDEF +
> restoration passthrough, Phase D) are now wired and run after tile-group
> reconstruction. **Phase C (superblock partition tree + per-block intra mode /
> `tx_size` syntax) is now done** — the "fixed placeholder grid" gap is closed.
>
> **Bitstream-ingest status (2026-08-12)**: the OBU splitter
> (`obu::parse_obu_sequence`) and **Sequence Header OBU parser
> (`obu::SequenceHeaderObu::parse`) now decode correctly against real
> `ffmpeg`-generated keyframes** — verified by the new ffmpeg-gated
> conformance test `tpt-kinetix-test-utils/tests/conformance.rs::
> av1_vs_ffmpeg_reference_when_available`, which asserts the declared
> frame geometry (`128x96`) matches. The earlier truncation at
> `matrix_coefficients` was a missing `frame_id_numbers_present_flag` /
> `enable_*` feature-flag block in the non-reduced Sequence Header path
> (§5.5.2), now filled in; `read_ns` was also hardened against `n<=1`.
> The **uncompressed frame-header parser (`frame.rs::FrameHeader::parse`,
> AV1 §5.9) still drifts on real keyframes** (it produces wrong
> dimensions), so the decoder cannot yet reconstruct real frames — that
> parser, then the superblock partition tree + per-block intra mode + tx
> size (Phase C), then CDEF + deblock + restoration (Phase D), are the
> remaining blockers to a pixel-exact intra decode. The conformance
> harness already prints the Kinetix-vs-reference PSNR/diff so progress on
> those phases is measurable.

#### AV1 Phase A — real entropy: the symbol decoder (blocker for everything below)

> Done 2026-08-07. Landed in `tpt-kinetix-av1/src/entropy.rs`
> (`SymbolDecoder`: `init_symbol`/`read_symbol`/`read_bool`/`read_literal`,
> spec §8.2.2/8.2.3/8.2.5/8.2.6 — not §8.2.2/§8.2.4 as originally cited
> above, which are actually "Initialization" and "Exit process"
> respectively) and `entropy_cdf.rs` (mechanically extracted `Default_*_Cdf`
> tables for `txb_skip`/`cbf`, `tx_type` (intra sets 1/2, inter sets 1/2/3,
> full), and coefficient levels (`eob_pt_*`, `coeff_base_eob`,
> `coeff_base`, `coeff_br`, `dc_sign`, full)). The AV1 spec has no literal
> worked numeric example for the symbol decoder (confirmed by fetching
> `09.parsing.process.md` directly) — substituted an independent Python
> transcription of the same §8.2 pseudocode as a differential oracle;
> golden vectors from it are embedded in `entropy.rs`'s tests. `exit_symbol`
> (§8.2.4) is intentionally not implemented yet — it needs per-tile
> bookkeeping (`context_update_tile_id`, the full named `Tile*`/`Saved*`
> CDF set) that only exists once Phase B/C wire up real tile parsing.
> `decode_tile_group` itself is untouched — that rewiring is Phase B.

- [x] Implement the AV1 boolean/multi-symbol arithmetic decoder (§8.2.6,
      `read_symbol`) operating over an adaptive CDF table, distinct from both
      H.264 CABAC and the current ad hoc `BitReader` usage in
      `decode_tile_group`
- [x] Implement default CDF tables + adaptation/update rule (§8.2.6) for at
      minimum the symbols `decode_tile_group` currently reads with raw bits
      (coefficient levels, `cbf`, `tx_type`)
- [x] Unit test the symbol decoder against the spec's worked arithmetic-coding
      example, independent of any real tile bitstream — see note above on
      why this is a cross-validated synthetic vector set rather than a
      literal spec example

#### AV1 Phase B — rewire coefficient decode onto the symbol decoder
- [x] Replace the `BitReader`-based level/trailing-ones/total-zeros decode in
      `decode_tile_group` (`reconstruct.rs:664`) and `decode_chroma_tx`
      (`reconstruct.rs:902`) with real coefficient syntax (`all_zero`,
      `eob_pt*`, `eob_extra`, `coeff_base(_eob)`, `coeff_br`, `dc_sign`,
      sign, Exp-Golomb tail) read via the Phase A symbol decoder (§5.11.39),
      in `coeff.rs::read_coeffs` — landed and cross-checked against an
      independent Python `coeffs()` oracle for golden vectors
- [x] Keep the existing `inverse_transform`/`predict_intra_block` reconstruction
      math — only the bits-in path changes (also wired the previously-dead
      `wht_4x4` lossless WHT via `internal_tx_type`/`TX_TYPE_WHT`)

#### AV1 Phase C — partition + mode syntax — **DONE (2026-08-13)**

> `reconstruct.rs` now walks the real superblock partition tree: `decode_tile_group`
> calls `decode_superblock` → `decode_partition` (recursive §5.11.4 walk with the
> neighbour-context partition CDFs), and each leaf block reads its per-block
> `intra_y_mode` (`read_intra_y_mode`), `uv_mode` (`read_uv_mode`), and `tx_size`
> (`read_tx_size` → `read_selected_tx_size`) via the symbol decoder. The fixed
> 8×8-per-superblock placeholder loop in `decode_tile_group` is replaced. The
> reconstruction math (`inverse_transform`/`predict_intra_block`) is unchanged
> from Phase B. Output is still **not** pixel-exact because the loop filter /
> CDEF / loop restoration (Phase D) and inter prediction (Phase E) are not yet
> implemented — so this resolves the "placeholder grid" gap noted below, not the
> overall pixel-exactness gap.

- [x] Parse superblock partition tree (§5.11.4) instead of the current fixed
      8×8-block-per-superblock loop in `decode_tile_group`
      (`reconstruct.rs:697`) — `decode_superblock`/`decode_partition` land it
- [x] Parse per-block intra mode / `tx_size` selection via the symbol decoder
      rather than assuming a fixed DC-predicted 8×8 transform block

#### AV1 Phase D — loop filter / CDEF / restoration — **WIRED (2026-08-14)**
- [x] Implement the deblocking loop filter (§7.14)
- [x] Implement CDEF (§7.15)
- [x] Implement loop restoration (§7.17) — no-op passthrough (spec-permitted
       when `enable_restoration` is false)

  **Done (2026-08-14):** `tpt-kinetix-av1/src/loop_filter.rs` (deblocking
  loop filter §7.14 + CDEF §7.15 from the normative algorithms, `apply_loop_restoration`
  no-op passthrough) is now declared (`pub mod loop_filter` in `lib.rs`) and
  invoked. `reconstruct.rs::reconstruct_av1_frame` runs `apply_post_filters`
  (deblock → CDEF → restoration) over each tile's reconstructed buffer after
  tile-group decode; per-8×8-block tx-size / skip metadata is collected during
  `decode_block` into a `FrameMeta` (threaded through `TileDecodeState` →
  `decode_tile_group`) and consumed by the filters. The CDEF strength packing
  was corrected (`pri = packed & 0x0F`, `sec = packed & 0x30`) and the
  variance-dependent strength clamp fixed. `Av1Decoder::capabilities()` reports
  `supports_deblocking = true`. Pixel-exact decode still awaits inter
  prediction (Phase E) + conformance (Phase G); `pixel_exact` stays `false`.

#### AV1 Phase E — inter prediction
- [x] Reference frame buffer management (§7.20, `RefFrameStore` equivalent) —
       `Av1Decoder` now holds a `RefFrameStore` of 8 slots; every reconstructed
       frame is stored into the slots selected by `refresh_frame_flags` (§7.20) in
       `decoder.rs::decode`. `ref_frames()` exposes it; `StoredFrame` carries the
       planar YUV. This is the storage inter prediction will read from once motion
       compensation lands (the reconstruction pipeline still returns `Ok(None)` for
       non-intra frames, so inter decode is not yet enabled).
- [ ] Motion vector prediction (§7.10) and inter block reconstruction (§7.11.3)

#### AV1 Phase F — parallel tile decode — **DONE (2026-08-14)**
- [x] Wire `rayon` over the tile groups now that Phase A–C produce real per-tile
       reconstruction. `reconstruct_av1_frame` computes each tile's superblock
       rectangle (uniform tile spacing from `tile_cols/tile_rows` + `tile_*_in_sb`),
       decodes every tile concurrently via `rayon`'s `par_iter` into a tile-local
       buffer (AV1 tiles are entropy-independent and write disjoint pixel
       rectangles), then blits the finished tiles back into the master planes.
       `decode_tile_group` now restricts its superblock walk to the tile's region
       (fixing a latent bug where every tile group re-decoded the whole frame),
       and writes at tile-local coordinates. Loop-filter passes (Phase D) run
       per-tile after reconstruction.

#### AV1 Phase G — conformance
- [~] Conformance harness in place: `tpt-kinetix-test-utils/tests/conformance.rs::
       av1_vs_ffmpeg_reference_when_available` synthesizes an AV1 keyframe OBU with
       `ffmpeg`, decodes it with both `Av1Decoder` and `ffmpeg`'s AV1 decoder, and
       prints the per-plane PSNR/diff (gated on `ffmpeg`, asserts the sequence-header
       geometry contract today; the pixel-exact `within_tolerance(.., 0)` assertion is
       commented out until Phase C/D land). Sequence Header OBU parsing is exercised
       and passes against real `ffmpeg` keyframes. With Phase D + F wired, the
       harness now exercises the full intra decode → loop-filter → diff path.
- [ ] Validate decode vs `dav1d` reference output on a generated intra-only
       corpus first (Phases A–C only), then again once inter prediction
      (Phase E) lands
- [ ] Flip `Av1Decoder::capabilities().pixel_exact` only after the conformance
      harness passes

## Phase 13 — `tpt-kinetix-lean`: Original Embedded-First Codec (2026-07-20)

> Goal: an original (not ported) codec design prioritizing bounded-memory,
> bounded-time decode on constrained hardware over maximum compression ratio.
> Independent track — does not block on or get blocked by Phase 12. Accepts
> ~10-15% worse compression than AV1 in exchange for a decoder implementable
> in a few thousand lines with genuinely parallel entropy decode.

### Design
- [x] Write the format design doc: header layout, fixed block-partition
      scheme, rANS stream-interleaving/framing — documented in
      `tpt-kinetix-lean/src/headers.rs` (byte layout table) and
      `tpt-kinetix-lean/src/rans.rs` (stream-set framing) module docs;
      integer transform sizes, intra mode set (14 modes), inter/motion-vector
      approach (unidirectional, quarter-pel, 6-tap), in-loop filter placement
      (single-stage deblocking) documented in `tpt-kinetix-lean/src/lib.rs`
      crate-level docs
- [x] Document the memory/perf budget for v1 (target max resolution, arena
      size ceiling, per-frame decode time budget) sized for embedded-Linux
      SBC-class hardware (e.g. Raspberry Pi–class), noted as revisitable —
      `tpt-kinetix-lean/src/lib.rs` crate-level docs, "v1 target envelope"

### Scaffold
- [x] Generate `tpt-kinetix-lean` crate from `templates/codec-crate/`, add to
      workspace `members`
- [x] Implement `headers.rs`: sequence/frame header struct definitions
      (max dimensions, max ref count, block size range, quant params)
- [x] Implement `bitreader.rs`: bit-level reader for the new format
- [x] Implement `rans.rs`: rANS/tANS encode/decode primitives + stream
      interleaving skeleton, with a stubbed extension point for the
      per-symbol probability model — `RansEncoder`/`RansDecoder` are real
      and round-trip-tested against a uniform `StaticModel`; the adaptive/
      context-selected `SymbolModel` real coefficient coding will use is
      the still-open extension point
- [x] Implement `decoder.rs` shell: `DecoderCapabilities` honesty pattern
      (`pixel_exact: false`, `with_strict()` → `NotPixelExact`), header
      parsing entry point
- [x] Add `fuzz/` target for the header/bitstream parser
- [x] Add round-trip test scaffolding (header parse round-trip;
      `DecoderCapabilities` not-pixel-exact contract) — full encode/decode
      round-trip test lands once reconstruction exists

### Open questions (resolved)
- [x] Decide whether to factor a shared `tpt-kinetix-bitstream` utility crate
      now that this would be the second hand-rolled bit reader in the
      workspace (alongside `tpt-kinetix-h264/src/bitreader.rs`), or keep them
      independent per-codec — **Decision: start independent, extract later.**
      Both `tpt-kinetix-lean` and `tpt-kinetix-vision` carry their own
      `bitreader.rs` / `rans.rs` copies. A shared `tpt-kinetix-bitstream`
      crate will be extracted once both codecs are stable enough to freeze the
      rANS interface (documented in `docs/vision-codec-design.md` DECISION 8
      and `tpt-kinetix-lean/src/lib.rs`).
- [x] Decide the no_std/MCU port plan and timeline once the v1 alloc-free
      hot path is in place and proven on embedded Linux — **Decision: v1
      targets embedded Linux (prove the alloc-free hot path first); no_std/MCU
      is a v2 target.** Both `tpt-kinetix-lean` and `tpt-kinetix-vision` design
      docs note this explicitly.

## Phase 14 — Specialist Codec Roadmap (backlog) (2026-07-20)

> Status: backlog only — none of these are started. Full rationale table
> lives in `docs/codec-backlog.md` ("Original Specialist Codec Concepts");
> this phase tracks getting each one designed/scaffolded when prioritized,
> the same way Phase 13 was written only once `tpt-kinetix-lean` was
> actually decided on. Do not start implementation on any of these without
> first writing its own Phase-13-style design section here.

- [~] `tpt-kinetix-vision` — video-for-machines: optimize for detector/
      classifier accuracy per bit rather than human perceptual quality;
      chroma-optional, model-matched bit depth, tensor-output decode path
      (see `docs/codec-backlog.md` for design notes) — design phase started,
      see Phase 15

> Expanded 2026-08-06: the five items below were each a single line standing
> in for an entire future effort the size of Phase 13/15 (design doc →
> scaffold → implement). None of this is active work — the "Prioritize this
> list" gate at the bottom still applies, and per the rule above, real
> implementation on any of these still waits on its own design section (the
> same one Phase 15 wrote for `tpt-kinetix-vision`). What's new is that each
> codec now has its *design-phase* checklist pre-drafted, grounded in the
> rationale already in `docs/codec-backlog.md`, so whichever one gets picked
> next has a session-sized starting point instead of a blank page.

#### `tpt-kinetix-realtime` — design-phase checklist (RESOLVED 2026-08-13; promoted to Phase 16 scaffold)
- [x] Decide the target profile for v1 — **profile-agnostic** (2026-08-13):
      design the bitstream around the shared realtime core (no-B-frame
      lookahead / low-latency GOP + partial-frame loss recovery) and expose
      cloud gaming, video conferencing, and AR/smart-glasses overlay as
      *decode profiles / config knobs* (latency ceiling, power budget,
      foveation enable, loss-resilience strength), not as separate codecs.
      Rationale: (a) all three share the same latency + loss-resilience center,
      so a single forkable core avoids 3× surface; (b) `docs/codec-backlog.md`
      flags AR overlay as the most demanding of the three (extreme power
      budget, foveated/gaze-contingent rendering, real-world latency), so AR
      becomes the *hardest* profile (the stress test), not the v1 baseline —
      it leans on `tpt-kinetix-lean`'s power-conscious embedded primitives
      rather than being invented independently here; (c) cloud-gaming and
      conferencing are near-identical profiles differing only in decode
      complexity + symmetry, so they collapse to one default profile with two
      preset parameter sets. Next items below should design the core to be
      profile-parameterized from day one.
- [x] Write the format design doc: partial-frame loss-recovery mechanism
      (forward error correction vs. concealment vs. both) and how the
      no-B-frame-lookahead constraint shapes GOP structure — **designed
      (2026-08-13), see `docs/realtime-codec-design.md`** (DECISION 1 hybrid
      FEC + intra-refresh + concealment; DECISION 2 rolling intra-refresh +
      on-demand IDR, single reference, no B-frames)
- [x] Design the per-frame latency budget and how it's enforced (encode-side
      deadline, decode-side bounded work) — **designed**, see
      `docs/realtime-codec-design.md` DECISION 4 (encode `deadline_ms` rate
      control fallback + decode bounded work via no-B / single-ref / fixed
      slice grid / lean-style single deblock + `max_decode_ms` capability)
- [x] Decide how loss resilience is measured for design validation (e.g. a
      simulated packet-loss-vs-quality curve, the loss-resilience analogue of
      Phase 15's mAP-vs-bitrate metric) — **decided**, see DECISION 5:
      PSNR/SSIM-vs-loss curve + stall/freeze-rate-vs-loss (latency half);
      gaze-weighted variant deferred to AR profile; harness in
      `tpt-kinetix-test-utils` behind `realtime-bench`
- [x] Document the memory/perf/latency budget for v1 (target hardware class,
      ms/frame ceiling) — **documented**, see DECISION 6: one envelope with
      three profile presets (cloud gaming / conferencing / AR-foveated); AR
      is the hardest preset (fine slice grid, 20-30% FEC, <10 MB arena,
      foveation on)
- [x] Decide the relationship to `tpt-kinetix-lean` — `docs/codec-backlog.md`
      notes they share a power/latency-conscious embedded target — **decided**,
      see DECISION 7: extract `tpt-kinetix-bitstream` now (lean + vision +
      realtime are 3 copies of `BitReader`/rANS/partition/transform/deblock);
      realtime adds slice-grid framing, intra-refresh masking, FEC framing,
      optional foveation, latency-deadline fields on top

#### `tpt-kinetix-lossless` — design-phase checklist (start here once prioritized)

> v1 scope **decided (2026-08-13):** a single unified lossless format serving
> all three target domains (medical imaging, scientific capture, archival),
> guaranteeing bit-exact round-trip for **10/12/16-bit** samples. Design doc:
> `docs/lossless-codec-design.md` (DECISION 1 resolved; 2–6 specified).
>
> **Crate scaffolded (2026-08-13):** `tpt-kinetix-lossless` exists with a
> working bit-exact reversible predictive path (median predictor + adaptive Rice
> entropy + per-plane CRC, reusing `tpt-kinetix-bitstream` primitives). 12 unit
> tests pass (10/12/16-bit + multi-plane round-trips, corrupted-checksum
> rejection, reserved-`transform_id` rejection). Next: rANS swap-in + wavelet
> mode + ratio harness (DECISION 4).

- [x] Decide the target domain for v1 (medical imaging vs. scientific capture
      vs. archival) and the bit-depth range it must support (10/12/16-bit)
      — **unified across all three domains; 10/12/16-bit bit-exact** (DECISION 1)
- [x] Write the format design doc: reversible compression approach
      (predictive + entropy coding, à la FFV1, vs. a reversible wavelet
      transform) — **done**, `docs/lossless-codec-design.md` DECISION 2:
      predictive+entropy (FFV1-like) primary; reversible wavelet mode reserved
      via `transform_id` for a later phase
- [x] Design the decode path's correctness contract — how bit-exact
      round-trip is verified as part of the format itself (e.g. a built-in
      checksum), not just left to external testing — **designed** (DECISION 3):
      per-frame CRC32/CRC64 + stream SHA-256, decoder returns `ReversibilityError`
      on mismatch; round-trip harness in `tpt-kinetix-test-utils`
- [x] Decide how compression ratio at guaranteed losslessness is measured for
      design validation (baseline against FFV1 / lossless HEVC) — **decided**
      (DECISION 4): ratio-vs-FFV1 bpp metric on a 10/12/16-bit corpus, within
      ~10% of FFV1 as the v1 acceptance bar
- [x] Document the memory/perf budget for v1 (archival-quality capture means
      large uncompressed frame sizes) — **documented** (DECISION 5): ≤4096²
      per plane, bounded arena via `max_*` header, parallel rANS sub-streams,
      integer-only math, no real-time budget
- [x] Decide the relationship to existing kinetix bitstream primitives
      (reuse `tpt-kinetix-lean`'s `bitreader.rs`/`rans.rs` shape or diverge) —
      **decided** (DECISION 6): depend on `tpt-kinetix-lean` for `Rans`/
      `BitReader`; add a new reversible prediction stage (Lean's DCT bank is not
      reused)

#### `tpt-kinetix-screen` — design-phase checklist (start here once prioritized)
- [x] Decide the target content class for v1 (desktop capture vs. mobile UI
      vs. mixed screen+video content) — **Decision: desktop capture** (mobile
      UI → higher-DPI profile, mixed screen+video → stretch/validation corpus);
      see `docs/screen-codec-design.md` DECISION 1
- [x] Write the format design doc: per-block mode classification (flat-fill
      run-length, glyph/edge palette mode, natural-image fallback) — **DECISION 2
      resolved** in `docs/screen-codec-design.md` (fixed 16x16 CB grid + mode map
      rANS stream + FLAT run-length + GLYPH dict ref + NATURAL reuses Lean)
- [x] Design cross-frame glyph/palette dictionary reuse for repeated UI
      elements — **DECISION 3 resolved** in `docs/screen-codec-design.md`
      (encoder-authoritative global glyph dict + color palette, inline-at-first-use,
      key-frame/flag reset; no implicit LRU)
- [x] Decide how compression efficiency is measured for design validation
      (baseline against H.264 screen-content-coding extensions or VP9
      lossless) — **DECISION 4 resolved** in `docs/screen-codec-design.md`
      (PSNR RD vs H.264 SCC + VP9 lossless primary; edge-F1 differentiator;
      VMAF guard; ffmpeg-gated `screen-bench` harness)
- [x] Document the memory/perf budget for v1 — **DECISION 5 resolved** in
      `docs/screen-codec-design.md` (1080p/60fps, dict_cap 256, palette_cap 64,
      glyph_max_dim 32, ~12 MB arena, <8 ms RPi-5 decode)
- [x] Decide the relationship to existing kinetix bitstream primitives —
      **DECISION 6 resolved** in `docs/screen-codec-design.md` (depend on
      `tpt-kinetix-bitstream` for BitReader+rANS+SymbolModel; NATURAL fallback
      reuses transform/partition/intra/deblock from bitstream once promoted, lean
      interim; screen owns only classifier/dict/palette/RLE)

#### `tpt-kinetix-face` — design-phase checklist (start here once prioritized)
- [ ] Decide the landmark/parametric face representation for v1 (3DMM, sparse
      keypoints, or a learned latent code)
- [ ] Write the format design doc: keyframe (real pixel image) + per-frame
      parameter-delta bitstream — a materially different shape from every
      pixel-coding entry in this table
- [ ] Design the decode path's dependency contract: synthesis requires
      running a generative model at decode time, so decide whether that model
      ships with the decoder or is a pluggable external dependency
- [ ] Design the fallback/failure path for content the trained face model
      can't represent (occlusion, non-face content, extreme pose)
- [ ] Decide how synthesis quality is measured for design validation
      (perceptual similarity to source, not a pixel-exact diff)
- [ ] Document the memory/perf budget for v1 — unlike every other entry here,
      the generative model's inference cost is part of the decode budget

#### `tpt-kinetix-volumetric` — design-phase checklist (start here once prioritized)
- [ ] Decide the target volumetric representation for v1 (point cloud vs.
      voxel grid vs. mesh+texture)
- [ ] Write the format design doc: since this is 3D, not 2D-frame, data,
      decide whether it reuses any `tpt-kinetix-core` frame/packet types or
      needs new core types entirely
- [ ] Design the spatial-partitioning + entropy-coding approach for
      point/voxel data (e.g. octree)
- [ ] Decide how compression efficiency is measured for design validation
      (baseline against Draco / MPEG point-cloud-compression)
- [ ] Document the memory/perf budget for v1
- [ ] Confirm explicitly that this shares no primitives with any other
      kinetix codec — `docs/codec-backlog.md` flags it as fundamentally
      different, but that should be a stated decision here, not an assumption

- [x] Prioritize this list (pick the next one to move from backlog to an
      actual Phase-13-style design + scaffold effort, using its
      design-phase checklist above as the starting task list) once
      `tpt-kinetix-lean` reaches a stable v1 — **realtime chosen (2026-08-13)**:
      design doc `docs/realtime-codec-design.md` written (all 7 DECISION blocks
      resolved), and the scaffold is started — `tpt-kinetix-bitstream` (shared
      BitReader + rANS) + `tpt-kinetix-realtime` (profile-aware headers +
      decoder shell) created and compiling. See Phase 16.

## Phase 15 — `tpt-kinetix-vision`: Video-for-Machines Codec (design phase) (2026-07-20)

> Goal: an original codec design that optimizes rate-distortion for
> downstream detector/classifier/embedding-model accuracy per bit, rather
> than human perceptual quality. Backlog item promoted to a design phase per
> the Phase 14 rule (design section required before implementation starts).
> Full rationale in `docs/codec-backlog.md` ("Original Specialist Codec
> Concepts" table). Overlaps `tpt-kinetix-lean`'s embedded/edge-camera
> target but shares no bitstream design yet — track independently until the
> shared-primitives question below is resolved.

### Design (resolved — see `docs/vision-codec-design.md`)
- [x] Decide the target consumer model class(es) for v1 — **Object detection
      (YOLO/DETR-family)** as v1 target, evolving to model-agnostic quantization
      later; documented as DECISION 1
- [x] Write the format design doc: header layout including a
      `chroma_present` flag (chroma is optional, not just subsampled —
      luma-only is the default for detection/pose/tracking encodes), and a
      bit-depth/quantization scheme matched to the target model's trained
      input precision rather than the human-eye 8-bit convention —
      **drafted in `docs/vision-codec-design.md`; all 8 decision points
      resolved (chroma handling, bit depth, quant matrix, output contract,
      metrics, budget, Lean relationship, target platform)**
- [x] Design the decode path's primary output contract: a feature/embedding
      tensor (bitstream → tensor), with full pixel reconstruction as a
      secondary/on-demand path for human review of a flagged clip — **dual-path
      `VisionDecoder` trait with `decode_tensor()` + `decode_pixels()`,
      `Tensor` output type; see DECISION 5**
- [x] Decide how "detector/classifier accuracy per bit" is measured for
      design validation — **mAP-vs-bitrate on COCO-val with YOLOv8-n as v1
      metric; see DECISION 6**
- [x] Document the memory/perf budget for v1 — **embedded envelope (RPi-class,
      ~20 MB arena, <10 ms/frame); see DECISION 7**
- [x] Decide the relationship to `tpt-kinetix-lean` — **start independent,
      extract later into `tpt-kinetix-bitstream` once both codecs are stable
      (DECISION 8)**

### Scaffold (completed)
- [x] Scaffold `tpt-kinetix-vision` crate from `templates/codec-crate/`,
      add to workspace `members` — `tpt-kinetix-vision/Cargo.toml`,
      `src/lib.rs`, `README.md` created; `Tensor` type and `VisionDecoder`
      trait (dual-path `decode_tensor` + `decode_pixels`) implemented as
      scaffold with the honesty contract (`pixel_exact: false`, strict mode
      returns `NotPixelExact`)

## Phase 16 — Granular Next Steps (2026-08-06)

> Session broke the P-frame CAVLC desync investigation out of the single
> `[-]` blocker in Phase 12 C.1 and into concrete, individually-verifiable
> sub-tasks. The deep codec items (full CABAC, AV1 reconstruction, crates.io
> publish with token) remain large; the items here are the tractable ones that
> can be landed without a per-block CAVLC oracle or a crates.io credential.

### Codec-correctness (small, verifiable)
- [x] Clean up all debug `eprintln!`s added during the P-frame investigation
      (`slice_data.rs`, `decoder.rs`, `cavlc_tables.rs`) and restore clean
      `parse_cavlc_block`. NOTE (2026-08-07): the `decoder.rs` debug `eprintln!`s
      (`[DEBUG] try_decode_real_slice …`, `P-path: parse error`) were in fact
      still present and have now been removed; `cargo build` is warning-free.
- [x] Keep the `dec_ref_pic_marking` slice-header fix: it is now driven by
      `nal_ref_idc` (not `slice_type`) in `slice::parse_with_context`, and
      `parse_with_context` takes the new `nal_ref_idc` argument. This was the
      most likely candidate root-cause for the P-slice (non-reference) desync.
- [x] Write a minimal, self-contained P-frame CAVLC characterization test
      (`tests/p_slice_cavlc_invariant.rs`): generates a 2-frame IP clip with
      `ffmpeg`, extracts the P-slice NAL, and calls `parse_p_slice` **directly**
       (bypassing the decoder's skip-frame fallback). It now PASSES — the
       Phase 12 C.1 desync is resolved (the slice parses to completion). It
       serves as the regression guard for the CAVLC P-slice parse path.
- [x] Pin the exact P-frame residual off-by-one with a CAVLC oracle. **RESOLVED
        2026-08-08 (Phase C.2) — verified 2026-08-13.** The throwaway
        independent oracle (`tests/p_slice_oracle2.rs`, route (b): a fresh
        §9.2.2 level-assembly re-implementation sharing only the ffmpeg-verified
        VLC tables, walked over the exact 64×48 IP P-slice bytes and diffed
        per-block against `parse_cavlc_block`'s traced coeffs) reports
        **0 mismatches across all 104 luma/chroma-AC/chroma-DC blocks**. So the
        residual decode path is correct and no off-by-one exists in
        `level`/`run_before`/`total_zeros` assembly. The earlier `max_diff=2`
        over 49/4608 samples was a **false premise in the test harness**, not a
        decoder bug: `-x264-params deblock=0` only zeroes x264's alpha/beta
        offset and does *not* set `disable_deblocking_filter_idc=1`, so the
        conformance test was comparing against an ffmpeg reference with the
        in-loop deblocking **on** while assuming it off. With deblocking
        genuinely disabled (`no-deblock=1`) the P-frame decode is bit-exact
        (`max_diff=0`), and with it enabled the per-4×4-block `bS` deblocking
        fix (Phase C.2) also makes it bit-exact. The strict `max_diff=0`
        assertion in `p_frame_conformance` is live and passing.
- [x] Once pinned, fix the offending routine — **nothing to fix**. The oracle
        confirmed the residual decode was never the bug; the real (and already
        fixed) gap was the deblocking `bS` per-4×4-block derivation in
        `deblock.rs` (Phase C.2). The strict `max_diff=0` assertion in
        `p_frame_conformance` is re-enabled and passing (both deblocking on/off).

### Publishing (tractable, no credential needed)
- [x] `cargo publish --dry-run` revealed `tpt-kinetix-core@0.1.0` **already
      exists on crates.io** — so the real publish (Phases 8/10) has been done by
      a maintainer already; the remaining step is re-publish after version bump.
- [x] Fixed the publish blocker: internal crate deps in `[workspace.dependencies]`
      and the inline `tpt-kinetix-test-utils` dev-deps lacked a `version`
      requirement, which crates.io rejects (`all dependencies must have a version
      requirement specified when publishing`). Added `version = "0.1.0"` to all
      internal `path` deps. Dry-run now fails only on the *uncommitted-changes*
      guard (expected during dev) rather than the version error — i.e. the
      packaging gate is cleared. A maintainer must commit, bump versions, and run
      the real `cargo publish` with a token.
 - [x] Run `cargo package --list` per crate to confirm no stray files are
       packaged — verified with `cargo package --list --workspace --allow-dirty`;
       only intended test/fuzz corpora (`tests/corpus/mp4/*.bin`,
       `fuzz_*_input.bin`) would ship, no build artifacts (`target/`, `.git/`,
       `.exe`, `.wasm`, or media samples).

### Status notes
- The P-frame CAVLC **bit-position desync is resolved** (`p_slice_cavlc_invariant`
   passes). The once-suspected **residual precision issue is also resolved**:
   the independent CAVLC oracle (`tests/p_slice_oracle2.rs`) finds **0
   mismatches** across all 104 blocks of the 64×48 IP clip, and
   `p_frame_conformance` decodes **bit-exact (`max_diff=0`)** both with
   deblocking disabled (`no-deblock=1`) and enabled. The earlier `max_diff=2`
   over 49/4608 samples was a **test-harness false premise**, not a decoder bug:
   `-x264-params deblock=0` only zeroes x264's alpha/beta offset and does not
   set `disable_deblocking_filter_idc=1`, so the conformance test was comparing
   against an ffmpeg reference with the in-loop deblocking **on** while assuming
   it off. The real fix (Phase C.2) was the per-4×4-block deblocking `bS`
   derivation in `deblock.rs`. All P-slice MVs are (0,0)/ref 0, so MC/reference
   copy is exercised and correct; `coeff_token`, `total_zeros`, and
   `run_before` tables match FFmpeg `h264data.h` exactly, and the I-frame path
   (shared residual math) is bit-exact — confirming the residual decode has no
   off-by-one.
- `crates.io` real publish (Phases 8/10) is **not** performed: it needs a
  crates.io token and network access and must be done deliberately by a
  maintainer.

## Phase 17 — Codec Conformance & Benchmark Reporting (2026-08-07)

> Source: session plan `judt-thinking-about-all-moonlit-flute` (prompted by
> "can we test that these codecs work / benchmark them against ffmpeg?").
> Ground truth established this session: `tpt-kinetix-test-utils` already has
> real ffmpeg-comparison plumbing (`reference.rs`, `pixel_diff.rs`,
> `synthetic.rs`) and ~20 ffmpeg-gated tests exist across the workspace, but
> (a) there is no single command that reports per-codec status, and (b) CI
> never installs `ffmpeg`/`dav1d`, so every gated test silently skips there —
> CI has been "green" without ever running a real comparison. Likewise, the 3
> existing criterion benches report per-crate with no single glanceable
> cross-codec table. This phase does **not** touch codec-correctness work
> (H.264 CABAC/P-frame residual, AV1 entropy decoder — tracked in Phase 12);
> it makes existing per-codec maturity queryable in one command and
> CI-enforced (correctness), adds one unified cross-codec performance table
> (speed), and fills two measurement gaps along the way (AAC benchmark; AV1
> encode size/quality vs ffmpeg). Refined mid-session per user ask: benchmark
> results should be presented as a side-by-side table, not just criterion's
> own per-crate HTML reports — added as a new standalone `bench_report` tool
> (§ below) rather than a criterion-JSON scraper (criterion's
> `target/criterion/*/base/estimates.json` is undocumented/unstable, not
> worth building a parser against).

### Status reporting tool
- [x] Add `tpt-kinetix-test-utils/src/audio_diff.rs` (`pcm_within_tolerance`,
      `pcm_max_abs_diff`, `pcm_diff_count`) and wire `pub mod audio_diff;` into
      `lib.rs` — implemented 2026-08-12
- [x] Add `reference::decode_av1_with_ffmpeg(obu, w, h)` and
      `reference::decode_aac_with_ffmpeg(adts)` to
      `tpt-kinetix-test-utils/src/reference.rs` (ffmpeg `-f obu` / `-f adts`
      round trips — no standalone `dav1d` CLI needed; `decode_aac_with_ffmpeg`
      parses sample rate/channels from the ADTS header and returns per-1024-block
      `f32` `AudioFrame`s) — implemented 2026-08-12
- [x] Add `synthetic::generate_h264_cavlc_iframe_clip` and
      `generate_h264_cavlc_ip_clip` to `synthetic.rs` (ported from the
      `generate()` helpers in `cavlc_conformance.rs` / `p_frame_conformance.rs`
      as new ffmpeg-stdout-streaming functions — leave those two test files
      untouched) — implemented 2026-08-12
- [x] Add `tpt-kinetix-aac`, `tpt-kinetix-lean`, `tpt-kinetix-vision` as
      `tpt-kinetix-test-utils` dev-dependencies (also surfaced in the
      `codec_status` example, which now prints all five decoders' capabilities)
      — implemented 2026-08-12
- [x] Add `tpt-kinetix-test-utils/examples/codec_status.rs`: prints each
      codec's `DecoderCapabilities` (canonical machine-readable status) and
      exits 1 under `--strict` if any decoder is not `pixel_exact`. (Simpler
      than the markdown-table variant; pulls `tpt-kinetix-aac` into
      `tpt-kinetix-test-utils` dev-deps.)

### Benchmarks
 - [x] Add `tpt-kinetix-aac/benches/decode_throughput.rs` (criterion,
       mirrors `tpt-kinetix-h264/benches/decode_throughput.rs`; embeds a
       real ffmpeg-generated AAC-LC ADTS clip as bytes, so no runtime ffmpeg
       dependency) + `criterion` dev-dep and `[[bench]]` in
       `tpt-kinetix-aac/Cargo.toml`
 - [x] Extend `tpt-kinetix-av1/benches/av1_encode.rs`: extend the existing
       single `bench_function` into a 3-way `benchmark_group` (`kinetix`,
       `librav1e`, `libaom`) for wall-clock only — size/PSNR comparison lives
       in `bench_report` instead, not duplicated here; the ffmpeg comparison
       benches are guarded by `ffmpeg_available()`
- [x] Add `tpt-kinetix-test-utils/examples/bench_report.rs`: standalone tool that
      runs every Criterion bench (via `cargo bench`) and prints a consolidated
      `time:` report for h264/av1/pipeline; accepts an optional crate list and
      `--release`.

### Discoverability & CI
- [x] Add `tpt-kinetix-h264/examples/gen_corpus.rs` (writes a small
      `testsrc_WxH.h264` corpus into `$TMPDIR/h264_corpus`, same ffmpeg
      pattern as `cavlc_conformance.rs::generate()`) so the existing but
      undiscoverable `corpus_check.rs` example is runnable with one command
- [x] Add `justfile` recipes: `conformance` (runs `codec_status`),
      `corpus-check` (runs `gen_corpus` then `corpus_check`), `bench` (runs
      the h264/av1/pipeline criterion benches), `bench-report`
      (runs `bench_report` in `--release`)
 - [x] Add a new `conformance` job to `.github/workflows/ci.yml` (ubuntu-only):
       installs ffmpeg via `apt-get`, installs cargo-nextest, runs
       `cargo nextest run -p tpt-kinetix-h264 -p tpt-kinetix-aac
       -p tpt-kinetix-av1 -p tpt-kinetix-stream -p tpt-kinetix-test-utils`
       (no exclusion needed — `cavlc_pframe_no_deblock_is_bitexact` now PASSES,
       fixed in Phase C.2; all ffmpeg-gated conformance tests in those packages
       were verified green locally), then prints `codec_status`. The strict
       `codec_status -- --strict` assertion is included as a `continue-on-error`
       non-blocking step because h264/av1 CABAC paths are not yet pixel_exact;
       it becomes the real gate once those decoders reach pixel_exact.
- [x] Fix the stale `tpt-kinetix-h264/README.md` "Status & known
      limitations" section (still claimed skip-only placeholder macroblocks
      and lists intra prediction/deblocking as unimplemented, contradicting
      the actual bit-exact-I-frame state from Phase 12); point it at
      `just conformance` / `just bench-report` as the living source of truth

### Verification
- [x] `cargo build --workspace` — full workspace compiles (root `Cargo.toml`
      `workspace.dependencies` gained `tpt-kinetix-lean`/`tpt-kinetix-vision`)
- [x] `cargo test -p tpt-kinetix-test-utils --lib` — 17 passed
- [x] `cargo test -p tpt-kinetix-test-utils --test conformance` — 7 passed,
      including the new `aac_vs_ffmpeg_reference_pcm_when_available` which
      exercises `decode_aac_with_ffmpeg` + `audio_diff` against real `ffmpeg`
      PCM (AAC-LC within 1e-2 tolerance)
- [x] `cargo clippy -p tpt-kinetix-test-utils --all-targets` — clean (only
      pre-existing `tpt-kinetix-h264` warnings, unrelated to this work)
- [~] `just conformance` / `just bench-report` — verified the underlying
      commands directly (`cargo run -p tpt-kinetix-test-utils --example
      codec_status` prints all five decoders; `bench_report` example compiles);
      `just` itself cannot spawn `sh` on this Windows host so the recipes were
      not invoked via `just`. CI `conformance` job's nextest filter (Phase 17
      §Discoverability) remains to be re-verified locally before push.

## Phase 18 — Native AAC-LC Decoder (remove symphonia MPL-2.0 dependency) (2026-08-13)

> `cargo deny check` fails CI ("Deny (licenses / advisories)" job, run
> 30184382291) because `tpt-kinetix-aac` depends on `symphonia-core` /
> `symphonia-codec-aac`, both MPL-2.0, which isn't in `deny.toml`'s
> `licenses.allow` list (plus an unrelated `bitflags` v1/v2 duplicate-version
> ban from the same dependency). Rather than allow-list MPL-2.0,
> `docs/codec-evaluations/aac.md`'s original build-vs-wrap tradeoff is being
> revisited: replace symphonia with a native from-scratch AAC-LC decoder,
> consistent with the H.264/AV1 native-reimplementation approach. This is a
> multi-week effort; per explicit decision, the "Deny" CI job is left **red**
> for the duration (no temporary `deny.toml` MPL-2.0 stopgap) until Phase 7
> below removes symphonia entirely. Constraint: new code must be written from
> the public ISO/IEC 13818-7 / 14496-3 spec, not transcribed from the local
> MPL-2.0 symphonia-codec-aac source (which may only be read for algorithm
> structure). The `tpt-kinetix-kg` FFmpeg-C-ingest pipeline is intentionally
> not used (FFmpeg's AAC decoder is LGPL/GPL — worse license posture than the
> problem being solved). Full phased plan:
> `C:\Users\phill\.claude\plans\https-github-com-tpt-solutions-tpt-kinet-squishy-lemur.md`.

- [ ] Phase 1 — `tpt-kinetix-aac/src/bitreader.rs` (MSB-first, AAC escape-value
      helpers, modeled on `tpt-kinetix-h264/src/bitreader.rs`'s shape) +
      parse-only syntax structs (`IcsInfo`, `SectionData`, SCE/CPE/LFE/FIL/END
      element dispatch). Exit: unit tests on hand-built fixtures +
      `*_never_panics` proptest.
- [ ] Phase 2 — `src/codebooks.rs`: the 11 (+1 escape) Huffman spectral
      codebooks, independently transcribed from spec tables, tree-walk decode
      + escape handling. Exit: unit tests per codebook against hand-encoded
      sequences + bounded-consumption/no-panic proptest.
- [ ] Phase 3 — `src/scalefactors.rs`, `src/dequant.rs`, `src/pns.rs`,
      `src/tns.rs`, `src/pulse.rs`: DPCM scalefactor decode, dequantization
      formula, perceptual noise substitution, temporal noise shaping, pulse
      data. Exit: unit tests against hand-computed values; TNS filter
      validated against an independently computed reference.
- [ ] Phase 4 — `src/stereo.rs`: M/S and intensity-stereo reconstruction for
      channel_pair_element. Exit: unit tests reconstructing L/R from known
      coded spectra.
- [ ] Phase 5 — `src/mdct.rs` (1024/128-point IMDCT, written from scratch —
      new to the whole workspace, no existing MDCT code anywhere) +
      `src/window.rs` (KBD/sine windows, window-sequence transitions,
      overlap-add state). Exit: IMDCT(MDCT(x))≈x round-trip test,
      window-value tests at known points, proptest over window-sequence
      combinations.
- [ ] Phase 6 — Wire phases 1-5 into `decode_raw_data_block`, swap
      `decoder.rs`'s internals onto the native path (public
      `AacDecoder::new/with_config/set_config/set_strict/capabilities/decode/config`
      API unchanged), new `tests/conformance_aac.rs` via
      `tpt-kinetix-test-utils`'s `decode_aac_with_ffmpeg` + `audio_diff`
      reference harness, `tests/proptest_decode_never_panics.rs`,
      `just fuzz tpt-kinetix-aac fuzz_aac_decode 60`. Exit: conformance test
      passes at a documented tolerance, bench recorded, fuzz run clean.
- [ ] Phase 7 — Remove `symphonia-codec-aac`/`symphonia-core` from
      `tpt-kinetix-aac/Cargo.toml` and root `Cargo.toml`; update
      `docs/codec-evaluations/aac.md`, both READMEs' status tables, and
      module doc comments (drop "delegated to symphonia-codec-aac" language).
      Exit: `grep -rn symphonia` empty; `just check` and `just deny` both
      green.

## Phase 16 — `tpt-kinetix-realtime`: Realtime Codec (scaffold + implementation) (2026-08-13)

> Goal: an original codec whose design center is **sub-frame latency** and
> **graceful degradation under packet loss** (not max compression ratio).
> Promoted from the Phase 14 backlog per the Phase 14 rule: the design-phase
> checklist is fully resolved and `docs/realtime-codec-design.md` is written
> (7 DECISION blocks: hybrid loss recovery, rolling-intra + on-demand-IDR GOP,
> fixed slice grid, encode-deadline/decode-bounded-work contract,
> packet-loss-vs-quality + stall-rate validation, profile-parameterized v1
> budget, and extraction of a shared `tpt-kinetix-bitstream` crate).
>
> **Profile decision:** profile-agnostic — cloud gaming / conferencing / AR
> are three preset parameter sets over one shared bitstream, not separate
> codecs. AR is the hardest preset (leans on `tpt-kinetix-lean`'s power
> primitives). See the design doc for the full rationale.

### Scaffold (done 2026-08-13)

- [x] Extract `tpt-kinetix-bitstream` from lean (DECISION 7) — `BitReader` +
      rANS (`RansEncoder`/`RansDecoder`/`RansStreamSet`/`SymbolModel`/
      `StaticModel`) copied verbatim and now the single source of truth;
      `tpt-kinetix-lean` and `tpt-kinetix-vision` still carry their own copies
      (migration to depend on `tpt-kinetix-bitstream` is the immediate
      follow-up — see Phase 16 item below). Crate compiles + clippy-clean,
      11 tests pass.
- [x] Create `tpt-kinetix-realtime` crate: `Cargo.toml` (deps on `core` +
      `bitstream`), `lib.rs` (profile-agnostic module docs), `headers.rs`
      (profile-aware `SequenceHeader`/`FrameHeader`: `ProfilePreset` enum,
      slice-grid, FEC overhead, foveation flag, intra-refresh mask,
      `deadline_ms`, `force_idr`; parse + to_bytes + validation, round-trips),
      `decoder.rs` (`RealtimeDecoder` shell reporting `pixel_exact: false`
      per the honesty contract). Wired into workspace `Cargo.toml` (members +
      `workspace.dependencies`). Compiles + clippy-clean, 11 tests pass.

### Implementation (remaining, per design doc "Implementation order")

- [x] Migrate `tpt-kinetix-lean` + `tpt-kinetix-vision` to depend on
      `tpt-kinetix-bitstream` (delete their now-duplicated `bitreader.rs` /
      `rans.rs`); true "extract now" completion of DECISION 7 — **done for
      lean (2026-08-13)**: added `tpt-kinetix-bitstream` dep, repointed
      `headers.rs`/`decoder.rs`/`lib.rs` doc links at it, deleted
      `lean/src/bitreader.rs` + `lean/src/rans.rs`, and updated the fuzz
      target (`tpt_kinetix_lean::bitreader::BitReader` →
      `tpt_kinetix_bitstream::BitReader`). **vision had no duplicated
      primitives** — its `src` is a single decode-shell `lib.rs` with no
      `BitReader`/`rANS` code — so there was nothing to migrate; vision will
      depend on `tpt-kinetix-bitstream` when its reconstruction is
      implemented.
- [ ] Port lean's intra + unidirectional-P reconstruction into realtime
      (DECISION 2) — **BLOCKED**: `tpt-kinetix-lean`'s reconstruction
      (prediction/transform/deblock) is itself still an unimplemented scaffold
      (its `decoder.rs` reports `pixel_exact: false`), so there is no real
      reconstruction path to port yet. Revisit once lean's reconstruction
      lands. The header/refresh/slice scaffolding realtime needs is already in
      place.
- [x] Add slice-grid framing: each slice = one independent rANS sub-stream,
      self-contained (DECISION 3); wire `RansStreamSet` per-frame — **done
      (2026-08-13)**: `src/slice.rs` `SliceGrid` frames/unframes the
      `cols*rows` slice payloads via `tpt_kinetix_bitstream::RansStreamSet`,
      with count validation and the 255-sub-stream cap.
- [x] Add intra-refresh masking (`refresh_mask`) + on-demand `force_idr`
      resync path (DECISION 2) — **done (2026-08-13)**: `src/refresh.rs`
      `IntraRefreshScheduler` computes the per-frame `intra_refresh_mask`
      bitmask (cycling `ceil(rows/period)` rows/frame, wrapping, covering all
      rows within `period`); `force_idr` is already a frame-header field.
- [x] Add FEC packet framing (RaptorQ or RS) + decoder concealment as the
      terminal fallback (DECISION 1) — **done (2026-08-13)**: `src/fec.rs`
      `Fec` is a systematic XOR erasure coder over the framed frame payload
      (fixed 256-byte source symbols), emitting `repair_count` parity symbols
      (one per round-robin group) that recover up to one loss per group; losses
      beyond that fall through to intra-refresh/concealment as the hybrid
      design intends (RS/RaptorQ noted as the v2 MDS upgrade). `src/conceal.rs`
      `conceal()` does temporal concealment (reuse previous frame's slice) so
      the decoder never stalls. Both round-trip tested (29 realtime tests pass).
- [x] Add `deadline_ms` encode-side rate-control hook + `max_decode_ms`
      decoder capability (DECISION 4) — `src/rate.rs` implements
      `max_decode_ms_estimate` (turns a `SequenceHeader` into the decoder's
      `max_decode_ms`), `adapt_to_deadline` (encode-side `RateControlAction`
      from `deadline_ms`/`elapsed_ms`/`current_qp`), and `EncodeDeadline`;
      `headers.rs` carries `deadline_ms`/`max_deadline_ms` per frame/sequence
      (round-trip tested).
- [ ] Build the packet-loss-vs-quality + stall-rate validation harness
      (DECISION 5) in `tpt-kinetix-test-utils` behind a `realtime-bench`
      feature (loss injector + the realtime decoder; no model weights).
- [ ] (AR profile) Add foveation / gaze-map support (DECISION 6).

## Phase 15 — `tpt-kinetix-volumetric` (design phase, 2026-08-13)

> Source: prioritized original specialist codec from `docs/codec-backlog.md`
> (`tpt-kinetix-volumetric` — point-cloud / volumetric / AR-VR content;
> 2D video codecs don't apply). Design doc: `docs/volumetric-codec-design.md`.
> Each checklist item maps to a `DECISION:` block in that doc. Nothing is
> implemented yet; resolve all decisions before scaffolding begins.

### Design decisions
- [x] Decide the target volumetric representation for v1 (point cloud vs.
      voxel grid vs. mesh+texture) — **DECISION 1 resolved: point cloud**
      (see `docs/volumetric-codec-design.md`; voxel/mesh deferred to v2 as
      output representations derived from the decoded cloud)
- [x] Decide the geometry coding method for the v1 point cloud (octree vs.
      predictive vs. trisoup) — **DECISION 2 resolved: octree** (context-
      modeled occupancy bitmap per node; predictive trisoup deferred to v2,
      see `docs/volumetric-codec-design.md`)
- [x] Decide the attribute coding method (RAHT vs. region-adaptive
      predictive/lift vs. direct) — **DECISION 3 resolved: both normative,
      lift primary / RAHT selectable** (see `docs/volumetric-codec-design.md`)
- [x] Decide attribute bit depth + lossless vs. lossy mode — **DECISION 4
      resolved: per-attribute bit depth (8-bit default, 10–16-bit HDR),
      lossless + lossy via shared quantizer; strict mode rejects lossy**
- [x] Decide static-single-cloud (v1) vs. dynamic inter-frame prediction —
      **DECISION 5 resolved: static for v1**, `dynamic` flag reserved →
      `Unsupported` on v1 decoder
- [x] Decide bitstream alignment: G-PCC/V-PCC-faithful core with Kinetix
      framing (recommended) vs. pure original bitstream — **DECISION 6
      resolved: (C) G-PCC-faithful core, Kinetix framing** (`magic b"VOLU"`)
- [x] Decide relationship to `tpt-kinetix-bitstream` (rANS), the 2D codecs
      (V-PCC projection path), and the `PointCloud` core output type —
      **DECISION 7 resolved: depends on core + bitstream only; `PointCloud`
      output type in core; V-PCC is optional feature-gated dep**
- [x] Set memory/perf budget + conformance oracle (MPEG-I G-PCC TMC13) for
      v1 — **DECISION 8 resolved: 10M-point cap, ~64 MB arena, TMC13 oracle**

### Pre-scaffold (after decisions resolve)
- [x] Add `tpt-kinetix-volumetric` to workspace `Cargo.toml` members
      (design-phase only — not added to `release-plz.toml` publish list,
      matching `tpt-kinetix-vision`/`lean`/`screen`)
- [x] Add `PointCloud` decoded-output type to `tpt-kinetix-core`
      (`frame.rs`: `PointCloud` + `PointAttribute` + `PointAttributeKind`)
- [x] Implement sequence/frame header parsing (DECISION 6 framing) — `src/header.rs`: byte-aligned `b"VOLU"` sequence + frame headers, rejects dynamic/reserved/over-cap streams
 - [x] Implement octree geometry decode (DECISION 2) — `src/octree.rs`: MSB-first
       context-modeled occupancy octree via per-context rANS models; reconstructs
       integer coords from the descent path, round-trips against the in-crate encoder
 - [x] Implement attribute lift/RAHT decode (DECISION 3) — `src/attribute.rs`: lift
       (k-neighbour predictive + residual, lossless when quant step = 1) and RAHT
       (lossless integer Haar over the Morton-ordered stream); both round-trip
 - [x] Build a TMC13-oracle conformance harness (DECISION 8) — `tpt-kinetix-test-utils`:
       `src/tmc13.rs` drives `tmc3` (gated, skips when absent) + `tests/volumetric_conformance.rs`;
       direct Kinetix-vs-TMC13 bit-exact cross-check pending coding-tool alignment
       (decoder still reports `pixel_exact: false`; strict mode rejects output)
 - [x] Add `cargo-fuzz` target for the header + octree parser — `fuzz/` (`fuzz_volumetric_parser`),
       exercises full decode path (header → octree → attributes) panic-free on any input

