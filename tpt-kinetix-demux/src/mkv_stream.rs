//! Incremental (sans-IO) WebM / Matroska parser for live ingest of the
//! royalty-free codecs: **AV1**, **VP9** and **Opus**.
//!
//! [`MkvStream`] is fed arbitrary byte chunks as they arrive (an HTTP POST body,
//! a pipe, a `MediaRecorder` upload) and yields [`MkvEvent`]s: the track list
//! (as [`StreamInfo`] with MP4-style codec configuration records ready for an
//! fMP4 muxer) and then the frames. It copes with what live muxers produce:
//! unknown-size `Segment` and `Cluster` elements, `SimpleBlock` and `BlockGroup`,
//! chunk boundaries anywhere, and no seek index.
//!
//! Frames are returned in file order; WebM carries presentation timestamps only
//! (AV1/VP9/Opus have no reordering at the container level), so `dts == pts`.
//!
//! Limits (hostile input): leaf elements are capped at [`MAX_ELEMENT_BYTES`],
//! at most [`MAX_BUFFERED_FRAMES`] frames wait for a late track announcement, and
//! laced blocks are rejected.

use tpt_kinetix_core::codec::{CodecId, MediaType};
use tpt_kinetix_core::error::KinetixError;
use tpt_kinetix_core::stream::StreamInfo;

use crate::rfconfig::{opus_head_to_dops, vp9_config_from_frame, vpcc_record};

/// Largest leaf element accepted (a frame, `CodecPrivate`, ...): 64 MiB.
pub const MAX_ELEMENT_BYTES: u64 = 64 << 20;
/// Frames held while waiting for the first VP9 key frame to complete the track config.
pub const MAX_BUFFERED_FRAMES: usize = 4096;

const ID_SEGMENT: u32 = 0x1853_8067;
const ID_INFO: u32 = 0x1549_A966;
const ID_TIMECODE_SCALE: u32 = 0x2A_D7B1;
const ID_TRACKS: u32 = 0x1654_AE6B;
const ID_TRACK_ENTRY: u32 = 0xAE;
const ID_TRACK_NUMBER: u32 = 0xD7;
const ID_TRACK_TYPE: u32 = 0x83;
const ID_CODEC_ID: u32 = 0x86;
const ID_CODEC_PRIVATE: u32 = 0x63A2;
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
const ID_BLOCK_GROUP: u32 = 0xA0;
const ID_BLOCK: u32 = 0xA1;
const ID_BLOCK_DURATION: u32 = 0x9B;
const ID_REFERENCE_BLOCK: u32 = 0xFB;
const ID_DISCARD_PADDING: u32 = 0x75A2;
const ID_CUES: u32 = 0x1C53_BB6B;
const ID_CUE_POINT: u32 = 0xBB;
const ID_CUE_TIME: u32 = 0xB3;
const ID_CUE_TRACK_POSITIONS: u32 = 0xB7;
const ID_CUE_TRACK: u32 = 0xF7;
const ID_CUE_CLUSTER_POSITION: u32 = 0xF1;

/// Something the parser found in the stream.
#[derive(Debug, Clone, PartialEq)]
pub enum MkvEvent {
    /// The supported tracks, announced once before the first frame. A frame's
    /// `stream` indexes this list.
    Tracks(Vec<StreamInfo>),
    /// One coded frame.
    Frame(MkvFrame),
    /// One `CuePoint` from the `Cues` index.
    ///
    /// Cues appear *after* the clusters they describe in a seekable file, so this
    /// arrives only once the body has already been streamed past — it is what
    /// makes a second, cheap pass at an arbitrary byte offset possible.
    Cue(MkvCue),
}

/// One entry of the Matroska `Cues` index: a random-access point.
#[derive(Debug, Clone, PartialEq)]
pub struct MkvCue {
    /// Presentation time of the cue, in milliseconds.
    pub time_ms: i64,
    /// The track number the cue applies to (1-based, as Matroska numbers them).
    pub track: u64,
    /// Byte offset of the cluster holding this cue, **relative to the start of the
    /// Segment's data** — which is how Matroska states it. Add
    /// [`MkvStream::segment_data_start`] to get an absolute file offset.
    pub cluster_position: u64,
}

/// A frame in container order.
#[derive(Debug, Clone, PartialEq)]
pub struct MkvFrame {
    /// Index into the announced track list.
    pub stream: usize,
    /// Presentation (= decode) time in milliseconds.
    pub pts_ms: i64,
    /// Whether the frame is a random-access point.
    pub key: bool,
    /// The codec payload (AV1 temporal unit without delimiter, VP9 frame, Opus packet).
    pub data: Vec<u8>,
    /// Duration in milliseconds when the container states it (`BlockDuration`).
    pub duration_ms: Option<u32>,
    /// Samples to discard from the end of this frame's presentation
    /// (`DiscardPadding`), in the codec's own rate — 48 kHz for Opus. `0` when
    /// the container states none.
    ///
    /// Matroska states the value in nanoseconds; it is converted here so callers
    /// can subtract it from a frame's sample count directly.
    pub discard_padding: u64,
    /// Absolute byte offset of the payload within the stream, as fed to
    /// [`MkvStream::push`]. A reader over a [`ReadAt`](crate::source::ReadAt)
    /// source uses it to fetch exactly this frame with one ranged read.
    pub offset: u64,
}

#[derive(Default, Clone)]
struct TrackBuilder {
    number: u64,
    kind: u64,
    codec_id: String,
    private: Vec<u8>,
    width: u32,
    height: u32,
    rate: f64,
    channels: u32,
    bit_depth: u32,
}

struct RawFrame {
    track: u64,
    pts_ms: i64,
    key: bool,
    data: Vec<u8>,
    duration_ms: Option<u32>,
    /// `DiscardPadding` in samples (see [`MkvFrame::discard_padding`]).
    discard_padding: u64,
    /// Absolute offset of `data` in the stream.
    offset: u64,
}

/// An incremental WebM/Matroska parser.
pub struct MkvStream {
    buf: Vec<u8>,
    /// Bytes of the current unknown-or-skipped element still to discard.
    skip: u64,
    timecode_scale_ns: u64,
    cluster_ts: i64,
    building: Option<TrackBuilder>,
    track_builders: Vec<TrackBuilder>,
    in_tracks: bool,
    announced: Option<Vec<usize>>, // builder index per announced stream
    infos: Vec<StreamInfo>,
    waiting: Vec<RawFrame>,
    /// A `Block` waiting to learn whether its `BlockGroup` has a `ReferenceBlock`.
    pending_group: Option<RawFrame>,
    seen_ebml: bool,
    events: Vec<MkvEvent>,
    /// Absolute offset of `buf[0]` in the stream, so frames can report theirs.
    base: u64,
    /// Absolute offset of the Segment's first data byte. `CueClusterPosition` is
    /// relative to this, so cue positions are only useful once it is known.
    segment_data_start: u64,
    /// The `CuePoint` being assembled, if the parser is inside `Cues`.
    cue: Option<CueBuilder>,
}

/// A `CuePoint` under construction.
#[derive(Debug, Default, Clone, Copy)]
struct CueBuilder {
    time_ticks: u64,
    track: u64,
    cluster_position: u64,
}

fn parse_id(data: &[u8]) -> Option<(u32, usize)> {
    let first = *data.first()?;
    let len = first.leading_zeros() as usize + 1;
    if len > 4 || data.len() < len {
        return if len > 4 { Some((0, usize::MAX)) } else { None };
    }
    let mut id = 0u32;
    for &b in &data[..len] {
        id = (id << 8) | u32::from(b);
    }
    Some((id, len))
}

/// Reads an EBML size; `u64::MAX` means "unknown size".
fn parse_size(data: &[u8]) -> Option<(u64, usize)> {
    let first = *data.first()?;
    let len = first.leading_zeros() as usize + 1;
    if len > 8 {
        return Some((0, usize::MAX));
    }
    if data.len() < len {
        return None;
    }
    let mut v = u64::from(first) & ((1u64 << (8 - len)) - 1);
    let mut all_ones = v == (1u64 << (8 - len)) - 1;
    for &b in &data[1..len] {
        v = (v << 8) | u64::from(b);
        all_ones &= b == 0xFF;
    }
    Some((if all_ones { u64::MAX } else { v }, len))
}

fn uint(b: &[u8]) -> u64 {
    b.iter().take(8).fold(0u64, |a, &x| (a << 8) | u64::from(x))
}

fn float(b: &[u8]) -> f64 {
    match b.len() {
        4 => f64::from(f32::from_be_bytes(b.try_into().unwrap())),
        8 => f64::from_be_bytes(b.try_into().unwrap()),
        _ => 0.0,
    }
}

impl Default for MkvStream {
    fn default() -> Self {
        Self::new()
    }
}

impl MkvStream {
    /// A parser expecting the start of a WebM/Matroska stream.
    pub fn new() -> Self {
        Self {
            buf: Vec::new(),
            skip: 0,
            timecode_scale_ns: 1_000_000,
            cluster_ts: 0,
            building: None,
            track_builders: Vec::new(),
            in_tracks: false,
            announced: None,
            infos: Vec::new(),
            waiting: Vec::new(),
            pending_group: None,
            seen_ebml: false,
            events: Vec::new(),
            base: 0,
            segment_data_start: 0,
            cue: None,
        }
    }

    /// Absolute offset of the Segment's first data byte — the base that
    /// `CueClusterPosition` values are stated against.
    pub fn segment_data_start(&self) -> u64 {
        self.segment_data_start
    }

    /// Feeds the next chunk and returns the events it completes.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<MkvEvent>, KinetixError> {
        let mut chunk = chunk;
        if self.skip > 0 {
            let n = self.skip.min(chunk.len() as u64) as usize;
            self.skip -= n as u64;
            chunk = &chunk[n..];
            // Skipped bytes leave the stream without ever entering `buf`, so
            // `base` (the offset of `buf[0]`) must account for them here.
            self.base += n as u64;
        }
        // `base` tracks buf[0] and only advances as the buffer is drained below.
        self.buf.extend_from_slice(chunk);
        self.drain(false)?;
        Ok(std::mem::take(&mut self.events))
    }

    /// Signals the end of the stream (flushes a group still waiting for its
    /// `ReferenceBlock`, and the last `CuePoint`).
    pub fn finish(&mut self) -> Result<Vec<MkvEvent>, KinetixError> {
        self.flush_cue();
        self.drain(true)?;
        self.flush_group()?;
        Ok(std::mem::take(&mut self.events))
    }

    fn drain(&mut self, _end: bool) -> Result<(), KinetixError> {
        let mut pos = 0usize;
        loop {
            if self.skip > 0 {
                let avail = (self.buf.len() - pos) as u64;
                let n = self.skip.min(avail) as usize;
                pos += n;
                self.skip -= n as u64;
                if self.skip > 0 {
                    break;
                }
            }
            let rest = &self.buf[pos..];
            let Some((id, idlen)) = parse_id(rest) else {
                break;
            };
            if idlen == usize::MAX {
                return Err(KinetixError::Parse("invalid EBML element id".into()));
            }
            let Some((size, szlen)) = parse_size(&rest[idlen..]) else {
                break;
            };
            if szlen == usize::MAX {
                return Err(KinetixError::Parse("invalid EBML element size".into()));
            }
            let header = idlen + szlen;
            if !self.seen_ebml && id != 0x1A45_DFA3 {
                return Err(KinetixError::Parse("not a WebM/Matroska stream".into()));
            }

            // A `CuePoint` ends when the next element is not one of its children: that is
            // the only point at which all its fields are known, because the cue's
            // time and cluster position arrive *after* `CueTrackPositions` is
            // entered.
            if self.cue.is_some()
                && !matches!(
                    id,
                    ID_CUE_TIME | ID_CUE_TRACK_POSITIONS | ID_CUE_TRACK | ID_CUE_CLUSTER_POSITION
                )
            {
                // Inlined rather than `self.flush_cue()`: `rest` borrows
                // `self.buf` here, so only disjoint field accesses are allowed.
                if let Some(c) = self.cue.take() {
                    let time_ms = (c.time_ticks * self.timecode_scale_ns / 1_000_000) as i64;
                    self.events.push(MkvEvent::Cue(MkvCue {
                        time_ms,
                        track: c.track,
                        cluster_position: c.cluster_position,
                    }));
                }
            }
            match id {
                // Master elements we descend into (their children arrive flat).
                ID_SEGMENT
                | ID_INFO
                | ID_TRACKS
                | ID_TRACK_ENTRY
                | ID_VIDEO
                | ID_AUDIO
                | ID_CLUSTER
                | ID_BLOCK_GROUP
                | ID_CUES
                | ID_CUE_POINT
                | ID_CUE_TRACK_POSITIONS => {
                    if id == ID_SEGMENT {
                        // `pos` still points at the Segment's *id*; the data
                        // starts after the header, which is what every
                        // `CueClusterPosition` is relative to.
                        self.segment_data_start = self.base + (pos + header) as u64;
                    }
                    if id == ID_CUE_POINT {
                        self.cue = Some(CueBuilder::default());
                    }
                    pos += header;
                    self.enter(id)?;
                }
                _ => {
                    if size == u64::MAX {
                        return Err(KinetixError::Parse(format!("unknown-size element {id:#x}")));
                    }
                    if id == 0x1A45_DFA3 {
                        self.seen_ebml = true;
                    }
                    let wanted = Self::wants_body(id);
                    if wanted {
                        if size > MAX_ELEMENT_BYTES {
                            return Err(KinetixError::Parse(format!(
                                "element {id:#x} of {size} bytes is too large"
                            )));
                        }
                        if (rest.len() as u64) < header as u64 + size {
                            break; // wait for the whole element
                        }
                        let body = rest[header..header + size as usize].to_vec();
                        let body_offset = self.base + pos as u64 + header as u64;
                        pos += header + size as usize;
                        self.leaf(id, &body, body_offset)?;
                    } else {
                        // Skip without buffering (Cues, Void, Tags, the EBML header ...).
                        pos += header;
                        self.skip = size;
                    }
                }
            }
        }
        self.buf.drain(..pos);
        // `base` is the absolute offset of buf[0]: it advances by exactly what
        // this pass consumed, which can exceed what `push` fed in (a single push
        // may complete elements that were buffered by earlier calls).
        self.base = self.base.saturating_add(pos as u64);
        Ok(())
    }

    /// Emits the pending `CuePoint`, if any. Called when the next non-cue element
    /// arrives, and by [`MkvStream::finish`] for the last one.
    fn flush_cue(&mut self) {
        if let Some(c) = self.cue.take() {
            let time_ms = (c.time_ticks * self.timecode_scale_ns / 1_000_000) as i64;
            self.events.push(MkvEvent::Cue(MkvCue {
                time_ms,
                track: c.track,
                cluster_position: c.cluster_position,
            }));
        }
    }

    /// Ends the stream, returning any events the final elements completed —
    /// notably the last `CuePoint`, whose end is only known at end of input.
    fn wants_body(id: u32) -> bool {
        matches!(
            id,
            ID_TIMECODE_SCALE
                | ID_TRACK_NUMBER
                | ID_TRACK_TYPE
                | ID_CODEC_ID
                | ID_CODEC_PRIVATE
                | ID_DEFAULT_DURATION
                | ID_PIXEL_WIDTH
                | ID_PIXEL_HEIGHT
                | ID_SAMPLING_FREQUENCY
                | ID_CHANNELS
                | ID_BIT_DEPTH
                | ID_CLUSTER_TIMESTAMP
                | ID_SIMPLE_BLOCK
                | ID_BLOCK
                | ID_BLOCK_DURATION
                | ID_REFERENCE_BLOCK
                | ID_DISCARD_PADDING
                | ID_CUE_TIME
                | ID_CUE_TRACK
                | ID_CUE_CLUSTER_POSITION
        )
    }

    fn enter(&mut self, id: u32) -> Result<(), KinetixError> {
        match id {
            ID_TRACKS => self.in_tracks = true,
            ID_TRACK_ENTRY => {
                self.finish_builder();
                self.building = Some(TrackBuilder::default());
            }
            ID_CLUSTER => {
                self.flush_group()?;
                self.finish_builder();
                self.in_tracks = false;
                self.cluster_ts = 0;
                self.maybe_announce()?;
            }
            ID_BLOCK_GROUP => self.flush_group()?,
            _ => {}
        }
        Ok(())
    }

    fn finish_builder(&mut self) {
        if let Some(b) = self.building.take() {
            self.track_builders.push(b);
        }
    }

    fn leaf(&mut self, id: u32, body: &[u8], body_offset: u64) -> Result<(), KinetixError> {
        // Cue children are handled first: they share no meaning with the track
        // and cluster elements below, and the cue is emitted when its
        // `CueTrackPositions` closes (see `enter`).
        if self.cue.is_some() {
            match id {
                ID_CUE_TIME => {
                    if let Some(c) = self.cue.as_mut() {
                        c.time_ticks = uint(body);
                    }
                }
                ID_CUE_TRACK => {
                    if let Some(c) = self.cue.as_mut() {
                        c.track = uint(body);
                    }
                }
                ID_CUE_CLUSTER_POSITION => {
                    if let Some(c) = self.cue.as_mut() {
                        c.cluster_position = uint(body);
                    }
                }
                _ => {}
            }
            return Ok(());
        }
        match id {
            ID_TIMECODE_SCALE => self.timecode_scale_ns = uint(body).max(1),
            ID_CLUSTER_TIMESTAMP => self.cluster_ts = uint(body) as i64,
            ID_SIMPLE_BLOCK => {
                self.flush_group()?;
                if let Some(f) = self.parse_block(body, body_offset, true)? {
                    self.accept(f)?;
                }
            }
            ID_BLOCK => {
                self.flush_group()?;
                self.pending_group = self.parse_block(body, body_offset, false)?;
            }
            ID_BLOCK_DURATION => {
                let ms = uint(body) as u128 * u128::from(self.timecode_scale_ns) / 1_000_000;
                if let Some(g) = &mut self.pending_group {
                    g.duration_ms = Some(ms.min(u128::from(u32::MAX)) as u32);
                }
            }
            ID_REFERENCE_BLOCK => {
                if let Some(g) = &mut self.pending_group {
                    g.key = false;
                }
            }
            ID_DISCARD_PADDING => {
                // Matroska states this in TimestampScale units (nanoseconds here),
                // but it is a trim in *samples*: convert so callers work in the
                // codec's own rate. 0x00CDFE60 (13.5 ms) is 648 of a 960-sample
                // Opus frame, which is how ffmpeg ends an encode.
                if let Some(g) = &mut self.pending_group {
                    let ns = uint(body);
                    g.discard_padding = (ns / 1_000_000) * 48 + (ns % 1_000_000) * 48 / 1_000_000;
                }
            }
            _ => {
                if let Some(b) = &mut self.building {
                    match id {
                        ID_TRACK_NUMBER => b.number = uint(body),
                        ID_TRACK_TYPE => b.kind = uint(body),
                        ID_CODEC_ID => {
                            b.codec_id = String::from_utf8_lossy(body)
                                .trim_end_matches('\0')
                                .to_string()
                        }
                        ID_CODEC_PRIVATE => b.private = body.to_vec(),
                        ID_PIXEL_WIDTH => b.width = uint(body).min(u64::from(u32::MAX)) as u32,
                        ID_PIXEL_HEIGHT => b.height = uint(body).min(u64::from(u32::MAX)) as u32,
                        ID_SAMPLING_FREQUENCY => b.rate = float(body),
                        ID_CHANNELS => b.channels = uint(body).min(255) as u32,
                        ID_BIT_DEPTH => b.bit_depth = uint(body).min(64) as u32,
                        _ => {}
                    }
                }
            }
        }
        Ok(())
    }

    /// `body_offset` is the absolute offset of `body` in the stream.
    fn parse_block(
        &self,
        body: &[u8],
        body_offset: u64,
        simple: bool,
    ) -> Result<Option<RawFrame>, KinetixError> {
        let Some((track, tl)) = parse_size(body) else {
            return Ok(None);
        };
        if tl == usize::MAX || body.len() < tl + 3 {
            return Ok(None);
        }
        let rel = i16::from_be_bytes([body[tl], body[tl + 1]]);
        let flags = body[tl + 2];
        if flags & 0x06 != 0 {
            return Err(KinetixError::Unsupported(
                "laced Matroska blocks are not supported".into(),
            ));
        }
        let ticks = self.cluster_ts + i64::from(rel);
        let pts_ms = (i128::from(ticks) * i128::from(self.timecode_scale_ns) / 1_000_000) as i64;
        Ok(Some(RawFrame {
            track,
            pts_ms,
            key: !simple || flags & 0x80 != 0,
            data: body[tl + 3..].to_vec(),
            duration_ms: None,
            discard_padding: 0,
            offset: body_offset + (tl + 3) as u64,
        }))
    }

    fn flush_group(&mut self) -> Result<(), KinetixError> {
        if let Some(f) = self.pending_group.take() {
            self.accept(f)?;
        }
        Ok(())
    }

    /// Routes a frame: straight out once tracks are announced, else buffered.
    fn accept(&mut self, f: RawFrame) -> Result<(), KinetixError> {
        if self.announced.is_none() {
            if self.waiting.len() >= MAX_BUFFERED_FRAMES {
                return Err(KinetixError::Parse(
                    "track configuration never completed".into(),
                ));
            }
            self.waiting.push(f);
            return self.maybe_announce();
        }
        self.emit(f);
        Ok(())
    }

    fn emit(&mut self, f: RawFrame) {
        let Some(map) = &self.announced else { return };
        let Some(stream) = map
            .iter()
            .position(|&b| self.track_builders[b].number == f.track)
        else {
            return; // a track we do not support
        };
        self.events.push(MkvEvent::Frame(MkvFrame {
            stream,
            pts_ms: f.pts_ms,
            key: f.key,
            data: f.data,
            duration_ms: f.duration_ms,
            discard_padding: f.discard_padding,
            offset: f.offset,
        }));
    }

    /// Builds the track list as soon as every supported track's configuration is
    /// known (VP9 needs the first key frame when `CodecPrivate` lacks it).
    fn maybe_announce(&mut self) -> Result<(), KinetixError> {
        if self.announced.is_some() || self.track_builders.is_empty() {
            return Ok(());
        }
        let mut infos = Vec::new();
        let mut map = Vec::new();
        for (bi, b) in self.track_builders.iter().enumerate() {
            let sample = self
                .waiting
                .iter()
                .find(|f| f.track == b.number && f.key)
                .map(|f| f.data.as_slice());
            match build_stream_info(infos.len() as u32, b, sample)? {
                Built::Info(info) => {
                    infos.push(info);
                    map.push(bi);
                }
                Built::Unsupported => {}
                Built::NeedFrame => return Ok(()), // retry once its key frame arrives
            }
        }
        if infos.is_empty() {
            return Err(KinetixError::Unsupported(
                "no AV1, VP9 or Opus track in the stream".into(),
            ));
        }
        self.infos = infos.clone();
        self.announced = Some(map);
        self.events.push(MkvEvent::Tracks(infos));
        for f in std::mem::take(&mut self.waiting) {
            self.emit(f);
        }
        Ok(())
    }
}

enum Built {
    Info(StreamInfo),
    Unsupported,
    NeedFrame,
}

fn build_stream_info(
    index: u32,
    b: &TrackBuilder,
    first_key_frame: Option<&[u8]>,
) -> Result<Built, KinetixError> {
    match (b.kind, b.codec_id.as_str()) {
        (1, "V_AV1") => {
            if b.private.first() != Some(&0x81) {
                return Err(KinetixError::Unsupported(
                    "V_AV1 track without an av1C CodecPrivate".into(),
                ));
            }
            let mut s = StreamInfo::new(index, CodecId::Av1, 90_000);
            s.width = b.width;
            s.height = b.height;
            s.extradata = b.private.clone();
            Ok(Built::Info(s))
        }
        (1, "V_VP9") => {
            let Some(frame) = first_key_frame else {
                return Ok(Built::NeedFrame);
            };
            let cfg = vp9_config_from_frame(frame, b.width, b.height)
                .ok_or_else(|| KinetixError::Parse("unreadable VP9 key frame header".into()))?;
            let mut s = StreamInfo::new(index, CodecId::Vp9, 90_000);
            s.width = if b.width > 0 { b.width } else { cfg.width };
            s.height = if b.height > 0 { b.height } else { cfg.height };
            s.extradata = vpcc_record(&cfg);
            Ok(Built::Info(s))
        }
        (2, "A_OPUS") => {
            let channels = if b.channels > 0 { b.channels } else { 2 };
            let dops = opus_head_to_dops(&b.private, channels as u8)
                .ok_or_else(|| KinetixError::Parse("unreadable OpusHead CodecPrivate".into()))?;
            let mut s = StreamInfo::new(index, CodecId::Opus, 48_000);
            s.channels = u16::from(dops[1]);
            s.sample_rate = 48_000;
            s.bits_per_sample = 16;
            let pre_skip = i64::from(u16::from_be_bytes([dops[2], dops[3]]));
            s.edit_media_time = (pre_skip > 0).then_some(pre_skip);
            s.extradata = dops;
            Ok(Built::Info(s))
        }
        (t, _) if t == 1 || t == 2 => Ok(Built::Unsupported),
        _ => Ok(Built::Unsupported),
    }
}

impl MkvStream {
    /// The announced tracks, once known.
    pub fn tracks(&self) -> &[StreamInfo] {
        &self.infos
    }

    /// Media type of announced stream `i`.
    pub fn media_type(&self, i: usize) -> Option<MediaType> {
        self.infos.get(i).map(|s| s.media_type)
    }
}
