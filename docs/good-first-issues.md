# Good First Issues — candidate backlog

This file catalogs the project's partially-complete (`[~]`) and not-yet-started
(`[ ]`) todo items that are well-scoped for a new contributor. Each entry has a
short title, a difficulty hint, and the file pointers needed to start.

When opening a GitHub issue from one of these, copy the **Issue** text and add the
`good first issue` label plus the suggested `difficulty` label.

---

## 1. H.264 CABAC entropy decoding

- **Difficulty:** hard
- **Pointer:** `tpt-kinetix-h264/src/entropy.rs` (CAVLC scaffold at
  `tpt-kinetix-h264/src/cavlc.rs`), slice header parsing in
  `tpt-kinetix-h264/src/nal.rs`
- **Why good-first-ish:** CABAC is a self-contained spec module (ITU-T H.264
  Annex 9); can be unit-tested against the spec's decoding-engine examples
  independently of prediction.
- **Issue:** "Implement H.264 CABAC entropy decoding (Main/High profiles). Context
  init tables per slice type; reset at slice boundaries. Add unit tests from the
  spec binary-arithmetic-decoder examples."

## 2. H.264 intra prediction

- **Difficulty:** medium
- **Pointer:** `tpt-kinetix-h264/src/prediction.rs`, transform/IQ scaffold in
  `tpt-kinetix-h264/src/transform.rs`, `DecoderCapabilities` in
  `tpt-kinetix-core/src/capabilities.rs`
- **Issue:** "Implement H.264 intra prediction modes (4x4 + 8x8 + 16x16) and flip
  `supports_intra_prediction` to true once tested."

## 3. H.264 deblocking filter

- **Difficulty:** medium
- **Pointer:** `tpt-kinetix-h264/src/deblock.rs`
- **Issue:** "Implement the H.264 in-loop deblocking filter (BS derivation +
  edge filtering) and update `DecoderCapabilities::supports_deblocking`."

## 4. Pixel-exact comparison harness across profiles

- **Difficulty:** easy (test wiring)
- **Pointer:** `tpt-kinetix-test-utils/src/reference.rs`,
  `tpt-kinetix-h264/tests/`
- **Issue:** "Wire the pixel-diff harness to run over a small set of real H.264
  clips (baseline/main/high) and assert PSNR ≥ 60 dB once reconstruction lands."

## 5. AV1 frame reconstruction

- **Difficulty:** hard
- **Pointer:** `tpt-kinetix-av1/src/decoder.rs` (placeholder frames),
  `tpt-kinetix-av1/src/obu.rs` (OBU parser, done)
- **Issue:** "Implement AV1 inverse transform + intra/inter prediction so the
  decoder emits real frames instead of grey placeholders; enable the dav1d
  pixel-diff harness."

## 6. AAC PCM reconstruction

- **Difficulty:** medium
- **Pointer:** `tpt-kinetix-aac/src/decoder.rs` (parse-only shell),
  `docs/codec-evaluations/aac.md` (recommends wrapping `symphonia-codec-aac`)
- **Issue:** "Make AAC sample-exact. Either wrap `symphonia-codec-aac` in
  `AacDecoder` or implement MDCT/Huffman/TNS. Flip `pixel_exact` once verified."

## 7. End-to-end RTMP → HLS playable test

- **Difficulty:** medium
- **Pointer:** `tpt-kinetix-stream/src/rtmp/server.rs`,
  `tpt-kinetix-stream/src/hls/`
- **Issue:** "Add an integration test that pushes an RTMP stream through the
  pipeline and asserts the produced HLS segments are playable (ffplay/hls.js)."

## 8. Publish crates to crates.io

- **Difficulty:** easy (process)
- **Pointer:** `README.md` publish-order section, `release-plz.toml`
- **Issue:** "Cargo-publish each crate in dependency order (core → demux/codecs
  → pipeline → stream → cli). Reserve names first; requires a
  `CARGO_REGISTRY_TOKEN` secret."

## 9. cargo-generate template: add encode path

- **Difficulty:** easy
- **Pointer:** `templates/codec-crate/`
- **Issue:** "Extend the codec-crate template with an encoder struct scaffold and
  `EncodeConfig` wiring so generated crates cover both directions."

## 10. Public `tpt-kinetix-kg` positioning

- **Difficulty:** easy (docs/strategy)
- **Pointer:** `tpt-kinetix-kg/README.md`, `docs/adding-a-codec.md`
- **Issue:** "Decide whether `tpt-kinetix-kg` ships as a public 'bring your own
  codec' tool; if so, write a public-facing README and usage example."
