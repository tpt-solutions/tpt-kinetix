# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
