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

### Decoder (intra keyframe reconstruction)

- `obu` parses OBU headers, LEB128 sizes, and the Sequence Header.
- `Av1Decoder` sequences OBUs; for intra-coded keyframes,
  `reconstruct::decode_tile_group` performs tile-group reconstruction through
  the real AV1 symbol (arithmetic) decoder in `entropy`, driven by the spec
  `coeffs()` syntax in `coeff` (`crate::coeff`). Coefficient contexts,
  transform-type selection, and end-of-block signalling all follow AV1
  §5.11.39 / §8.3.2 using the mechanically-extracted tables in
  `coeff_tables` and `entropy_cdf`.
- **Scope of the current (Phase B) decoder**: intra blocks only, square
  4×4 / 8×8 / 16×16 transform sizes, and the simplified inverse transforms
  in `reconstruct` (DCT, identity, 4×4 ADST). The surrounding block
  structure is still a fixed placeholder grid (DC-predicted 8×8 luma + 4×4
  U/V blocks) rather than a real superblock partition tree (AV1 Phase C), so
  output is **not yet pixel-exact** against `dav1d` and a real stream will
  fail loudly (the frame is rejected rather than decoded into silent garbage).
- Inter prediction, non-square transforms, the full AV1 transform set, and
  loop filtering are **not** implemented.
- Validation against `dav1d` is wired through `tpt-kinetix-test-utils::reference`
  and will be enabled once the partition/mode syntax (Phase C) lands.

### Fuzzing

- `cargo fuzz run fuzz_obu_parse` exercises the OBU parser against arbitrary input.
