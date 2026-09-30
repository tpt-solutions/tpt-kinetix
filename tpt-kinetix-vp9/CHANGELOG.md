# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
