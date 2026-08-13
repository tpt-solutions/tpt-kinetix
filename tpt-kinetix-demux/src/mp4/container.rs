//! Top-level `moov` walker that assembles [`Mp4Track`] structs from box data.

use anyhow::{anyhow, Result};
use tpt_kinetix_core::codec::{media_type_from_handler, CodecId, MediaType};

use super::boxes::{
    parse_box_header, parse_co64, parse_hdlr, parse_mdhd, parse_mvhd, parse_stco, parse_stsc,
    parse_stsd, parse_stss, parse_stsz, parse_stts, parse_tkhd, MdhdBox, StscBox, StssBox, StszBox,
    SttsBox, TkhdBox,
};

/// A fully-parsed MP4 track, including its complete sample table.
#[derive(Debug, Clone)]
pub struct Mp4Track {
    pub track_id: u32,
    pub timescale: u32,
    pub duration: u64,
    /// `b"vide"` for video, `b"soun"` for audio.
    pub handler_type: [u8; 4],
    /// Broad media category, derived from the handler type.
    pub media_type: MediaType,
    /// Codec identified from the first `stsd` sample entry, if present.
    pub codec: Option<CodecId>,
    /// Pixel width (0 for audio tracks).
    pub width: u32,
    /// Pixel height (0 for audio tracks).
    pub height: u32,
    /// Time-to-sample table.
    pub stts: SttsBox,
    /// Sync-sample (key-frame) table, absent when all samples are key-frames.
    pub stss: Option<StssBox>,
    /// Sample size table.
    pub stsz: StszBox,
    /// Chunk offsets (stco promoted to u64, or co64 as-is).
    pub chunk_offsets: Vec<u64>,
    /// Sample-to-chunk mapping.
    pub stsc: StscBox,
}

impl Mp4Track {
    /// Returns the number of samples in this track.
    pub fn sample_count(&self) -> usize {
        if self.stsz.default_size != 0 {
            // We don't store a separate count in that case; derive from stts.
            self.stts
                .entries
                .iter()
                .map(|e| e.sample_count as usize)
                .sum()
        } else {
            self.stsz.sample_sizes.len()
        }
    }
}

// ---------------------------------------------------------------------------
// Box-walking helpers
// ---------------------------------------------------------------------------

/// Iterates over child boxes inside a container box payload.
///
/// Yields `(box_type, payload_slice)` pairs.  Skips boxes whose declared size
/// is zero or that would reach past the end of `data`.
fn walk_boxes(mut data: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> + '_ {
    std::iter::from_fn(move || {
        if data.is_empty() {
            return None;
        }
        let (after_hdr, hdr) = parse_box_header(data).ok()?;
        if hdr.size == 0 {
            // size==0 means "rest of file"; consume everything
            let payload = after_hdr;
            data = &[];
            return Some((hdr.box_type, payload));
        }
        // header bytes consumed = data.len() - after_hdr.len()
        let header_len = data.len() - after_hdr.len();
        let payload_len = (hdr.size as usize).saturating_sub(header_len);
        if payload_len > after_hdr.len() {
            // Truncated box — stop iteration.
            data = &[];
            return None;
        }
        let payload = &after_hdr[..payload_len];
        data = &after_hdr[payload_len..];
        Some((hdr.box_type, payload))
    })
}

// ---------------------------------------------------------------------------
// Track parsing
// ---------------------------------------------------------------------------

/// Parses one `trak` box and returns an [`Mp4Track`].
fn parse_trak(trak_payload: &[u8]) -> Result<Mp4Track> {
    let mut tkhd: Option<TkhdBox> = None;
    let mut mdhd: Option<MdhdBox> = None;
    let mut handler_type = [0u8; 4];
    let mut stts: Option<SttsBox> = None;
    let mut stss: Option<StssBox> = None;
    let mut stsz: Option<StszBox> = None;
    let mut chunk_offsets: Option<Vec<u64>> = None;
    let mut stsc: Option<StscBox> = None;
    let mut codec: Option<CodecId> = None;

    for (box_type, payload) in walk_boxes(trak_payload) {
        match &box_type {
            b"tkhd" => {
                tkhd = parse_tkhd(payload).ok().map(|(_, v)| v);
            }
            b"mdia" => {
                // Walk mdia children
                for (mdia_type, mdia_payload) in walk_boxes(payload) {
                    match &mdia_type {
                        b"mdhd" => {
                            mdhd = parse_mdhd(mdia_payload).ok().map(|(_, v)| v);
                        }
                        b"hdlr" => {
                            if let Ok((_, h)) = parse_hdlr(mdia_payload) {
                                handler_type = h.handler_type;
                            }
                        }
                        b"minf" => {
                            // Walk minf → stbl
                            for (minf_type, minf_payload) in walk_boxes(mdia_payload) {
                                if &minf_type == b"stbl" {
                                    for (stbl_type, stbl_payload) in walk_boxes(minf_payload) {
                                        match &stbl_type {
                                            b"stts" => {
                                                stts =
                                                    parse_stts(stbl_payload).ok().map(|(_, v)| v);
                                            }
                                            b"stss" => {
                                                stss =
                                                    parse_stss(stbl_payload).ok().map(|(_, v)| v);
                                            }
                                            b"stsz" => {
                                                stsz =
                                                    parse_stsz(stbl_payload).ok().map(|(_, v)| v);
                                            }
                                            b"stco" => {
                                                if let Ok((_, co)) = parse_stco(stbl_payload) {
                                                    chunk_offsets = Some(
                                                        co.offsets
                                                            .into_iter()
                                                            .map(|o| o as u64)
                                                            .collect(),
                                                    );
                                                }
                                            }
                                            b"co64" => {
                                                if let Ok((_, co)) = parse_co64(stbl_payload) {
                                                    chunk_offsets = Some(co.offsets);
                                                }
                                            }
                                            b"stsc" => {
                                                stsc =
                                                    parse_stsc(stbl_payload).ok().map(|(_, v)| v);
                                            }
                                            b"stsd" => {
                                                if let Ok((_, stsd)) = parse_stsd(stbl_payload) {
                                                    codec = stsd
                                                        .codec_fourcc()
                                                        .map(CodecId::from_fourcc);
                                                }
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    let tkhd = tkhd.ok_or_else(|| anyhow!("missing tkhd box"))?;
    let mdhd = mdhd.ok_or_else(|| anyhow!("missing mdhd box"))?;
    let stts = stts.ok_or_else(|| anyhow!("missing stts box"))?;
    let stsz = stsz.ok_or_else(|| anyhow!("missing stsz box"))?;
    let chunk_offsets = chunk_offsets.ok_or_else(|| anyhow!("missing stco/co64 box"))?;
    let stsc = stsc.ok_or_else(|| anyhow!("missing stsc box"))?;

    Ok(Mp4Track {
        track_id: tkhd.track_id,
        timescale: mdhd.timescale,
        duration: mdhd.duration,
        handler_type,
        media_type: media_type_from_handler(handler_type),
        codec,
        width: tkhd.width,
        height: tkhd.height,
        stts,
        stss,
        stsz,
        chunk_offsets,
        stsc,
    })
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Parses an MP4 file from raw bytes and returns all tracks found in `moov`.
pub fn parse_mp4(data: &[u8]) -> Result<Vec<Mp4Track>> {
    // Walk top-level boxes looking for moov.
    let mut tracks = Vec::new();
    let mut found_moov = false;

    for (box_type, payload) in walk_boxes(data) {
        if &box_type == b"moov" {
            found_moov = true;
            // Walk moov children for mvhd and trak boxes.
            for (moov_type, moov_payload) in walk_boxes(payload) {
                match &moov_type {
                    b"mvhd" => {
                        // Parse but currently we propagate timescale per-track via mdhd.
                        let _ = parse_mvhd(moov_payload);
                    }
                    b"trak" => {
                        match parse_trak(moov_payload) {
                            Ok(track) => tracks.push(track),
                            Err(_) => {
                                // Skip malformed tracks; caller gets whatever parsed cleanly.
                            }
                        }
                    }
                    _ => {}
                }
            }
            break; // Only one moov box expected.
        }
    }

    if !found_moov {
        return Err(anyhow!("no moov box found in MP4 data"));
    }

    Ok(tracks)
}
