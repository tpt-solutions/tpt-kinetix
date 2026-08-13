# `tpt-kinetix-volumetric`

Point-cloud / volumetric codec for AR-VR content, designed from scratch for the TPT Kinetix media processing engine.

## Status

**Scaffold.** The decoder reports `pixel_exact: false`, returns `NotPixelExact` in strict mode, and `Unsupported` for any stream declaring the reserved `dynamic` flag. Geometry (octree) and attribute (lift/RAHT) decode are not yet wired.

## Design

`volumetric` encodes/decodes **3D point clouds** — the dominant representation for captured volumetric / AR-VR content (Depthkit, 8i, LiDAR / depth fusion). The data shape is fundamentally 3D and unstructured (a set of positions plus per-point attributes), with no fixed 2D tiling, which is why it is a separate representation from the 2D frame codecs.

v1 targets a **static single cloud**:
- **Geometry:** context-modeled occupancy **octree**.
- **Attributes:** **region-adaptive predictive (lift)** (default) or **RAHT** (selectable), both lossless and lossy.
- **Framing:** G-PCC-faithful coding tools wrapped in Kinetix framing (`magic b"VOLU"`); MPEG-I G-PCC TMC13 is the bit-exact conformance oracle.

The decoded output is a `tpt_kinetix_core::frame::PointCloud` (positions + per-point attribute channels), parallel to `VideoFrame` for the 2D codecs.

See [`docs/volumetric-codec-design.md`](../docs/volumetric-codec-design.md) for the full design specification (all 8 design decisions resolved).

## Usage

```rust
use tpt_kinetix_volumetric::{VolumetricDecoderImpl, VolumetricDecoder};

let mut dec = VolumetricDecoderImpl::new();
let caps = dec.capabilities();
assert!(!caps.pixel_exact);

// In non-strict mode, decode returns None until geometry/attribute decode is implemented.
let pkt = /* a compressed Packet */;
let cloud = dec.decode(&pkt)?; // Ok(None) today
```

## Adding a codec

This crate was scaffolded following the process documented in [`docs/adding-a-codec.md`](../docs/adding-a-codec.md).
