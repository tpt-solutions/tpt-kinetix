# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- **Performance: the `TPT_VP9_*` debug switches no longer cost a full
  environment lookup in the decoder's inner loops.** All 25 debug-variable
  reads in the crate now go through a new `dbg_env` module, which scans the
  environment once per `decode` call and, while no `TPT_VP9_*` switch is set,
  answers from a single relaxed atomic load instead of locking the environment,
  scanning it and allocating an `OsString`.

  This was the single largest win in Phase 3. Two of the switches were in the
  innermost loops:
  - `booldec::read_bool` consulted `TPT_VP9_TRACE` **once per bool decoded** —
    millions of times per frame, for every coefficient token and every mode.
  - `loop_filter::loop_filter_edge` consulted `TPT_VP9_DBG56` **once per
    deblocking edge of every superblock**, and evaluated it *before* the cheap
    `off == 56` test that gates the actual debug output. The cheap integer
    comparison is now tested first.

  Measured on the cached `testsrc` corpus (decode only, A/B by reverting only
  these source changes, identical frame counts):
  - 320x240: 20.64s -> 8.00s for 18000 frames (**2.58x**)
  - 1920x1080: 38.29s -> 19.51s for 2400 frames (**1.96x**)

- The guard mirrors `tpt-kinetix-av1`'s existing `dbg_env`, which had already
  been written for the same reason — AV1 had it, VP9 did not.

Decoded output is **unchanged**: the libvpx row of the `ffmpeg_compare` harness
still reports `verified=true` (byte-identical planes vs ffmpeg), and all 13
`conformance_vp9` tests plus the rest of the crate's suites pass. A new unit
test asserts the fast path never makes a *set* switch invisible, so the
`TPT_VP9_*` debugging tools keep working.

## [0.1.0](https://github.com/tpt-solutions/tpt-kinetix/releases/tag/v0.1.0) - 2026-07-19

### Added

- Initial `tpt-kinetix-vp9` crate: native Rust VP9 decoder implementing the
  Bitstream & Decoding Process specification (RFC 9628) for profile 0
  (8-bit 4:2:0).
- Uncompressed and compressed header parsing, per-segment derived quantizers
  and loop-filter levels.
- Bool (range) decoder, round-trip tested against a reference bool encoder.
- Normative probability / scan / filter tables, extracted mechanically from a
  pinned FFmpeg commit and re-verified with
  `cargo run -p tpt-kinetix-kg -- verify-tables src/tables.rs` (26/26).
- Tile and superblock decode: partition trees, mode info, motion vectors,
  coefficients, intra prediction and motion compensation.
- Loop filtering, frame-context adaptation and reference management.
- Superframe packet splitting.
- `Vp9Decoder::capabilities()` reporting `pixel_exact: true`; strict mode
  rejects streams outside the supported subset with `KinetixError::Unsupported`.
- ffmpeg-gated conformance corpus (13 clips: lossless/lossy, content, intra-only,
  inter, odd size 125x67, multitile) asserting byte-exact output on every plane
  of every frame against `ffmpeg -c:v vp9`.
- Pipeline integration via the `codec-vp9` feature and `Vp9DecodeStage`.
- CLI wiring for `probe` and `transcode --vcodec av1` (VP9 is the royalty-free
  transcode input path).

### Other

- rename kinetix-* crates to tpt-kinetix-*, add probe subcommand and CI jobs
