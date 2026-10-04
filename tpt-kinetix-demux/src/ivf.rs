//! IVF (a.k.a. "DKIF"): the bare AV1/VP9 test container.
//!
//! A 32-byte file header followed by frames of `[u32 size][u64 timestamp][payload]`.
//! It carries no audio and no codec configuration beyond a fourcc, but that is
//! exactly what makes it useful: raw AV1 or VP9 elementary streams drop straight
//! into a decoder test or fuzz corpus with no mux in the way.
//!
//! Like [`crate::TsDemuxer`] this takes a whole buffer. There is no index worth
//! building, so streaming would only avoid the one allocation.

use tpt_kinetix_core::{
    codec::{CodecId, MediaType},
    error::KinetixError,
    packet::Packet,
    stream::StreamInfo,
    timestamp::Timestamp,
};

use crate::Demuxer;

/// The file header size: 32 bytes, always present.
pub const IVF_HEADER_LEN: usize = 32;
/// The per-frame header: a 4-byte size and an 8-byte timestamp.
pub const IVF_FRAME_HEADER_LEN: usize = 12;

/// IVF timestamps are a frame counter; this is the assumed rate when the header's
/// rate/scale pair does not give a usable timescale.
const ASSUMED_FRAME_RATE: u64 = 30;

/// A parsed IVF file.
#[derive(Debug, Clone)]
pub struct IvfDemuxer {
    data: Vec<u8>,
    info: StreamInfo,
    /// `(payload start, payload end)` per frame, so `seek` needs no rescan.
    frames: Vec<(usize, usize)>,
    /// Ticks between consecutive frames, from the header's `scale`.
    ticks_per_frame: u64,
    cursor: usize,
}

fn u32le(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

fn u16le(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}

/// Maps IVF's fourcc to a codec. AV1 files use `AV01`, VP9 `VP90`.
fn codec_of(fourcc: &[u8; 4]) -> Option<CodecId> {
    match fourcc {
        b"AV01" => Some(CodecId::Av1),
        b"VP90" => Some(CodecId::Vp9),
        _ => None,
    }
}

impl IvfDemuxer {
    /// Parses an IVF file.
    pub fn new(data: Vec<u8>) -> Result<Self, KinetixError> {
        let (info, frames, ticks_per_frame) = parse(&data)?;
        Ok(Self {
            data,
            info,
            frames,
            ticks_per_frame,
            cursor: 0,
        })
    }

    /// The single video track.
    pub fn stream_info(&self) -> &StreamInfo {
        &self.info
    }

    /// The number of frames.
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// The presentation time of frame `i`, in the track's timescale.
    fn timestamp(&self, i: usize) -> u64 {
        i as u64 * self.ticks_per_frame
    }
}

/// The parsed header plus the per-frame payload ranges and the inter-frame tick
/// step.
type ParsedIvf = (StreamInfo, Vec<(usize, usize)>, u64);

fn parse(data: &[u8]) -> Result<ParsedIvf, KinetixError> {
    if data.len() < IVF_HEADER_LEN {
        return Err(KinetixError::Parse(
            "IVF file is shorter than its header".into(),
        ));
    }
    if &data[0..4] != b"DKIF" {
        return Err(KinetixError::Parse(
            "not an IVF file (missing DKIF magic)".into(),
        ));
    }
    let fourcc: [u8; 4] = [data[8], data[9], data[10], data[11]];
    let Some(codec) = codec_of(&fourcc) else {
        return Err(KinetixError::Unsupported(format!(
            "IVF codec {:?} is not supported (AV1 and VP9 are)",
            String::from_utf8_lossy(&fourcc)
        )));
    };
    let header_len = u16le(&data[6..8]) as usize;
    if header_len < IVF_HEADER_LEN || header_len > data.len() {
        return Err(KinetixError::Parse(format!(
            "IVF header length {header_len} is out of range"
        )));
    }
    let width = u32::from(u16le(&data[12..14]));
    let height = u32::from(u16le(&data[14..16]));
    // The header states a timebase of `scale / rate`, and frame `i` has
    // timestamp `i * scale` in a clock of `rate` ticks per second. ffmpeg writes
    // rate=30, scale=1 for 30 fps, so the timescale is the *rate* — reading the
    // scale as the timescale collapses every timestamp to zero.
    let rate = u32le(&data[16..20]);
    let scale = u32le(&data[20..24]);
    let timescale = if rate == 0 {
        ASSUMED_FRAME_RATE
    } else {
        u64::from(rate)
    };
    let ticks_per_frame = if scale == 0 {
        timescale / ASSUMED_FRAME_RATE
    } else {
        u64::from(scale)
    };

    let mut info = StreamInfo::new(0, codec, timescale as u32);
    info.media_type = MediaType::Video;
    info.width = width;
    info.height = height;

    // Walk the frames once; the payloads stay where they are in `data`.
    let mut frames = Vec::new();
    let mut pos = header_len;
    while pos + IVF_FRAME_HEADER_LEN <= data.len() {
        let size = u32le(&data[pos..pos + 4]) as usize;
        let start = pos + IVF_FRAME_HEADER_LEN;
        let Some(end) = start.checked_add(size) else {
            break; // a size that overflows: a truncated tail, not a hard error
        };
        if end > data.len() {
            // A truncated tail is common in fuzz corpora: stop rather than fail,
            // so the intact prefix stays usable.
            break;
        }
        frames.push((start, end));
        pos = end;
    }
    if frames.is_empty() {
        return Err(KinetixError::Parse("IVF file contains no frames".into()));
    }
    Ok((info, frames, ticks_per_frame))
}

impl Demuxer for IvfDemuxer {
    fn read_packet(&mut self) -> Result<Option<Packet>, KinetixError> {
        let Some(&(start, end)) = self.frames.get(self.cursor) else {
            return Ok(None);
        };
        let index = self.cursor;
        self.cursor += 1;
        // `time_base` is seconds-per-tick as (num, den), so a 30 ticks-per-second
        // clock is (1, 30).
        let ts = Timestamp::new(self.timestamp(index) as i64, (1, self.info.timescale));
        // IVF has no per-frame key-frame flag — the format simply does not carry
        // one. Every frame is reported as a key frame: the first must be one to be
        // decodable, and marking the rest keeps a caller that seeks on key frames
        // able to make progress. The decoder is what rejects mid-sequence frames.
        Ok(Some(Packet {
            data: self.data[start..end].to_vec(),
            stream_index: 0,
            pts: ts,
            dts: ts,
            is_key_frame: true,
        }))
    }

    fn seek(&mut self, target_pts_ms: i64) -> Result<(), KinetixError> {
        // Land on the last frame at or before the target.
        let scale = u64::from(self.info.timescale.max(1));
        let target = target_pts_ms.max(0) as u64 * scale / 1000;
        let mut chosen = 0usize;
        for i in 0..self.frames.len() {
            if self.timestamp(i) <= target {
                chosen = i;
            } else {
                break;
            }
        }
        self.cursor = chosen;
        Ok(())
    }
}
