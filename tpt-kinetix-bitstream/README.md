# tpt-kinetix-bitstream

Shared bitstream primitives for the TPT Kinetix original video codecs
(`tpt-kinetix-lean`, `tpt-kinetix-vision`, `tpt-kinetix-realtime`).

This crate is the single source of truth for the low-level machinery those
codecs share, so it is implemented, tested, and fuzzed exactly once:

- `BitReader` — MSB-first bit-level reader over a byte slice.
- `RansEncoder` / `RansDecoder` / `RansStreamSet` / `SymbolModel` — byte-oriented
  rANS entropy coding with independently-decodable sub-stream framing.

See `docs/realtime-codec-design.md` (DECISION 7) for the extraction rationale.
