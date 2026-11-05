//! Ogg writer for one Opus track (RFC 3533 pages, RFC 7845 Opus mapping).
//!
//! Opus packets are copied, never decoded. The first page carries `OpusHead`
//! built from the track's `dOps` configuration, the second `OpusTags`, and then
//! the audio. Each page's granule position is the sample count at its last
//! packet, including the pre-skip, so a packet whose duration was trimmed (WebM
//! `DiscardPadding`) ends the stream early, as Ogg intends.

use std::io::Write;

use tpt_kinetix_core::{
    codec::{CodecId, MediaType},
    stream::StreamInfo,
};

use crate::MuxError;

/// Packets per page at most, and bytes per page at most (before the last packet).
const PAGE_PACKETS: usize = 32;
const PAGE_BYTES: usize = 4096;
/// The lacing table holds at most 255 segments, and a packet of 255 segments
/// fills 65,025 bytes; longer packets would need splitting across pages.
const MAX_PACKET: usize = 65_025;
const MAX_SEGMENTS: usize = 255;

const HEADER_BOS: u8 = 0x02;
const HEADER_EOS: u8 = 0x04;

/// Writes one Opus track as an Ogg stream.
pub struct OggWriter<W: Write> {
    out: W,
    serial: u32,
    sequence: u32,
    /// Running granule position: pre-skip plus the duration of every packet so far.
    granule: u64,
    /// Packets waiting for their page, with the granule at their end.
    pending: Vec<(Vec<u8>, u64)>,
    pending_bytes: usize,
    pending_segments: usize,
}

impl<W: Write> OggWriter<W> {
    /// Starts an Ogg stream for `info`, which must be an Opus audio track whose
    /// `extradata` is its `dOps` configuration. `serial` identifies the stream.
    pub fn new(out: W, info: &StreamInfo, serial: u32) -> Result<Self, MuxError> {
        if info.media_type != MediaType::Audio || info.codec != CodecId::Opus {
            return Err(MuxError::Unsupported(
                "Ogg output takes one Opus track".into(),
            ));
        }
        let head = opus_head(&info.extradata)?;
        let pre_skip = u64::from(u16::from_le_bytes([head[10], head[11]]));
        let mut w = Self {
            out,
            serial,
            sequence: 0,
            granule: pre_skip,
            pending: Vec::new(),
            pending_bytes: 0,
            pending_segments: 0,
        };
        w.emit_page(HEADER_BOS, 0, &[&head])?;
        w.emit_page(0, 0, &[&opus_tags()])?;
        Ok(w)
    }

    /// Appends one Opus packet lasting `duration` samples at 48 kHz.
    pub fn write_packet(&mut self, data: &[u8], duration: u32) -> Result<(), MuxError> {
        if data.len() > MAX_PACKET {
            return Err(MuxError::InvalidConfig(format!(
                "Opus packet of {} bytes is too large for an Ogg page",
                data.len()
            )));
        }
        let segments = data.len() / 255 + 1;
        if self.pending_segments + segments > MAX_SEGMENTS
            || self.pending.len() >= PAGE_PACKETS
            || self.pending_bytes >= PAGE_BYTES
        {
            self.flush(false)?;
        }
        self.granule += u64::from(duration);
        self.pending_bytes += data.len();
        self.pending_segments += segments;
        self.pending.push((data.to_vec(), self.granule));
        Ok(())
    }

    /// Writes the last page (marked end-of-stream), flushes, and returns the output.
    pub fn finish(mut self) -> Result<W, MuxError> {
        self.flush(true)?;
        self.out.flush()?;
        Ok(self.out)
    }

    fn flush(&mut self, end: bool) -> Result<(), MuxError> {
        if self.pending.is_empty() {
            if end {
                // An empty end-of-stream page keeps the stream well formed.
                let (seq, g) = (self.sequence, self.granule);
                self.sequence += 1;
                return self.write_raw(HEADER_EOS, g, seq, &[]);
            }
            return Ok(());
        }
        let granule = self.pending.last().map_or(0, |(_, g)| *g);
        let packets: Vec<Vec<u8>> = self.pending.drain(..).map(|(d, _)| d).collect();
        let refs: Vec<&[u8]> = packets.iter().map(Vec::as_slice).collect();
        self.pending_bytes = 0;
        self.pending_segments = 0;
        let flags = if end { HEADER_EOS } else { 0 };
        let seq = self.sequence;
        self.sequence += 1;
        self.write_raw(flags, granule, seq, &refs)
    }

    /// Writes one page built from `packets`, numbering it with the next sequence number.
    fn emit_page(&mut self, flags: u8, granule: u64, packets: &[&[u8]]) -> Result<(), MuxError> {
        let seq = self.sequence;
        self.sequence += 1;
        self.write_raw(flags, granule, seq, packets)
    }

    fn write_raw(
        &mut self,
        flags: u8,
        granule: u64,
        seq: u32,
        packets: &[&[u8]],
    ) -> Result<(), MuxError> {
        let page = build_page(flags, granule, self.serial, seq, packets);
        self.out.write_all(&page)?;
        Ok(())
    }
}

/// One Ogg page with its lacing table and checksum.
fn build_page(flags: u8, granule: u64, serial: u32, seq: u32, packets: &[&[u8]]) -> Vec<u8> {
    let mut lacing = Vec::new();
    for p in packets {
        let mut rest = p.len();
        while rest >= 255 {
            lacing.push(255u8);
            rest -= 255;
        }
        lacing.push(rest as u8);
    }
    let mut page =
        Vec::with_capacity(27 + lacing.len() + packets.iter().map(|p| p.len()).sum::<usize>());
    page.extend_from_slice(b"OggS");
    page.push(0); // stream structure version
    page.push(flags);
    page.extend_from_slice(&granule.to_le_bytes());
    page.extend_from_slice(&serial.to_le_bytes());
    page.extend_from_slice(&seq.to_le_bytes());
    page.extend_from_slice(&[0; 4]); // checksum, filled in below
    page.push(lacing.len() as u8);
    page.extend_from_slice(&lacing);
    for p in packets {
        page.extend_from_slice(p);
    }
    let crc = ogg_crc(&page);
    page[22..26].copy_from_slice(&crc.to_le_bytes());
    page
}

/// The Ogg CRC: polynomial 0x04C11DB7, not reflected, zero initial value and no
/// final XOR, computed over the whole page with the checksum field zeroed.
fn ogg_crc(bytes: &[u8]) -> u32 {
    let mut crc = 0u32;
    for &b in bytes {
        crc ^= u32::from(b) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04C1_1DB7
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// `OpusHead` (RFC 7845 section 5.1) from an MP4 `dOps` payload (big-endian fields).
fn opus_head(dops: &[u8]) -> Result<Vec<u8>, MuxError> {
    if dops.len() < 11 || dops[0] != 0 {
        return Err(MuxError::InvalidConfig(
            "unreadable Opus dOps configuration".into(),
        ));
    }
    let channels = dops[1];
    let pre_skip = u16::from_be_bytes([dops[2], dops[3]]);
    let rate = u32::from_be_bytes([dops[4], dops[5], dops[6], dops[7]]);
    let gain = i16::from_be_bytes([dops[8], dops[9]]);
    let mut head = Vec::with_capacity(19 + dops.len());
    head.extend_from_slice(b"OpusHead");
    head.push(1); // version
    head.push(channels);
    head.extend_from_slice(&pre_skip.to_le_bytes());
    head.extend_from_slice(&rate.to_le_bytes());
    head.extend_from_slice(&gain.to_le_bytes());
    // Channel mapping family and, when it is not 0, the stream layout that follows.
    head.extend_from_slice(&dops[10..]);
    Ok(head)
}

/// `OpusTags` (RFC 7845 section 5.2) with no user comments.
fn opus_tags() -> Vec<u8> {
    const VENDOR: &[u8] = b"tpt-kinetix";
    let mut tags = Vec::with_capacity(16 + VENDOR.len());
    tags.extend_from_slice(b"OpusTags");
    tags.extend_from_slice(&(VENDOR.len() as u32).to_le_bytes());
    tags.extend_from_slice(VENDOR);
    tags.extend_from_slice(&0u32.to_le_bytes());
    tags
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opus_info() -> StreamInfo {
        let mut s = StreamInfo::new(0, CodecId::Opus, 48_000);
        s.media_type = MediaType::Audio;
        s.channels = 2;
        // dOps: version 0, 2 channels, pre-skip 312, 48 kHz, gain 0, family 0.
        s.extradata = vec![0, 2, 0x01, 0x38, 0, 0, 0xBB, 0x80, 0, 0, 0];
        s
    }

    /// Splits an Ogg byte stream into `(flags, granule, packets)` per page.
    fn pages(mut bytes: &[u8]) -> Vec<(u8, u64, Vec<Vec<u8>>)> {
        let mut out = Vec::new();
        while !bytes.is_empty() {
            assert_eq!(&bytes[..4], b"OggS");
            let flags = bytes[5];
            let granule = u64::from_le_bytes(bytes[6..14].try_into().unwrap());
            let segs = bytes[26] as usize;
            let table = &bytes[27..27 + segs];
            let mut at = 27 + segs;
            let mut packets = Vec::new();
            let mut cur = Vec::new();
            for &lace in table {
                cur.extend_from_slice(&bytes[at..at + lace as usize]);
                at += lace as usize;
                if lace < 255 {
                    packets.push(std::mem::take(&mut cur));
                }
            }
            out.push((flags, granule, packets));
            bytes = &bytes[at..];
        }
        out
    }

    #[test]
    fn opus_head_is_little_endian_and_the_header_pages_come_first() {
        let mut w = OggWriter::new(Vec::new(), &opus_info(), 7).unwrap();
        w.write_packet(&[0xF8, 0xFF, 0xFE], 960).unwrap();
        let bytes = w.finish().unwrap();

        let p = pages(&bytes);
        assert_eq!(p[0].0, HEADER_BOS);
        let head = &p[0].2[0];
        assert_eq!(&head[..8], b"OpusHead");
        assert_eq!(head[8], 1);
        assert_eq!(head[9], 2);
        assert_eq!(u16::from_le_bytes([head[10], head[11]]), 312);
        assert_eq!(u32::from_le_bytes(head[12..16].try_into().unwrap()), 48_000);
        assert_eq!(&p[1].2[0][..8], b"OpusTags");
    }

    #[test]
    fn granules_count_pre_skip_and_the_last_page_ends_the_stream() {
        let mut w = OggWriter::new(Vec::new(), &opus_info(), 7).unwrap();
        for _ in 0..40 {
            w.write_packet(&[1, 2, 3], 960).unwrap();
        }
        let bytes = w.finish().unwrap();
        let audio: Vec<_> = pages(&bytes).into_iter().skip(2).collect();
        // 40 packets at 32 per page: two pages of audio.
        assert_eq!(audio.len(), 2);
        assert_eq!(audio[0].2.len(), 32);
        assert_eq!(audio[0].1, 312 + 32 * 960);
        assert_eq!(audio[1].0 & HEADER_EOS, HEADER_EOS);
        assert_eq!(audio[1].1, 312 + 40 * 960);
    }

    #[test]
    fn a_trimmed_last_packet_moves_the_final_granule() {
        let mut w = OggWriter::new(Vec::new(), &opus_info(), 7).unwrap();
        w.write_packet(&[1], 960).unwrap();
        w.write_packet(&[2], 312).unwrap();
        let bytes = w.finish().unwrap();
        let audio = pages(&bytes).into_iter().nth(2).unwrap();
        assert_eq!(audio.1, 312 + 960 + 312);
    }

    #[test]
    fn packets_longer_than_255_bytes_lace_across_segments() {
        let big = vec![9u8; 600]; // 255 + 255 + 90 bytes
        let mut w = OggWriter::new(Vec::new(), &opus_info(), 7).unwrap();
        w.write_packet(&big, 960).unwrap();
        let bytes = w.finish().unwrap();
        let audio = pages(&bytes).into_iter().nth(2).unwrap();
        assert_eq!(audio.2, vec![big]);
    }

    #[test]
    fn the_checksum_verifies() {
        let page = build_page(0, 5, 1, 0, &[b"hello"]);
        let mut zeroed = page.clone();
        zeroed[22..26].copy_from_slice(&[0; 4]);
        assert_eq!(
            ogg_crc(&zeroed).to_le_bytes(),
            page[22..26],
            "the stored checksum is the CRC of the page with that field zeroed"
        );
    }

    #[test]
    fn video_and_oversized_packets_are_refused() {
        let mut video = opus_info();
        video.media_type = MediaType::Video;
        assert!(OggWriter::new(Vec::new(), &video, 1).is_err());
        let mut w = OggWriter::new(Vec::new(), &opus_info(), 1).unwrap();
        assert!(w.write_packet(&vec![0u8; MAX_PACKET + 1], 960).is_err());
    }
}
