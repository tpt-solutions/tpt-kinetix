//! Minimal EBML / Matroska / WebM parser.
//!
//! This is a **basic** reader: it parses the EBML element tree (variable-length
//! integer IDs and sizes), extracts track metadata from the `Tracks` master
//! element, and pulls coded frames out of `SimpleBlock` / `Block` elements
//! inside clusters. It is sufficient to enumerate tracks and iterate packets for
//! common single-file WebM/MKV inputs; it does not implement seeking indices,
//! lacing beyond the common cases, or the full Matroska element set.
//!
//! # Example
//!
//! ```no_run
//! use tpt_kinetix_demux::mkv::MkvDemuxer;
//! use tpt_kinetix_demux::Demuxer;
//!
//! let bytes = std::fs::read("video.webm").unwrap();
//! let mut mkv = MkvDemuxer::new(bytes).unwrap();
//! for track in mkv.tracks() {
//!     println!("track {} codec {}", track.track_number, track.codec_id);
//! }
//! while let Some(pkt) = mkv.read_packet().unwrap() {
//!     println!("packet: {} bytes on stream {}", pkt.data.len(), pkt.stream_index);
//! }
//! ```

use tpt_kinetix_core::{error::KinetixError, packet::Packet, timestamp::Timestamp};

use crate::Demuxer;

// ── EBML element IDs (as full 4-or-fewer-byte IDs incl. length marker) ────────
const ID_EBML: u32 = 0x1A45_DFA3;
const ID_SEGMENT: u32 = 0x1853_8067;
const ID_TRACKS: u32 = 0x1654_AE6B;
const ID_TRACK_ENTRY: u32 = 0xAE;
const ID_TRACK_NUMBER: u32 = 0xD7;
const ID_TRACK_TYPE: u32 = 0x83;
const ID_CODEC_ID: u32 = 0x86;
const ID_CLUSTER: u32 = 0x1F43_B675;
const ID_TIMESTAMP: u32 = 0xE7; // Cluster timestamp
const ID_SIMPLE_BLOCK: u32 = 0xA3;
const ID_BLOCK_GROUP: u32 = 0xA0;
const ID_BLOCK: u32 = 0xA1;

/// Matroska track type values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MkvTrackType {
    /// Video track (type 1).
    Video,
    /// Audio track (type 2).
    Audio,
    /// Any other track type.
    Other(u8),
}

impl MkvTrackType {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => MkvTrackType::Video,
            2 => MkvTrackType::Audio,
            other => MkvTrackType::Other(other),
        }
    }
}

/// Metadata for one Matroska track.
#[derive(Debug, Clone)]
pub struct MkvTrack {
    /// Track number (as used in block headers).
    pub track_number: u64,
    /// Track type.
    pub track_type: MkvTrackType,
    /// Matroska codec id string, e.g. `"V_VP9"`, `"V_MPEG4/ISO/AVC"`, `"A_OPUS"`.
    pub codec_id: String,
}

/// A basic Matroska / WebM demuxer.
#[derive(Debug)]
pub struct MkvDemuxer {
    tracks: Vec<MkvTrack>,
    /// Queued packets extracted from clusters, in file order.
    packets: std::collections::VecDeque<Packet>,
}

impl MkvDemuxer {
    /// Parse the EBML tree from `data` and extract tracks + packets.
    pub fn new(data: Vec<u8>) -> Result<Self, KinetixError> {
        let mut parser = EbmlParser::new(&data);
        parser.parse()?;
        Ok(Self {
            tracks: parser.tracks,
            packets: parser.packets.into(),
        })
    }

    /// The tracks discovered in the file.
    pub fn tracks(&self) -> &[MkvTrack] {
        &self.tracks
    }
}

impl Demuxer for MkvDemuxer {
    fn read_packet(&mut self) -> Result<Option<Packet>, KinetixError> {
        Ok(self.packets.pop_front())
    }

    fn seek(&mut self, _target_pts_ms: i64) -> Result<(), KinetixError> {
        Err(KinetixError::Parse(
            "MKV seeking is not yet supported".into(),
        ))
    }
}

// ── EBML low-level primitives ────────────────────────────────────────────────

/// Read an EBML element ID (keeps the length-marker bits, as is conventional).
///
/// Returns `(id, bytes_consumed)`.
fn read_element_id(data: &[u8]) -> Option<(u32, usize)> {
    let first = *data.first()?;
    let len = first.leading_zeros() as usize + 1;
    if len > 4 || data.len() < len {
        return None;
    }
    let mut id = 0u32;
    for &b in &data[..len] {
        id = (id << 8) | b as u32;
    }
    Some((id, len))
}

/// Read an EBML variable-length size integer (VINT), clearing the length marker.
///
/// Returns `(value, bytes_consumed)`. An all-ones value denotes "unknown size";
/// we surface it as `u64::MAX`.
fn read_vint_size(data: &[u8]) -> Option<(u64, usize)> {
    let first = *data.first()?;
    if first == 0 {
        return None;
    }
    let len = first.leading_zeros() as usize + 1;
    if len > 8 || data.len() < len {
        return None;
    }
    // Clear the marker bit. `len` can be 8 (full-width length prefix `0x01`),
    // and `0xFFu8 >> 8` would panic (shift amount == bit width), so mask in a
    // wider type.
    let mask = (0xFFu64 >> len) as u8;
    let mut value = (first & mask) as u64;
    let mut all_ones = (first as u64 | !(0xFFu64 >> len)) == 0xFF;
    for &b in &data[1..len] {
        value = (value << 8) | b as u64;
        all_ones &= b == 0xFF;
    }
    if all_ones {
        return Some((u64::MAX, len));
    }
    Some((value, len))
}

/// Parse a big-endian unsigned integer from up to 8 bytes.
fn parse_uint(bytes: &[u8]) -> u64 {
    let mut v = 0u64;
    for &b in bytes.iter().take(8) {
        v = (v << 8) | b as u64;
    }
    v
}

// ── EBML tree walker ─────────────────────────────────────────────────────────

struct EbmlParser<'a> {
    data: &'a [u8],
    tracks: Vec<MkvTrack>,
    packets: Vec<Packet>,
}

impl<'a> EbmlParser<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            tracks: Vec::new(),
            packets: Vec::new(),
        }
    }

    fn parse(&mut self) -> Result<(), KinetixError> {
        // Validate the top-level EBML header exists.
        let (id, _) = read_element_id(self.data)
            .ok_or_else(|| KinetixError::Parse("not an EBML file".into()))?;
        if id != ID_EBML {
            return Err(KinetixError::Parse(format!(
                "expected EBML header id, got {id:#x}"
            )));
        }
        self.walk(self.data, 0);
        Ok(())
    }

    /// Recursively walk a region of elements. `cluster_ts` tracks the current
    /// cluster timestamp for packet PTS.
    fn walk(&mut self, region: &[u8], mut cluster_ts: u64) {
        let mut pos = 0usize;
        while pos < region.len() {
            let (id, id_len) = match read_element_id(&region[pos..]) {
                Some(v) => v,
                None => break,
            };
            let after_id = pos + id_len;
            let (size, size_len) = match read_vint_size(&region[after_id..]) {
                Some(v) => v,
                None => break,
            };
            let body_start = after_id + size_len;

            // Resolve "unknown size" by consuming the rest of the region.
            let body_len = if size == u64::MAX {
                region.len() - body_start
            } else {
                (size as usize).min(region.len() - body_start)
            };
            let body = &region[body_start..body_start + body_len];

            match id {
                ID_SEGMENT | ID_TRACKS | ID_TRACK_ENTRY | ID_BLOCK_GROUP => {
                    self.walk(body, cluster_ts);
                }
                ID_CLUSTER => {
                    self.walk(body, 0);
                }
                ID_TIMESTAMP => {
                    cluster_ts = parse_uint(body);
                }
                ID_SIMPLE_BLOCK | ID_BLOCK => {
                    if let Some(pkt) = parse_block(body, cluster_ts, id == ID_SIMPLE_BLOCK) {
                        self.packets.push(pkt);
                    }
                }
                _ => {}
            }

            // Track metadata: handled by scanning TrackEntry children directly.
            if id == ID_TRACKS {
                self.parse_tracks(body);
            }

            pos = body_start + body_len;
        }
    }

    fn parse_tracks(&mut self, tracks_body: &[u8]) {
        let mut pos = 0usize;
        while pos < tracks_body.len() {
            let (id, id_len) = match read_element_id(&tracks_body[pos..]) {
                Some(v) => v,
                None => break,
            };
            let (size, size_len) = match read_vint_size(&tracks_body[pos + id_len..]) {
                Some(v) => v,
                None => break,
            };
            let body_start = pos + id_len + size_len;
            let body_len = (size as usize).min(tracks_body.len() - body_start);
            let body = &tracks_body[body_start..body_start + body_len];
            if id == ID_TRACK_ENTRY {
                if let Some(track) = parse_track_entry(body) {
                    self.tracks.push(track);
                }
            }
            pos = body_start + body_len;
        }
    }
}

/// Parse a single `TrackEntry` element body into an [`MkvTrack`].
fn parse_track_entry(body: &[u8]) -> Option<MkvTrack> {
    let mut number = None;
    let mut ttype = MkvTrackType::Other(0);
    let mut codec = String::new();

    let mut pos = 0usize;
    while pos < body.len() {
        let (id, id_len) = read_element_id(&body[pos..])?;
        let (size, size_len) = read_vint_size(&body[pos + id_len..])?;
        let vs = pos + id_len + size_len;
        let vlen = (size as usize).min(body.len().saturating_sub(vs));
        let val = &body[vs..vs + vlen];
        match id {
            ID_TRACK_NUMBER => number = Some(parse_uint(val)),
            ID_TRACK_TYPE => {
                if let Some(&b) = val.first() {
                    ttype = MkvTrackType::from_u8(b);
                }
            }
            ID_CODEC_ID => {
                codec = String::from_utf8_lossy(val)
                    .trim_end_matches('\0')
                    .to_string()
            }
            _ => {}
        }
        pos = vs + vlen;
    }

    Some(MkvTrack {
        track_number: number?,
        track_type: ttype,
        codec_id: codec,
    })
}

/// Parse a `SimpleBlock` / `Block` body into a [`Packet`].
///
/// Block layout: track-number VINT, 2-byte signed timecode (relative to cluster),
/// 1 flags byte, then frame data (assumes no lacing / single frame).
fn parse_block(body: &[u8], cluster_ts: u64, simple: bool) -> Option<Packet> {
    let (track_number, tn_len) = read_vint_size(body)?;
    let rest = &body[tn_len..];
    if rest.len() < 3 {
        return None;
    }
    let rel_ts = i16::from_be_bytes([rest[0], rest[1]]);
    let flags = rest[2];
    let data = rest[3..].to_vec();

    // For SimpleBlock the keyframe flag is the top bit of the flags byte.
    let is_key = if simple { flags & 0x80 != 0 } else { false };

    let pts_ticks = (cluster_ts as i64).saturating_add(rel_ts as i64);
    // Matroska timestamps are in TimestampScale units (default 1ms); expose as ms.
    let pts = Timestamp::new(pts_ticks, (1, 1000));

    Some(Packet {
        pts,
        dts: pts,
        data,
        stream_index: track_number as u32,
        is_key_frame: is_key,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vint_size_single_byte() {
        // 0x81 => length 1, value 1.
        assert_eq!(read_vint_size(&[0x81]), Some((1, 1)));
        // 0xA3 => length 1, value 0x23.
        assert_eq!(read_vint_size(&[0xA3]), Some((0x23, 1)));
    }

    #[test]
    fn vint_size_eight_byte_does_not_panic() {
        // 0x01 => length 8 (full-width marker), value from the remaining 7 bytes.
        assert_eq!(read_vint_size(&[0x01, 0, 0, 0, 0, 0, 0, 1]), Some((1, 8)));
    }

    #[test]
    fn fuzz_regression_crash_d06ac77249c00a8361756de4544f20343593e189() {
        let data = vec![
            0x1a, 0x45, 0xdf, 0xa3, 0x85, 0xff, 0x6f, 0x6f, 0x6f, 0xb2, 0xa3, 0x01, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x01, 0x85, 0xff, 0xb2, 0xa3, 0x86, 0xb2, 0xa3, 0x45, 0xdf,
            0xa3, 0x35,
        ];
        // Must not panic; error is acceptable.
        let _ = MkvDemuxer::new(data);
    }

    #[test]
    fn fuzz_regression_crash_9cc84c6dcdd0ddd1601891a18e882db3143ac72d() {
        // Cluster Timestamp near u64::MAX combined with a block's relative
        // timecode used to overflow `cluster_ts as i64 + rel_ts as i64`.
        let data = vec![
            26, 69, 223, 163, 129, 68, 163, 137, 129, 26, 69, 223, 163, 129, 129, 22, 84, 174,
            107, 1, 231, 163, 127, 255, 255, 255, 255, 255, 255, 255, 255, 161, 161, 161, 161,
            161, 161, 161, 161, 161, 129, 129, 22, 84, 174, 107, 1, 231, 128, 128, 128, 22, 84,
            174, 107, 137, 129, 26, 69, 223, 163, 129, 129, 22, 84, 174, 107, 1, 231, 128, 163,
            137, 129, 26, 69, 223, 163, 129, 129, 22, 84, 231, 128, 128, 128, 128, 128, 128, 128,
            128, 174, 174, 215, 174, 174, 174, 174, 174, 174, 255, 0, 0, 0, 0, 0, 0, 255, 255, 26,
            69, 231, 128, 163, 137, 129, 26, 69, 223, 163, 129, 129, 22, 84, 231, 128, 128, 128,
            128, 128, 128, 128, 128, 174, 174, 215, 174, 174, 174, 174, 174, 174, 255, 0, 0, 0, 0,
            0, 0, 255, 255, 26, 69, 223, 163, 127, 255, 255, 255, 255, 235, 17, 234, 26, 161, 161,
            161, 161, 161, 161, 161, 161, 161, 129, 129, 22, 84, 174, 107, 1, 231, 128, 128, 128,
            22, 84, 174, 107, 137, 129, 26, 69, 223, 163, 129, 129, 22, 84, 174, 107, 1, 231, 66,
            128, 128, 128, 129, 128, 22, 129, 129, 0, 0, 22, 129, 129, 0, 0,
        ];
        // Must not panic; error is acceptable.
        let _ = MkvDemuxer::new(data);
    }

    #[test]
    fn vint_size_two_byte() {
        // 0x40 0x02 => length 2, value 2.
        assert_eq!(read_vint_size(&[0x40, 0x02]), Some((2, 2)));
    }

    #[test]
    fn element_id_multibyte() {
        // EBML header id 0x1A45DFA3 (4 bytes).
        let bytes = [0x1A, 0x45, 0xDF, 0xA3];
        assert_eq!(read_element_id(&bytes), Some((ID_EBML, 4)));
    }

    #[test]
    fn rejects_non_ebml() {
        let err = MkvDemuxer::new(vec![0x00, 0x01, 0x02, 0x03]).unwrap_err();
        assert!(matches!(err, KinetixError::Parse(_)));
    }

    #[test]
    fn parses_minimal_tracks_and_block() {
        // Build a tiny EBML doc:
        // EBML header (empty) + Segment { Tracks { TrackEntry {..} } Cluster { Timestamp SimpleBlock } }
        let mut doc = Vec::new();

        // EBML header: id + size 0.
        doc.extend_from_slice(&[0x1A, 0x45, 0xDF, 0xA3, 0x80]);

        // --- TrackEntry body ---
        let mut track_entry = Vec::new();
        track_entry.extend_from_slice(&[ID_TRACK_NUMBER as u8, 0x81, 0x01]); // TrackNumber = 1
        track_entry.extend_from_slice(&[ID_TRACK_TYPE as u8, 0x81, 0x01]); // TrackType = video
        let codec = b"V_VP9";
        track_entry.push(ID_CODEC_ID as u8);
        track_entry.push(0x80 | codec.len() as u8);
        track_entry.extend_from_slice(codec);

        // TrackEntry element
        let mut tracks_body = Vec::new();
        tracks_body.push(ID_TRACK_ENTRY as u8);
        tracks_body.push(0x80 | track_entry.len() as u8);
        tracks_body.extend_from_slice(&track_entry);

        // Tracks element (id 0x1654AE6B)
        let mut segment_body = Vec::new();
        segment_body.extend_from_slice(&[0x16, 0x54, 0xAE, 0x6B]);
        segment_body.push(0x80 | tracks_body.len() as u8);
        segment_body.extend_from_slice(&tracks_body);

        // --- Cluster ---
        let mut cluster_body = Vec::new();
        // Timestamp = 100
        cluster_body.extend_from_slice(&[ID_TIMESTAMP as u8, 0x81, 100]);
        // SimpleBlock: track 1, rel ts 0, keyframe flag, 3 bytes data.
        let mut block = Vec::new();
        block.push(0x81); // track number vint = 1
        block.extend_from_slice(&0i16.to_be_bytes()); // rel timecode
        block.push(0x80); // flags: keyframe
        block.extend_from_slice(&[0xDE, 0xAD, 0xBE]);
        cluster_body.push(ID_SIMPLE_BLOCK as u8);
        cluster_body.push(0x80 | block.len() as u8);
        cluster_body.extend_from_slice(&block);

        // Cluster element (id 0x1F43B675)
        segment_body.extend_from_slice(&[0x1F, 0x43, 0xB6, 0x75]);
        segment_body.push(0x80 | cluster_body.len() as u8);
        segment_body.extend_from_slice(&cluster_body);

        // Segment element (id 0x18538067)
        doc.extend_from_slice(&[0x18, 0x53, 0x80, 0x67]);
        doc.push(0x80 | segment_body.len() as u8);
        doc.extend_from_slice(&segment_body);

        let mut mkv = MkvDemuxer::new(doc).expect("parse mkv");
        assert_eq!(mkv.tracks().len(), 1);
        assert_eq!(mkv.tracks()[0].track_number, 1);
        assert_eq!(mkv.tracks()[0].track_type, MkvTrackType::Video);
        assert_eq!(mkv.tracks()[0].codec_id, "V_VP9");

        let pkt = mkv.read_packet().unwrap().expect("one packet");
        assert_eq!(pkt.stream_index, 1);
        assert!(pkt.is_key_frame);
        assert_eq!(pkt.data, vec![0xDE, 0xAD, 0xBE]);
        assert_eq!(pkt.pts.value, 100);
        assert!(mkv.read_packet().unwrap().is_none());
    }
}
