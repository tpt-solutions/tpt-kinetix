# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
