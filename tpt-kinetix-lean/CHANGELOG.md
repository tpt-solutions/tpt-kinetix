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
- remove reference C/oracle files; vision: overhaul reconstruct + deblock/prediction/headers; h264: interlaced + cabac + ref_pic updates; av1: reconstruct/partition; screen/lean/lossless/cli updates
- implement full block reconstruction (intra+inter, WHT, deblock) with DPB; screen: implement mode classifier + flat/glyph/NATURAL reconstruction; vision: add headers/prediction/quant/transform/deblock/reconstruct modules; av1: remove partition debug instrumentation; update todos
- Add new codec crates (face, lossless, realtime, screen, volumetric) and bitstream foundation
- Rework AV1 inverse transforms and harden H.264 CABAC MVD decoding
- Fix H.264 inter CBP table, coeff_token FLC codes, and dec_ref_pic_marking gating
- Enable H.264 intra prediction and deblocking, add CABAC I-slice contexts
- Add H.264 slice-data decoding pipeline and new tpt-kinetix-lean codec crate

### Changed

- **Performance: the Walsh–Hadamard transform matrices are cached instead of
  rebuilt on every call.** `hadamard_2d_raw` called `hadamard_matrix(n)` on
  *every* invocation, which allocated a nested `Vec<Vec<i32>>` (`1 + n` heap
  allocations) and rebuilt the matrix in `O(n² log n)` — and it is called once
  per block per frame in both the encode and the decode direction. The matrices
  are pure functions of `n` and `n` only ever takes one of a few power-of-two
  values, so they are now built once per size behind a `OnceLock` and shared
  (thread-safe for concurrent decode). Matrix entries and accumulation order
  are unchanged, so the transform remains bit-exact.
- **Performance: the per-block hot loops no longer allocate.** Reconstruction
  and encoding allocated ~5 `Vec`s *per block* (prediction block, the two
  neighbour rows, the dequantised coefficients, the residual), and the encoder
  allocated ~6 more *per intra mode trial* — 14 modes per block, so ~84
  allocations per block, which is what made lean encode the slowest path in the
  crate. Both directions now use one per-frame scratch (`BlockScratch` /
  `EncodeScratch`) threaded through the block loop.
- `predict_directional`'s extended top/left arrays moved from two per-block
  `vec!` allocations to fixed stack scratch (bounded and asserted at run time).
- New `transform::inverse_2d_with_scratch` for callers that already own a
  buffer; `inverse_2d` is now a thin allocating wrapper over it.

Measured with `cargo bench -p tpt-kinetix-lean` at 320x240 (Criterion's own
change figures, which compare consecutive runs of the two binaries on this
machine):

- `lean_320x240/encode`: 477 -> 610 Kelem/s (**+28%**)
- `lean_320x240/decode`: 11.7 -> 13.8 Melem/s (**+18%**)

### Security

- **Sequence headers now reject `max_block_size_log2 > 6`.** Both
  `min_block_size_log2` and `max_block_size_log2` are 4-bit fields and only
  `min <= max` was validated, while `block_sizes` derives the reconstruction
  block size from `min_block_size_log2`. A stream could therefore declare a
  32k×32k minimum block and make the decoder request a multi-gigabyte
  allocation from a handful of bytes of input. v1 defines blocks in the
  8x8..64x64 range, so larger values are now rejected at parse time — which is
  also what makes the fixed-size scratch buffers above sound.

Decoded and encoded output is unchanged: all 30 lean tests pass, including the
`qp == 0` bit-exact round-trip suite this crate's lossless guarantee rests on.

## [0.1.0](https://github.com/tpt-solutions/tpt-kinetix/releases/tag/v0.1.0) - 2026-07-19

### Added

- Initial `tpt-kinetix-lean` crate: an original, embedded-first video codec
  designed by this project (no external standard to conform to).
- Sequence/frame header parsing and validation, declaring max frame dimensions
  and reference count up front so the decoder arena is sized once.
- rANS entropy-coding primitives with multi-stream framing (interleaved,
  independently decodable sub-streams rather than a bit-serial chain).
- `LeanDecoder` with `capabilities()` reporting `pixel_exact = false`; strict
  mode returns `KinetixError::NotPixelExact` until reconstruction lands.
- Fuzz target for the header/entropy path.
- Design rationale documented in the crate-level docs in `src/lib.rs`.

### Known limitations

- Block reconstruction (intra/inter prediction, transform, in-loop filter) is
  not implemented; `decode()` returns `Ok(None)` once a frame header parses.
