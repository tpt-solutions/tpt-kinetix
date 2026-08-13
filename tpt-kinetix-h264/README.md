# tpt-kinetix-h264

H.264/AVC bitstream decoder for the TPT Kinetix engine, parallelised with `rayon`
at the macroblock-row level.

See the [workspace README](../README.md) for the full project overview,
architecture diagram, and quickstart guide.

## Status & known limitations

`tpt-kinetix-h264` is an **early-stage scaffold**. Bitstream parsing (NAL, SPS, PPS,
slice headers) and the concurrency architecture are in place, but full pixel
reconstruction is not yet complete. The current decoder emits geometrically
correct YUV420p frames but **not pixel-exact output**.

### Implemented

- Annex B and AVCC NAL unit extraction (`nal`)
- Emulation-prevention byte removal
- SPS parsing incl. high-profile extensions and frame cropping (`sps`)
- PPS parsing incl. slice-group maps (`pps`)
- Slice-header parsing (subset — see below) (`slice`)
- CAVLC residual parsing (partial — `total_zeros` tables approximated) (`slice`)
- Integer inverse transform + inverse quant scaffold (`macroblock`)
- `rayon` parallel macroblock-row reconstruction (`decoder`)
- In-loop deblocking filter — `α`/`β`/`tC0` tables, `bS` derivation, strong/weak
  edge filtering for luma and chroma, wired into `decoder` (`deblock`)
- Intra prediction — 4×4 / 8×8 / 16×16 luma modes and 4-mode chroma prediction
  (`prediction`), wired into per-macroblock reconstruction in `decoder`

### Not yet implemented / unsupported

- **CABAC** entropy decoding (only CAVLC is present; `entropy_coding_mode_flag`
  is parsed but the arithmetic decoder is absent)
- Real bitstream-driven macroblock parsing that *produces* intra/inter `mb_type`,
  prediction modes, and non-zero coefficients — currently the decoder emits
  skip-only placeholder macroblocks, so intra/inter prediction paths are
  exercised by tests rather than by `decode()` on a real stream
- **Inter prediction / motion compensation** — the DPB is not populated and
  reference frames are not sampled
- **B-frames** and weighted prediction
- **Field / interlaced coding** (`frame_mbs_only_flag == 0`)
- `pred_weight_table` is parsed but its values are discarded (weighted
  prediction is not applied). `ref_pic_list_modification` *and*
  `dec_ref_pic_marking` (sliding-window + MMCO 1–6) **are** applied — see
  `ref_pic::modify_ref_pic_list` (§8.2.4.3) and `ref_pic::Dpb::mark_decoded_picture`
  (§8.2.5); the resulting reference pictures are visible via `H264Decoder::dpb`
- Multiple/arbitrary slice groups (FMO) reconstruction
- Complete `total_zeros` / `run_before` CAVLC tables for all VLC indices

As a result, decoded output is **not** suitable for playback or conformance yet.
The `tpt-kinetix-test-utils::reference` harness can diff Kinetix output against
`ffmpeg` once reconstruction is completed.

### Roadmap

Pixel-exact decoding requires completing intra prediction, motion compensation,
the deblocking filter, and CABAC. These are tracked in the project `todo.md`
under Phase 3.
