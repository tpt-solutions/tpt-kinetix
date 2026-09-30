# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://github.com/tpt-solutions/tpt-kinetix/releases/tag/v0.1.0) - 2026-07-19

### Added

- Initial `tpt-kinetix-bitstream` crate: shared bitstream primitives for the
  TPT Kinetix original codecs (`lean`, `vision`, `realtime`, `screen`, `face`,
  `lossless`, `volumetric`).
- `BitReader` — MSB-first bit-level reader over a byte slice.
- `RansEncoder` / `RansDecoder` / `RansStreamSet` / `SymbolModel` — byte-oriented
  rANS entropy coding with independently-decodable sub-stream framing.
- Extracted from `tpt-kinetix-lean` (realtime codec DECISION 7) so the low-level
  machinery is implemented, tested and fuzzed exactly once.
