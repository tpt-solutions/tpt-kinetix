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

- `basis` module — 3DMM basis asset + loader (DECISION 2/3/6, step 1): a
  fixed, versioned basis (base mesh, identity/expression displacement bases,
  mean albedo) with a `basis_hash` so a decoder with a mismatched basis rejects
  rather than rendering a wrong face. The built-in basis is a **deterministic
  placeholder** (procedural head proxy); selecting a production 3DMM (FLAME /
  FaceWarehouse) is open question 1.
- `synthesizer` module — deterministic 3DMM rasterizer (`DeterministicRasterizer`,
  DECISION 2 / step 4): displaces the mesh by identity + expression bases,
  places it via pose, shades by a Lambert/SH model, and z-buffer rasterizes to
  an RGB24 frame. No neural network on the v1 decode path.
- `FaceDecoder` — end-to-end: parses the sequence header, loads + verifies the
  pinned basis, rANS-decodes each frame's parameter vector, and runs the
  synthesizer. Strict mode returns `NotPixelExact` on a missing/mismatched basis
  (DECISION 8). `capabilities()` reports `pixel_exact = false` (synthesized, by
  design).
- `FaceEncoder` — assembles a sequence header + key-frame payload from
  `FaceParams` for encode/decode round-trip testing.
- `params` module — parameter-vector codec (DECISION 3): rANS-encodes the five
  coefficient groups (`FaceParamCodec`) as independent sub-streams via
  `tpt-kinetix-bitstream`'s `RansStreamSet`, with a zero-biased `FaceCoefModel`
  for the delta groups; per-group dequantization from `group_qp`; identity sent
  once per call (key frame), per-frame expression/pose/illumination/appearance
  deltas for inter frames.
- `header` module — byte-aligned sequence/frame header parsing & (de)serialization
  per DECISION 3 (`magic`/`version`/`basis_hash` pin, `group_qp`, optional
  per-frame qp override); fails loudly on bad magic / unsupported version /
  truncation.
- `representation` module — the v1 representation decision (DECISION 1):
  `FaceRepresentation` (3DMM primary, sparse-landmark companion, learned latent
  deferred to v2) + nominal `V1_3DMM_DIMS`.
- `FaceParams` type (the 3DMM-style parameter vector: identity / expression / pose / illumination / appearance)
- `FaceSynthesizer` trait seam (deterministic rasterizer planned for v1)
- `FaceDecoder::capabilities()` reports `pixel_exact = false` — output is **synthesized, not pixel-exact**, by design

## License

MIT OR Apache-2.0
