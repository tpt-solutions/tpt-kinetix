# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1](https://github.com/tpt-solutions/tpt-kinetix/releases/tag/v0.1.1) - 2026-10-07

### Other

- bump all crates to 0.1.1 and fix crates.io publishing
- AV1 SIMD helpers and bitreader speedups; H.264 env-debug hooks; VP9 div128 fixture notes
- cache Hadamard matrices, remove per-block allocations, bound block size
- Add ffmpeg comparison harness and fix codec bugs it surfaced
- Add per-crate Criterion benches, perf baseline/compare tooling, and publishing docs
- Tidy repo root: move H.264 oracle sources and tools, drop debug artifacts
- AV1 decoder and coefficient/inter/transform updates; codec crate conformance reporting updates
- Add conformance reporting, AV1 decoder updates, and per-crate changelogs
- Fix decoder bitstream-desync and dequant bugs in AV1, H.264, and AAC
- Merge branch 'master' of https://github.com/tpt-solutions/tpt-kinetix
- Advance AAC, AV1, and H.264 decode paths, plus realtime and face codec scaffolding
- Expand AAC decoder modules and advance H.264/AV1/realtime decode paths
- Advance H.264/AV1 decode paths and AAC codebook integration
- Advance H.264/AV1 decode paths and add volumetric codec scaffolding
- Add new codec crates (face, lossless, realtime, screen, volumetric) and bitstream foundation

### Changed

- **Performance: the Walsh–Hadamard transform matrices are cached instead of
  rebuilt on every call.** `hadamard_2d_raw` rebuilt the matrix on *every*
  invocation — a nested `Vec<Vec<i32>>` (`1 + n` heap allocations) plus
  `O(n² log n)` work — and it runs once per block per frame in both the encode
  and the decode direction. The matrices are pure functions of `n` and `n` only
  ever takes a few power-of-two values, so they are now built once per size
  behind a `OnceLock` (thread-safe for concurrent decode). Entries and
  accumulation order are unchanged, so the transform stays bit-exact.
- **Performance: the per-block hot loops no longer allocate.** Reconstruction
  allocated ~5 `Vec`s per block and the encoder ~6 more per intra mode trial
  (14 modes per block, so ~84 allocations per block). Both directions now share
  one per-frame scratch (`BlockScratch` / `EncodeScratch`).
- `predict_directional`'s extended top/left arrays moved from two per-block
  `vec!` allocations to fixed stack scratch (bounded and asserted at run time).
- New `transform::inverse_2d_with_scratch` for callers that already own a
  buffer; `inverse_2d` is now a thin allocating wrapper over it.

Measured with `cargo bench -p tpt-kinetix-realtime` (Criterion chains each run
against the previous one, so the two steps are reported separately):

- transform caching alone: 1920x1080 encode **+36.9%**, decode **+26.7%**
- plus the allocation removal: a further **+7.2%** encode, **+7.1%** decode
  (1280x720 decode +16.6%)
- cumulative at 1920x1080: encode ~**+47%**, decode ~**+36%**

### Security

- **Sequence headers now reject `max_block_size_log2 > 6`.** Both block-size
  fields are 4 bits wide and only `min <= max` was validated, while `block_sizes`
  derives the reconstruction block size from `min_block_size_log2`. A stream
  could therefore declare a 32k×32k minimum block and make the decoder request a
  multi-gigabyte allocation from a handful of bytes of input. v1 defines blocks
  in the 8x8..64x64 range, so larger values are now rejected at parse time —
  which is also what makes the fixed-size scratch buffers sound.

Output is unchanged: all 62 realtime tests pass, including the `qp == 0`
bit-exact round-trip and the slice-boundary regression tests below.

### Fixed

- `slice_index_for` is now the exact inverse of `chunk_range`. The previous
  floor formula disagreed with the chunk boundaries whenever the block total
  did not divide evenly by the slice count (e.g. 320x240 = 1200 blocks over 64
  slices), so decoders read blocks from the wrong slice and failed with
  "chroma block index out of range" on every uneven geometry. It also routed
  blocks of sub-slice-grid frames (fewer blocks than slices) into empty slices,
  panicking on `slices[slice][local]`. Both manifestations are covered by the
  new `uneven_slice_chunks_round_trip_at_qp0` /
  `sub_slice_grid_frame_round_trips` tests. Chroma reconstruction now offsets
  the Cb/Cr chunks by the chroma chunk length (it only matched luma's by
  coincidence of equal chunk counts in 4:2:0 with 8px luma blocks).

## [0.1.0](https://github.com/tpt-solutions/tpt-kinetix/releases/tag/v0.1.0) - 2026-07-19

### Added

- Initial `tpt-kinetix-realtime` crate: an original low-latency, loss-resilient
  video codec for cloud gaming, video conferencing and AR/smart-glasses overlay.
- Profile-agnostic core with three preset parameter sets over one shared format.
- Sequence/frame headers covering the profile-aware parameter sets.
- Fuzz target for the header path.
- Full design (loss recovery, GOP structure, latency budget, validation metric,
  v1 budget, shared-primitive relationship to `tpt-kinetix-lean`) documented in
  `docs/realtime-codec-design.md`.
- Validation harness support via the `realtime-bench` feature
  (`tpt-kinetix-test-utils::realtime_bench`).

### Known limitations

- Block reconstruction, slice framing, intra-refresh masking, hybrid FEC and
  concealment are not implemented; `capabilities()` reports `pixel_exact: false`.
