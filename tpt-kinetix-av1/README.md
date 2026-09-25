# tpt-kinetix-av1

AV1 support for the TPT Kinetix engine: OBU-level bitstream parsing, a decoder
scaffold, and a `rav1e`-backed encoder.

See the [workspace README](../README.md) for the full project overview,
architecture diagram, and quickstart guide.

## Status & known limitations

### Encoder (functional)

- `Av1Encoder` wraps `rav1e` with a safe API (`encode_frame`, `flush`).
- Accepts codec-agnostic `tpt_kinetix_core::EncodeConfig` via
  `Av1Encoder::from_encode_config` (rate control, speed preset, keyframe
  interval), or the crate-local `Av1EncoderConfig`.
- Consumes YUV420p `VideoFrame`s and produces AV1 `Packet`s.

### Decoder (intra + inter reconstruction)

- `Av1Decoder` parses OBUs and reconstructs real intra and inter frames through
  the AV1 symbol decoder, partition/mode syntax, transforms, deblocking, CDEF,
  loop restoration, reference management, and temporal MV reconstruction.
- The local synthetic intra corpus is 6/6 byte-exact against dav1d. The synthetic
  inter corpus has four of five entries byte-exact; the last differs by one V
  sample in one frame.
- The decoder is **not yet pixel-exact** on the official FFmpeg FATE AV1 set:
  the current run passes 1/198 comparable frames. Decoder-model, film-grain, and
  Annex-B samples are classified as expected-unsupported by the harness. The
  closest official keyframe (`frames_refs_short_signaling` frame 0) now reaches
  ~67 dB luma PSNR against dav1d after fixing the CDEF strength-table read
  order; the residual is a genuine reconstruction gap, not a header desync. See
  `todo-av1.md` for the remaining official-vector gaps.
- `capabilities().pixel_exact` remains `false`; strict mode returns
  `KinetixError::NotPixelExact` rather than claiming conformance.

### Fuzzing

- `cargo fuzz run fuzz_obu_parse` exercises the OBU parser against arbitrary input.
