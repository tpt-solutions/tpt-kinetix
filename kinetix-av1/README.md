# kinetix-av1

AV1 support for the TPT Kinetix engine: OBU-level bitstream parsing, a decoder
scaffold, and a `rav1e`-backed encoder.

See the [workspace README](../README.md) for the full project overview,
architecture diagram, and quickstart guide.

## Status & known limitations

### Encoder (functional)

- `Av1Encoder` wraps `rav1e` with a safe API (`encode_frame`, `flush`).
- Accepts codec-agnostic `kinetix_core::EncodeConfig` via
  `Av1Encoder::from_encode_config` (rate control, speed preset, keyframe
  interval), or the crate-local `Av1EncoderConfig`.
- Consumes YUV420p `VideoFrame`s and produces AV1 `Packet`s.

### Decoder (scaffold)

- `obu` parses OBU headers, LEB128 sizes, and the Sequence Header.
- `Av1Decoder` sequences OBUs and extracts frame geometry, but **emits
  placeholder grey frames** — full tile/frame reconstruction (transform,
  prediction, loop filters, film grain) is not yet implemented and output is
  **not pixel-exact**.
- Validation against `dav1d` is wired through `kinetix-test-utils::reference`
  and will be enabled once real reconstruction lands.

### Fuzzing

- `cargo fuzz run fuzz_obu_parse` exercises the OBU parser against arbitrary input.
