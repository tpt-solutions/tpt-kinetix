# tpt-kinetix-vp9

A VP9 video decoder for the
[TPT Kinetix](https://github.com/tpt-solutions/tpt-kinetix) media engine.
Native Rust implementation of the VP9 Bitstream & Decoding Process
Specification (RFC 9628), profile 0 (8-bit 4:2:0).

## Status

**Pixel-exact for the supported subset.** The full decode pipeline is
implemented and runs end-to-end on real `libvpx-vp9` streams: uncompressed/
compressed headers, bool decoder, tiles and superblock partitions, intra/inter
modes, MV prediction, coefficients, inverse transforms, motion compensation,
loop filtering, frame-context adaptation, and reference management.

`capabilities().pixel_exact == true`: the whole ffmpeg-gated conformance corpus
(13 clips: lossless/lossy solids and micro-clips, content clips, intra-only,
inter, odd size 125x67, multitile) decodes byte-exact against
`ffmpeg -c:v vp9` on every plane of every frame, and the test asserts it.
The conformance-frontier work (per-block loop-filter mask construction and the
persistent loop-filter delta header state) is documented in `todo-vp9.md`.

## Capabilities

- `Vp9Decoder::capabilities()` reports `pixel_exact: true` for the supported
  profile 0 subset; strict mode rejects streams outside it (other profiles
  with `KinetixError::Unsupported`).
- Profile 0 only (8-bit, 4:2:0). Other profiles are rejected with
  `KinetixError::Unsupported`.
- Superframe packets are split internally.
- Frame-parallel and context-adaptation probability semantics are implemented.

## Layout

- `bitreader` — MSB-first reader for the uncompressed header.
- `booldec` — the VP9 bool (range) decoder, round-trip tested against a
  reference bool encoder.
- `tables` — normative probability/scan/filter tables, extracted mechanically
  from a pinned FFmpeg commit and re-verified with
  `cargo run -p tpt-kinetix-kg -- verify-tables src/tables.rs` (26/26).
- `header` — uncompressed + compressed header parsing, per-segment derived
  quantizers/loop-filter levels.
- `frame` / `frame_mode` / `frame_recon` — tile & superblock decode (partition
  trees, mode info, MVs, coefficients, reconstruction).
- `predict` — intra prediction and motion compensation.
- `transform` — inverse DCT/ADST/WHT.
- `loop_filter` — the deblocking filter.
- `decoder` — `Vp9Decoder`: packet sequencing, reference management, frame
  context adaptation.

## Testing

- `cargo test -p tpt-kinetix-vp9` — unit tests (bool decoder round-trip,
  bit reader), conformance (ffmpeg-gated, skips when absent), and a
  `*_never_panics` proptest.
- `cargo +nightly fuzz run fuzz_vp9_frame` from `fuzz/` (compile-only on CI;
  the CI `fuzz-check` job builds it on Ubuntu).

## License

MIT OR Apache-2.0
