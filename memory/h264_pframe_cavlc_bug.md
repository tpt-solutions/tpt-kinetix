---
name: H.264 P-frame CAVLC decode bug diagnosis
description: Root-cause status of the non-bit-exact P-frame decode in tpt-kinetix-h264 (Phase C.1). What's been ruled out and the remaining investigation step.
type: project
---

# H.264 P-frame CAVLC decode — diagnosis (Phase C.1, tpt-kinetix-h264)

Repro: `tpt-kinetix-h264` test `cavlc_pframe_no_deblock_is_bitexact` fails on a 64x48 2-frame testsrc clip (x264 baseline, keyint=2, CAVLC). Decoder bails to a flat-grey skip frame on a CAVLC error in the first coded P-MB's last luma block (blk 15, nc=1, code `001010` invalid for coeff_token Table 9-5 table-0).

## What has been verified CORRECT (ruled out)
- CAVLC tables in `src/cavlc_tables.rs` cross-checked against FFmpeg `h264data.c`: `total_zeros` and `run_before` match exactly; `coeff_token`/level decode are I-frame-validated (`cavlc_iframe_no_deblock_is_bitexact` passes).
- The "chroma-AC total_zeros" suspect is NOT a bug — the single combined `TOTAL_ZEROS` table (tzVlcIndex 1..15) covers both luma-4x4 and chroma-AC-4x4 per spec Tables 9-7/9-8.
- Motion parsing: manual bit decode confirms the first coded MB is `mb_type=0` (P_L0_16x16), `mvd=(0,0)`, `cbp=0x0d` (`cbp_c=0`), `mb_qp_delta=0` at the correct bit positions.
- A **static 2-frame clip (identical frames → zero residual, cbp=0) decodes BIT-EXACT**. This proves MC, MB structure, motion-vector parsing, zero-residual reconstruction, and the chroma inter-MC path (`inter_skip_copies_reference` unit test) are all correct.
- A spec-faithful independent reference residual parser agreed block-for-block with the decoder through blk 14, then failed identically at blk 15 (so the parse *logic* is self-consistent).
- Skip MBs (rows 0–1) are bit-exact; diffs are confined to the 4 coded MBs (row 2): ~850 luma diffs + chroma diffs.

## Likely root cause (NOT yet pinned)
A bit-position desync in the coded MB's residual that only surfaces at the last block. Because tables/logic are I-frame-validated, the suspect is a high-activity case the low-activity I-frame corpus never hit (e.g. `tc=15`, `nC=10` → fixed-6-bit coeff_token table 3). The chroma diffs in earlier localization are probably a measurement artifact of the resilient block-zeroing experiment (they contradict `mvd=0`+`cbp_c=0` + the passing chroma-MC test).

## Next step to pin it
Need ffmpeg per-block coefficient ground truth (ffmpeg CLI does not export CAVLC coeffs easily). Either (a) hand-decode the residual blocks against the verified tables and find the first block whose consumed bits diverge from the decoder, or (b) build a second authoritative residual decoder in a test and diff positions/values. Then fix the wrong VLC read and remove the temporary bounds check + debug `println!`s in `src/slice_data.rs`.

Related: Phase C.2 (P-frame reconstruction correctness) still blocked on this. decoder.rs/slice.rs currently carry in-progress `nal_ref_idc` fix + debug prints that are part of the existing unblock work.
