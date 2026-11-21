# `tpt-kinetix-splat`

4D Gaussian splat I/O for the TPT Kinetix media processing engine.

## Status

**Experimental, scaffolding only (M0).** No parsers or codec are implemented yet.
Planned, in order: a data model covering 3D and 4D Gaussians, 3DGS PLY and SPZ
interop, the native `SPLT` sequence format (Hilbert-ordered, quantised, time-slab
chunks), then MP4 packaging and CLI support.

## Design

Splat math comes from [`tpt-math-splat`](https://crates.io/crates/tpt-math-splat)
(covariance from rotation and scale, 4D conditioning on time, EWA projection,
spherical harmonics, Morton/Hilbert keys). This crate is the I/O and codec layer
on top of it.

It is kept separate from `tpt-kinetix-volumetric`: that crate is a static
point-cloud codec whose `PointCloud` type carries positions and colour/normal/reflectance
attributes. Splats need covariance, opacity and spherical-harmonic coefficients,
which that data model does not have.

## Correctness

Like the other original-format codecs, `SPLT` will be pinned by golden vectors
rather than an external reference decoder. Interop formats will be checked
against reference files. No ffmpeg oracle exists for splats.

Licensed under MIT OR Apache-2.0.
