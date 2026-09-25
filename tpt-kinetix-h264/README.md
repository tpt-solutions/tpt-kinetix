# tpt-kinetix-h264

H.264/AVC bitstream decoder for the TPT Kinetix engine, parallelised with `rayon`
at the macroblock-row level.

See the [workspace README](../README.md) for the full project overview,
architecture diagram, and quickstart guide.

## Status & known limitations

`tpt-kinetix-h264` reports `pixel_exact: true`. CAVLC and CABAC I/P/B
reconstruction, progressive High-profile 8×8 transforms, PAFF field pictures,
MBAFF frames, reference-list construction, MMCO, and in-loop deblocking are
byte-exact for the supported 8-bit 4:2:0 subset. Strict mode still returns
`KinetixError::NotPixelExact` when a stream uses a feature outside that subset.

The current official ITU fixture run has 33 hard-checked bit-exact clips. Three
curated streams remain explicit `KnownGap` fixtures with reproduced diagnostics:
`CAMA1_Sony_C` (real MBAFF CABAC I desync), `HCHP1_HHI_B` (hierarchical B
intra-neighbour availability), and `Sharp_MP_PAFF_1r2` (real PAFF pixels). These
are conformance-frontier failures, not permission to claim approximate output
is exact. See `todo-h264.md` and `tests/itu_conformance.rs` for the current
first-divergence evidence.

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

### Known conformance gaps

- `CAMA1_Sony_C`: real MBAFF CABAC I desync after the initial frames.
- `HCHP1_HHI_B`: localized Intra_4×4 neighbour-availability mismatch that
  propagates through a hierarchical GOP.
- `Sharp_MP_PAFF_1r2`: some PAFF field pictures still differ despite correct
  frame count and most exact fields.
- Features outside the declared 8-bit 4:2:0 subset (for example >8-bit or
  4:2:2/4:4:4) remain rejected in strict mode.

Callers should check `H264Decoder::capabilities().pixel_exact` and use strict
mode when the input feature set is not known.

### Roadmap

The capability flip is complete. The remaining conformance work is to close the
three explicit official-fixture gaps without regressing the 33 byte-exact
manifest entries. Strict-mode feature rejection for unsupported pixel formats
and bit depths remains intentional.
