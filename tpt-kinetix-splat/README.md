# `tpt-kinetix-splat`

Gaussian splat I/O for the TPT Kinetix media processing engine: 3D Gaussian Splatting (3DGS) PLY
and SPZ files, a native static codec (`SPLT`), a 4D time-sliced sequence container (`SPLS`), glTF
export, and MP4 carriage of sequences.

## Status

**Experimental, not yet published to crates.io.** Read and write work for the formats below. The
`tpt-kinetix splat` CLI and the package and MP4 integrations are built on this crate.

| Format | Read | Write | Notes |
| --- | --- | --- | --- |
| 3DGS `.ply` | ✅ | ✅ | INRIA property order. Frame (RDF or RUB) recorded in a header comment. |
| `.spz` | ✅ v2, v3, v4 | ✅ v4 | Checked against the reference decoder. Files with extensions are refused on read. |
| `.splt` (static) | ✅ | ✅ | Lossless or lossy. Version 2 uses residual contexts; version 1 still reads. |
| `.spls` (4D sequence) | ✅ | ✅ | Time-slab chunks, each independently decodable. `slabs_at(t)` seeks to a time. |
| glTF `.glb` | — | ✅ | Static clouds only, with `KHR_gaussian_splatting`. |
| MP4 `spls` track | ✅ | ✅ | One sample per time slab (via `tpt-kinetix-ops`). |

## Design

Splat math comes from [`tpt-math-splat`](https://crates.io/crates/tpt-math-splat)
(covariance from rotation and scale, 4D conditioning on time, EWA projection,
spherical harmonics, Morton/Hilbert keys). This crate is the I/O and codec layer
on top of it.

The data model is `GaussianCloud` for 3D splats and `GaussianCloud4D` for 4D splats. One
model covers 3DGS and 4DGS; a static 3D cloud is the degenerate case. It is kept separate from
`tpt-kinetix-volumetric`, whose `PointCloud` has no covariance, opacity or spherical-harmonic
coefficients.

## Using it

```sh
tpt-kinetix splat probe scene.ply
tpt-kinetix splat convert scene.ply scene.splt --lossy --position-bits 16 --attribute-bits 8
tpt-kinetix splat convert scene.spz scene.glb
tpt-kinetix splat convert sequence.spls sequence.mp4
```

From Rust, use `read_ply` / `write_ply`, `read_spz` / `write_spz`, `read_splt` / `write_splt`, and
the `sequence` module for `SPLS`.

## Correctness

- **SPZ:** the reader matches the reference `spz_to_ply` on v2 and v4: positions, scales, opacity
  and SH are bit-equal, and rotations are within 2e-5. The writer's output is decoded exactly by
  the reference. The oracle tests skip unless `SPZ_ORACLE_DIR` points at the generated files.
- **SPLT and SPLS:** round trips are checked by proptests (lossless bit-exact up to point order,
  lossy within half a quantisation step), and by a test on the reference cloud. No external
  reference decoder exists for SPLT.
- **4D:** no 4D reference exists. `SPLS` is tested only against itself, plus a CPU render check
  that compares a decoded slab with the source (lossless 83–100 dB, lossy 16/8 about 71 dB).
- **Fuzzing:** targets for PLY, SPZ, SPLT and SPLS. Run them on Linux; the Windows nightly has
  no ASan runtime.

## Known limits

- **Coordinate frames:** RUB ↔ RDF conversion is not implemented. Writers do not convert, and SPZ
  cannot store the frame tag.
- **SPZ extensions:** refused on read and not written.
- **glTF validation:** the `KHR_gaussian_splatting` attributes have not been validated by an
  extension-aware validator. The published `gltf-validator` does not know the extension.
- **glTF compression:** the SPZ compression extension is not implemented. No ratified spec exists.
- **4D input:** sequences come only from `SPLS` or MP4 `spls` tracks. There is no 4D PLY or SPZ
  source format.

Licensed under MIT OR Apache-2.0. Changes are listed in [CHANGELOG.md](CHANGELOG.md).
