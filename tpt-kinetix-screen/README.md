# tpt-kinetix-screen

An original **screen/UI-capture codec** for the
[TPT Kinetix](https://github.com/tpt-solutions/tpt-kinetix) media engine.

Screen content is not natural imagery: it has sharp edges, large flat regions,
and repeated glyph/UI elements that general-purpose codecs (H.264/AV1/HEVC)
waste bits on. This codec classifies every block into one of three modes:

- **`FLAT`** — solid color / simple gradient (run-length coalesced).
- **`GLYPH`** — a reference into a cross-frame glyph/palette dictionary plus
  fg/bg colors (exploits repeated UI elements across frames).
- **`NATURAL`** — a transform/entropy-coded block for the occasional embedded
  photo or video region.

The classifier and dictionary are screen-specific; the entropy backend and the
`NATURAL` fallback reuse the shared `tpt-kinetix-bitstream` primitives.

> **Status: scaffold.** The byte-aligned sequence/frame headers and the shared
> `BitReader`/rANS primitives exist and round-trip, but block reconstruction
> (mode classifier, flat-fill run-length, glyph dictionary + palette, and the
> `NATURAL` transform path) is not implemented yet. `ScreenDecoder::capabilities`
> reports `pixel_exact = false`.

## Design

Full design notes (all resolved decision points): [`docs/screen-codec-design.md`](../../docs/screen-codec-design.md).

## License

MIT OR Apache-2.0
