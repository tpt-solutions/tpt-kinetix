# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.0](https://github.com/tpt-solutions/tpt-kinetix/compare/v0.2.1...v0.3.0) - 2026-11-22

### Added

- 3DGS PLY read and write. The frame (RDF or RUB) is recorded in a header comment.
- SPZ read (v2, v3 and v4) and write (v4). Checked against the reference decoder. Files with extensions are refused on read.
- `SPLT` static codec, version 2: Hilbert ordering, per-channel quantisation, residual contexts, smallest-three rotations in lossy mode. Version 1 still reads.
- `SPLS` 4D sequences: time-slab chunking with independently decodable slabs and `slabs_at(t)` seeking.
- glTF export of static clouds as GLB with `KHR_gaussian_splatting` (ellipse kernel, linear scale and opacity).
- Criterion bench `splat_codec`, and fuzz targets for PLY, SPZ, SPLT and SPLS.

### Known limits

- The `KHR_gaussian_splatting` attributes have not been validated by an extension-aware validator. The published `gltf-validator` does not know the extension.
- The SPZ compression glTF extension is not implemented. No ratified spec exists yet.
- RUB ↔ RDF conversion is not implemented. Writers do not convert.
- SPZ extensions are refused on read and are not written.
- No 4D source format is read yet. 4D data can only come from SPLS input. No 4D reference exists, so the format is tested only against itself.
