//! Latency budget and how it is enforced (DECISION 4).
//!
//! Realtime's latency guarantee is a design center, not a tuning. Two halves:
//!
//! 1. **Encode deadline.** The encoder carries a `deadline_ms` per frame and,
//!    if it blows the budget, falls back to a faster mode (drop refinement,
//!    raise QP) rather than overrunning and stalling the pipeline. The
//!    [`adapt_to_deadline`] policy is the deterministic rule an encoder applies
//!    once it measures how long a frame took.
//! 2. **Decode bounded work.** No B-frames, a single backward reference, a
//!    fixed slice grid, and a lean-style single in-loop filter mean per-frame
//!    decode cost is `O(pixels)` with a known constant. [`max_decode_ms_estimate`]
//!    turns a [`SequenceHeader`] into the decoder's `max_decode_ms` so callers
//!    can reject a stream whose worst-case decode would miss their deadline.
//!
//! The decode-cost constant is a **placeholder** to be calibrated by the
//! packet-loss / stall-rate validation harness (DECISION 5) once a real
//! reconstruction path exists; the shape of the contract (bounded, monotonic
//! in pixels, independent of GOP length) is what matters here.

use crate::headers::SequenceHeader;

/// Assumed decode cost per pixel, in nanoseconds. Placeholder — replace with a
/// measured value from the DECISION 5 benchmark harness.
pub const COST_PER_PIXEL_NS: u32 = 5;

/// Assumed fixed setup cost per slice, in nanoseconds (grid bookkeeping +
/// stream init). Bounded and additive.
pub const SLICE_SETUP_NS: u32 = 2_000;

/// Worst-case decode time in milliseconds for a frame of the declared sequence,
/// under the bounded-work contract. Monotonic in pixel count; independent of
/// GOP length because there is exactly one reference (no multi-frame chain).
pub fn max_decode_ms_estimate(seq: &SequenceHeader) -> u32 {
    let pixels = seq.max_width as u64 * seq.max_height as u64;
    let pixel_ns = pixels * COST_PER_PIXEL_NS as u64;
    let slices = seq.slice_grid_cols as u64 * seq.slice_grid_rows as u64;
    let slice_ns = slices * SLICE_SETUP_NS as u64;
    ((pixel_ns + slice_ns) / 1_000_000) as u32
}

/// The encode deadline carried per frame (mirrors [`FrameHeader::deadline_ms`]).
///
/// [`FrameHeader::deadline_ms`]: crate::headers::FrameHeader::deadline_ms
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncodeDeadline {
    pub deadline_ms: u8,
}

/// What the encoder does when a frame overruns its [`EncodeDeadline`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateControlAction {
    /// Within budget — keep the current mode and QP.
    Keep,
    /// Over budget but recoverable by dropping refinement (keep QP).
    SkipRefinement,
    /// Over budget — raise QP by `delta` (coarser, faster) to get back under.
    RaiseQp(u8),
}

/// Decide the rate-control fallback from the measured encode time.
///
/// `deadline_ms` is the frame's budget, `elapsed_ms` is how long the encode
/// actually took, `current_qp` is the QP in effect (used only to clamp the
/// raise so it stays below the 51 QP ceiling). Pure and deterministic so it is
/// unit-testable without a running encoder.
pub fn adapt_to_deadline(deadline_ms: u8, elapsed_ms: u32, current_qp: u8) -> RateControlAction {
    let deadline = deadline_ms as u32;
    if elapsed_ms <= deadline {
        return RateControlAction::Keep;
    }
    let overshoot = elapsed_ms - deadline;
    if overshoot < 8 {
        // Small slip: drop refinement, keep quality.
        return RateControlAction::SkipRefinement;
    }
    // Larger slip: raise QP by a capped step proportional to the overshoot.
    let step = (overshoot / 8).min(16) as u8;
    let raised = current_qp.saturating_add(step).min(51);
    if raised <= current_qp {
        RateControlAction::SkipRefinement
    } else {
        RateControlAction::RaiseQp(raised - current_qp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::headers::{ChromaFormat, ProfilePreset};

    fn seq(w: u16, h: u16, cols: u8, rows: u8) -> SequenceHeader {
        SequenceHeader {
            version: 1,
            max_width: w,
            max_height: h,
            profile: ProfilePreset::CloudGaming,
            slice_grid_cols: cols,
            slice_grid_rows: rows,
            fec_overhead_pct: 10,
            foveation_enabled: false,
            min_block_size_log2: 3,
            max_block_size_log2: 6,
            bit_depth: 8,
            chroma_format: ChromaFormat::Yuv420,
            num_rans_streams: cols * rows,
            max_ref_frames: 1,
            max_deadline_ms: 16,
        }
    }

    #[test]
    fn decode_estimate_monotonic_in_pixels() {
        let small = seq(1920, 1080, 4, 4);
        let big = seq(3840, 2160, 4, 4);
        assert!(max_decode_ms_estimate(&big) > max_decode_ms_estimate(&small));
    }

    #[test]
    fn decode_estimate_positive_and_slice_aware() {
        let base = seq(1280, 720, 4, 4);
        let fine = seq(1280, 720, 15, 16); // 240 slices, within the 255 cap
        assert!(max_decode_ms_estimate(&base) > 0);
        assert!(max_decode_ms_estimate(&fine) > max_decode_ms_estimate(&base));
    }

    #[test]
    fn within_budget_keeps() {
        assert_eq!(adapt_to_deadline(16, 10, 20), RateControlAction::Keep);
        assert_eq!(adapt_to_deadline(16, 16, 20), RateControlAction::Keep);
    }

    #[test]
    fn small_overshoot_skips_refinement() {
        assert_eq!(
            adapt_to_deadline(16, 20, 20),
            RateControlAction::SkipRefinement
        );
    }

    #[test]
    fn large_overshoot_raises_qp_capped() {
        // overshoot 24 -> step 3
        assert_eq!(adapt_to_deadline(16, 40, 20), RateControlAction::RaiseQp(3));
        // huge overshoot -> step capped at 16 (QP 20 + 16 = 36)
        assert_eq!(adapt_to_deadline(16, 300, 20), RateControlAction::RaiseQp(16));
    }

    #[test]
    fn raise_qp_respects_ceiling() {
        // near ceiling: 50 + step(>=1) clamps to 51, delta 1
        assert_eq!(adapt_to_deadline(16, 40, 50), RateControlAction::RaiseQp(1));
    }
}
