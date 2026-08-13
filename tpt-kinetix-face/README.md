# tpt-kinetix-face

A talking-head / video-conferencing codec for the
[TPT Kinetix](https://github.com/tpt-solutions/tpt-kinetix) media engine that
uses **landmark-driven parametric synthesis** instead of pixel coding.

> **Status: scaffold.** The decoder reports its capabilities honestly and
> returns `Ok(None)` until the synthesis pipeline is implemented. Full design
> (all 8 resolved decisions): [`docs/face-codec-design.md`](../../docs/face-codec-design.md).

## Why not just use AV1/H.264?

General codecs treat a face as arbitrary natural image content and spend bits
on background, lighting falloff, and skin micro-texture that a parametric face
model reproduces for free from a tiny control vector (~100–300 scalars/frame).
For conferencing the background is often static or synthetic, so the structural
prior the face model carries is exactly the signal general codecs discard.

## Current capabilities

- `FaceParams` type (the 3DMM-style parameter vector: identity / expression / pose / illumination / appearance)
- `FaceSynthesizer` trait seam (deterministic rasterizer planned for v1)
- `FaceDecoder::capabilities()` reports `pixel_exact = false` — output is **synthesized, not pixel-exact**, by design

## License

MIT OR Apache-2.0
