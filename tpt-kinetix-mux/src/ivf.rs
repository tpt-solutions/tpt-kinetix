//! IVF writer: the bare AV1 or VP9 container that `tpt-kinetix-demux` reads back.
//!
//! A 32-byte file header, then one `[u32 size][u64 pts][payload]` record per
//! frame, all little-endian. IVF carries one video track and no audio or
//! codec configuration beyond the fourcc, so this writer takes exactly one AV1
//! or VP9 track.
//!
//! The header's `rate`/`scale` pair fixes the clock: frame timestamps are in
//! `rate` ticks per second, and `scale` is the ticks per frame. The header is
//! therefore written with the first frame, when its duration is known.

use std::io::Write;

use tpt_kinetix_core::{
    codec::{CodecId, MediaType},
    packet::Packet,
    stream::StreamInfo,
    timestamp::Timestamp,
};

use crate::MuxError;

const HEADER_LEN: usize = 32;
/// The frame rate assumed when the first frame carries no duration.
const ASSUMED_FRAME_RATE: u32 = 30;
/// The temporal delimiter OBU (type 2, size field present, empty payload).
const TEMPORAL_DELIMITER: [u8; 2] = [0x12, 0x00];

/// Writes one AV1 or VP9 video track as IVF.
pub struct IvfWriter<W: Write> {
    out: W,
    info: StreamInfo,
    /// Set once the header is out, which happens with the first frame.
    started: bool,
}

impl<W: Write> IvfWriter<W> {
    /// Starts an IVF file for `info`, which must be an AV1 or VP9 video track.
    pub fn new(out: W, info: &StreamInfo) -> Result<Self, MuxError> {
        if info.media_type != MediaType::Video || !matches!(info.codec, CodecId::Av1 | CodecId::Vp9)
        {
            return Err(MuxError::Unsupported(
                "IVF carries one AV1 or VP9 video track".into(),
            ));
        }
        Ok(Self {
            out,
            info: info.clone(),
            started: false,
        })
    }

    /// Appends one frame. `duration` is the frame's length in the track's
    /// timescale; the first frame's value sets the header's frame rate.
    ///
    /// AV1 frames get a temporal delimiter in front when they lack one: IVF
    /// carries whole temporal units, and WebM and MP4 samples do not.
    pub fn write_packet(&mut self, p: &Packet, duration: Option<u32>) -> Result<(), MuxError> {
        if p.stream_index != 0 {
            return Err(MuxError::InvalidConfig(format!(
                "IVF has one track; packet for stream {}",
                p.stream_index
            )));
        }
        let scale = self.info.timescale.max(1);
        if !self.started {
            let ticks_per_frame = duration
                .filter(|d| *d > 0)
                .unwrap_or_else(|| (scale / ASSUMED_FRAME_RATE).max(1));
            self.write_header(scale, ticks_per_frame)?;
            self.started = true;
        }
        let pts = ticks_of(&p.pts, scale)?.max(0) as u64;
        let mut payload = Vec::with_capacity(p.data.len() + TEMPORAL_DELIMITER.len());
        if self.info.codec == CodecId::Av1 && !starts_with_temporal_delimiter(&p.data) {
            payload.extend_from_slice(&TEMPORAL_DELIMITER);
        }
        payload.extend_from_slice(&p.data);
        let size = u32::try_from(payload.len())
            .map_err(|_| MuxError::InvalidConfig("IVF frame larger than 4 GiB".into()))?;
        self.out.write_all(&size.to_le_bytes())?;
        self.out.write_all(&pts.to_le_bytes())?;
        self.out.write_all(&payload)?;
        Ok(())
    }

    /// Flushes the output and returns it. A file with no frames still gets a header.
    pub fn finish(mut self) -> Result<W, MuxError> {
        if !self.started {
            let scale = self.info.timescale.max(1);
            self.write_header(scale, (scale / ASSUMED_FRAME_RATE).max(1))?;
        }
        self.out.flush()?;
        Ok(self.out)
    }

    fn write_header(&mut self, rate: u32, ticks_per_frame: u32) -> Result<(), MuxError> {
        let fourcc: &[u8; 4] = match self.info.codec {
            CodecId::Av1 => b"AV01",
            _ => b"VP90",
        };
        let mut h = Vec::with_capacity(HEADER_LEN);
        h.extend_from_slice(b"DKIF");
        h.extend_from_slice(&0u16.to_le_bytes()); // version
        h.extend_from_slice(&(HEADER_LEN as u16).to_le_bytes());
        h.extend_from_slice(fourcc);
        h.extend_from_slice(&(self.info.width as u16).to_le_bytes());
        h.extend_from_slice(&(self.info.height as u16).to_le_bytes());
        h.extend_from_slice(&rate.to_le_bytes());
        h.extend_from_slice(&ticks_per_frame.to_le_bytes());
        h.extend_from_slice(&0u32.to_le_bytes()); // frame count: unknown, not patched
        h.extend_from_slice(&0u32.to_le_bytes()); // unused
        self.out.write_all(&h)?;
        Ok(())
    }
}

fn starts_with_temporal_delimiter(data: &[u8]) -> bool {
    data.first().is_some_and(|b| (b >> 3) & 0x0f == 2)
}

/// `ts` in ticks of a clock running at `scale` ticks per second, rounded.
fn ticks_of(ts: &Timestamp, scale: u32) -> Result<i64, MuxError> {
    let (num, den) = (i128::from(ts.time_base.0), i128::from(ts.time_base.1));
    if den == 0 {
        return Err(MuxError::InvalidTimestamps(
            "time base with a zero denominator".into(),
        ));
    }
    let n = i128::from(ts.value) * num * i128::from(scale);
    let r = if n >= 0 {
        (n + den / 2) / den
    } else {
        (n - den / 2) / den
    };
    i64::try_from(r).map_err(|_| MuxError::InvalidTimestamps("timestamp out of range".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpt_kinetix_demux::{Demuxer, IvfDemuxer};

    fn info(codec: CodecId) -> StreamInfo {
        let mut s = StreamInfo::new(0, codec, 90_000);
        s.media_type = MediaType::Video;
        s.width = 320;
        s.height = 240;
        s
    }

    fn packet(pts: i64, data: &[u8], key: bool) -> Packet {
        Packet {
            pts: Timestamp {
                value: pts,
                time_base: (1, 90_000),
            },
            dts: Timestamp {
                value: pts,
                time_base: (1, 90_000),
            },
            data: data.to_vec(),
            stream_index: 0,
            is_key_frame: key,
        }
    }

    #[test]
    fn vp9_frames_round_trip_through_the_demuxer() {
        let mut w = IvfWriter::new(Vec::new(), &info(CodecId::Vp9)).unwrap();
        for i in 0..3i64 {
            let data = vec![i as u8; 10 + i as usize];
            w.write_packet(&packet(i * 3000, &data, i == 0), Some(3000))
                .unwrap();
        }
        let bytes = w.finish().unwrap();
        assert_eq!(&bytes[0..4], b"DKIF");
        assert_eq!(&bytes[8..12], b"VP90");

        let mut d = IvfDemuxer::new(bytes).unwrap();
        assert_eq!(d.stream_info().codec, CodecId::Vp9);
        assert_eq!(d.stream_info().timescale, 90_000);
        assert_eq!(d.frame_count(), 3);
        for i in 0..3usize {
            let p = d.read_packet().unwrap().expect("frame");
            assert_eq!(p.data, vec![i as u8; 10 + i]);
        }
        assert!(d.read_packet().unwrap().is_none());
    }

    #[test]
    fn av1_frames_gain_a_temporal_delimiter_only_when_missing() {
        let mut w = IvfWriter::new(Vec::new(), &info(CodecId::Av1)).unwrap();
        // A sequence header OBU (type 1, size 0), with no delimiter in front.
        w.write_packet(&packet(0, &[0x0a, 0x00], true), Some(3000))
            .unwrap();
        // A frame that already starts with a delimiter keeps exactly one.
        w.write_packet(&packet(3000, &[0x12, 0x00, 0x32, 0x00], false), Some(3000))
            .unwrap();
        let bytes = w.finish().unwrap();
        assert_eq!(&bytes[8..12], b"AV01");

        let mut d = IvfDemuxer::new(bytes).unwrap();
        let first = d.read_packet().unwrap().unwrap();
        assert_eq!(first.data, [0x12, 0x00, 0x0a, 0x00]);
        let second = d.read_packet().unwrap().unwrap();
        assert_eq!(second.data, [0x12, 0x00, 0x32, 0x00]);
    }

    #[test]
    fn audio_and_extra_streams_are_refused() {
        let mut audio = info(CodecId::Opus);
        audio.media_type = MediaType::Audio;
        assert!(IvfWriter::new(Vec::new(), &audio).is_err());
        assert!(IvfWriter::new(Vec::new(), &info(CodecId::H264)).is_err());

        let mut w = IvfWriter::new(Vec::new(), &info(CodecId::Vp9)).unwrap();
        let mut p = packet(0, &[1], true);
        p.stream_index = 1;
        assert!(w.write_packet(&p, Some(3000)).is_err());
    }

    #[test]
    fn empty_file_still_has_a_header() {
        let w = IvfWriter::new(Vec::new(), &info(CodecId::Vp9)).unwrap();
        let bytes = w.finish().unwrap();
        assert_eq!(bytes.len(), HEADER_LEN);
        assert_eq!(&bytes[0..4], b"DKIF");
        // The demuxer deliberately refuses a file with no frames.
        assert!(IvfDemuxer::new(bytes).is_err());
    }
}
