//! H.264 / AVC stateful decoder.
//!
//! Wires together NAL parsing, SPS/PPS stores, and macroblock reconstruction.
//! Slice-level parallelism is injected via `rayon` at the macroblock-row level.

use std::collections::HashMap;

use rayon::prelude::*;

use kinetix_core::{
    error::KinetixError, frame::VideoFrame, packet::Packet, pixel_format::PixelFormat,
};

use crate::{
    macroblock::Macroblock,
    nal::{parse_nal_units_from_annexb, NalUnitType},
    pps::PicParameterSet,
    sps::SeqParameterSet,
};

/// Stateful H.264 / AVC decoder.
///
/// Feed compressed [`Packet`]s via [`H264Decoder::decode`] and receive decoded [`VideoFrame`]s.
pub struct H264Decoder {
    sps_store: HashMap<u32, SeqParameterSet>,
    pps_store: HashMap<u32, PicParameterSet>,
    /// Decoded Picture Buffer — stores reference frames for inter prediction.
    dpb: Vec<VideoFrame>,
    frame_count: u64,
    /// When `true` (the default), macroblock rows are reconstructed with `rayon`
    /// parallel iterators. Set to `false` to force serial reconstruction, which
    /// is useful for benchmarking the parallel speedup.
    parallel: bool,
}

impl H264Decoder {
    pub fn new() -> Self {
        Self {
            sps_store: HashMap::new(),
            pps_store: HashMap::new(),
            dpb: Vec::new(),
            frame_count: 0,
            parallel: true,
        }
    }

    /// Enable or disable `rayon` parallel macroblock-row reconstruction.
    ///
    /// Parallel reconstruction is enabled by default. Disabling it is primarily
    /// useful for benchmarks that compare single-threaded vs. parallel throughput.
    pub fn set_parallel(&mut self, parallel: bool) {
        self.parallel = parallel;
    }

    /// Builder-style variant of [`H264Decoder::set_parallel`].
    pub fn with_parallel(mut self, parallel: bool) -> Self {
        self.parallel = parallel;
        self
    }

    /// Directly insert a parsed SPS into the decoder's parameter-set store.
    ///
    /// This is primarily intended for tests and benchmarks that need to drive
    /// slice reconstruction at a chosen resolution without hand-crafting a
    /// byte-exact SPS bitstream.
    #[doc(hidden)]
    pub fn insert_sps(&mut self, sps: SeqParameterSet) {
        self.sps_store.insert(sps.seq_parameter_set_id, sps);
    }

    /// Decode a compressed bitstream [`Packet`] into a [`VideoFrame`].
    ///
    /// Returns `Ok(None)` when the decoder needs more data before a frame can
    /// be emitted. Returns `Ok(Some(frame))` when a frame is ready.
    ///
    /// NAL units are extracted from Annex B byte-stream format.
    /// Slice-level parallelism is applied via `rayon` at the macroblock-row boundary.
    pub fn decode(&mut self, packet: &Packet) -> Result<Option<VideoFrame>, KinetixError> {
        let nal_units = parse_nal_units_from_annexb(&packet.data);
        if nal_units.is_empty() {
            return Ok(None);
        }

        let mut output_frame: Option<VideoFrame> = None;

        for nal in &nal_units {
            match nal.nal_unit_type {
                NalUnitType::Sps => {
                    if let Ok(sps) = SeqParameterSet::parse(&nal.rbsp) {
                        self.sps_store.insert(sps.seq_parameter_set_id, sps);
                    }
                }
                NalUnitType::Pps => {
                    if let Ok(pps) = PicParameterSet::parse(&nal.rbsp) {
                        self.pps_store.insert(pps.pic_parameter_set_id, pps);
                    }
                }
                NalUnitType::IdrSlice | NalUnitType::NonIdrSlice => {
                    // Look up the active SPS/PPS (use the first available as a fallback).
                    let sps = match self.sps_store.values().next() {
                        Some(s) => s,
                        None => continue,
                    };

                    let width = sps.pic_width_pixels();
                    let height = sps.pic_height_pixels();
                    if width == 0 || height == 0 {
                        continue;
                    }

                    let frame = self.decode_slice(nal.nal_unit_type, width, height, packet)?;
                    output_frame = Some(frame);
                }
                _ => {}
            }
        }

        Ok(output_frame)
    }

    /// Flush any buffered frames from the decoded picture buffer.
    pub fn flush(&mut self) -> Result<Vec<VideoFrame>, KinetixError> {
        let frames = self.dpb.drain(..).collect();
        Ok(frames)
    }

    fn decode_slice(
        &mut self,
        nal_type: NalUnitType,
        width: u32,
        height: u32,
        packet: &Packet,
    ) -> Result<VideoFrame, KinetixError> {
        let mb_cols = width.div_ceil(16);
        let mb_rows = height.div_ceil(16);
        let total_mbs = (mb_cols * mb_rows) as usize;

        // Build a row-indexed list of macroblock stubs.
        // Each row can be decoded independently (simplified — ignores deblocking
        // filter dependencies between rows, which is acceptable for the scaffold).
        let mb_rows_data: Vec<Vec<Macroblock>> = (0..mb_rows)
            .map(|_row| (0..mb_cols).map(|_col| Macroblock::new_skip()).collect())
            .collect();

        // Parallel macroblock row reconstruction via rayon.
        let luma_stride = width as usize;
        let chroma_stride = (width / 2) as usize;
        let luma_size = luma_stride * height as usize;
        let chroma_size = chroma_stride * (height as usize / 2);

        // Reconstruct each row into its own plane slice, then assemble.
        // Each row writes to non-overlapping regions so parallel access is safe.
        let mut luma = vec![128u8; luma_size];
        let mut chroma_cb = vec![128u8; chroma_size];
        let mut chroma_cr = vec![128u8; chroma_size];

        // Use rayon to process macroblock rows concurrently.
        // Each row writes to a disjoint 16-row band of the luma/chroma planes.
        let reconstruct_row = |row_idx: usize, row_mbs: &Vec<Macroblock>| {
            let row_height = if (row_idx + 1) * 16 > height as usize {
                height as usize - row_idx * 16
            } else {
                16
            };
            let luma_row_size = luma_stride * row_height;
            let chroma_row_size = chroma_stride * (row_height / 2).max(1);
            let mut luma_row = vec![128u8; luma_row_size];
            let mut cb_row = vec![128u8; chroma_row_size];
            let mut cr_row = vec![128u8; chroma_row_size];
            for (col_idx, mb) in row_mbs.iter().enumerate() {
                mb.reconstruct_luma(&mut luma_row, col_idx as u32, 0, luma_stride);
                mb.reconstruct_chroma(&mut cb_row, &mut cr_row, col_idx as u32, 0, chroma_stride);
            }
            (luma_row, cb_row, cr_row)
        };

        let row_results: Vec<(Vec<u8>, Vec<u8>, Vec<u8>)> = if self.parallel {
            mb_rows_data
                .par_iter()
                .enumerate()
                .map(|(row_idx, row_mbs)| reconstruct_row(row_idx, row_mbs))
                .collect()
        } else {
            mb_rows_data
                .iter()
                .enumerate()
                .map(|(row_idx, row_mbs)| reconstruct_row(row_idx, row_mbs))
                .collect()
        };

        for (row_idx, (luma_row, cb_row, cr_row)) in row_results.iter().enumerate() {
            let y_off = row_idx * 16 * luma_stride;
            let copy_len = luma_row.len().min(luma.len() - y_off);
            luma[y_off..y_off + copy_len].copy_from_slice(&luma_row[..copy_len]);

            let c_off = row_idx * 8 * chroma_stride;
            let cc_len = cb_row.len().min(chroma_cb.len().saturating_sub(c_off));
            if cc_len > 0 {
                chroma_cb[c_off..c_off + cc_len].copy_from_slice(&cb_row[..cc_len]);
                chroma_cr[c_off..c_off + cc_len].copy_from_slice(&cr_row[..cc_len]);
            }
        }

        let _ = total_mbs;

        let mut data = luma;
        data.extend(chroma_cb);
        data.extend(chroma_cr);

        self.frame_count += 1;
        Ok(VideoFrame {
            pts: packet.pts,
            dts: packet.dts,
            data,
            width,
            height,
            pixel_format: PixelFormat::Yuv420p,
            is_key_frame: matches!(nal_type, NalUnitType::IdrSlice),
        })
    }
}

impl Default for H264Decoder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kinetix_core::Timestamp;

    #[test]
    fn empty_packet_returns_none() {
        let mut dec = H264Decoder::new();
        let pkt = Packet {
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            data: vec![],
            stream_index: 0,
            is_key_frame: false,
        };
        assert!(matches!(dec.decode(&pkt), Ok(None)));
    }

    #[test]
    fn flush_on_empty_dpb_returns_empty() {
        let mut dec = H264Decoder::new();
        let frames = dec.flush().unwrap();
        assert!(frames.is_empty());
    }
}
