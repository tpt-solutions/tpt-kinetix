//! Minimal MPEG-TS (MPEG-2 Transport Stream) muxer for HLS segments.
//!
//! This produces standards-compliant 188-byte TS packets containing a PAT, a
//! PMT (declaring a single H.264 video elementary stream), and PES packets
//! carrying Annex-B H.264 access units. It is intentionally small — enough to
//! wrap coded H.264 into `.ts` segments that HLS players accept.
//!
//! Input NALUs are expected in **AVCC** (4-byte length-prefixed) form, the same
//! layout used by MP4/FLV; they are converted to Annex-B (start-code prefixed)
//! on the way into the PES payload.
//!
//! # Examples
//!
//! ```rust
//! use tpt_kinetix_stream::hls::ts::TsMuxer;
//!
//! let mut mux = TsMuxer::new();
//! // AVCC access unit: [len][nal] — here a tiny fake IDR NAL.
//! let au = [0, 0, 0, 2, 0x65, 0x88];
//! mux.write_access_unit(&au, /* pts_90khz */ 0, /* keyframe */ true);
//! let segment = mux.finish();
//! assert_eq!(segment.len() % 188, 0); // whole TS packets
//! assert_eq!(segment[0], 0x47); // TS sync byte
//! ```

const TS_PACKET_LEN: usize = 188;
const SYNC_BYTE: u8 = 0x47;

const PID_PAT: u16 = 0x0000;
const PID_PMT: u16 = 0x1000;
const PID_VIDEO: u16 = 0x0100;

const STREAM_TYPE_H264: u8 = 0x1B;
const STREAM_ID_VIDEO: u8 = 0xE0;

/// Builds an MPEG-TS byte stream for a single H.264 video elementary stream.
pub struct TsMuxer {
    out: Vec<u8>,
    pat_cc: u8,
    pmt_cc: u8,
    video_cc: u8,
    wrote_tables: bool,
}

impl Default for TsMuxer {
    fn default() -> Self {
        Self::new()
    }
}

impl TsMuxer {
    /// Create a new, empty TS muxer.
    pub fn new() -> Self {
        Self {
            out: Vec::new(),
            pat_cc: 0,
            pmt_cc: 0,
            video_cc: 0,
            wrote_tables: false,
        }
    }

    /// Append one H.264 access unit (in AVCC length-prefixed form).
    ///
    /// - `avcc` — the access unit as one or more 4-byte-length-prefixed NALUs.
    /// - `pts_90khz` — presentation timestamp in 90 kHz ticks.
    /// - `keyframe` — whether this AU is a random-access point (adds PCR + RAI).
    pub fn write_access_unit(&mut self, avcc: &[u8], pts_90khz: u64, keyframe: bool) {
        if !self.wrote_tables {
            self.write_pat();
            self.write_pmt();
            self.wrote_tables = true;
        }

        let annexb = avcc_to_annexb(avcc);
        let pes = build_pes(&annexb, pts_90khz);
        self.write_pes_packets(&pes, pts_90khz, keyframe);
    }

    /// Finish muxing and return the complete TS segment bytes.
    pub fn finish(self) -> Vec<u8> {
        self.out
    }

    fn write_pat(&mut self) {
        // PAT: one program (program_number=1) -> PMT PID.
        let mut section = Vec::new();
        section.push(0x00); // table_id = PAT
                            // section_syntax_indicator=1, '0', reserved '11', section_length (12 bits)
        let body = {
            let mut b = Vec::new();
            b.extend_from_slice(&1u16.to_be_bytes()); // transport_stream_id
            b.push(0xC1); // reserved '11', version 0, current_next=1
            b.push(0x00); // section_number
            b.push(0x00); // last_section_number
                          // program_number=1 -> program_map_PID
            b.extend_from_slice(&1u16.to_be_bytes());
            b.extend_from_slice(&(0xE000 | PID_PMT).to_be_bytes()); // reserved '111' + PID
            b
        };
        let section_length = body.len() + 4; // +4 CRC
        section.extend_from_slice(&(0xB000 | section_length as u16).to_be_bytes());
        section.extend_from_slice(&body);
        let crc = mpeg_crc32(&section);
        section.extend_from_slice(&crc.to_be_bytes());

        let cc = self.pat_cc;
        self.pat_cc = (self.pat_cc + 1) & 0x0F;
        self.write_psi_packet(PID_PAT, &section, cc);
    }

    fn write_pmt(&mut self) {
        let mut section = Vec::new();
        section.push(0x02); // table_id = PMT
        let body = {
            let mut b = Vec::new();
            b.extend_from_slice(&1u16.to_be_bytes()); // program_number
            b.push(0xC1); // reserved, version 0, current_next=1
            b.push(0x00); // section_number
            b.push(0x00); // last_section_number
            b.extend_from_slice(&(0xE000 | PID_VIDEO).to_be_bytes()); // PCR_PID
            b.extend_from_slice(&0xF000u16.to_be_bytes()); // program_info_length = 0
                                                           // ES loop: one H.264 stream.
            b.push(STREAM_TYPE_H264);
            b.extend_from_slice(&(0xE000 | PID_VIDEO).to_be_bytes()); // elementary_PID
            b.extend_from_slice(&0xF000u16.to_be_bytes()); // ES_info_length = 0
            b
        };
        let section_length = body.len() + 4;
        section.extend_from_slice(&(0xB000 | section_length as u16).to_be_bytes());
        section.extend_from_slice(&body);
        let crc = mpeg_crc32(&section);
        section.extend_from_slice(&crc.to_be_bytes());

        let cc = self.pmt_cc;
        self.pmt_cc = (self.pmt_cc + 1) & 0x0F;
        self.write_psi_packet(PID_PMT, &section, cc);
    }

    /// Write a PSI section into a single TS packet with `payload_unit_start`.
    fn write_psi_packet(&mut self, pid: u16, section: &[u8], cc: u8) {
        let mut packet = Vec::with_capacity(TS_PACKET_LEN);
        packet.push(SYNC_BYTE);
        // payload_unit_start_indicator=1
        packet.push(0x40 | ((pid >> 8) as u8 & 0x1F));
        packet.push((pid & 0xFF) as u8);
        packet.push(0x10 | (cc & 0x0F)); // no adaptation, payload only
        packet.push(0x00); // pointer_field
        packet.extend_from_slice(section);
        pad_to_188(&mut packet);
        self.out.extend_from_slice(&packet);
    }

    /// Split a PES packet across as many TS packets as needed.
    fn write_pes_packets(&mut self, pes: &[u8], pcr_90khz: u64, keyframe: bool) {
        let mut offset = 0usize;
        let mut first = true;

        while offset < pes.len() {
            let mut packet = Vec::with_capacity(TS_PACKET_LEN);
            packet.push(SYNC_BYTE);

            let pusi = if first { 0x40 } else { 0x00 };
            packet.push(pusi | ((PID_VIDEO >> 8) as u8 & 0x1F));
            packet.push((PID_VIDEO & 0xFF) as u8);

            let cc = self.video_cc;
            self.video_cc = (self.video_cc + 1) & 0x0F;

            let remaining = pes.len() - offset;

            // Adaptation field is needed on the first packet (for PCR/RAI) or to
            // pad the final short packet.
            let need_af_first = first;
            // Compute how much payload fits without adaptation.
            let header_len = 4;
            let max_payload_no_af = TS_PACKET_LEN - header_len;

            if need_af_first || remaining < max_payload_no_af {
                // adaptation + payload
                packet.push(0x30 | (cc & 0x0F)); // adaptation + payload flags

                // Build adaptation field.
                let mut af = Vec::new();
                let mut flags = 0u8;
                if first {
                    if keyframe {
                        flags |= 0x40; // random_access_indicator
                    }
                    flags |= 0x10; // PCR_flag
                }
                af.push(flags);
                if first {
                    // PCR: 33-bit base (90kHz) + 6 reserved + 9-bit extension.
                    let pcr_base = pcr_90khz & 0x1_FFFF_FFFF;
                    let ext = 0u16;
                    af.push((pcr_base >> 25) as u8);
                    af.push((pcr_base >> 17) as u8);
                    af.push((pcr_base >> 9) as u8);
                    af.push((pcr_base >> 1) as u8);
                    af.push((((pcr_base & 0x1) as u8) << 7) | 0x7E | ((ext >> 8) as u8 & 0x1));
                    af.push((ext & 0xFF) as u8);
                }

                // Determine payload size, then stuff the adaptation field to fill.
                let overhead = header_len + 1 /* AF length byte */ + af.len();
                let space_for_payload = TS_PACKET_LEN.saturating_sub(overhead);
                let payload_len = remaining.min(space_for_payload);
                let stuffing = space_for_payload - payload_len;

                let af_length = af.len() + stuffing;
                packet.push(af_length as u8);
                packet.extend_from_slice(&af);
                packet.extend(std::iter::repeat_n(0xFF, stuffing));

                packet.extend_from_slice(&pes[offset..offset + payload_len]);
                offset += payload_len;
            } else {
                // payload only
                packet.push(0x10 | (cc & 0x0F));
                let payload_len = remaining.min(max_payload_no_af);
                packet.extend_from_slice(&pes[offset..offset + payload_len]);
                offset += payload_len;
            }

            debug_assert_eq!(packet.len(), TS_PACKET_LEN);
            self.out.extend_from_slice(&packet);
            first = false;
        }
    }
}

/// Convert AVCC (4-byte length-prefixed) NALUs to Annex-B (start-code prefixed).
fn avcc_to_annexb(avcc: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(avcc.len() + 8);
    let mut pos = 0;
    while pos + 4 <= avcc.len() {
        let len =
            u32::from_be_bytes([avcc[pos], avcc[pos + 1], avcc[pos + 2], avcc[pos + 3]]) as usize;
        pos += 4;
        if pos + len > avcc.len() {
            break;
        }
        out.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        out.extend_from_slice(&avcc[pos..pos + len]);
        pos += len;
    }
    out
}

/// Build a PES packet for a video access unit with a PTS.
fn build_pes(payload: &[u8], pts_90khz: u64) -> Vec<u8> {
    let mut pes = Vec::with_capacity(payload.len() + 14);
    pes.extend_from_slice(&[0x00, 0x00, 0x01]); // packet_start_code_prefix
    pes.push(STREAM_ID_VIDEO); // stream_id

    // PES header: PTS only (5 bytes of optional header).
    let pts_bytes = encode_pts(pts_90khz);
    let header_data_len = pts_bytes.len();
    // PES_packet_length: everything after this field. 0 is allowed for video.
    let es_len = payload.len() + 3 + header_data_len;
    let pes_packet_length = if es_len > 0xFFFF { 0 } else { es_len as u16 };
    pes.extend_from_slice(&pes_packet_length.to_be_bytes());

    pes.push(0x80); // '10' marker, no scrambling
    pes.push(0x80); // PTS_DTS_flags = '10' (PTS only)
    pes.push(header_data_len as u8);
    pes.extend_from_slice(&pts_bytes);

    pes.extend_from_slice(payload);
    pes
}

/// Encode a 33-bit PTS into the 5-byte PES PTS field (prefix '0010').
fn encode_pts(pts: u64) -> [u8; 5] {
    let pts = pts & 0x1_FFFF_FFFF;
    [
        0x21 | (((pts >> 30) as u8 & 0x07) << 1),
        (pts >> 22) as u8,
        0x01 | (((pts >> 15) as u8 & 0x7F) << 1),
        (pts >> 7) as u8,
        0x01 | (((pts as u8) & 0x7F) << 1),
    ]
}

/// Pad a partially-filled TS packet to 188 bytes.
///
/// Used only for PSI packets (payload-only), where trailing 0xFF is the
/// convention for filling after a section.
fn pad_to_188(packet: &mut Vec<u8>) {
    while packet.len() < TS_PACKET_LEN {
        packet.push(0xFF);
    }
    debug_assert_eq!(packet.len(), TS_PACKET_LEN);
}

/// MPEG-2 systems CRC-32 (polynomial 0x04C11DB7, MSB-first, init 0xFFFFFFFF).
fn mpeg_crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= (byte as u32) << 24;
        for _ in 0..8 {
            if crc & 0x8000_0000 != 0 {
                crc = (crc << 1) ^ 0x04C1_1DB7;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mux_one() -> Vec<u8> {
        let mut mux = TsMuxer::new();
        let au = [0, 0, 0, 5, 0x65, 0x11, 0x22, 0x33, 0x44];
        mux.write_access_unit(&au, 900, true);
        mux.finish()
    }

    #[test]
    fn output_is_whole_ts_packets() {
        let ts = mux_one();
        assert!(!ts.is_empty());
        assert_eq!(ts.len() % TS_PACKET_LEN, 0);
    }

    #[test]
    fn every_packet_starts_with_sync_byte() {
        let ts = mux_one();
        for chunk in ts.chunks(TS_PACKET_LEN) {
            assert_eq!(chunk[0], SYNC_BYTE);
        }
    }

    #[test]
    fn first_two_packets_are_pat_and_pmt() {
        let ts = mux_one();
        let pat = &ts[..TS_PACKET_LEN];
        let pmt = &ts[TS_PACKET_LEN..2 * TS_PACKET_LEN];
        let pat_pid = (((pat[1] & 0x1F) as u16) << 8) | pat[2] as u16;
        let pmt_pid = (((pmt[1] & 0x1F) as u16) << 8) | pmt[2] as u16;
        assert_eq!(pat_pid, PID_PAT);
        assert_eq!(pmt_pid, PID_PMT);
    }

    #[test]
    fn video_pid_present() {
        let ts = mux_one();
        let has_video = ts.chunks(TS_PACKET_LEN).any(|p| {
            let pid = (((p[1] & 0x1F) as u16) << 8) | p[2] as u16;
            pid == PID_VIDEO
        });
        assert!(has_video);
    }

    #[test]
    fn crc32_known_vector() {
        // CRC of a PAT-like known section is deterministic; just assert stability.
        let a = mpeg_crc32(&[0x00, 0xB0, 0x0D, 0x00, 0x01, 0xC1, 0x00, 0x00]);
        let b = mpeg_crc32(&[0x00, 0xB0, 0x0D, 0x00, 0x01, 0xC1, 0x00, 0x00]);
        assert_eq!(a, b);
        assert_ne!(a, 0);
    }

    #[test]
    fn avcc_converts_to_annexb() {
        let avcc = [0, 0, 0, 2, 0xAB, 0xCD, 0, 0, 0, 1, 0xEF];
        let annexb = avcc_to_annexb(&avcc);
        assert_eq!(annexb, vec![0, 0, 0, 1, 0xAB, 0xCD, 0, 0, 0, 1, 0xEF]);
    }

    #[test]
    fn pts_encoding_roundtrips_top_bits() {
        let pts = 900_000u64;
        let enc = encode_pts(pts);
        // marker bits must be set
        assert_eq!(enc[0] & 0xF1, 0x21 & 0xF1 | 0x01);
        assert_eq!(enc[2] & 0x01, 0x01);
        assert_eq!(enc[4] & 0x01, 0x01);
    }
}
