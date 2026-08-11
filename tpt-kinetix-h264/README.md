# tpt-kinetix-h264

H.264/AVC bitstream decoder for the TPT Kinetix engine, parallelised with `rayon`
at the macroblock-row level.

See the [workspace README](../README.md) for the full project overview,
architecture diagram, and quickstart guide.

## Status & known limitations

`tpt-kinetix-h264` is **partially pixel-exact**. The CAVLC decode path — both
I-slices *and* P-slices, with the in-loop deblocking filter enabled or disabled
— decodes **bit-exact** against `ffmpeg` for baseline/main profiles, validated
on a generated corpus (`tests/cavlc_conformance.rs`,
`tests/p_frame_conformance.rs`, Phase 12). The CABAC I-slice path is
implemented and spec-verified, but has an **unresolved desync bug** on some
`ffmpeg`-test-pattern streams (`todo.md` Phase 12, D.3), so the decoder is
**not** yet fully pixel-exact and `H264Decoder::capabilities().pixel_exact`
remains `false`.

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
  `mb_qp_delta`, I/P macroblocks with neighbour tracking (`slice_data`)
- CABAC arithmetic decoding engine + I-slice context tables/binarizations
  (`entropy`, `cabac_tables`) — *I-slice path present; unresolved desync bug on
  some streams (Phase 12 D.3)*
- Integer inverse transform + inverse quant: spec-exact 4×4 residual (§8.5.12),
  Intra_16×16 DC Hadamard (§8.5.10), and chroma DC transform (§8.5.11)
  (`transform`)
- Intra prediction — 4×4 / 8×8 / 16×16 luma modes and 4-mode chroma prediction
  (`prediction`), **bit-exact vs `ffmpeg`**
- Inter prediction / motion compensation — DPB + POC (§8.2.1), reference-list
  construction (§8.2.4), MV prediction (§8.4.1), 6-tap luma + bilinear chroma
  sub-pel interpolation (§8.4.2.2); **P-frame decode is bit-exact vs `ffmpeg`**
  (`ref_pic`, `decoder`)
- `ref_pic_list_modification` (§8.2.4.3) and `dec_ref_pic_marking` / MMCO 1–6
  (§8.2.5) wired into reference-picture management (`ref_pic`); visible via
  `H264Decoder::dpb`
- In-loop deblocking filter — `α`/`β`/`tC0`, per-4×4-block `bS`
  (coefficient-OR + MV/ref rule), strong/weak edge filtering for luma and
  chroma; **bit-exact vs `ffmpeg`** (`deblock`)
- `rayon` parallel macroblock-row reconstruction (`decoder`)

### Not yet implemented / unsupported

- **CABAC** P/B-slice decoding and I_PCM-under-CABAC (the I-slice CABAC path
  has a known desync bug on some streams — `todo.md` Phase 12 D.3)
- **B-frames** and weighted prediction (`pred_weight_table` is parsed but its
  values are discarded; `ref_pic_list_modification` and `dec_ref_pic_marking`
  **are** applied — see `ref_pic::modify_ref_pic_list` (§8.2.4.3) and
  `ref_pic::Dpb::mark_decoded_picture` (§8.2.5))
- **8×8 transform** (`transform_8x8_mode_flag`) — parsed but not yet decoded
- **Field / interlaced coding** (`frame_mbs_only_flag == 0`, MBAFF/PAFF)
- Multiple/arbitrary slice groups (FMO) reconstruction
- High-profile scaling lists applied at dequant

As a result, decoded output for unsupported paths is **not** pixel-exact and
callers should check `H264Decoder::capabilities().pixel_exact` before trusting
frames.

### Roadmap

Full pixel-exact decode (flipping `capabilities().pixel_exact = true`) requires
resolving the CABAC I-slice desync, then completing CABAC P/B-slice decode,
B-frames, weighted prediction, the 8×8 transform, and interlaced coding. These
are tracked in the project `todo.md` under Phase 12.
