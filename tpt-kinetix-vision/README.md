# `tpt-kinetix-vision`

Video-for-machines codec: optimizes downstream ML model accuracy per bit rather than human perceptual quality.

## Status

**Scaffold.** The decoder reports `pixel_exact: false` and returns `NotPixelExact` in strict mode until the reconstruction pipeline is implemented.

## Design

Vision is an original codec (not ported from FFmpeg) with a dual-path decode contract:

- `decode_tensor()` — fast path: decodes entropy and coefficient layers, then dequantizes to produce a feature `Tensor`. No inverse transform, no prediction, no deblocking.
- `decode_pixels()` — slow path: full reconstruction pipeline producing a `VideoFrame` for human review.

The primary consumer is a detector/classifier backbone (e.g. YOLO/DETR-family). The bitstream is optimized for feature preservation at low bitrate, not SSIM/PSNR.

See [`docs/vision-codec-design.md`](../docs/vision-codec-design.md) for the full design specification.

## Usage

```rust
use tpt_kinetix_vision::{VisionDecoderImpl, VisionDecoder};

let mut dec = VisionDecoderImpl::new();
let caps = dec.capabilities();
assert!(!caps.pixel_exact);

// In non-strict mode, decode returns None until reconstruction is implemented.
let pkt = /* a compressed Packet */;
let tensor = dec.decode_tensor(&pkt)?; // Ok(None) today
let frame = dec.decode_pixels(&pkt)?;  // Ok(None) today
```

## Adding a codec

This crate was scaffolded following the process documented in [`docs/adding-a-codec.md`](../docs/adding-a-codec.md).
