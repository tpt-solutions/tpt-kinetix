# tpt-kinetix-h264

H.264/AVC bitstream decoder for the TPT Kinetix engine, parallelised with `rayon`
at the macroblock-row level.

See the [workspace README](../README.md) for the full project overview,
architecture diagram, and quickstart guide.

## Status & known limitations

`tpt-kinetix-h264` is **pixel-exact for a substantial subset** of H.264 and
honest about the rest. CAVLC *and* CABAC decode of I/P/B slices — intra
prediction, inter prediction (motion compensation), weighted prediction,
reference-picture management (DPB/POC/`ref_pic_list_modification`/MMCO), and the
in-loop deblocking filter — all decode **bit-exact** (max_abs_diff == 0) against
`ffmpeg` for 4:2:0, progressive, 16-px-aligned pictures without the 8×8
transform. This is exercised continuously by `tests/conformance_matrix.rs` and the
per-feature `*_conformance.rs` suites.

`H264Decoder::capabilities().pixel_exact` therefore remains `false` only because
of the genuine gaps below — the global flag is a hard honesty guarantee, not a
per-feature one. For any stream the decoder cannot decode pixel-exactly,
`H264Decoder::with_strict(true)` makes `decode()` return
`KinetixError::NotPixelExact` instead of emitting approximate frames.

This prose describes current reality; the canonical, machine-readable status is
`H264Decoder::capabilities()` (run `just conformance` to print it) and the CI
`conformance` job.

### Implemented

- Annex B and AVCC NAL unit extraction (`nal`)
- Emulation-prevention byte removal
- SPS parsing incl. high-profile extensions, frame cropping, and scaling lists (`sps`)
- PPS parsing incl. slice-group maps and `transform_8x8_mode_flag` (`pps`)
- Full slice-header parsing (§7.3.3), exposing `data_bit_offset` (`slice`)
- CAVLC residual parsing with **spec-exact** tables (Tables 9-5..9-10) (`cavlc_tables`)
- Slice-data parsing loop (§7.3.4): `mb_type`, `coded_block_pattern`,
  `mb_qp_delta`, I/P/B macroblocks with neighbour tracking (`slice_data`)
- CABAC arithmetic decoding engine + full I/P/B-slice context tables/binarizations
  (`entropy`, `cabac_tables`) — bit-exact vs `ffmpeg` for I/P/B slices
- Integer inverse transform + inverse quant: spec-exact 4×4 residual (§8.5.12),
  Intra_16×16 DC Hadamard (§8.5.10), and chroma DC transform (§8.5.11)
  (`transform`)
- Intra prediction — 4×4 / 8×8 / 16×16 luma modes and 4-mode chroma prediction
  (`prediction`), **bit-exact vs `ffmpeg`**
- Inter prediction / motion compensation — DPB + POC (§8.2.1), reference-list
  construction (§8.2.4), MV prediction (§8.4.1), 6-tap luma + bilinear chroma
  sub-pel interpolation (§8.4.2.2); **P/B-frame decode bit-exact vs `ffmpeg`**
  (`ref_pic`, `decoder`, `motion_comp`)
- B-slice parsing + direct (spatial/temporal) mode + bi-predictive motion
  compensation; **B-frame decode bit-exact vs `ffmpeg`** (`slice_data`, `mv`,
  `reconstruct`)
- Explicit **and** implicit weighted prediction (§8.4.2.3.2), including
  `pred_weight_table` wired through reconstruction (`reconstruct`)
- `ref_pic_list_modification` (§8.2.4.3) and `dec_ref_pic_marking` / MMCO 1–6
  (§8.2.5) wired into reference-picture management (`ref_pic`); visible via
  `H264Decoder::dpb`
- In-loop deblocking filter — `α`/`β`/`tC0`, per-4×4-block `bS`
  (coefficient-OR + MV/ref rule), strong/weak edge filtering for luma and
  chroma; **bit-exact vs `ffmpeg`** (`deblock`)
- `rayon` parallel macroblock-row reconstruction (`decoder`)

### Not yet pixel-exact / unsupported

- **8×8 transform** (`transform_8x8_mode_flag`) — parsed but not yet decoded
  (High-profile streams are rejected in strict mode). Tracked as Phase F.
- **Field / interlaced coding** (`frame_mbs_only_flag == 0`, MBAFF/PAFF) —
  rejected in strict mode. Tracked as Phase G.
- **Non-16-aligned picture dimensions** — cropped-edge edge-sample handling for
  the partial final macroblock row/column still shows small (≤ a few dozen
  sample) diffs clustered at the crop boundary; tracked as a follow-up to
  Phase 12 A.
- Multiple/arbitrary slice groups (FMO) reconstruction
- High-profile scaling lists applied at dequant

As a result, decoded output for those paths is **not** pixel-exact; callers
should check `H264Decoder::capabilities().pixel_exact` and/or use strict mode
before trusting frames.

### Roadmap

Flipping `capabilities().pixel_exact = true` (Phase H) is gated on closing the
unsupported subset above: the 8×8 transform (Phase F), interlaced coding
(Phase G), and the non-16-aligned dimension gap. Until then the decoder stays
honest — bit-exact where it claims to be, and `NotPixelExact` everywhere else.
