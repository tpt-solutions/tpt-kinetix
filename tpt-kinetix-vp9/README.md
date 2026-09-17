# tpt-kinetix-vp9

A VP9 video decoder for the
[TPT Kinetix](https://github.com/tpt-solutions/tpt-kinetix) media engine.
Native Rust implementation of the VP9 Bitstream & Decoding Process
Specification (RFC 9628), profile 0 (8-bit 4:2:0).

## Status

**In progress — structurally complete, not yet pixel-exact.** The full decode
pipeline is implemented and runs end-to-end on real `libvpx-vp9` streams:
uncompressed header, compressed header (probability updates), bool decoder,
tiles and superblock partitions, intra/inter mode info with the complete
reference-selection ladders, MV prediction, coefficient decoding, inverse
transforms, motion compensation (8-tap, unscaled and scaled references) and
the deblocking loop filter.

What is **not** done yet: the reconstruction output does not match a reference
decoder pixel-for-pixel (`capabilities().pixel_exact == false`). Decoded frames
have the correct geometry but wrong samples. The next debugging step (per the
conformance corpus in `tests/conformance_vp9.rs`) is to trace the first
keyframe's skip-flag / mode parsing against `ffmpeg -c:v vp9`, then bisect
coefficients → prediction → loop filter as the AV1 crate did.

## Capabilities

- `Vp9Decoder::capabilities()` reports `pixel_exact: false` while the output
  is not reference-exact; strict mode returns `KinetixError::NotPixelExact`.
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
