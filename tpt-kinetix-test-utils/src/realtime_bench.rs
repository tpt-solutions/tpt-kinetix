//! `tpt-kinetix-realtime` validation harness (DECISION 5).
//!
//! Realtime's design center is graceful degradation under packet loss, not max
//! compression. So its validation is the **packet-loss × quality** curve and
//! the **stall/freeze rate** — the realtime analogue of a PSNR-vs-bitrate curve
//! (DECISION 5 in `docs/realtime-codec-design.md`).
//!
//! This module encodes a synthetic moving clip through the *real* realtime
//! pipeline (`encode_frame_slices` → `SliceGrid::frame` → `Fec` protection),
//! packetizes each frame into FEC source symbols, injects reproducible packet
//! loss at a given rate, then recovers what it can via FEC and falls back to
//! temporal concealment (reuse the previous decoded frame) for anything still
//! missing. It decodes the recovered / concealed payloads with the *real*
//! [`RealtimeDecoder`] and reports, per loss rate:
//!
//! * `mean_psnr_y_db` / `min_psnr_y_db` — Y-plane PSNR of the decoded vs. the
//!   original clip (the quality half of the goal).
//! * `stall_rate` — fraction of frames that the decoder could not fully recover
//!   and had to conceal (the latency/degradation half of the goal: a frame that
//!   misses its budget is a *stall*).
//!
//! At `qp == 0` the realtime reconstruction is lossless, so a **zero-loss**
//! run reproduces the source exactly (PSNR = +inf) and has a 0% stall rate —
//! that is the baseline the curve is measured against.
//!
//! The harness is deterministic: the same `(clip, loss_rate, seed)` always
//! yields the same result, so it doubles as a regression check.

use tpt_kinetix_core::{
    frame::VideoFrame, packet::Packet, pixel_format::PixelFormat, timestamp::Timestamp,
};

use tpt_kinetix_realtime::{
    decoder::RealtimeDecoder, fec::Fec, reconstruct::FrameBuffer, slice::SliceGrid,
};

use crate::pixel_diff::psnr_yuv420p;

/// A FEC-protected, packetized frame ready for loss injection.
struct Transmission {
    /// Serialized frame header bytes that precede the slice payload.
    header: Vec<u8>,
    /// Length of the framed slice payload (before FEC symbol padding).
    framed_len: usize,
    /// Equal-length FEC source symbols (one per packet that can be lost).
    symbols: Vec<Vec<u8>>,
    /// FEC parity symbols (always delivered in this model).
    parities: Vec<Vec<u8>>,
}

/// Result of running the clip through the loss + recovery pipeline at one loss rate.
#[derive(Debug, Clone, PartialEq)]
pub struct RealtimeLossResult {
    /// The packet-loss rate (0.0..=1.0) this row was produced at.
    pub loss_rate: f64,
    /// Mean Y-plane PSNR (dB) across all frames. `f64::INFINITY` when every
    /// frame decoded losslessly (e.g. zero loss at `qp == 0`).
    pub mean_psnr_y_db: f64,
    /// Worst-case Y-plane PSNR (dB) across all frames.
    pub min_psnr_y_db: f64,
    /// Fraction of frames that required concealment (could not be fully
    /// recovered by FEC) — the stall rate.
    pub stall_rate: f64,
    /// Number of frames in the clip.
    pub frames: usize,
}

/// Build a [`SequenceHeader`] for the harness. Conferencing preset, an 8×8
/// slice grid, 20% FEC overhead (DECISION 1/6), `qp == 0` lossless base.
pub fn harness_sequence(
    width: u16,
    height: u16,
) -> tpt_kinetix_realtime::headers::SequenceHeader {
    use tpt_kinetix_realtime::headers::{ChromaFormat, ProfilePreset};
    tpt_kinetix_realtime::headers::SequenceHeader {
        version: 1,
        max_width: width,
        max_height: height,
        profile: ProfilePreset::Conferencing,
        slice_grid_cols: 8,
        slice_grid_rows: 8,
        fec_overhead_pct: 20,
        foveation_enabled: false,
        min_block_size_log2: 3,
        max_block_size_log2: 3,
        bit_depth: 8,
        chroma_format: ChromaFormat::Yuv420,
        num_rans_streams: 64,
        max_ref_frames: 1,
        max_deadline_ms: 16,
    }
}

/// Generate a synthetic moving clip so consecutive frames differ — this is what
/// makes packet loss observable: a concealed (previous-frame) frame no longer
/// matches the true current frame, so concealment shows up as a PSNR hit.
pub fn generate_clip(width: u32, height: u32, num_frames: usize) -> Vec<VideoFrame> {
    let w = width as usize;
    let h = height as usize;
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    let mut frames = Vec::with_capacity(num_frames);
    for t in 0..num_frames {
        let mut data = Vec::with_capacity(w * h + 2 * cw * ch);
        for y in 0..h {
            for x in 0..w {
                data.push(((x + t * 5) ^ (y + t * 3)) as u8);
            }
        }
        let cb = ((t * 7) % 256) as u8;
        let cr = ((t * 11) % 256) as u8;
        for _ in 0..cw * ch {
            data.push(cb);
        }
        for _ in 0..cw * ch {
            data.push(cr);
        }
        frames.push(VideoFrame {
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            data,
            width,
            height,
            pixel_format: PixelFormat::Yuv420p,
            is_key_frame: false,
        });
    }
    frames
}

/// Deterministic xorshift RNG state (no external `rand` dependency).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// `true` with probability `rate` (0.0..=1.0).
    fn drop(&mut self, rate: f64) -> bool {
        let v = (self.next() >> 32) as u32;
        let threshold = (rate * u32::MAX as f64) as u32;
        v < threshold
    }
}

fn split_planes(frame: &VideoFrame) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let w = frame.width as usize;
    let h = frame.height as usize;
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    let ys = w * h;
    let cs = cw * ch;
    let luma = frame.data[..ys].to_vec();
    let cb = frame.data[ys..ys + cs].to_vec();
    let cr = frame.data[ys + cs..ys + 2 * cs].to_vec();
    (luma, cb, cr)
}

fn black_frame(width: u32, height: u32) -> VideoFrame {
    let w = width as usize;
    let h = height as usize;
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    let len = w * h + 2 * cw * ch;
    VideoFrame {
        pts: Timestamp::NONE,
        dts: Timestamp::NONE,
        data: vec![0u8; len],
        width,
        height,
        pixel_format: PixelFormat::Yuv420p,
        is_key_frame: false,
    }
}

/// FEC coder sized for the frame's payload and the sequence's overhead budget.
fn make_fec(seq: &tpt_kinetix_realtime::headers::SequenceHeader, payload_len: usize) -> Fec {
    use tpt_kinetix_realtime::fec::DEFAULT_SYMBOL_SIZE;
    let symbol_size = DEFAULT_SYMBOL_SIZE;
    let n_symbols = payload_len.div_ceil(symbol_size).max(1);
    let repair = if n_symbols <= 1 {
        0
    } else {
        let r = (n_symbols * seq.fec_overhead_pct as usize) / 100;
        r.max(1).min(n_symbols - 1)
    };
    Fec::new(symbol_size, repair)
}

/// Encode the clip into FEC-protected [`Transmission`]s using the real realtime
/// encode path. Returns one transmission per frame, in order.
fn transmit_clip(
    seq: &tpt_kinetix_realtime::headers::SequenceHeader,
    clip: &[VideoFrame],
) -> Vec<Transmission> {
    use tpt_kinetix_realtime::headers::{FrameHeader, FrameType};
    use tpt_kinetix_realtime::reconstruct::encode_frame_slices;

    let w = seq.max_width;
    let h = seq.max_height;
    let grid = SliceGrid {
        cols: seq.slice_grid_cols,
        rows: seq.slice_grid_rows,
    };

    let mut out = Vec::with_capacity(clip.len());
    let mut prev: Option<FrameBuffer> = None;
    for (t, frame) in clip.iter().enumerate() {
        let (luma, cb, cr) = split_planes(frame);
        let src = FrameBuffer::from_yuv420(w as u32, h as u32, luma, cb, cr)
            .expect("frame planes must match declared dimensions");

        let is_key = t == 0;
        let mut header = FrameHeader {
            frame_type: if is_key { FrameType::Key } else { FrameType::Inter },
            width: w,
            height: h,
            base_qp: 0, // lossless base: zero-loss run reproduces the source exactly.
            ref_frame_count: if is_key { 0 } else { 1 },
            deadline_ms: 16,
            force_idr: is_key,
            foveation_center_x: 0,
            foveation_center_y: 0,
            intra_refresh_mask: vec![0u8; seq.refresh_mask_len()],
            payload_len: 0,
        };

        let slices = encode_frame_slices(seq, &header, &src, prev.as_ref())
            .expect("encode_frame_slices");
        let framed = grid.frame(&slices).expect("frame slices");
        header.payload_len = framed.len() as u32;
        let header_bytes = header.to_bytes();

        let fec = make_fec(seq, framed.len());
        let symbols = fec.split_payload(&framed);
        let parities = fec.encode(&symbols).expect("fec encode");

        out.push(Transmission {
            header: header_bytes,
            framed_len: framed.len(),
            symbols,
            parities,
        });
        prev = Some(src);
    }
    out
}

/// Decode the clip under a fixed packet-loss rate, recovering via FEC where
/// possible and concealing (reuse the previous decoded frame) otherwise.
///
/// Returns `(per-frame Y PSNR, stall count)`.
fn simulate(
    seq: &tpt_kinetix_realtime::headers::SequenceHeader,
    clip: &[VideoFrame],
    loss_rate: f64,
    seed: u64,
) -> (Vec<f64>, usize) {
    let transmissions = transmit_clip(seq, clip);
    let mut rng = Rng(seed);
    let mut decoder = RealtimeDecoder::new();
    decoder.set_sequence_header(*seq);

    let mut psnrs = Vec::with_capacity(transmissions.len());
    let mut stalls = 0usize;
    let mut last_decoded: Option<VideoFrame> = None;

    for (t, tx) in transmissions.iter().enumerate() {
        let received: Vec<Option<Vec<u8>>> = tx
            .symbols
            .iter()
            .map(|s| {
                if rng.drop(loss_rate) {
                    None
                } else {
                    Some(s.clone())
                }
            })
            .collect();

        let fec = make_fec(seq, tx.framed_len);
        let decoded = match fec.recover(&received, &tx.parities) {
            Ok(recovered) => {
                let payload = fec.reassemble(&recovered, tx.framed_len);
                let mut data = tx.header.clone();
                data.extend_from_slice(&payload);
                let packet = Packet {
                    pts: Timestamp::NONE,
                    dts: Timestamp::NONE,
                    data,
                    stream_index: 0,
                    is_key_frame: t == 0,
                };
                decoder
                    .decode(&packet)
                    .expect("decode must not fail on a recovered payload")
                    .expect("decode returns a frame for a recovered payload")
            }
            Err(_) => {
                stalls += 1;
                // Terminal fallback (DECISION 1): never stall a frame. Reuse the
                // last decoded frame (temporal concealment); if we never had a
                // base (first keyframe lost), fall back to a black frame.
                match &last_decoded {
                    Some(prev) => prev.clone(),
                    None => black_frame(seq.max_width as u32, seq.max_height as u32),
                }
            }
        };

        let (y, _, _) = psnr_yuv420p(&decoded, &clip[t]).expect("psnr");
        psnrs.push(y);
        last_decoded = Some(decoded);
    }

    (psnrs, stalls)
}

fn summarize(loss_rate: f64, psnrs: &[f64], stalls: usize, frames: usize) -> RealtimeLossResult {
    let finite: Vec<f64> = psnrs.iter().copied().filter(|v| v.is_finite()).collect();
    let mean = if finite.is_empty() {
        f64::INFINITY
    } else {
        finite.iter().sum::<f64>() / finite.len() as f64
    };
    let min = if finite.is_empty() {
        f64::INFINITY
    } else {
        finite.iter().copied().fold(f64::INFINITY, f64::min)
    };
    RealtimeLossResult {
        loss_rate,
        mean_psnr_y_db: mean,
        min_psnr_y_db: min,
        stall_rate: stalls as f64 / frames as f64,
        frames,
    }
}

/// Run the loss-resilience sweep for a clip at each requested loss rate.
///
/// Returns one [`RealtimeLossResult`] per entry of `loss_rates`, in the same
/// order. Each row is deterministic for a fixed `seed`.
pub fn run_loss_curve(
    seq: &tpt_kinetix_realtime::headers::SequenceHeader,
    clip: &[VideoFrame],
    loss_rates: &[f64],
    seed: u64,
) -> Vec<RealtimeLossResult> {
    loss_rates
        .iter()
        .map(|&rate| {
            let (psnrs, stalls) = simulate(seq, clip, rate, seed);
            summarize(rate, &psnrs, stalls, clip.len())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seq() -> tpt_kinetix_realtime::headers::SequenceHeader {
        harness_sequence(256, 256)
    }

    #[test]
    fn zero_loss_is_lossless_and_stall_free() {
        let s = seq();
        let clip = generate_clip(256, 256, 12);
        let results = run_loss_curve(&s, &clip, &[0.0], 0xDEAD_BEEF);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].stall_rate, 0.0, "no loss must not conceal");
        assert!(
            results[0].mean_psnr_y_db.is_infinite(),
            "qp==0 with no loss must reproduce the source exactly"
        );
        assert!(results[0].min_psnr_y_db.is_infinite());
    }

    #[test]
    fn heavy_loss_produces_stalls_and_quality_hit() {
        let s = seq();
        let clip = generate_clip(256, 256, 16);
        // 30% packet loss should defeat FEC on multiple frames and force
        // concealment, which shows up as finite (non-infinite) PSNR.
        let results = run_loss_curve(&s, &clip, &[0.30], 0xC0FFEE);
        assert_eq!(results.len(), 1);
        assert!(
            results[0].stall_rate > 0.0,
            "30% loss should stall at least one frame"
        );
        assert!(
            results[0].min_psnr_y_db.is_finite(),
            "concealed frames must differ from the source"
        );
    }

    #[test]
    fn sweep_is_deterministic() {
        let s = seq();
        let clip = generate_clip(256, 256, 12);
        let rates = [0.0, 0.05, 0.10, 0.20];
        let a = run_loss_curve(&s, &clip, &rates, 7);
        let b = run_loss_curve(&s, &clip, &rates, 7);
        assert_eq!(a, b, "same seed must yield identical curves");
    }

    #[test]
    fn curve_has_one_row_per_requested_rate() {
        let s = seq();
        let clip = generate_clip(256, 256, 8);
        let results = run_loss_curve(&s, &clip, &[0.0, 0.1, 0.2, 0.3, 0.5], 42);
        assert_eq!(results.len(), 5);
        assert_eq!(results[0].loss_rate, 0.0);
        assert_eq!(results[4].loss_rate, 0.5);
    }
}
