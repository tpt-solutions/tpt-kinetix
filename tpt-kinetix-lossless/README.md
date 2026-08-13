# `tpt-kinetix-lossless`

A **bit-exact reversible** codec for high-bit-depth still/frame data — medical
imaging (DICOM CT/MR/X-ray), scientific capture (sensor / frame-grabber feeds),
and archival preservation of high-bit-depth masters. Unlike perceptual codecs
(AV1/HEVC/H.264) or [`tpt-kinetix-lean`](../tpt-kinetix-lean) (bounded-time
embedded *lossy* video), this codec has **no quality axis**: every preserved bit
is verified.

See [`docs/lossless-codec-design.md`](../../docs/lossless-codec-design.md) for the
full design (DECISION 1–6).

## v1 format

- **Unified format** across medical / scientific / archival domains, guaranteeing
  bit-exact round-trip for **10/12/16-bit** samples (per-plane `bit_depth` field).
- **Predictive + entropy** (FFV1-like): each sample is predicted from its
  left/up/up-left neighbours via FFV1's median predictor; the signed residual is
  Rice-coded with a per-sample adaptive parameter.
- **Built-in reversibility contract (DECISION 3):** every plane carries a CRC of
  its reconstructed samples (CRC-32 for <16-bit, CRC-64 for 16-bit). The decoder
  verifies it and returns an error on mismatch — losslessness is part of the
  format, not just external testing.
- **Forward-compatible:** a `transform_id` field reserves a reversible-wavelet
  mode for a later phase; a v1 decoder rejects any non-zero `transform_id` with
  [`KinetixError::Unsupported`] instead of decoding to silent garbage.
- Reuses the shared `tpt-kinetix-bitstream` primitives (`BitReader`, rANS), which
  originated in `tpt-kinetix-lean` and were factored into that crate (realtime
  codec DECISION 7).

## Status

This is an early **v1 scaffold**: the reversible predictive path is implemented
and bit-exact (verified by the round-trip tests). Entropy here is a simple
adaptive Rice coder; swapping in the shared rANS with a context-adaptive model
and adding the reserved wavelet mode are follow-up work (DECISION 2/4).

## Example

```rust
use tpt_kinetix_lossless::{LosslessEncoder, LosslessDecoder, Plane, SequenceHeader, PlaneSpec};

let seq = SequenceHeader {
    version: 1,
    max_width: 64,
    max_height: 64,
    transform_id: 0,
    planes: vec![PlaneSpec { bit_depth: 16 }],
};
let plane = Plane {
    width: 16,
    height: 16,
    bit_depth: 16,
    data: (0..256).map(|i| i as u16).collect(),
};

let bytes = LosslessEncoder::new().encode_frame(&seq, &[plane.clone()]).unwrap();
let decoded = LosslessDecoder::new().decode_frame(&seq, &bytes).unwrap();
assert_eq!(decoded[0], plane);
```

## Conformance

`decode(encode(x)) == x` for 10/12/16-bit single- and multi-plane synthetic
corpora, enforced by the crate's round-trip tests. A corrupted payload fails the
per-plane checksum and returns an error rather than wrong data.
