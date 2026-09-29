# `tpt-kinetix-vision`

Video-for-machines codec: optimizes downstream ML model accuracy per bit rather than human perceptual quality.

## Status

**Dual-path decode implemented** (an original codec — there is no external
reference oracle, so `capabilities().pixel_exact` is `false` and strict mode
returns `NotPixelExact` by design):

- `decode_tensor()` — fast path: entropy + dequantization + stride-16 pooling
  into a feature `Tensor`. No inverse transform, no prediction, no deblocking.
- `decode_pixels()` — slow path: full reconstruction (14-mode intra +
  unidirectional-P inter prediction, Walsh–Hadamard transform, deblocking)
  producing a `VideoFrame`.
- 8-bit, `chroma_present = 0` (luma-only) or 4:2:0, block sizes 8x8..64x64,
  three built-in ML-weighted quantization matrices.
- Declared-but-unimplemented features reject with `Unsupported` (10-bit,
  fractional qp, multi-stream entropy, embedded quant matrices).

The primary consumer is a detector/classifier backbone (e.g. YOLO/DETR-family). The bitstream is optimized for feature preservation at low bitrate, not SSIM/PSNR.

See [`docs/vision-codec-design.md`](../docs/vision-codec-design.md) for the full design specification.

## Usage

```rust
use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_vision::{SequenceHeader, VisionDecoder, VisionDecoderImpl};

let mut dec = VisionDecoderImpl::new();
dec.set_sequence_header(/* the stream's SequenceHeader */);
let caps = dec.capabilities();
assert!(!caps.pixel_exact);

let pkt: Packet = /* 15-byte frame header + payload_len bytes of rANS payload */;
let tensor = dec.decode_tensor(&pkt)?; // fast path
let frame = dec.decode_pixels(&pkt)?;  // slow path (DPB-managed inter decode)
```

End-to-end without a file: `cargo run -p tpt-kinetix-cli -- vision --demo`
(add `--tensor` for the fast path only).

## Adding a codec

This crate was scaffolded following the process documented in [`docs/adding-a-codec.md`](../docs/adding-a-codec.md).
