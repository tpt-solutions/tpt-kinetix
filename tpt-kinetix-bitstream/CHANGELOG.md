# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- **Performance: `BitReader::read_bits` now refills a 64-bit window** instead of
  stepping one bit at a time. This is the single hottest shared primitive in the
  workspace (every original-format codec parses headers through it).
  `read_bits_16` 193 -> 308 MiB/s (+59%), `read_u32_be` 203 -> 620 MiB/s (+205%)
  on the 1 MiB Criterion payload. The bit-at-a-time path is retained for the tail,
  where fewer than the window's bytes remain, so the exact exhaustion and
  partial-consumption semantics are unchanged (`read_bit` is unchanged apart from
  being marked `#[inline]`).
- **Performance: `SkewedModel` carries a flat inverse table** (`inv[c]` = the
  symbol owning slot `c`, 4 KiB of `u8` for the 4096-slot alphabet) so
  `SymbolModel::find` is one indexed load rather than a binary search over 257
  cumulative frequencies on the rANS decode inner loop. The micro-benchmark
  `bitstream_rans/decode_noise` goes 41.3 -> 181.0 MiB/s (+338%), and the
  rANS-heavy `tpt-kinetix-lossless` 1080p 10-bit decode goes 11.9 -> 35.0
  Melem/s (+195%). All decode output is bit-identical: the inverse table is
  asserted equal to the previous "largest `s` with `cum[s] <= c_freq`" definition
  for every one of the 4096 slots, across five skew values.

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
