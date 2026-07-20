//! A concrete [`tpt_kinetix_h264::DecodeTracer`] that collects every per-stage
//! callback into a map, keyed by macroblock/plane/block/stage, so tests and
//! debugging tools can inspect or diff intermediate decoder state instead of
//! only the final reconstructed frame.

use std::collections::HashMap;

use tpt_kinetix_h264::{DecodeTracer, TracePlane};

/// Which pipeline stage a captured sample buffer came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stage {
    CavlcCoeffs,
    IntraPred,
    Reconstructed,
    Deblocked,
}

/// Key identifying one captured buffer: macroblock position, plane, block
/// index within the macroblock, and pipeline stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TraceKey {
    pub mb_x: u32,
    pub mb_y: u32,
    pub plane: TracePlane,
    pub blk: u8,
    pub stage: Stage,
}

/// Collects every traced value into a map for later inspection.
///
/// CAVLC coefficients are stored as `i32` (widened from `i16`) and
/// prediction/reconstruction samples as `u8`, both flattened into a single
/// `Vec` per key so one map can hold every stage.
#[derive(Debug, Default)]
pub struct MapTracer {
    pub values: HashMap<TraceKey, Vec<i32>>,
}

impl MapTracer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up a captured buffer by macroblock/plane/block/stage.
    pub fn get(&self, mb_x: u32, mb_y: u32, plane: TracePlane, blk: u8, stage: Stage) -> Option<&[i32]> {
        self.values
            .get(&TraceKey { mb_x, mb_y, plane, blk, stage })
            .map(|v| v.as_slice())
    }
}

impl DecodeTracer for MapTracer {
    fn on_cavlc_coeffs(&mut self, mb_x: u32, mb_y: u32, plane: TracePlane, blk: u8, coeffs: &[i16; 16]) {
        let key = TraceKey { mb_x, mb_y, plane, blk, stage: Stage::CavlcCoeffs };
        self.values.insert(key, coeffs.iter().map(|&v| v as i32).collect());
    }

    fn on_intra_pred(&mut self, mb_x: u32, mb_y: u32, plane: TracePlane, blk: u8, pred: &[u8]) {
        let key = TraceKey { mb_x, mb_y, plane, blk, stage: Stage::IntraPred };
        self.values.insert(key, pred.iter().map(|&v| v as i32).collect());
    }

    fn on_reconstructed(&mut self, mb_x: u32, mb_y: u32, plane: TracePlane, blk: u8, samples: &[u8]) {
        let key = TraceKey { mb_x, mb_y, plane, blk, stage: Stage::Reconstructed };
        self.values.insert(key, samples.iter().map(|&v| v as i32).collect());
    }

    fn on_deblocked(&mut self, mb_x: u32, mb_y: u32, plane: TracePlane, samples: &[u8]) {
        let key = TraceKey { mb_x, mb_y, plane, blk: 0, stage: Stage::Deblocked };
        self.values.insert(key, samples.iter().map(|&v| v as i32).collect());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_reconstructed_block() {
        let mut t = MapTracer::new();
        let samples = [7u8; 16];
        t.on_reconstructed(0, 0, TracePlane::Luma, 3, &samples);
        let got = t.get(0, 0, TracePlane::Luma, 3, Stage::Reconstructed).unwrap();
        assert_eq!(got, [7i32; 16]);
        assert!(t.get(0, 0, TracePlane::Luma, 4, Stage::Reconstructed).is_none());
    }
}
