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
- Bit-exact against dav1d on the official FFmpeg FATE AV1 set (204/204 frames,
  including film grain, 10-bit and 4:2:2/4:4:4 streams), on the local synthetic
  intra/inter corpora, and on a libaom-encode crosscheck
  (`tests/libaom_crosscheck.rs`). Reproduce the FATE number with
  `KINETIX_AV1_FATE_DIR=<dir> cargo run --release -p tpt-kinetix-av1 --example av1_fate_score`.
- `capabilities().pixel_exact` is `true`. See `docs/CONFORMANCE.md` at the
  workspace root for the generated results table.

### Fuzzing

- `cargo fuzz run fuzz_obu_parse` exercises the OBU parser against arbitrary input.
