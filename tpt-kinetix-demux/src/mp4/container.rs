//! Top-level `moov` walker that assembles [`Mp4Track`] structs from box data.

use anyhow::{anyhow, Result};
use tpt_kinetix_core::codec::{media_type_from_handler, CodecId, MediaType};
use tpt_kinetix_core::stream::StreamInfo;

use super::boxes::SampleEntry;
use super::boxes::{
    parse_box_header, parse_co64, parse_ctts, parse_elst, parse_hdlr, parse_mdhd, parse_mvhd,
    parse_stco, parse_stsc, parse_stsd, parse_stss, parse_stsz, parse_stts, parse_tkhd, CttsBox,
    MdhdBox, StscBox, StssBox, StszBox, SttsBox, TkhdBox,
};
use super::config::parse_sample_entry;

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
    /// Composition-time offsets (`pts - dts`); empty when the track has no `ctts`.
    pub ctts: CttsBox,
    /// Media time at which presentation starts, from the first non-empty edit
    /// of the track's `elst` (`None` when the track has no edit list).
    pub edit_media_time: Option<i64>,
    /// Codec configuration record (`avcC`, `hvcC`, `av1C`, `vpcC`, AAC
    /// `AudioSpecificConfig`, `dOps`, …); empty when the entry has none.
    pub extradata: Vec<u8>,
    /// Channel count (audio tracks), else 0.
    pub channels: u16,
    /// Sample rate in Hz (audio tracks), else 0.
    pub sample_rate: u32,
    /// Bits per sample (audio tracks), else 0.
    pub bits_per_sample: u16,
}

impl Mp4Track {
    /// The codec-agnostic description of this track (`index` is its position in
    /// the demuxer's track list, i.e. the `stream_index` of its packets).
    pub fn stream_info(&self, index: u32) -> StreamInfo {
        let codec = self.codec.unwrap_or(CodecId::Unknown([0; 4]));
        let mut s = StreamInfo::new(index, codec, self.timescale);
        s.media_type = self.media_type;
        s.duration = self.duration;
        s.width = self.width;
        s.height = self.height;
        s.channels = self.channels;
        s.sample_rate = self.sample_rate;
        s.bits_per_sample = self.bits_per_sample;
        s.extradata = self.extradata.clone();
        s.edit_media_time = self.edit_media_time;
        s
    }

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
pub(crate) fn walk_boxes(mut data: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> + '_ {
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
    let mut ctts = CttsBox::default();
    let mut codec: Option<CodecId> = None;
    let mut first_entry: Option<SampleEntry> = None;
    let mut edit_media_time: Option<i64> = None;

    for (box_type, payload) in walk_boxes(trak_payload) {
        match &box_type {
            b"tkhd" => {
                tkhd = parse_tkhd(payload).ok().map(|(_, v)| v);
            }
            b"edts" => {
                for (edts_type, edts_payload) in walk_boxes(payload) {
                    if &edts_type == b"elst" {
                        if let Ok((_, entries)) = parse_elst(edts_payload) {
                            edit_media_time = entries
                                .iter()
                                .find(|e| e.media_time >= 0)
                                .map(|e| e.media_time);
                        }
                    }
                }
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
                                            b"ctts" => {
                                                if let Ok((_, c)) = parse_ctts(stbl_payload) {
                                                    ctts = c;
                                                }
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
                                                    first_entry = stsd.entries.first().cloned();
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

    let media_type = media_type_from_handler(handler_type);
    let entry_cfg = first_entry
        .as_ref()
        .map(|e| parse_sample_entry(media_type, &e.extra))
        .unwrap_or_default();
    // `mp4a` is the sample-entry for any MPEG-4 audio: the object type says
    // whether it is AAC or MP3.
    if codec == Some(CodecId::Aac) && matches!(entry_cfg.object_type, Some(0x69 | 0x6B)) {
        codec = Some(CodecId::Mp3);
    }
    // Prefer the sample entry's own size over `tkhd` (which may be a display
    // size under a transform), falling back to `tkhd` when it has none.
    let (width, height) = if entry_cfg.width != 0 && entry_cfg.height != 0 {
        (entry_cfg.width, entry_cfg.height)
    } else {
        (tkhd.width, tkhd.height)
    };

    Ok(Mp4Track {
        track_id: tkhd.track_id,
        timescale: mdhd.timescale,
        duration: mdhd.duration,
        handler_type,
        media_type,
        codec,
        width,
        height,
        stts,
        stss,
        stsz,
        chunk_offsets,
        stsc,
        ctts,
        edit_media_time,
        extradata: entry_cfg.extradata,
        channels: entry_cfg.channels,
        sample_rate: entry_cfg.sample_rate,
        bits_per_sample: entry_cfg.bits_per_sample,
    })
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Parses the *payload* of a `moov` box (everything after its header) into
/// tracks. Malformed tracks are skipped, matching [`parse_mp4`].
pub fn parse_moov_payload(payload: &[u8]) -> Vec<Mp4Track> {
    let mut tracks = Vec::new();
    for (moov_type, moov_payload) in walk_boxes(payload) {
        match &moov_type {
            b"mvhd" => {
                // Parsed but unused: timescale is taken per-track from `mdhd`.
                let _ = parse_mvhd(moov_payload);
            }
            b"trak" => {
                if let Ok(track) = parse_trak(moov_payload) {
                    tracks.push(track);
                }
            }
            _ => {}
        }
    }
    tracks
}

/// Parses an MP4 file from raw bytes and returns all tracks found in `moov`.
pub fn parse_mp4(data: &[u8]) -> Result<Vec<Mp4Track>> {
    // Walk top-level boxes looking for moov.
    for (box_type, payload) in walk_boxes(data) {
        if &box_type == b"moov" {
            return Ok(parse_moov_payload(payload)); // Only one moov box expected.
        }
    }
    Err(anyhow!("no moov box found in MP4 data"))
}
