# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1](https://github.com/tpt-solutions/tpt-kinetix/releases/tag/v0.1.1) - 2026-10-07

### Other

- bump all crates to 0.1.1 and fix crates.io publishing
- fix the partial-edge-block decode bug; make ffmpeg_compare report honestly
- refresh the ffmpeg comparison; add screen round-trip tests, find 2 bugs
- cache natural-mode Hadamard matrices, remove per-block allocations
- Add ffmpeg comparison harness and fix codec bugs it surfaced
- Add per-crate Criterion benches, perf baseline/compare tooling, and publishing docs
- Tidy repo root: move H.264 oracle sources and tools, drop debug artifacts
- AV1 decoder and coefficient/inter/transform updates; codec crate conformance reporting updates
- Add conformance reporting, AV1 decoder updates, and per-crate changelogs
- remove reference C/oracle files; vision: overhaul reconstruct + deblock/prediction/headers; h264: interlaced + cabac + ref_pic updates; av1: reconstruct/partition; screen/lean/lossless/cli updates
- implement MBAFF B-slice decode (ref lists, field MC gate) + B-frame reconstruction; fix CABAC CBP neighbor context for MBAFF frame pairs
- implement full block reconstruction (intra+inter, WHT, deblock) with DPB; screen: implement mode classifier + flat/glyph/NATURAL reconstruction; vision: add headers/prediction/quant/transform/deblock/reconstruct modules; av1: remove partition debug instrumentation; update todos
- Advance AAC, AV1, and H.264 decode paths, plus realtime and face codec scaffolding
- Add new codec crates (face, lossless, realtime, screen, volumetric) and bitstream foundation

### Changed

- **Performance: the natural-mode Walsh–Hadamard matrices are cached instead of
  rebuilt on every call.** `hadamard_2d_raw` rebuilt the matrix on *every*
  invocation — a nested `Vec<Vec<i32>>` (`1 + n` heap allocations) plus
  `O(n² log n)` work — and it runs once per block per frame in both directions.
  The matrices are now built once per size behind a `OnceLock`. Entries and
  accumulation order are unchanged, so the transform stays bit-exact.
- **Performance: the natural path no longer allocates per block.** Extraction,
  the two neighbour rows, prediction, residual, the transformed block and the
  inverse transform's temporaries were each fresh `Vec`s per block (~8). They
  now come from one per-frame `NaturalScratch`, with allocation-free
  `*_into` variants used by both the encode and decode loops. The public
  `encode_natural_block` / `decode_natural_block` keep their allocating
  signatures and are now thin wrappers over the shared code, so they double as
  the reference for it.
- The two allocating helpers in `reconstruct.rs` (`extract_luma_block`,
  `natural_neighbors`) are removed — their `*_into` replacements in
  `natural.rs` are the only callers' path now.

Measured with `cargo bench -p tpt-kinetix-screen` at 1920x1080 (Criterion
chains runs, so the steps are separate):

- matrix caching: encode **+29.7%**, decode **+30.6%**
- plus the allocation removal: a further **+1.6%** encode, **+1.1%** decode

So unlike lean/realtime/vision, where both fixes mattered, here the matrix
rebuild was very nearly the whole story: on a UI-like source most blocks take
the FLAT or GLYPH paths, which never touched either the transform or the
per-block allocations, so only NATURAL blocks benefited from the second step.
Both changes are kept — the allocation removal is still a real ~1.5%, and it
matters more on sources with more natural content.

Output is unchanged: all 32 screen tests pass, including the `qp == 0`
bit-exact natural-block round-trip.

### Fixed

- Stream counts (mode/flat-run/glyph/natural-block/coefficent counts) are coded
  as four rANS symbols (u32 LE) instead of a single byte-sized symbol. The
  byte-wide counts wrapped at 256, corrupting every frame wider than 255 coding
  blocks (e.g. 320x240 with 16x16 blocks has 300) and every 16x16 natural block
  with a full 256-coefficient payload (count wrapped to 0). The bitstream
  format changes; the golden vector in `reconstruct.rs` was updated deliberately.

## [0.1.0](https://github.com/tpt-solutions/tpt-kinetix/releases/tag/v0.1.0) - 2026-07-19

### Added

- Initial `tpt-kinetix-screen` crate: an original screen/UI-capture codec tuned
  for sharp edges, flat regions and repeated glyphs.
- Three-mode block classification design — `FLAT` (run-length coalesced solid /
  gradient), `GLYPH` (cross-frame glyph/palette dictionary reference plus fg/bg
  colors) and `NATURAL` (transform/entropy fallback for embedded photo/video).
- Byte-aligned sequence/frame headers.
- `ScreenDecoder::capabilities()` reporting `pixel_exact: false`; strict mode
  returns `KinetixError::NotPixelExact`.
- Fuzz target for the header path.
- Full design notes in `docs/screen-codec-design.md`.

### Known limitations

- Block reconstruction (mode classifier, flat-fill run-length, glyph dictionary
  + palette, and the `NATURAL` transform path) is not implemented.
