//! ITU-T H.264.1 (H.264 conformance) bitstream suite — bit-exact decode vs the
//! standard's own reference YUV.
//!
//! Unlike the other conformance tests in this crate (which diff against an
//! `ffmpeg` decode of an `ffmpeg`/`x264`-*encoded* synthetic clip), this test
//! decodes the **official ITU conformance bitstreams** and compares byte-exact
//! against the reference `_rec.yuv` / `.yuv` shipped in each clip's archive.
//! That output is the normative reference — no third-party decoder is in the
//! loop.
//!
//! The fixtures are large (bitstreams + raw YUV, hundreds of MB for the full
//! curated set) and are **not committed**. Fetch them with:
//!
//! ```sh
//! just fetch-h264-conformance
//! ```
//!
//! which populates `tests/fixtures/itu/<CLIP>/`. When the directory is absent
//! or empty the test prints a notice and passes (same policy as the
//! `ffmpeg`-gated tests) so CI without the fixtures stays green.
//!
//! `MANIFEST` classifies each curated clip: `BitExact` clips are hard-asserted
//! byte-identical to the reference; `Limitation` clips exercise a feature the
//! decoder deliberately does not support pixel-exactly (multi-slice pictures,
//! non-4:2:0 chroma, …) and are asserted to *not* silently claim exactness;
//! clips not in the manifest are reported informationally only.

use std::path::{Path, PathBuf};

use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::H264Decoder;

/// Expected outcome for a curated clip.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Expect {
    /// Decoder output must be byte-identical to the reference YUV, every frame.
    BitExact,
    /// The clip exercises an unsupported-pixel-exact feature; we only require
    /// that the decoder does not produce a full-length byte-identical result by
    /// accident (i.e. the limitation is real and reported). `reason` is
    /// informational. Currently unused (the one fixture that used it,
    /// CABACI3_Sony_B, was promoted to `BitExact` 2026-09-06) but kept as
    /// part of the harness's vocabulary for the next real limitation found.
    #[allow(dead_code)]
    Limitation(&'static str),
    /// A real gap found by this suite that isn't fixed yet: the decoder should
    /// support this clip pixel-exactly but currently does not. Reported and
    /// tracked, not hard-asserted, so the suite stays green while the gap is
    /// worked. Promote to `BitExact` once fixed. See todo-h264.md.
    KnownGap(&'static str),
}

/// Curated subset of the ITU AVCv1 + FRExt suites, chosen to cover exactly the
/// feature set `capabilities().pixel_exact == true` claims. Keep in sync with
/// `tools/fetch-h264-conformance.sh`.
const MANIFEST: &[(&str, Expect)] = &[
    // --- progressive CAVLC (Baseline / Main), I & I/P ---
    ("BA1_Sony_D", Expect::BitExact),     // CAVLC, I-only
    ("CVPCMNL1_SVA_C", Expect::BitExact), // CAVLC I, I_PCM macroblocks (loop filter off)
    ("CVPCMNL2_SVA_C", Expect::BitExact), // 1280x720 I_PCM
    ("BA2_Sony_F", Expect::BitExact),     // CAVLC I/P, multi-ref (300 frames)
    ("CANL1_Sony_E", Expect::BitExact),   // CABAC I/P no loop filter
    ("CANL2_Sony_E", Expect::BitExact),   // CABAC I/P multi-ref
    ("NL1_Sony_D", Expect::BitExact),     // CAVLC I/P no loop filter
    ("NL2_Sony_H", Expect::BitExact),     // CAVLC I/P no loop filter, 300 frames
    ("SVA_NL2_E", Expect::BitExact),      // CAVLC no loop filter
    ("NL3_SVA_E", Expect::BitExact), // CAVLC I/P/B, spatial direct — exact once display-order reordering is on
    (
        "BA1_FT_C",
        Expect::KnownGap(
            "CIF I/P — frame 0 already wrong (max_diff 127) + 2x frame count; structural",
        ),
    ),
    // CAVLC I/P/B spatial-direct, 5 refs. Two spatial-direct bugs fixed
    // 2026-09-05 (B_8x8 direct/explicit interleaving order + col_zero_flag
    // corner-index formula) got diff_bytes to 1899->520 (max 112->4); the
    // remaining tiny diff on plain explicit-MV B macroblocks was the same
    // deblocking bug fixed for CVBS3_Sony_C below (SESSION #32ay) — the
    // `derive_bs_pair` "mirrored L0/L1" boundary-strength check compared raw
    // `ref_idx` (list0) against `ref_idx_l1` (list1) integers directly; index
    // 0 in RefPicList0 and index 0 in RefPicList1 are different physical
    // pictures, so the mirror-equivalence branch was falsely satisfied
    // whenever the two raw indices happened to be numerically equal,
    // yielding bS=0 for a real reference/motion discontinuity. diff_bytes
    // 520 -> 0.
    ("BA3_SVA_C", Expect::BitExact),
    // --- progressive CABAC, I & I/P/B ---
    ("CABA1_Sony_D", Expect::BitExact), // I-only CABAC
    ("CABA2_Sony_E", Expect::BitExact), // CABAC I/P multi-ref (300 frames)
    // CABAC I/P/B, 5 refs, temporal direct mode (§8.4.1.2.3, every B slice
    // has direct_spatial_mv_pred_flag=0). The derivation math in
    // derive_temporal_direct/apply_temporal_direct was already correct and
    // wired up; the remaining bug was that apply_temporal_direct always
    // sampled a single "outer corner" 4x4 co-located block per 8x8 direct
    // quadrant (the direct_8x8_inference_flag==1 rule) regardless of this
    // clip's actual SPS direct_8x8_inference_flag=0, silently dropping
    // real sub-8x8 co-located motion whenever the colocated macroblock was
    // itself split below 8x8 (FFmpeg `pred_temp_direct_motion`'s per-`i4`
    // loop vs. its `IS_SUB_8X8` corner-sample shortcut). Fixed 2026-09-06 by
    // threading sps.direct_8x8_inference_flag through TemporalDirectCtx and
    // branching apply_temporal_direct to sample each of the 4 sub-blocks
    // independently when the flag is false. diff_bytes 114,652 -> 0.
    ("CABA3_Sony_C", Expect::BitExact),
    // Same fix as CABA3_Sony_C (SPS direct_8x8_inference_flag=0, 2026-09-06).
    // diff_bytes 92,117 -> 0.
    ("CANL3_Sony_C", Expect::BitExact),
    // NOT the same bug class as CABA3_Sony_C/CANL3_Sony_C/CACQP3_Sony_D/
    // CABACI3_Sony_B: this clip's SPS has direct_8x8_inference_flag=TRUE (and
    // it's CAVLC, not CABAC — the manifest comment above this entry from
    // 2026-09-06 mislabeled it CABAC), so the temporal-direct corner-sample
    // fix doesn't touch it. Root-caused 2026-09-06 SESSION #32ay: a
    // deblocking bug (see BA3_SVA_C's comment above for the mechanism —
    // `derive_bs_pair`'s L0/L1 "mirror" check compared raw same-valued
    // `ref_idx`/`ref_idx_l1` integers across DIFFERENT reference-picture
    // lists as if that made them the same physical picture). Confirmed via a
    // pure-prediction trace (`DecodeTracer::on_motion_comp`, zero residual
    // per `on_cavlc_coeffs`) showing macroblock (6,2) of picture POC=7
    // predicting bit-exact luma for 12 of 16 samples in one 4×4 block from
    // RefPicList1[0] (POC 9) with mv=(0,4), while the bottom-right 2×2
    // corner — sitting on the boundary with macroblock (6,3), an L0-only
    // block whose `ref_idx==0` coincided numerically with this block's
    // `ref_idx_l1==0` — was off by exactly the deblocking correction the
    // false bS=0 skipped. Fixed by resolving `ref_idx`/`ref_idx_l1` to POC
    // before deblocking in the single-slice progressive B path (mirroring
    // `finalize_picture`'s existing multi-slice fix from SESSION #32aw,
    // which never covered this path). diff_bytes 10,166 -> 0.
    ("CVBS3_Sony_C", Expect::BitExact),
    // multi-slice, IPB with a P/B slice-type mix per picture. Real
    // multi-slice CABAC B decode landed 2026-09-06 (SESSION #32av); the
    // residual sub-1% diff that session left open was root-caused and fixed
    // the same day (SESSION #32aw): a P-type slice's RefPicList0
    // (§8.2.4.2.1, frame_num-based) and a B-type slice's RefPicList0/1
    // (§8.2.4.2.3, POC-based) are built by different algorithms, so at a
    // P/B slice-boundary macroblock edge within the SAME picture, the raw
    // `ref_idx` deblocking's boundary-strength derivation (§8.7.2.1)
    // compared could (and did) denote different physical reference pictures
    // on each side. Fixed by resolving each macroblock's
    // `ref_idx`/`ref_idx_l1` to its referenced picture's actual POC (a
    // list-construction-independent identity) before deblocking, using a
    // new per-slice `ref_poc_per_slice` table. diff_bytes 595 -> 0.
    ("CABAST3_Sony_E", Expect::BitExact),
    // Same class and same 2026-09-06 fix as CABAST3_Sony_E (SESSION #32aw).
    // diff_bytes 1,917 -> 0.
    ("CABASTBR3_Sony_B", Expect::BitExact),
    // Same fix as CABA3_Sony_C (SPS direct_8x8_inference_flag=0, 2026-09-06).
    // diff_bytes 10,595 -> 0.
    ("CACQP3_Sony_D", Expect::BitExact),
    // --- MBAFF ---
    (
        "CAMA1_Sony_C",
        Expect::KnownGap(
            "real MBAFF CABAC I stream — CABAC MBAFF-I desync on most frames \
             (end_of_slice_flag mismatch, bin-level oracle needed); \
             I_PCM-under-CABAC is now handled but the main desync remains",
        ),
    ),
    // --- FRExt High 4:2:0 (8x8 transform) ---
    // High CABAC, GOP I B B P, 1 ref, temporal direct, loop filter on.
    // Promoted to BitExact 2026-09-09: the last non-exact B frame (poc 188 /
    // 196) was a §9.3.3.1.1.7 ctxIdxInc desync — `set_partition_l0` narrowed
    // |mvd| to u8 *before* capping at 70, so a large component (264) wrapped
    // to 8 and fed ctxIdxInc 1 instead of 2 to the next sub-partition's
    // mvd bin-0. Fixed in slice_data/ctx.rs.
    ("HPCA_BRCM_C", Expect::BitExact),
    ("HPCANL_BRCM_C", Expect::BitExact),
    (
        "HCHP1_HHI_B",
        Expect::KnownGap(
            "hierarchical GOP-16, spatial direct, ref-pic-list reorder + MMCO. \
             2026-09-09: not a feature gap — a localised Intra_4x4 recon bug. \
             Origin is poc 4 (decode frame 2), MB(9,9)+MB(9,10), both \
             Intra_4x4 in a B slice: per-4x4-block near-constant DC error \
             (~-14 left cols, ~+18 right cols, max 72), no CABAC desync. \
             It propagates down the GOP-16 hierarchy (poc 2/6 inherit ~half, \
             poc 1/3/5/7 ~quarter). ffmpeg matches the ITU ref for all 250 \
             frames. constrained_intra_pred_flag is parsed but never consumed \
             in reconstruct.rs — suspect intra ref-sample availability / \
             pre-deblock neighbour sampling for intra MBs adjacent to inter \
             MBs. Needs a decoder that exposes pre-deblock samples (no .trc \
             for this clip).",
        ),
    ),
    // --- multiple IDR / multiple parameter sets ---
    // Both promoted to BitExact 2026-09-07 (SESSION #32aq): there was never a
    // real frame_num gap in either clip — that "gap" was entirely an artifact
    // of a `decode_impl` bug where the top-of-function `frame_queue.pop_front()`
    // short-circuit returned a backlogged frame *without ever parsing the
    // current call's own packet* whenever `with_display_order`'s reorder
    // buffer had queued more than one frame (which a second IDR's bulk
    // flush of the whole reorder buffer always triggers). That silently
    // dropped ~15 real NALs per clip, which is what earlier sessions
    // (#32al/#32am/#32an) observed as a "frame_num jumps 0->16" gap and
    // chased as an MV-prediction/residual bug. See todo-h264.md SESSION
    // #32aq for the full root-cause trail.
    ("MIDR_MW_D", Expect::BitExact),
    ("MPS_MW_A", Expect::BitExact),
    (
        "Sharp_MP_PAFF_1r2",
        Expect::KnownGap("real PAFF 720x480 — correct frame count, grey-scaffold pixels"),
    ),
    // IPB stream, multi-slice, mixes P/B slices per picture. Was blocked on
    // the same direct_8x8_inference_flag=0 temporal-direct corner-sampling
    // bug as CABA3_Sony_C/CANL3_Sony_C/CACQP3_Sony_D (fixed 2026-09-06).
    // diff_bytes 93,983 -> 0.
    ("CABACI3_Sony_B", Expect::BitExact),
];

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/itu")
}

/// Locate the bitstream (`.264` / `.jsv`) and reference YUV (`*_rec.yuv` /
/// `*.yuv`) inside a clip directory.
fn clip_files(dir: &Path) -> Option<(PathBuf, PathBuf)> {
    let mut bitstream = None;
    let mut refyuv = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let p = entry.path();
        match p.extension().and_then(|e| e.to_str()) {
            Some("264") | Some("jsv") | Some("h264") | Some("avc") | Some("26l") | Some("jvt")
            | Some("bits") => bitstream = Some(p),
            // The suite ships the normative reference under several extensions:
            // `.yuv`, and container hints like `.qcif` / `.cif` / `.4cif` that
            // are still just raw planar YUV420p.
            Some("yuv") | Some("qcif") | Some("cif") | Some("4cif") => refyuv = Some(p),
            _ => {}
        }
    }
    Some((bitstream?, refyuv?))
}

/// Split an Annex B byte stream into one buffer per NAL unit, each carrying a
/// 4-byte start code.
fn split_nals(annexb: &[u8]) -> Vec<Vec<u8>> {
    let mut starts = Vec::new();
    let mut i = 0usize;
    while i + 3 <= annexb.len() {
        if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else if i + 4 <= annexb.len()
            && annexb[i] == 0
            && annexb[i + 1] == 0
            && annexb[i + 2] == 0
            && annexb[i + 3] == 1
        {
            starts.push(i + 4);
            i += 4;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::with_capacity(starts.len());
    for (idx, &payload_start) in starts.iter().enumerate() {
        // Back off by the *minimum* start-code length (3). If the next start
        // code was actually 4 bytes, its leading `00` is now the last byte of
        // this range and the trailing-zero trim below drops it; a 3-byte start
        // needs no trim. (Backing off by 4 unconditionally, as this did before,
        // silently ate the final real RBSP byte of every NAL that a 3-byte
        // start code followed — which truncated dense PPS NALs mid-scaling-list
        // on several FRExt clips.)
        let mut end = starts.get(idx + 1).map(|&s| s - 3).unwrap_or(annexb.len());
        while end > payload_start && annexb[end - 1] == 0 {
            end -= 1;
        }
        let mut unit = vec![0u8, 0, 0, 1];
        unit.extend_from_slice(&annexb[payload_start..end]);
        out.push(unit);
    }
    out
}

/// Decode a whole Annex B stream to a flat list of display-order YUV420p frames.
fn decode_all(annexb: &[u8]) -> Vec<tpt_kinetix_core::frame::VideoFrame> {
    let mut dec = H264Decoder::new().with_display_order();
    let mut frames = Vec::new();
    for (n, unit) in split_nals(annexb).into_iter().enumerate() {
        let pkt = Packet {
            pts: Timestamp::new(n as i64, (1, 25)),
            dts: Timestamp::new(n as i64, (1, 25)),
            data: unit,
            stream_index: 0,
            is_key_frame: n == 0,
        };
        match dec.decode(&pkt) {
            Ok(Some(f)) => frames.push(f),
            Ok(None) => {}
            Err(_) => {}
        }
    }
    if let Ok(rest) = dec.flush() {
        frames.extend(rest);
    }
    frames
}

struct ClipResult {
    name: String,
    /// `None` = did not decode / no frames.
    outcome: Option<ClipOutcome>,
}

struct ClipOutcome {
    frames_decoded: usize,
    frames_expected: usize,
    /// Per-frame max abs sample diff over the frames that line up.
    max_diff: i32,
    /// Total differing bytes.
    diff_bytes: usize,
    /// Total bytes compared.
    total_bytes: usize,
    /// First frame index with any diff, if any.
    first_bad_frame: Option<usize>,
    /// How many reference frames have a byte-exact match *somewhere* in the
    /// decoded set (only computed when the in-order compare failed). If this
    /// equals `frames_expected` the decode is correct and only the output
    /// **order** is wrong — a distinct, lesser gap (our decoder currently emits
    /// in decode order, not display/POC order).
    exact_via_reorder: Option<usize>,
    width: u32,
    height: u32,
}

fn run_clip(dir: &Path) -> ClipResult {
    let name = dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("?")
        .to_string();
    let Some((bs_path, yuv_path)) = clip_files(dir) else {
        return ClipResult {
            name,
            outcome: None,
        };
    };
    let Ok(annexb) = std::fs::read(&bs_path) else {
        return ClipResult {
            name,
            outcome: None,
        };
    };
    let Ok(reference) = std::fs::read(&yuv_path) else {
        return ClipResult {
            name,
            outcome: None,
        };
    };

    let frames = decode_all(&annexb);
    if frames.is_empty() {
        return ClipResult {
            name,
            outcome: None,
        };
    }
    let (w, h) = (frames[0].width, frames[0].height);
    let frame_len = (w as usize * h as usize * 3) / 2;
    if frame_len == 0 || reference.len() < frame_len {
        return ClipResult {
            name,
            outcome: None,
        };
    }
    let frames_expected = reference.len() / frame_len;
    let n = frames.len().min(frames_expected);

    let mut max_diff = 0i32;
    let mut diff_bytes = 0usize;
    let mut total_bytes = 0usize;
    let mut first_bad_frame = None;
    for i in 0..n {
        if frames[i].data.len() != frame_len {
            first_bad_frame.get_or_insert(i);
            continue;
        }
        let refslice = &reference[i * frame_len..(i + 1) * frame_len];
        let mut frame_bad = false;
        for (a, b) in frames[i].data.iter().zip(refslice) {
            let d = (*a as i32 - *b as i32).abs();
            if d != 0 {
                diff_bytes += 1;
                max_diff = max_diff.max(d);
                frame_bad = true;
            }
        }
        total_bytes += frame_len;
        if frame_bad {
            first_bad_frame.get_or_insert(i);
        }
        if std::env::var_os("ITU_PER_FRAME").is_some() && i < 8 {
            let fmax = frames[i]
                .data
                .iter()
                .zip(refslice)
                .map(|(a, b)| (*a as i32 - *b as i32).abs())
                .max()
                .unwrap_or(0);
            eprintln!("    {name} frame {i}: max_diff={fmax}");
        }
    }

    // If the in-order compare failed, check whether every reference frame is
    // nonetheless present byte-exact somewhere in the decoded set (⇒ decode is
    // correct, only display order is wrong).
    let exact_via_reorder = if first_bad_frame.is_some() {
        let mut hit = 0usize;
        for ri in 0..frames_expected {
            let rs = &reference[ri * frame_len..(ri + 1) * frame_len];
            if frames
                .iter()
                .any(|f| f.data.len() == frame_len && f.data == rs)
            {
                hit += 1;
            }
        }
        Some(hit)
    } else {
        None
    };

    ClipResult {
        name,
        outcome: Some(ClipOutcome {
            frames_decoded: frames.len(),
            frames_expected,
            max_diff,
            diff_bytes,
            total_bytes,
            first_bad_frame,
            exact_via_reorder,
            width: w,
            height: h,
        }),
    }
}

#[test]
fn itu_h264_conformance_suite() {
    let root = fixtures_root();
    let mut dirs: Vec<PathBuf> = match std::fs::read_dir(&root) {
        Ok(rd) => rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect(),
        Err(_) => Vec::new(),
    };
    dirs.sort();

    if dirs.is_empty() {
        eprintln!(
            "itu_h264_conformance_suite: no fixtures under {} — run `just fetch-h264-conformance`. Skipping.",
            root.display()
        );
        return;
    }

    let mut failures: Vec<String> = Vec::new();
    let mut checked_bitexact = 0usize;

    for dir in &dirs {
        let res = run_clip(dir);
        let expect = MANIFEST
            .iter()
            .find(|(n, _)| *n == res.name)
            .map(|(_, e)| *e);

        match (&res.outcome, expect) {
            (None, Some(Expect::BitExact)) => {
                failures.push(format!(
                    "{}: manifest says BitExact but the clip failed to decode to any frames",
                    res.name
                ));
            }
            (None, Some(Expect::KnownGap(r))) => {
                eprintln!(
                    "  {:<22} — did not decode (no frames)  [known gap: {r}]",
                    res.name
                );
            }
            (None, _) => {
                eprintln!("  {:<22} — did not decode (no frames)", res.name);
            }
            (Some(o), exp) => {
                let bitexact = o.diff_bytes == 0
                    && o.first_bad_frame.is_none()
                    && o.frames_decoded >= o.frames_expected;
                let reorder_note = match o.exact_via_reorder {
                    Some(hit) if hit == o.frames_expected => {
                        " DECODE-EXACT (display-order gap only)".to_string()
                    }
                    Some(hit) => format!(" {hit}/{} ref frames exact somewhere", o.frames_expected),
                    None => String::new(),
                };
                eprintln!(
                    "  {:<22} {}x{}  {}/{} frames  max_diff={:>3}  diff_bytes={}/{}  first_bad={:?}{reorder_note}  [{}]",
                    res.name,
                    o.width,
                    o.height,
                    o.frames_decoded,
                    o.frames_expected,
                    o.max_diff,
                    o.diff_bytes,
                    o.total_bytes,
                    o.first_bad_frame,
                    match exp {
                        Some(Expect::BitExact) => "expect BitExact",
                        Some(Expect::Limitation(r)) => r,
                        Some(Expect::KnownGap(r)) => r,
                        None => "informational",
                    }
                );
                match exp {
                    Some(Expect::BitExact) => {
                        checked_bitexact += 1;
                        if !bitexact {
                            failures.push(format!(
                                "{}: expected byte-exact vs ITU reference, got max_diff={} diff_bytes={} first_bad_frame={:?} ({}/{} frames)",
                                res.name,
                                o.max_diff,
                                o.diff_bytes,
                                o.first_bad_frame,
                                o.frames_decoded,
                                o.frames_expected
                            ));
                        }
                    }
                    Some(Expect::Limitation(_)) if bitexact => {
                        failures.push(format!(
                            "{}: manifest marks this a known limitation, but the decoder produced a byte-exact result — reclassify it as BitExact",
                            res.name
                        ));
                    }
                    Some(Expect::KnownGap(_)) if bitexact => {
                        failures.push(format!(
                            "{}: manifest marks this a KnownGap, but the decoder is now byte-exact — promote it to BitExact",
                            res.name
                        ));
                    }
                    _ => {}
                }
            }
        }
    }

    eprintln!(
        "\nITU conformance: {} clip(s) present, {} hard-checked bit-exact, {} failure(s)",
        dirs.len(),
        checked_bitexact,
        failures.len()
    );
    assert!(
        failures.is_empty(),
        "ITU conformance failures:\n{}",
        failures.join("\n")
    );
}
