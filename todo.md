# TODO

## Open tasks

### 4D Gaussian splat support (`tpt-kinetix-splat`, new crate)

Built on `tpt-math-splat` 0.1.0 / `tpt-math-numeric` 0.2.0 (both already on crates.io). Full design:
the approved plan (new crate, not an extension of `tpt-kinetix-volumetric`: splats need covariance/opacity/SH, which `PointCloud` lacks).
One data model covers 3DGS and 4DGS (static 3D = degenerate case).

- [x] **M0 Scaffold** — workspace member + `[workspace.dependencies]` (`tpt-math-splat = "0.1.0"`), `README.md`, `CHANGELOG.md`,
      `release-plz.toml` entry, PATENTS.md row, README status row, `fuzz/` dir + `exclude` entry, `justfile` fuzz-build line; `just deny` passes.
      Bench list not added yet (no Criterion bench in the crate). `proptest` is pinned locally (`"1"`) because the workspace has no `proptest` entry.
- [~] **M1 Data model + interop** — done: `GaussianCloud` (3D, format-neutral; `[point][channel][coeff]` SH), `SplatError`,
      3DGS `.ply` read/write (INRIA property order, bounds checked before allocation), 9 unit + 4 proptest tests (never-panic on random and
      mutated input, bit-exact round trip). Fuzz target `fuzz_splat_ply` ran 60s in WSL Ubuntu-24.04 (`cargo +nightly fuzz run -s none`, no crashes). The Windows nightly
      has no ASan runtime, so Linux is the place to fuzz.
      Not started: 4D `SplatSequence` (M3).
      - [x] **SPZ read** (`src/spz.rs`): legacy gzip v2/v3 (`flate2`) and NGSP v4 (`ruzstd`). Reader matches the reference
        `spz_to_ply` exactly on v2 (horned, racoon) and v4 (horned): positions, scales, opacity, DC, rest SH are bit-equal, and rotation is
        within 2e-5. Output stays in SPZ's native RUB frame (the reference default, no conversion). Files with extensions (flag 0x2) are refused.
      - [x] **SPZ write** (`src/spz_write.rs`): v4 NGSP, each stream one ZSTD frame. Default feature `zstd-encode` compresses with
        libzstd (C, the only C in this crate; `aws-lc-sys` already in the workspace). Output is 19.7 MB vs the reference's 18.1 MB,
        and the reference decodes it exactly. Without the feature, streams are raw ZSTD blocks (51 MB).
      - [x] **SPZ v3 read/write** verified: a v3 file written by the reference (`saveSpz`, version 3) decodes through our reader exactly.
      - [x] **Coordinate frame tag**: `GaussianCloud::frame()` / `CoordinateFrame::{Rdf, Rub}`. PLY reads as RDF (3DGS convention,
        and the default), SPZ reads as RUB. The tag is informational; no conversion happens. Writers don't convert and SPZ can't store the tag.
      - [ ] **RUB↔RDF conversion**: not implemented. The reference's default SPZ→PLY does none, so we match it. Add it (with SH
        rotation, checked against the reference's `convertCoordinates`) when something needs it.
      - [ ] **SPZ extensions**: refused on read, not written.
      - Oracle tooling (WSL Ubuntu-24.04, `/root/oracle`): `spz_to_ply`, `ply_to_spz` built from nianticlabs/spz. Run the tests with
        `SPZ_ORACLE_DIR` pointing at the generated files (see `tests/spz_oracle.rs`); they skip without it.
      - Fuzz: `fuzz_splat_ply` (22.8M runs / 60s) and `fuzz_splat_spz` (8.7M runs / 60s) in Ubuntu-24.04, no crashes.
      Note: the 4D model is deferred. `GaussianCloud` is 3D only so far.
- [~] **M2 Static `SPLT` codec** (`src/splt.rs`) — done: header with magic/version/flags (lossy, RUB frame), Hilbert ordering
      (`tpt-math-splat::sfc`), per-channel quantisation with stored ranges, zigzag-varint delta residuals, rANS per channel
      (`tpt-kinetix-bitstream`). Lossless is bit-exact up to point order (the file stores sorted order, not the input order);
      lossy stays within half a quantisation step per channel. Verified by proptests (lossless round trip, lossy bound, stable
      re-encoding, never-panic), deterministic tests (NaN/inf, frame tag, bad headers, truncation), the reference cloud
      (`splt_oracle.rs`), and `fuzz_splat_splt` (930k runs / 60s, no crashes).
      **Compression (reference cloud, 786k splats, PLY = 195 MB):** the first entropy stage (a generic skewed model) expanded the
      data. Each channel now stores its own 256-entry frequency table (fitted to its byte histogram) ahead of its rANS stream.
      Results: lossless 31.8% of PLY; lossy 16/12 15.3%, 16/10 13.8%, **16/8 9.7%** (about SPZ's ~9%, so 16/8 is the
      operating point to compare against SPZ; SH dominates the cost at high attribute depths); 12/8 9.4%.
      Still open: smallest-three quaternion coding for rotations, context modelling across neighbouring points, and a seed
      corpus for `fuzz_splat_splt` (random bytes now fail at the table check, so deeper decode paths are covered only by proptests).
- [~] **M3 4D temporal chunking** — done: `GaussianCloud4D` model (`src/cloud4d.rs`: means xyzt, log scales, isoclinic quaternion
      pair, opacity, SH; temporal variance via `tpt-math-splat`), time-slab chunking (`src/chunk.rs`: a splat is in a slab when its
      `condition_on_time` weight at the nearest slab point exceeds `eps`; checked against `condition_on_time` by proptest),
      4D Hilbert ordering within each chunk, and the `SPLS` sequence container (`src/sequence.rs`: header, slab index with time
      and byte ranges, independently decodable slab payloads, `slabs_at(t)` seeking). Channel coding is shared with SPLT via `src/block.rs`.
      Verified: lossless slabs decode to exactly their chunk, lossy stays within bounds, never-panic proptests, and `fuzz_splat_spls`
      (72M runs / 60s, no crashes).
      Open: no 4D sample or reference exists, so the format is tested only against itself. Membership costs O(slabs × splats); a
      sweep would make it O(splats log splats) if sequences get large. Static 3D clouds are not converted to 4D.
      Render check (`tests/render_4d.rs`, M5 core): a test-only CPU renderer draws the source cloud and the decoded slab at the
      same time. Lossless is 83–100 dB and lossy 16/8 about 71 dB, against a negative control (different time) at 26 dB.
- [~] **M4 Container + CLI** — done: `tpt-kinetix splat convert` (PLY/SPZ/SPLT → SPLT lossless or lossy, or → SPZ/PLY) and
      `tpt-kinetix splat probe` (format, splat count, SH degree, frame; SPLS slabs and time range). Formats are detected from magic
      bytes. The PLY frame is recorded as a `comment tpt-kinetix frame rub` header line when the cloud is RUB, so the SPZ → PLY
      conversion no longer mislabels the frame. MP4: an SPLS sequence goes into an MP4 `spls` track with one sample per time
      slab (`tpt-kinetix-ops::splat_mp4`), via a new `CodecId::Spls` (core), a `splc` sample entry (mux), and a `meta` handler.
      The MP4 round-trips to byte-identical SPLS, and ffprobe reads the track (7 samples, 17.5 s). The CLI
      `splat probe` and `splat convert` handle sequences (SPLS <-> MP4), verified end to end. Open: 4D input from PLY/SPZ (no 4D source format yet).
- [ ] **M5 Verification renderer + extras** — CPU reference renderer (`condition_on_time` -> EWA -> sorted alpha blend), test-only oracle;
      render-PSNR tests (decode vs source); glTF `KHR_gaussian_splatting` (+ SPZ compression ext); time-slab segment packaging in `tpt-kinetix-package`.
      Progress: render check done (`tests/render_4d.rs`). Time-slab packaging done: `tpt-kinetix-package::splat` writes one standalone
      `SPLS` segment per slab (`seg-{k}.spls`), and a JSON playlist with time ranges, sizes and URIs (`splat_playlist`, `splat_segment`).
      Players don't read splat tracks, so this is Kinetix-to-Kinetix delivery.
      glTF: `tpt-kinetix-splat::gltf` writes static clouds as GLB with `KHR_gaussian_splatting` (ellipse kernel, srgb_rec709_display).
      RUB clouds export unchanged, and RDF (PLY) clouds get the 180° X flip (positions, quaternions, and SH signs read from the basis).
      Verified by a view-dependent colour invariance test, GLB container tests, and a CLI `.glb` output. Not verified: the official
      glTF validator, which could not run in this session (the npm download was denied). Run it before relying on the file.
      Not in scope: COLOR_0 fallback, and sequences (glTF has no 4D form).

Verification for every milestone: `just check`, `just deny`, fuzz >= 60s on any new parser, golden vectors committed.
Notes: write from papers/specs (original 3DGS code is non-commercial licensed, don't copy it); no ffmpeg oracle exists for splats.
View-dependent depth sorting stays out of scope for the codec.
