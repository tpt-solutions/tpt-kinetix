//! WebM / Matroska writer — the royalty-free container for AV1, VP9 and Opus.
//!
//! [`WebmWriter`] is `Write`-based and works in two modes:
//!
//! * **live** (unknown segment and cluster sizes, no seeking): the output is a
//!   byte stream a browser `MediaRecorder` or an ffmpeg publisher would produce,
//!   readable incrementally by `MkvStream`;
//! * **finite** (seekable `Write + Seek`, the default): cluster and segment
//!   sizes are patched after the fact and a `Cues` index with one entry per key
//!   frame is appended, so the file seeks.
//!
//! Only the royalty-free codecs are supported, and only as passthrough: AV1
//! (`V_AV1`, `av1C` carried verbatim), VP9 (`V_VP9`, no `CodecPrivate`) and Opus
//! (`A_OPUS`, an `OpusHead` synthesised from the `dOps` record).
//!
//! ```no_run
//! use tpt_kinetix_core::codec::CodecId;
//! use tpt_kinetix_core::stream::StreamInfo;
//! use tpt_kinetix_mux::WebmWriter;
//!
//! let mut v = StreamInfo::new(0, CodecId::Vp9, 90_000);
//! v.width = 320;
//! v.height = 240;
//! let mut out = Vec::new();
//! let mut w = WebmWriter::new(&mut out);
//! w.set_tracks(&[v]).unwrap();
//! # let _ = w;
//! ```

use std::io::{self, Seek, SeekFrom, Write};

use tpt_kinetix_core::codec::{CodecId, MediaType};
use tpt_kinetix_core::error::KinetixError;
use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::stream::StreamInfo;

// EBML / Matroska element IDs used here.
const ID_EBML: u32 = 0x1A45_DFA3;
const ID_DOCTYPE: u32 = 0x4282;
const ID_SEGMENT: u32 = 0x1853_8067;
const ID_INFO: u32 = 0x1549_A966;
const ID_TIMECODE_SCALE: u32 = 0x2A_D7B1;
const ID_MUXING_APP: u32 = 0x4D80;
const ID_WRITING_APP: u32 = 0x5741;
const ID_DURATION: u32 = 0x4489;
const ID_TRACKS: u32 = 0x1654_AE6B;
const ID_TRACK_ENTRY: u32 = 0xAE;
const ID_TRACK_NUMBER: u32 = 0xD7;
const ID_TRACK_UID: u32 = 0x73C5;
const ID_TRACK_TYPE: u32 = 0x83;
const ID_CODEC_ID: u32 = 0x86;
const ID_CODEC_PRIVATE: u32 = 0x63A2;
const ID_CODEC_DELAY: u32 = 0x56AA;
const ID_SEEK_PRE_ROLL: u32 = 0x56BB;
const ID_DEFAULT_DURATION: u32 = 0x23_E383;
const ID_VIDEO: u32 = 0xE0;
const ID_PIXEL_WIDTH: u32 = 0xB0;
const ID_PIXEL_HEIGHT: u32 = 0xBA;
const ID_AUDIO: u32 = 0xE1;
const ID_SAMPLING_FREQUENCY: u32 = 0xB5;
const ID_CHANNELS: u32 = 0x9F;
const ID_BIT_DEPTH: u32 = 0x6264;
const ID_CLUSTER: u32 = 0x1F43_B675;
const ID_CLUSTER_TIMESTAMP: u32 = 0xE7;
const ID_SIMPLE_BLOCK: u32 = 0xA3;
const ID_CUES: u32 = 0x1C53_BB6B;
const ID_CUE_POINT: u32 = 0xBB;
const ID_CUE_TIME: u32 = 0xB3;
const ID_CUE_TRACK_POSITIONS: u32 = 0xB7;
const ID_CUE_TRACK: u32 = 0xF7;
const ID_CUE_CLUSTER_POSITION: u32 = 0xF1;

/// Matroska timestamps are in milliseconds (`TimecodeScale` = 1 ms).
const TIMECODE_SCALE_NS: u64 = 1_000_000;
/// `Block` timestamps are 16-bit signed offsets from the cluster timestamp, so a
/// new cluster is forced at least this often.
const MAX_CLUSTER_MS: i64 = 30_000;

/// WebM writing options.
#[derive(Debug, Clone)]
pub struct WebmOptions {
    /// Start a new cluster at every key frame of the first (video) track, and at
    /// least every this many milliseconds.
    pub cluster_ms: i64,
    /// `WritingApp` string (also used as `MuxingApp`).
    pub writing_app: String,
}

impl Default for WebmOptions {
    fn default() -> Self {
        Self {
            cluster_ms: 2000,
            writing_app: "tpt-kinetix".to_string(),
        }
    }
}

/// Writes an EBML element id (a class-A vint: the length is implied by the id).
fn vint_id(out: &mut Vec<u8>, id: u32) {
    let bytes = id.to_be_bytes();
    let first = bytes.iter().position(|b| *b != 0).unwrap_or(3);
    out.extend_from_slice(&bytes[first..]);
}

/// Writes an EBML size vint of exactly `len` bytes; `value` must fit in `7 * len`
/// bits (`vint_unknown` marks an unknown size).
fn vint_size(out: &mut Vec<u8>, value: u64, len: usize) {
    debug_assert!((1..=8).contains(&len));
    let v = value | (1u64 << (7 * len));
    out.extend_from_slice(&v.to_be_bytes()[8 - len..]);
}

/// The smallest size-vint width holding `value`.
fn vint_len(value: u64) -> usize {
    (1..=8).find(|len| value < (1u64 << (7 * len))).unwrap_or(8)
}

/// An all-ones (unknown-size) vint of `len` bytes.
fn vint_unknown(len: usize) -> u64 {
    (1u64 << (7 * len)) - 1
}

/// `id` + size + payload.
fn elem(out: &mut Vec<u8>, id: u32, body: &[u8]) {
    vint_id(out, id);
    vint_size(out, body.len() as u64, vint_len(body.len() as u64));
    out.extend_from_slice(body);
}

fn elem_uint(out: &mut Vec<u8>, id: u32, value: u64) {
    let bytes = value.to_be_bytes();
    let first = bytes.iter().position(|b| *b != 0).unwrap_or(7);
    elem(out, id, &bytes[first..]);
}

fn elem_float(out: &mut Vec<u8>, id: u32, value: f64) {
    elem(out, id, &value.to_be_bytes());
}

fn elem_string(out: &mut Vec<u8>, id: u32, value: &str) {
    elem(out, id, value.as_bytes());
}

/// The Matroska `CodecId` string for a supported track, or `None`.
fn codec_id(codec: CodecId) -> Option<&'static str> {
    match codec {
        CodecId::Av1 => Some("V_AV1"),
        CodecId::Vp9 => Some("V_VP9"),
        CodecId::Opus => Some("A_OPUS"),
        _ => None,
    }
}

/// The Matroska `CodecPrivate` for a track, derived from its MP4-style codec
/// configuration record (`StreamInfo::extradata`).
///
/// AV1's `av1C` record is carried verbatim (it is what Matroska stores); VP9
/// needs none; Opus needs an `OpusHead`, which this builds from the `dOps`
/// record (big-endian, no magic).
fn codec_private(info: &StreamInfo) -> Result<Option<Vec<u8>>, KinetixError> {
    match info.codec {
        CodecId::Av1 => {
            if info.extradata.first() != Some(&0x81) {
                return Err(KinetixError::Unsupported(
                    "an AV1 track needs an av1C codec configuration record".into(),
                ));
            }
            Ok(Some(info.extradata.clone()))
        }
        CodecId::Vp9 => Ok(None),
        CodecId::Opus => {
            let dops = info.extradata.as_slice();
            if dops.len() < 11 {
                return Err(KinetixError::Unsupported(
                    "an Opus track needs a dOps codec configuration record".into(),
                ));
            }
            let channels = dops[1];
            if channels == 0 {
                return Err(KinetixError::Unsupported(
                    "the Opus configuration record has no channels".into(),
                ));
            }
            let mut head = Vec::with_capacity(19 + channels as usize);
            head.extend_from_slice(b"OpusHead");
            head.push(1); // version
            head.push(channels);
            // Pre-skip, rate, gain and mapping family: little-endian in OpusHead,
            // big-endian in dOps.
            head.extend_from_slice(&u16::from_be_bytes([dops[2], dops[3]]).to_le_bytes());
            head.extend_from_slice(
                &u32::from_be_bytes([dops[4], dops[5], dops[6], dops[7]]).to_le_bytes(),
            );
            head.extend_from_slice(&i16::from_be_bytes([dops[8], dops[9]]).to_le_bytes());
            let family = dops[10];
            head.push(family);
            if family != 0 {
                let map = dops.get(11..).ok_or_else(|| {
                    KinetixError::Unsupported("the Opus channel map is truncated".into())
                })?;
                head.extend_from_slice(map);
            }
            Ok(Some(head))
        }
        _ => Ok(None),
    }
}

/// The Opus pre-skip in nanoseconds, from the `dOps` record (big-endian).
/// This is Matroska's `CodecDelay`.
fn opus_codec_delay(info: &StreamInfo) -> Option<u64> {
    if info.codec != CodecId::Opus {
        return None;
    }
    let dops = info.extradata.as_slice();
    if dops.len() < 4 {
        return None;
    }
    let pre_skip = u16::from_be_bytes([dops[2], dops[3]]);
    // 48 kHz is the only sample rate Opus decodes at.
    (pre_skip > 0).then(|| u64::from(pre_skip) * 1_000_000_000 / 48_000)
}

/// `DefaultDuration` in nanoseconds, when the stream info states one. Video
/// durations come from the packets, so this is only set for audio, whose
/// `StreamInfo` may carry a nominal frame size.
fn default_duration_ns(info: &StreamInfo) -> Option<u64> {
    if info.media_type != MediaType::Audio {
        return None;
    }
    let samples: u64 = match info.codec {
        CodecId::Opus => 960, // a nominal 20 ms frame
        _ => return None,
    };
    let rate = u64::from(info.sample_rate.max(1));
    Some(samples * 1_000_000_000 / rate)
}

/// A track as the writer needs it.
struct Track {
    /// Matroska track number (1-based, as written in every `Block`).
    number: u64,
    info: StreamInfo,
    private: Option<Vec<u8>>,
}

/// One `CuePoint`, remembered until `finish` writes the `Cues` element.
struct Cue {
    time_ms: u64,
    track: u64,
    /// Byte offset of the cluster's payload, relative to the segment's data.
    cluster_position: u64,
}

/// A cluster being written.
struct Cluster {
    /// Absolute offset of the cluster's size field (patched in finite mode).
    size_pos: u64,
    /// Offset just after the size field, i.e. where the payload starts.
    payload_pos: u64,
    /// Cluster timestamp in milliseconds.
    timestamp_ms: i64,
}

/// How the writer patches an already-written size vint: seek to an offset, write
/// those bytes, and leave the sink positioned for appending.
type Patch<W> = Box<dyn FnMut(&mut W, u64, &[u8]) -> Result<(), KinetixError>>;

/// Writes AV1 / VP9 / Opus into a WebM (Matroska) container.
///
/// See the [module documentation](self) for the two modes.
pub struct WebmWriter<W: Write> {
    out: W,
    opts: WebmOptions,
    /// `None` = live mode (unknown sizes, no Cues, no patching). The closure
    /// seeks to an offset, writes and leaves the sink positioned for appending.
    seekable: Option<Patch<W>>,
    tracks: Vec<Track>,
    lead: usize,
    cluster: Option<Cluster>,
    cues: Vec<Cue>,
    /// Offset of the segment's payload (where `ClusterPosition` counts from).
    segment_data_pos: u64,
    /// Position of the segment's size field, patched in finite mode.
    segment_size_pos: u64,
    /// Absolute position after everything written so far.
    pos: u64,
    /// Position of the reserved `Duration` double inside `Info`.
    duration_pos: u64,
    /// Time of the last frame written, for `Duration`.
    last_ms: i64,
    finished: bool,
}

impl<W: Write> WebmWriter<W> {
    /// A writer over `out` in live mode: unknown sizes, no seeking, no Cues.
    pub fn new(out: W) -> Self {
        Self {
            out,
            opts: WebmOptions::default(),
            seekable: None,
            tracks: Vec::new(),
            lead: 0,
            cluster: None,
            cues: Vec::new(),
            segment_data_pos: 0,
            segment_size_pos: 0,
            duration_pos: 0,
            pos: 0,
            last_ms: 0,
            finished: false,
        }
    }

    /// A writer with `opts` (live mode).
    pub fn with_options(out: W, opts: WebmOptions) -> Self {
        Self {
            opts,
            ..Self::new(out)
        }
    }

    /// Declares the tracks and writes the `EBML` header, `Segment`, `Info` and
    /// `Tracks`. Tracks that are not AV1, VP9 or Opus are skipped; audio and
    /// video must both be present for a useful file.
    pub fn set_tracks(&mut self, infos: &[StreamInfo]) -> Result<(), KinetixError> {
        if !self.tracks.is_empty() {
            return Err(KinetixError::Unsupported(
                "the track list was already written".into(),
            ));
        }
        let mut tracks = Vec::new();
        for (i, info) in infos.iter().enumerate() {
            if matches!(info.media_type, MediaType::Audio | MediaType::Video) {
                if codec_id(info.codec).is_none() {
                    continue;
                }
                let private = codec_private(info)?;
                tracks.push(Track {
                    number: tracks.len() as u64 + 1,
                    info: info.clone(),
                    private,
                });
            }
            let _ = i;
        }
        if tracks.is_empty() {
            return Err(KinetixError::Unsupported(
                "no AV1, VP9 or Opus track to write".into(),
            ));
        }
        self.lead = tracks
            .iter()
            .position(|t| t.info.media_type == MediaType::Video)
            .unwrap_or(0);
        self.tracks = tracks;

        let mut buf = Vec::new();
        self.write_header(&mut buf)?;
        self.write_info(&mut buf)?;
        self.write_tracks(&mut buf)?;
        self.flush(&buf)?;
        Ok(())
    }

    /// Number of tracks written.
    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }

    /// Bytes written so far.
    pub fn bytes_written(&self) -> u64 {
        self.pos
    }

    /// Ends the stream: closes the open cluster, patches the segment and
    /// cluster sizes (finite mode) and appends `Cues`.
    pub fn finish(&mut self) -> Result<(), KinetixError> {
        if self.finished || self.tracks.is_empty() {
            self.finished = true;
            return Ok(());
        }
        self.close_cluster()?;
        if self.seekable.is_some() {
            self.write_cues()?;
            self.patch_duration()?;
            self.patch_segment_size()?;
        }
        self.finished = true;
        Ok(())
    }

    /// Consumes the writer and returns the underlying sink.
    pub fn into_inner(self) -> W {
        self.out
    }
}

impl<W: Write> WebmWriter<W> {
    /// Feeds one packet; `duration` is its duration in the track's timescale
    /// ticks (`None` when unknown, which is fine for Matroska).
    pub fn write_packet(
        &mut self,
        packet: &Packet,
        duration: Option<u32>,
    ) -> Result<(), KinetixError> {
        if self.finished {
            return Ok(());
        }
        let Some(track) = self.tracks.get(packet.stream_index as usize) else {
            return Ok(()); // a track we do not write
        };
        let timescale = track.info.timescale.max(1);
        let pts_ms = packet.pts.as_millis().unwrap_or(0);
        let key = packet.is_key_frame;
        let data = packet.data.clone();
        let number = track.number;
        let is_lead = number == self.tracks[self.lead].number;

        let base = self.cluster.as_ref().map_or(pts_ms, |c| c.timestamp_ms);
        let long = pts_ms - base >= MAX_CLUSTER_MS;
        let due = is_lead && key && pts_ms - base >= self.opts.cluster_ms;
        if self.cluster.is_none() || long || due {
            self.close_cluster()?;
            self.open_cluster(pts_ms)?;
        }
        let base = self.cluster.as_ref().map_or(0, |c| c.timestamp_ms);
        self.write_block(number, (pts_ms - base) as i16, key, &data)?;
        self.last_ms = self.last_ms.max(pts_ms + duration_ms(duration, timescale));
        Ok(())
    }

    /// Like [`Self::write_packet`], but with the frame's duration in
    /// milliseconds (what the live ingest paths know).
    pub fn write_packet_ms(
        &mut self,
        packet: &Packet,
        duration_ms: Option<u32>,
    ) -> Result<(), KinetixError> {
        let timescale = self
            .tracks
            .get(packet.stream_index as usize)
            .map_or(1000, |t| t.info.timescale.max(1));
        let d = duration_ms.map(|ms| (u64::from(ms) * u64::from(timescale) / 1000) as u32);
        self.write_packet(packet, d)
    }
}

fn duration_ms(duration: Option<u32>, timescale: u32) -> i64 {
    match duration {
        Some(d) => i64::from(d) * 1000 / i64::from(timescale.max(1)),
        None => 0,
    }
}

fn map_io(e: io::Error) -> KinetixError {
    KinetixError::Io(e)
}

impl<W: Write> WebmWriter<W> {
    fn write_header(&mut self, buf: &mut Vec<u8>) -> Result<(), KinetixError> {
        let mut body = Vec::new();
        elem_uint(&mut body, 0x4286, 1); // EBMLVersion
        elem_uint(&mut body, 0x42F7, 1); // EBMLReadVersion
        elem_uint(&mut body, 0x42F2, 4); // EBMLMaxIDLength
        elem_uint(&mut body, 0x42F3, 8); // EBMLMaxSizeLength
        elem_string(&mut body, ID_DOCTYPE, "webm");
        elem_uint(&mut body, 0x4287, 2); // DocTypeVersion
        elem_uint(&mut body, 0x4285, 2); // DocTypeReadVersion
        let mut head = Vec::new();
        elem(&mut head, ID_EBML, &body);

        // Segment: unknown size in live mode, patched in finite mode.
        vint_id(&mut head, ID_SEGMENT);
        if self.seekable.is_some() {
            self.segment_size_pos = self.pos + head.len() as u64;
            vint_size(&mut head, 0, 8);
        } else {
            vint_size(&mut head, vint_unknown(8), 8);
        }
        self.segment_data_pos = self.pos + head.len() as u64;
        buf.extend_from_slice(&head);
        Ok(())
    }

    fn write_info(&mut self, buf: &mut Vec<u8>) -> Result<(), KinetixError> {
        let mut body = Vec::new();
        elem_uint(&mut body, ID_TIMECODE_SCALE, TIMECODE_SCALE_NS);
        elem_string(&mut body, ID_MUXING_APP, &self.opts.writing_app);
        elem_string(&mut body, ID_WRITING_APP, &self.opts.writing_app);
        // `Duration` is only known at the end, so a fixed-width 8-byte double is
        // reserved here (as a zero) and patched by `finish`. Its offset is found
        // by serialising the element first, so no offset arithmetic is needed.
        elem_float(&mut body, ID_DURATION, 0.0);
        let mut info = Vec::new();
        elem(&mut info, ID_INFO, &body);
        // The Duration payload is the last 8 bytes of `info`.
        self.duration_pos = self.pos + buf.len() as u64 + info.len() as u64 - 8;
        buf.extend_from_slice(&info);
        Ok(())
    }

    /// Fills in the reserved `Duration` double (finite mode only).
    fn patch_duration(&mut self) -> Result<(), KinetixError> {
        let ms = self.last_ms.max(0);
        if ms <= 0 || self.seekable.is_none() {
            return Ok(());
        }
        let bytes = (ms as f64).to_be_bytes();
        match self.seekable.as_mut() {
            Some(patch) => patch(&mut self.out, self.duration_pos, &bytes),
            None => Ok(()),
        }
    }

    fn write_tracks(&mut self, buf: &mut Vec<u8>) -> Result<(), KinetixError> {
        let mut body = Vec::new();
        for t in &self.tracks {
            let mut entry = Vec::new();
            elem_uint(&mut entry, ID_TRACK_NUMBER, t.number);
            elem_uint(&mut entry, ID_TRACK_UID, t.number);
            let kind = match t.info.media_type {
                MediaType::Video => 1u64,
                MediaType::Audio => 2,
                _ => 17,
            };
            elem_uint(&mut entry, ID_TRACK_TYPE, kind);
            if let Some(ns) = default_duration_ns(&t.info) {
                elem_uint(&mut entry, ID_DEFAULT_DURATION, ns);
            }
            let id = codec_id(t.info.codec).unwrap_or("V_VP9");
            elem_string(&mut entry, ID_CODEC_ID, id);
            if let Some(p) = &t.private {
                elem(&mut entry, ID_CODEC_PRIVATE, p);
            }
            let mut sub = Vec::new();
            match t.info.media_type {
                MediaType::Video => {
                    elem_uint(&mut sub, ID_PIXEL_WIDTH, u64::from(t.info.width));
                    elem_uint(&mut sub, ID_PIXEL_HEIGHT, u64::from(t.info.height));
                    elem(&mut entry, ID_VIDEO, &sub);
                }
                MediaType::Audio => {
                    // Opus's pre-skip is `CodecDelay` in Matroska: players trim
                    // that many samples from the first packet, which is what makes
                    // the stream start gapless. Without it the encoder's warm-up
                    // samples are audible and the stream starts late.
                    if let Some(delay) = opus_codec_delay(&t.info) {
                        elem_uint(&mut sub, ID_CODEC_DELAY, delay);
                        // SeekPreRoll is the same value, per the Matroska spec.
                        elem_uint(&mut sub, ID_SEEK_PRE_ROLL, delay);
                    }
                    elem_float(
                        &mut sub,
                        ID_SAMPLING_FREQUENCY,
                        f64::from(t.info.sample_rate),
                    );
                    elem_uint(&mut sub, ID_CHANNELS, u64::from(t.info.channels));
                    if t.info.bits_per_sample > 0 {
                        elem_uint(&mut sub, ID_BIT_DEPTH, u64::from(t.info.bits_per_sample));
                    }
                    elem(&mut entry, ID_AUDIO, &sub);
                }
                _ => {}
            }
            elem(&mut body, ID_TRACK_ENTRY, &entry);
        }
        elem(buf, ID_TRACKS, &body);
        Ok(())
    }

    fn open_cluster(&mut self, timestamp_ms: i64) -> Result<(), KinetixError> {
        // `Cluster` timestamps are unsigned, but `Block` timecodes are signed
        // offsets from them, so a stream that starts before zero (Opus pre-skip
        // puts the first audio packet at -7 ms) keeps its negative offset here.
        let timestamp_ms = timestamp_ms.max(0);
        let mut buf = Vec::new();
        vint_id(&mut buf, ID_CLUSTER);
        let size_pos = self.pos + buf.len() as u64;
        if self.seekable.is_some() {
            vint_size(&mut buf, 0, 8);
        } else {
            vint_size(&mut buf, vint_unknown(8), 8);
        }
        let payload_pos = self.pos + buf.len() as u64;
        elem_uint(&mut buf, ID_CLUSTER_TIMESTAMP, timestamp_ms.max(0) as u64);
        self.flush(&buf)?;
        self.cues.push(Cue {
            time_ms: timestamp_ms.max(0) as u64,
            track: self.tracks[self.lead].number,
            cluster_position: payload_pos - self.segment_data_pos,
        });
        self.cluster = Some(Cluster {
            size_pos,
            payload_pos,
            timestamp_ms,
        });
        Ok(())
    }

    fn write_block(
        &mut self,
        track: u64,
        rel_ms: i16,
        key: bool,
        data: &[u8],
    ) -> Result<(), KinetixError> {
        let mut body = Vec::with_capacity(data.len() + 4);
        vint_size(&mut body, track, 1); // track number as a 1-byte vint
        body.extend_from_slice(&rel_ms.to_be_bytes());
        // Keyframe flag; no lacing, no invisible, no discardable.
        body.push(if key { 0x80 } else { 0x00 });
        body.extend_from_slice(data);
        let mut buf = Vec::new();
        elem(&mut buf, ID_SIMPLE_BLOCK, &body);
        self.flush(&buf)
    }

    fn close_cluster(&mut self) -> Result<(), KinetixError> {
        let Some(c) = self.cluster.take() else {
            return Ok(());
        };
        if self.seekable.is_none() {
            return Ok(());
        }
        let size = self.pos - c.payload_pos;
        self.patch_vint_at(c.size_pos, size, 8)
    }

    fn write_cues(&mut self) -> Result<(), KinetixError> {
        if self.cues.is_empty() {
            return Ok(());
        }
        let mut body = Vec::new();
        for cue in &self.cues {
            let mut point = Vec::new();
            elem_uint(&mut point, ID_CUE_TIME, cue.time_ms);
            let mut positions = Vec::new();
            elem_uint(&mut positions, ID_CUE_TRACK, cue.track);
            elem_uint(
                &mut positions,
                ID_CUE_CLUSTER_POSITION,
                cue.cluster_position,
            );
            elem(&mut point, ID_CUE_TRACK_POSITIONS, &positions);
            elem(&mut body, ID_CUE_POINT, &point);
        }
        let mut buf = Vec::new();
        elem(&mut buf, ID_CUES, &body);
        self.flush(&buf)
    }

    fn patch_segment_size(&mut self) -> Result<(), KinetixError> {
        if self.seekable.is_none() {
            return Ok(());
        }
        let size = self.pos - self.segment_data_pos;
        self.patch_vint_at(self.segment_size_pos, size, 8)
    }

    fn flush(&mut self, buf: &[u8]) -> Result<(), KinetixError> {
        self.out.write_all(buf).map_err(map_io)?;
        self.pos += buf.len() as u64;
        Ok(())
    }

    /// Overwrites a size vint of `len` bytes at `at` with `value` (finite mode).
    /// The patch closure seeks, writes and restores the position itself.
    fn patch_vint_at(&mut self, at: u64, value: u64, len: usize) -> Result<(), KinetixError> {
        let mut buf = Vec::new();
        vint_size(&mut buf, value, len);
        match self.seekable.as_mut() {
            Some(patch) => patch(&mut self.out, at, &buf),
            None => Ok(()),
        }
    }
}

impl<W: Write + Seek> WebmWriter<W> {
    /// A writer in finite mode: cluster and segment sizes are patched after the
    /// fact and a `Cues` index is appended, so the file is seekable.
    pub fn new_seekable(out: W) -> Self {
        Self {
            seekable: Some(Box::new(|out: &mut W, at: u64, bytes: &[u8]| {
                out.seek(SeekFrom::Start(at)).map_err(map_io)?;
                out.write_all(bytes).map_err(map_io)?;
                // Patching never changes the length, so the end of the file is
                // where the writer's next append goes.
                out.seek(SeekFrom::End(0)).map_err(map_io)?;
                Ok(())
            })),
            ..Self::new(out)
        }
    }
}
