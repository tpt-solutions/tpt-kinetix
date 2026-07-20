//! Per-stage decode instrumentation, used to debug pixel-exactness gaps by
//! inspecting intermediate values (CAVLC coefficients, intra prediction
//! output, reconstructed samples) per macroblock, without polluting the
//! decode hot path.
//!
//! [`H264Decoder::decode`](crate::H264Decoder::decode) always uses
//! [`NoopTracer`], whose methods are empty and inline away — tracing only
//! costs anything when a caller opts in via
//! [`H264Decoder::decode_with_tracer`](crate::H264Decoder::decode_with_tracer)
//! with a real [`DecodeTracer`] implementation (see
//! `tpt-kinetix-test-utils::trace_dump` for one, and
//! `examples/trace_mb.rs` for a usage example).

/// A macroblock-plane pair, used to key per-stage trace callbacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TracePlane {
    Luma,
    Cb,
    Cr,
}

/// Hooks into the H.264 intra decode pipeline. All methods default to
/// no-ops, so implementors only need to override the stages they care about.
pub trait DecodeTracer {
    /// Called once per residual block after CAVLC parsing, with the
    /// coefficients in zigzag scan order. `blk` is the raster 4x4 block index
    /// (0..15 for luma, 0..3 for chroma AC).
    fn on_cavlc_coeffs(&mut self, _mb_x: u32, _mb_y: u32, _plane: TracePlane, _blk: u8, _coeffs: &[i16; 16]) {}

    /// Called once per 4x4/16x16 block after intra prediction, before the
    /// residual is added.
    fn on_intra_pred(&mut self, _mb_x: u32, _mb_y: u32, _plane: TracePlane, _blk: u8, _pred: &[u8]) {}

    /// Called once per 4x4 block after prediction + residual have been
    /// summed into the frame buffer (pre-deblock).
    fn on_reconstructed(&mut self, _mb_x: u32, _mb_y: u32, _plane: TracePlane, _blk: u8, _samples: &[u8]) {}

    /// Called once per macroblock edge after the in-loop deblocking filter
    /// runs (not yet wired into the live intra decode path — see
    /// `tpt-kinetix-h264/src/deblock.rs`).
    fn on_deblocked(&mut self, _mb_x: u32, _mb_y: u32, _plane: TracePlane, _samples: &[u8]) {}
}

/// The default, zero-overhead tracer: every hook is a no-op.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopTracer;

impl DecodeTracer for NoopTracer {}
