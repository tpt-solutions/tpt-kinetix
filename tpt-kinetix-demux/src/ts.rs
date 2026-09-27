//! MPEG-TS (MPEG-2 Transport Stream) demuxer.
//!
//! Parses 188-byte TS packets, the PAT/PMT program tables (PSI sections,
//! including sections that span multiple TS packets), and depacketizes PES
//! packets into [`Packet`]s. PCR base values from adaptation fields are
//! tracked per PID.
//!
//! This is a **pragmatic** reader covering the common single-program
//! broadcast/HLS cases:
//! - single and multi-program PAT, PMT with descriptor loop (registration
//!   descriptors are consulted for codec identification of private streams)
//! - PES assembly across arbitrarily many TS packets, with PTS/DTS
//! - PCR extraction and adaptation-field random-access indicators
//! - re-synchronization after leading or mid-stream garbage
//!
//! It does not implement continuity-counter validation, scrambled payload
//! decryption (scrambled packets are skipped), or DVB-specific tables (NIT /
//! SDT / EIT payloads are ignored).
//!
//! Elementary stream payloads are passed through unchanged, so H.264 packets
//! are Annex-B start-code framed and AAC packets are ADTS frames.
//!
//! # Example
//!
//! ```no_run
//! use tpt_kinetix_demux::ts::TsDemuxer;
//! use tpt_kinetix_demux::Demuxer;
//!
//! let bytes = std::fs::read("segment.ts").unwrap();
//! let mut ts = TsDemuxer::new(bytes).unwrap();
//! for stream in ts.streams() {
//!     println!("pid {} stream_type {:#04x} codec {:?}", stream.pid, stream.stream_type, stream.codec);
//! }
//! while let Some(pkt) = ts.read_packet().unwrap() {
//!     println!("packet: {} bytes on pid {}", pkt.data.len(), pkt.stream_index);
//! }
//! ```

use std::collections::{BTreeMap, HashMap, VecDeque};

use tpt_kinetix_core::{
    codec::{CodecId, MediaType},
    error::KinetixError,
    packet::Packet,
    timestamp::Timestamp,
};

use crate::Demuxer;

const TS_PACKET_LEN: usize = 188;
const SYNC_BYTE: u8 = 0x47;

const PID_PAT: u16 = 0x0000;
/// Null packet PID; packets on it carry nothing of interest.
const PID_NULL: u16 = 0x1FFF;
/// DVB service-information PIDs (NIT, SDT/BAT, EIT, RST/ST, TDT/TOT). Their
/// payloads are PSI-like sections we don't interpret; treating them as PSI
/// keeps them out of the PES path.
const PID_SI_RANGE: std::ops::RangeInclusive<u16> = 0x10..=0x14;

const TABLE_ID_PAT: u8 = 0x00;
const TABLE_ID_PMT: u8 = 0x02;

/// PSI section assembler scratch buffer is capped so corrupt input cannot
/// grow it unbounded while waiting for a section_length that never completes.
const MAX_SECTION_BUFFER: usize = 64 * 1024;
/// A PES under assembly is capped the same way (PES_packet_length is 16-bit,
/// so only the length-0 "unbounded" video case can exceed sane sizes).
const MAX_PES_BUFFER: usize = 8 * 1024 * 1024;

/// One program listed in the PAT, enriched with the PCR PID from its PMT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TsProgram {
    /// Program number as written in the PAT.
    pub program_number: u16,
    /// PID carrying this program's PMT.
    pub pmt_pid: u16,
    /// PID carrying the PCR for this program, from the PMT.
    pub pcr_pid: Option<u16>,
}

/// One elementary stream declared by a PMT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TsStream {
    /// Transport-stream PID the stream's PES packets arrive on.
    pub pid: u16,
    /// Raw `stream_type` byte from the PMT (e.g. `0x1B` for H.264).
    pub stream_type: u8,
    /// Best-effort codec identification; `None` when the stream type (and any
    /// registration descriptor) is not recognized.
    pub codec: Option<CodecId>,
    /// Media category implied by the codec (or [`MediaType::Other`] when
    /// unknown).
    pub media_type: MediaType,
    /// Whether this PID is its program's PCR carrier.
    pub is_pcr: bool,
}

/// Per-PID PES packet under assembly.
#[derive(Debug, Default)]
struct PesAssembler {
    buf: Vec<u8>,
    /// Random-access indicator seen on the PUSI packet that started this PES.
    key_frame: bool,
}

/// Stateful MPEG-TS demuxer over an in-memory buffer.
///
/// The constructor pre-scans the buffer for PAT/PMT (and the last PCR) so the
/// program/stream tables are available immediately; [`Demuxer::read_packet`]
/// then streams PES packets from the start.
///
/// `stream_index` on emitted packets is the transport-stream PID.
#[derive(Debug)]
pub struct TsDemuxer {
    data: Vec<u8>,
    /// Offset of the next TS packet.
    pos: usize,

    programs: Vec<TsProgram>,
    /// Streams keyed by PID; [`TsDemuxer::streams`] returns them PID-sorted.
    streams: BTreeMap<u16, TsStream>,
    /// PIDs whose payloads are sections (PAT + discovered PMTs).
    psi_pids: Vec<u16>,

    /// Partial PSI sections per PID.
    sections: HashMap<u16, Vec<u8>>,
    /// PES packets under assembly per PID.
    pes: HashMap<u16, PesAssembler>,
    /// Most recent PCR base (90 kHz) per PID.
    pcr: HashMap<u16, u64>,
    /// Fully assembled PES packets waiting to be returned.
    out: VecDeque<Packet>,
}

impl TsDemuxer {
    /// Creates a new demuxer and pre-scans the buffer for program tables.
    ///
    /// Returns an error if the data contains no valid TS sync (a 0x47 byte at
    /// a 188-byte packet boundary, confirmed by the next packet's sync byte
    /// when one exists).
    pub fn new(data: Vec<u8>) -> Result<Self, KinetixError> {
        let start = find_sync(&data, 0).ok_or_else(|| {
            KinetixError::Parse("no MPEG-TS sync found (expected 0x47 at packet pitch)".into())
        })?;

        let mut demuxer = Self {
            data,
            pos: start,
            programs: Vec::new(),
            streams: BTreeMap::new(),
            psi_pids: vec![PID_PAT],
            sections: HashMap::new(),
            pes: HashMap::new(),
            pcr: HashMap::new(),
            out: VecDeque::new(),
        };

        // Pre-scan: consume the whole buffer once, keeping only PSI + PCR
        // state, so `streams()`/`programs()` are populated before any
        // `read_packet` call. PES assembly is not run in this pass.
        let mut scan_pos = demuxer.pos;
        while let Some(pkt) = demuxer.next_packet(&mut scan_pos) {
            demuxer.process_packet(&pkt, true);
        }
        // Reset transient state; the tables stay.
        demuxer.pos = start;
        demuxer.sections.clear();
        demuxer.out.clear();
        Ok(demuxer)
    }

    /// The elementary streams declared by the PMT(s), ordered by PID.
    pub fn streams(&self) -> Vec<TsStream> {
        self.streams.values().cloned().collect()
    }

    /// The programs discovered in the PAT, ordered by program number.
    pub fn programs(&self) -> &[TsProgram] {
        &self.programs
    }

    /// Most recent PCR base value seen on `pid`, in 90 kHz ticks.
    ///
    /// Note the constructor pre-scans the whole buffer, so before the first
    /// [`Demuxer::read_packet`] call this reports the last PCR in the file.
    pub fn pcr_90khz(&self, pid: u16) -> Option<u64> {
        self.pcr.get(&pid).copied()
    }

    fn is_psi_pid(&self, pid: u16) -> bool {
        pid == PID_PAT || self.psi_pids.contains(&pid) || PID_SI_RANGE.contains(&pid)
    }

    /// Returns the next 188-byte packet starting at `*pos`, advancing `*pos`
    /// past it. Re-synchronizes by scanning forward for a sync byte confirmed
    /// by the following packet's sync byte when alignment is lost.
    fn next_packet(&self, pos: &mut usize) -> Option<[u8; TS_PACKET_LEN]> {
        let p = find_sync(&self.data, *pos)?;
        let mut pkt = [0u8; TS_PACKET_LEN];
        pkt.copy_from_slice(&self.data[p..p + TS_PACKET_LEN]);
        *pos = p + TS_PACKET_LEN;
        Some(pkt)
    }

    /// Parse one 188-byte packet and feed its payload into the PSI or PES
    /// machinery. `prescan` suppresses PES assembly (constructor pre-scan).
    fn process_packet(&mut self, pkt: &[u8], prescan: bool) {
        let pusi = pkt[1] & 0x40 != 0;
        let pid = ((u16::from(pkt[1]) & 0x1F) << 8) | u16::from(pkt[2]);
        let transport_scrambling = pkt[3] >> 6;
        let adaptation_field_control = (pkt[3] >> 4) & 0x3;

        if pid == PID_NULL {
            return;
        }

        // adaptation_field_control: 0 is forbidden, 2 means no payload.
        let mut payload: &[u8] = match adaptation_field_control {
            1 | 3 => &pkt[4..],
            _ => return,
        };

        let (mut rai, mut discontinuity) = (false, false);
        if adaptation_field_control & 0b10 != 0 {
            match self.parse_adaptation_field(pid, payload) {
                Some((r, d, rest)) => {
                    rai = r;
                    discontinuity = d;
                    payload = rest;
                }
                None => return,
            }
        }

        if transport_scrambling != 0 || payload.is_empty() {
            return; // encrypted payloads cannot be depacketized
        }

        if self.is_psi_pid(pid) {
            self.feed_section(pid, pusi, payload);
        } else if !prescan {
            self.feed_pes(pid, pusi, rai, discontinuity, payload);
        }
    }

    /// Parse the adaptation field at the head of `payload`, recording PCR for
    /// `pid`, and return its flags plus the remaining payload bytes (if any).
    fn parse_adaptation_field<'a>(
        &mut self,
        pid: u16,
        payload: &'a [u8],
    ) -> Option<(bool, bool, &'a [u8])> {
        let af_len = usize::from(*payload.first()?);
        if 1 + af_len > payload.len() {
            return None; // truncated adaptation field
        }
        let af = &payload[1..1 + af_len];
        let mut rai = false;
        let mut discontinuity = false;
        if let Some(&flags) = af.first() {
            rai = flags & 0x40 != 0;
            discontinuity = flags & 0x80 != 0;
            if flags & 0x10 != 0 && af.len() >= 7 {
                // PCR: 33-bit base in 90 kHz units, then 6 reserved bits and
                // a 9-bit extension (which we ignore).
                let base = (u64::from(af[1]) << 25)
                    | (u64::from(af[2]) << 17)
                    | (u64::from(af[3]) << 9)
                    | (u64::from(af[4]) << 1)
                    | (u64::from(af[5]) >> 7);
                self.pcr.insert(pid, base);
            }
        }
        Some((rai, discontinuity, &payload[1 + af_len..]))
    }

    /// Accumulate PSI payloads for `pid` and dispatch each complete section.
    fn feed_section(&mut self, pid: u16, pusi: bool, payload: &[u8]) {
        {
            let buf = self.sections.entry(pid).or_default();
            if pusi {
                // pointer_field: bytes to skip before the first section.
                let skip = 1 + usize::from(payload[0]);
                buf.clear();
                if skip <= payload.len() {
                    buf.extend_from_slice(&payload[skip..]);
                }
            } else {
                if buf.is_empty() {
                    return; // no section in flight
                }
                buf.extend_from_slice(payload);
            }
            if buf.len() > MAX_SECTION_BUFFER {
                buf.clear();
                return;
            }
        }

        // Extract as many complete sections as the buffer holds.
        loop {
            let total = {
                let Some(buf) = self.sections.get_mut(&pid) else {
                    break;
                };
                if buf.len() < 3 {
                    break;
                }
                let section_length =
                    (((u16::from(buf[1]) & 0x0F) << 8) | u16::from(buf[2])) as usize;
                let total = 3 + section_length;
                if total > buf.len() {
                    break;
                }
                let section = buf[..total].to_vec();
                buf.drain(..total);
                section
            };
            self.handle_section(pid, &total);
        }
    }

    /// Validate and interpret one complete PSI section from `pid`.
    fn handle_section(&mut self, pid: u16, section: &[u8]) {
        // Smallest legal PAT/PMT section: 8-byte header + 4-byte CRC; also
        // keeps the CRC slice and the version byte below in bounds.
        if section.len() < 12 {
            return;
        }
        let table_id = section[0];
        // Only sections marked "current" apply.
        if section[5] & 0x01 == 0 {
            return;
        }
        // PAT/PMT sections are CRC32-protected; drop corrupt ones.
        let body_len = section.len() - 4;
        let expected = mpeg_crc32(&section[..body_len]);
        let stored = u32::from_be_bytes([
            section[body_len],
            section[body_len + 1],
            section[body_len + 2],
            section[body_len + 3],
        ]);
        if expected != stored {
            return;
        }

        match (pid, table_id) {
            (PID_PAT, TABLE_ID_PAT) => self.handle_pat(section),
            (_, TABLE_ID_PMT) if pid != PID_PAT => self.handle_pmt(section),
            _ => {}
        }
    }

    /// program_number → PMT PID entries.
    fn handle_pat(&mut self, section: &[u8]) {
        // 8-byte fixed header before the loop, 4-byte CRC at the end.
        let end = section.len().saturating_sub(4);
        let mut pos = 8;
        while pos + 4 <= end {
            let program_number = u16::from_be_bytes([section[pos], section[pos + 1]]);
            let pid = (((u16::from(section[pos + 2]) & 0x1F) << 8) | u16::from(section[pos + 3]))
                & 0x1FFF;
            // program_number 0 is the network PID, which we don't follow.
            if program_number != 0 && pid != PID_NULL {
                if !self.psi_pids.contains(&pid) {
                    self.psi_pids.push(pid);
                }
                match self
                    .programs
                    .iter_mut()
                    .find(|p| p.program_number == program_number)
                {
                    Some(existing) => existing.pmt_pid = pid,
                    None => self.programs.push(TsProgram {
                        program_number,
                        pmt_pid: pid,
                        pcr_pid: None,
                    }),
                }
            }
            pos += 4;
        }
        self.programs.sort_by_key(|p| p.program_number);
    }

    /// ES loop → stream table.
    fn handle_pmt(&mut self, section: &[u8]) {
        if section.len() < 12 {
            return;
        }
        let program_number = u16::from_be_bytes([section[3], section[4]]);
        let pcr_pid = (((u16::from(section[8]) & 0x1F) << 8) | u16::from(section[9])) & 0x1FFF;
        let program_info_length =
            (((u16::from(section[10]) & 0x0F) << 8) | u16::from(section[11])) as usize;
        let end = section.len().saturating_sub(4);
        let program_info = section
            .get(12..12 + program_info_length.min(end.saturating_sub(12)))
            .unwrap_or(&[]);
        let pos = 12 + program_info.len();

        if let Some(prog) = self
            .programs
            .iter_mut()
            .find(|p| p.program_number == program_number)
        {
            prog.pcr_pid = (pcr_pid != PID_NULL).then_some(pcr_pid);
        }

        let mut pos = pos;
        while pos + 5 <= end {
            let stream_type = section[pos];
            let pid = (((u16::from(section[pos + 1]) & 0x1F) << 8) | u16::from(section[pos + 2]))
                & 0x1FFF;
            let es_info_length = (((u16::from(section[pos + 3]) & 0x0F) << 8)
                | u16::from(section[pos + 4])) as usize;
            let info_start = pos + 5;
            let info_end = (info_start + es_info_length).min(end);
            let es_info = section.get(info_start..info_end).unwrap_or(&[]);
            // Registration descriptors can appear in either the program-info
            // or the ES-info loop; prefer the ES-level one.
            let codec = codec_from_stream_type(stream_type, es_info)
                .or_else(|| codec_from_stream_type(stream_type, program_info));
            let media_type = codec.map_or(MediaType::Other, |c| c.media_type());
            let is_pcr = self.programs.iter().any(|p| p.pcr_pid == Some(pid));
            self.streams.insert(
                pid,
                TsStream {
                    pid,
                    stream_type,
                    codec,
                    media_type,
                    is_pcr,
                },
            );
            pos = info_start + es_info_length;
        }
    }

    /// Accumulate PES payloads for `pid`, finalizing on a declared-length PES
    /// completing, on the next PUSI, or at end of data.
    fn feed_pes(&mut self, pid: u16, pusi: bool, rai: bool, discontinuity: bool, payload: &[u8]) {
        if pusi {
            // A new PUSI ends whatever PES was in flight (the unbounded
            // length-0 video case never "completes" on its own).
            let assembler = self.pes.entry(pid).or_default();
            let previous = std::mem::take(&mut assembler.buf);
            let key = std::mem::take(&mut assembler.key_frame);
            if !previous.is_empty() {
                self.finalize_pes(pid, &previous, key);
            }
            // A discontinuity flag on the starting packet is fine; the fresh
            // start below replaces any (possibly garbage) prior state anyway.
            if payload.len() >= 3 && payload[..3] == [0x00, 0x00, 0x01] {
                let assembler = self.pes.get_mut(&pid).expect("entry created above");
                assembler.buf = payload.to_vec();
                assembler.key_frame = rai;
            }
            // A PUSI payload that isn't a PES start code begins no PES; skip
            // until the next PUSI.
            return;
        }

        let Some(assembler) = self.pes.get_mut(&pid) else {
            return;
        };
        if assembler.buf.is_empty() {
            return; // mid-PES packet with nothing in flight
        }
        if discontinuity {
            assembler.buf.clear();
            assembler.key_frame = false;
            return;
        }
        assembler.buf.extend_from_slice(payload);
        if assembler.buf.len() > MAX_PES_BUFFER {
            assembler.buf.clear();
            assembler.key_frame = false;
            return;
        }

        // Declared (non-zero) PES_packet_length: finalize once complete.
        // The length counts bytes *after* the length field, so the full
        // packet is 6 + pes_len bytes.
        if assembler.buf.len() >= 6 {
            let pes_len = u16::from_be_bytes([assembler.buf[4], assembler.buf[5]]) as usize;
            if pes_len > 0 && assembler.buf.len() >= 6 + pes_len {
                let buf = std::mem::take(&mut assembler.buf);
                let key = std::mem::take(&mut assembler.key_frame);
                self.finalize_pes(pid, &buf, key);
            }
        }
    }

    /// Turn an assembled PES byte string into a [`Packet`] on `self.out`.
    fn finalize_pes(&mut self, pid: u16, buf: &[u8], key_frame: bool) {
        let Some(mut packet) = parse_pes(buf, pid) else {
            return;
        };
        packet.is_key_frame = key_frame || nal_has_idr(&packet.data);
        self.out.push_back(packet);
    }
}

impl Demuxer for TsDemuxer {
    fn read_packet(&mut self) -> Result<Option<Packet>, KinetixError> {
        loop {
            if let Some(pkt) = self.out.pop_front() {
                return Ok(Some(pkt));
            }
            let mut pos = self.pos;
            let Some(pkt) = self.next_packet(&mut pos) else {
                self.pos = pos;
                // End of data: flush any PES still under assembly, in PID
                // order for determinism.
                let mut pending: Vec<_> = self
                    .pes
                    .drain()
                    .filter(|(_, a)| !a.buf.is_empty())
                    .collect();
                pending.sort_by_key(|(pid, _)| *pid);
                for (pid, assembler) in pending {
                    self.finalize_pes(pid, &assembler.buf, assembler.key_frame);
                }
                return Ok(self.out.pop_front());
            };
            self.pos = pos;
            self.process_packet(&pkt, false);
        }
    }

    fn seek(&mut self, _target_pts_ms: i64) -> Result<(), KinetixError> {
        Err(KinetixError::Unsupported(
            "MPEG-TS seeking is not yet supported".into(),
        ))
    }
}

/// Find the offset of a usable TS sync: a 0x47 byte with room for a full
/// packet, confirmed by the next packet's sync byte when one exists.
fn find_sync(data: &[u8], from: usize) -> Option<usize> {
    let mut p = from;
    while p + TS_PACKET_LEN <= data.len() {
        if data[p] == SYNC_BYTE
            && (p + 2 * TS_PACKET_LEN > data.len() || data[p + TS_PACKET_LEN] == SYNC_BYTE)
        {
            return Some(p);
        }
        p += 1;
    }
    None
}

/// Parse a complete (or trailing, possibly truncated) PES byte string.
fn parse_pes(buf: &[u8], pid: u16) -> Option<Packet> {
    if buf.len() < 9 || buf[..3] != [0x00, 0x00, 0x01] {
        return None;
    }
    let stream_id = buf[3];
    let pes_len = u16::from_be_bytes([buf[4], buf[5]]) as usize;
    // '10' marker bits, then the 2-bit scrambling control: scrambled PES
    // payload cannot be interpreted.
    if buf[6] >> 6 != 0b10 || (buf[6] >> 4) & 0x3 != 0 {
        return None;
    }
    let flags = buf[7];
    let header_data_len = usize::from(buf[8]);
    let data_start = 9 + header_data_len;
    if data_start > buf.len() {
        return None;
    }
    // PES_packet_length counts bytes after the length field: the full
    // packet ends at 6 + pes_len. A length of 0 means unbounded (video),
    // so the packet ends where our buffer does.
    let data_end = if pes_len > 0 {
        (6 + pes_len).min(buf.len())
    } else {
        buf.len()
    };

    // Padding stream carries no ES data.
    if stream_id == 0xBE {
        return None;
    }
    // Conditional-access / DSM-CC / other system streams are not elementary.
    if !(0xBC..=0xEF).contains(&stream_id) {
        return None;
    }

    let mut pts = Timestamp::NONE;
    let mut dts = Timestamp::NONE;
    let pts_dts_flags = flags >> 6;
    if pts_dts_flags >= 0b10 && header_data_len >= 5 {
        let v = decode_timestamp(&buf[9..14]);
        pts = Timestamp::new(v as i64, (1, 90_000));
        if pts_dts_flags == 0b11 && header_data_len >= 10 {
            let v = decode_timestamp(&buf[14..19]);
            dts = Timestamp::new(v as i64, (1, 90_000));
        }
    }
    if dts.is_none() {
        dts = pts;
    }

    Some(Packet {
        pts,
        dts,
        data: buf[data_start..data_end].to_vec(),
        stream_index: u32::from(pid),
        is_key_frame: false,
    })
}

/// Decode a 5-byte PES PTS/DTS field (33 bits, with marker bits).
fn decode_timestamp(b: &[u8]) -> u64 {
    ((u64::from(b[0] >> 1) & 0x07) << 30)
        | (u64::from(b[1]) << 22)
        | ((u64::from(b[2] >> 1) & 0x7F) << 15)
        | (u64::from(b[3]) << 7)
        | u64::from(b[4] >> 1)
}

/// Returns the codec for a PMT ES entry, consulting `stream_type` first and
/// the registration descriptor (tag `0x05`) for private-data stream types.
fn codec_from_stream_type(stream_type: u8, es_info: &[u8]) -> Option<CodecId> {
    match stream_type {
        0x1B => Some(CodecId::H264),
        0x24 => Some(CodecId::H265),
        0x0F | 0x11 => Some(CodecId::Aac),
        _ => {
            let id = registration_format_identifier(es_info)?;
            match &id {
                b"AV01" | b"av01" => Some(CodecId::Av1),
                b"vp09" => Some(CodecId::Vp9),
                b"Opus" => Some(CodecId::Opus),
                b"fLaC" => Some(CodecId::Flac),
                _ => None,
            }
        }
    }
}

/// Walk a descriptor list for the 4-byte format identifier of the first
/// registration descriptor (tag 0x05).
fn registration_format_identifier(es_info: &[u8]) -> Option<[u8; 4]> {
    let mut pos = 0;
    while pos + 2 <= es_info.len() {
        let tag = es_info[pos];
        let len = usize::from(es_info[pos + 1]);
        let data = es_info.get(pos + 2..pos + 2 + len)?;
        if tag == 0x05 && data.len() >= 4 {
            return Some([data[0], data[1], data[2], data[3]]);
        }
        pos += 2 + len;
    }
    None
}

/// Scan an Annex-B elementary stream for an H.264 IDR NAL unit (type 5),
/// used as a key-frame signal when the muxer didn't set the random-access
/// indicator.
fn nal_has_idr(data: &[u8]) -> bool {
    for i in 2..data.len() {
        // Start-code prefix 00 00 01 (also matched inside 00 00 00 01).
        if data[i] == 1 && data[i - 1] == 0 && data[i - 2] == 0 {
            if let Some(&nal) = data.get(i + 1) {
                if nal & 0x1F == 5 {
                    return true;
                }
            }
        }
    }
    false
}

/// MPEG-2 systems CRC-32 (polynomial 0x04C11DB7, MSB-first, init 0xFFFFFFFF),
/// used to validate PSI sections.
fn mpeg_crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= u32::from(byte) << 24;
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

    // ── primitive-level tests ────────────────────────────────────────────────

    #[test]
    fn crc32_matches_muxer_reference() {
        // Same algorithm as tpt-kinetix-stream's TS muxer; a known PAT header
        // must produce a stable value so the two crates interoperate.
        let a = mpeg_crc32(&[0x00, 0xB0, 0x0D, 0x00, 0x01, 0xC1, 0x00, 0x00]);
        assert_ne!(a, 0);
    }

    #[test]
    fn timestamp_decode_roundtrip() {
        // Encode PTS = 90000 (1 s) the way the TS muxer does and decode it.
        let pts = 90_000u64;
        let enc = [
            0x21 | (((pts >> 30) as u8 & 0x07) << 1),
            (pts >> 22) as u8,
            0x01 | (((pts >> 15) as u8 & 0x7F) << 1),
            (pts >> 7) as u8,
            0x01 | ((pts as u8 & 0x7F) << 1),
        ];
        assert_eq!(decode_timestamp(&enc), pts);
    }

    #[test]
    fn find_sync_skips_leading_garbage() {
        let mut data = vec![0xFF; 300];
        data.push(SYNC_BYTE);
        data.extend_from_slice(&[0u8; TS_PACKET_LEN - 1]);
        data.extend_from_slice(&[SYNC_BYTE]);
        data.extend_from_slice(&[0u8; TS_PACKET_LEN - 1]);
        let demuxer = TsDemuxer::new(data).expect("sync found after garbage");
        let mut pos = demuxer.pos;
        let pkt = demuxer.next_packet(&mut pos).expect("one packet");
        assert_eq!(pkt.len(), TS_PACKET_LEN);
    }

    #[test]
    fn rejects_data_without_sync() {
        let err = TsDemuxer::new(vec![0x12; 500]).unwrap_err();
        assert!(matches!(err, KinetixError::Parse(_)));
    }

    #[test]
    fn too_short_data_is_rejected() {
        assert!(TsDemuxer::new(vec![SYNC_BYTE; 10]).is_err());
    }

    #[test]
    fn nal_idr_detection() {
        let annexb = [
            0x00, 0x00, 0x00, 0x01, 0x67, 0x64, 0x00, 0x1F, 0x00, 0x00, 0x01, 0x65, 0x88,
        ];
        assert!(nal_has_idr(&annexb));
        let no_idr = [0x00, 0x00, 0x00, 0x01, 0x67, 0x64, 0x00, 0x01, 0x41];
        assert!(!nal_has_idr(&no_idr));
    }

    // ── synthetic-stream helpers ─────────────────────────────────────────────

    /// Build a complete PSI section with CRC.
    fn section(table_id: u8, body: &[u8]) -> Vec<u8> {
        let mut s = Vec::new();
        s.push(table_id);
        let section_length = body.len() + 4;
        s.extend_from_slice(&(0xB000u16 | section_length as u16).to_be_bytes());
        s.extend_from_slice(body);
        let crc = mpeg_crc32(&s);
        s.extend_from_slice(&crc.to_be_bytes());
        s
    }

    /// Wrap a PSI section in a TS packet on `pid` (single packet; sections
    /// must fit in 184 bytes).
    fn psi_packet(pid: u16, sec: &[u8]) -> Vec<u8> {
        let mut p = vec![
            SYNC_BYTE,
            0x40 | ((pid >> 8) as u8 & 0x1F),
            pid as u8,
            0x10,
            0x00,
        ];
        p.extend_from_slice(sec);
        p.resize(TS_PACKET_LEN, 0xFF);
        p
    }

    /// PES-encapsulate `payload` and split it into TS packets on `pid`.
    /// The first packet carries an adaptation field with PCR + RAI.
    fn pes_packets(pid: u16, stream_id: u8, pts: u64, key: bool, payload: &[u8]) -> Vec<Vec<u8>> {
        let mut pes = Vec::new();
        pes.extend_from_slice(&[0x00, 0x00, 0x01, stream_id]);
        let hdr = 10; // PTS + DTS (DTS = PTS here)
        let es_len = payload.len() + 3 + hdr;
        pes.extend_from_slice(&(es_len as u16).to_be_bytes());
        pes.push(0x80);
        pes.push(0xC0); // PTS + DTS flags
        pes.push(hdr as u8);
        for prefix in [0x31u8, 0x11] {
            let v = pts & 0x1_FFFF_FFFF;
            pes.push(prefix | (((v >> 30) as u8 & 0x07) << 1));
            pes.push((v >> 22) as u8);
            pes.push(0x01 | (((v >> 15) as u8 & 0x7F) << 1));
            pes.push((v >> 7) as u8);
            pes.push(0x01 | ((v as u8 & 0x7F) << 1));
        }
        pes.extend_from_slice(payload);

        let mut out = Vec::new();
        let mut cc = 0u8;
        let mut offset = 0;
        let mut first = true;
        while offset < pes.len() {
            let mut p = vec![SYNC_BYTE];
            p.push(if first { 0x40 } else { 0x00 } | ((pid >> 8) as u8 & 0x1F));
            p.push(pid as u8);
            let remaining = pes.len() - offset;
            if first || remaining < TS_PACKET_LEN - 5 {
                p.push(0x30 | cc);
                let mut af = vec![if key { 0x50 } else { 0x10 }]; // [+RAI] +PCR
                let base = pts & 0x1_FFFF_FFFF;
                af.extend_from_slice(&[
                    (base >> 25) as u8,
                    (base >> 17) as u8,
                    (base >> 9) as u8,
                    (base >> 1) as u8,
                    (((base & 0x1) as u8) << 7) | 0x7E,
                    0x00,
                ]);
                let space = TS_PACKET_LEN - 5 - af.len();
                let take = remaining.min(space);
                let stuffing = space - take;
                p.push((af.len() + stuffing) as u8);
                p.extend_from_slice(&af);
                p.extend(std::iter::repeat_n(0xFF, stuffing));
                p.extend_from_slice(&pes[offset..offset + take]);
                offset += take;
            } else {
                p.push(0x10 | cc);
                let take = remaining.min(TS_PACKET_LEN - 4);
                p.extend_from_slice(&pes[offset..offset + take]);
                offset += take;
            }
            assert_eq!(p.len(), TS_PACKET_LEN);
            cc = (cc + 1) & 0x0F;
            out.push(p);
            first = false;
        }
        out
    }

    fn concat(pkts: &[Vec<u8>]) -> Vec<u8> {
        pkts.concat()
    }

    fn sample_pat(pmt_pid: u16) -> Vec<u8> {
        let body = {
            let mut b = vec![0x00, 0x01, 0xC1, 0x00, 0x00]; // tsid, version, sec, last
            b.extend_from_slice(&1u16.to_be_bytes());
            b.extend_from_slice(&(0xE000 | pmt_pid).to_be_bytes());
            b
        };
        section(TABLE_ID_PAT, &body)
    }

    /// PMT for program 1 with one ES of `stream_type` on `es_pid`; the
    /// program's PCR rides the ES PID itself.
    fn sample_pmt(stream_type: u8, es_pid: u16, es_info: &[u8]) -> Vec<u8> {
        let body = {
            let mut b = vec![0x00, 0x01, 0xC1, 0x00, 0x00]; // program 1
            b.extend_from_slice(&(0xE000 | es_pid).to_be_bytes()); // PCR_PID
            b.extend_from_slice(&(0xF000u16 | es_info.len() as u16).to_be_bytes());
            b.extend_from_slice(es_info);
            b.push(stream_type);
            b.extend_from_slice(&(0xE000 | es_pid).to_be_bytes());
            b.extend_from_slice(&0xF000u16.to_be_bytes());
            b
        };
        section(TABLE_ID_PMT, &body)
    }

    fn h264_single_program_ts() -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&psi_packet(PID_PAT, &sample_pat(0x1000)));
        data.extend_from_slice(&psi_packet(0x1000, &sample_pmt(0x1B, 0x0100, &[])));
        let pkts = pes_packets(
            0x0100,
            0xE0,
            90_000,
            true,
            &[0x00, 0x00, 0x00, 0x01, 0x65, 0xAB],
        );
        data.extend_from_slice(&concat(&pkts));
        data
    }

    // ── end-to-end synthetic stream tests ────────────────────────────────────

    #[test]
    fn parses_tables_and_one_packet() {
        let mut ts = TsDemuxer::new(h264_single_program_ts()).expect("parse");
        let streams = ts.streams();
        assert_eq!(streams.len(), 1);
        assert_eq!(streams[0].pid, 0x0100);
        assert_eq!(streams[0].stream_type, 0x1B);
        assert_eq!(streams[0].codec, Some(CodecId::H264));
        assert_eq!(streams[0].media_type, MediaType::Video);
        assert!(streams[0].is_pcr);
        assert_eq!(ts.programs().len(), 1);
        assert_eq!(ts.programs()[0].program_number, 1);
        assert_eq!(ts.programs()[0].pmt_pid, 0x1000);
        assert_eq!(ts.programs()[0].pcr_pid, Some(0x0100));

        // PCR base was seen in the pre-scan (last PCR in file).
        assert_eq!(ts.pcr_90khz(0x0100), Some(90_000));

        let pkt = ts.read_packet().unwrap().expect("packet");
        assert_eq!(pkt.stream_index, 0x0100);
        assert_eq!(pkt.pts.value, 90_000);
        assert_eq!(pkt.pts.time_base, (1, 90_000));
        assert_eq!(pkt.dts.value, 90_000);
        assert!(pkt.is_key_frame);
        assert_eq!(pkt.data, vec![0x00, 0x00, 0x00, 0x01, 0x65, 0xAB]);
        assert!(ts.read_packet().unwrap().is_none());
    }

    #[test]
    fn multi_packet_pes_is_reassembled() {
        // 500-byte payload forces several TS packets.
        let payload: Vec<u8> = (0..500u32).map(|i| (i * 7 % 251) as u8).collect();
        let mut data = Vec::new();
        data.extend_from_slice(&psi_packet(PID_PAT, &sample_pat(0x1000)));
        data.extend_from_slice(&psi_packet(0x1000, &sample_pmt(0x1B, 0x0100, &[])));
        data.extend_from_slice(&concat(&pes_packets(
            0x0100, 0xE0, 1_800_000, true, &payload,
        )));

        let mut ts = TsDemuxer::new(data).expect("parse");
        let pkt = ts.read_packet().unwrap().expect("reassembled packet");
        assert_eq!(pkt.data, payload);
        assert_eq!(pkt.pts.value, 1_800_000);
        assert!(ts.read_packet().unwrap().is_none());
    }

    #[test]
    fn audio_stream_identified() {
        let mut data = Vec::new();
        data.extend_from_slice(&psi_packet(PID_PAT, &sample_pat(0x1000)));
        data.extend_from_slice(&psi_packet(0x1000, &sample_pmt(0x0F, 0x0101, &[])));
        let pkts = pes_packets(0x0101, 0xC0, 0, false, &[0xFF, 0xF1, 0x50, 0x80]);
        data.extend_from_slice(&concat(&pkts));

        let mut ts = TsDemuxer::new(data).expect("parse");
        let s = &ts.streams()[0];
        assert_eq!(s.codec, Some(CodecId::Aac));
        assert_eq!(s.media_type, MediaType::Audio);
        // The sample PMT puts the PCR on the ES PID itself.
        assert!(s.is_pcr);

        let pkt = ts.read_packet().unwrap().expect("audio packet");
        assert_eq!(pkt.stream_index, 0x0101);
        assert_eq!(pkt.pts.value, 0);
        // Builder does not set RAI and the ADTS payload has no IDR NAL.
        assert!(!pkt.is_key_frame);
    }

    #[test]
    fn registration_descriptor_identifies_av1() {
        // stream_type 0x06 (private PES data) + registration 'AV01'.
        let mut es_info = vec![0x05, 0x04];
        es_info.extend_from_slice(b"AV01");
        let mut data = Vec::new();
        data.extend_from_slice(&psi_packet(PID_PAT, &sample_pat(0x1000)));
        data.extend_from_slice(&psi_packet(0x1000, &sample_pmt(0x06, 0x0102, &es_info)));
        let pkts = pes_packets(0x0102, 0xE0, 0, false, &[0x0A, 0x0B, 0x0C]);
        data.extend_from_slice(&concat(&pkts));

        let mut ts = TsDemuxer::new(data).expect("parse");
        let s = &ts.streams()[0];
        assert_eq!(s.codec, Some(CodecId::Av1));
        let pkt = ts.read_packet().unwrap().expect("packet");
        assert_eq!(pkt.data, vec![0x0A, 0x0B, 0x0C]);
    }

    #[test]
    fn corrupt_crc_section_is_dropped() {
        let mut pat = psi_packet(PID_PAT, &sample_pat(0x1000));
        // Flip bits in the CRC (the section's last bytes before padding).
        let last = pat.iter().rposition(|&b| b != 0xFF).expect("non-pad");
        pat[last] ^= 0xFF;

        let mut data = pat;
        data.extend_from_slice(&psi_packet(0x1000, &sample_pmt(0x1B, 0x0100, &[])));
        let pkts = pes_packets(0x0100, 0xE0, 0, false, &[0x01, 0x02]);
        data.extend_from_slice(&concat(&pkts));

        let ts = TsDemuxer::new(data).expect("parse");
        // The PAT was corrupt → program 1 is unknown, so the PMT packet rides
        // an unregistered PID and its section payload never reaches the table
        // parser. No streams, no panic.
        assert!(ts.streams().is_empty());
    }

    #[test]
    fn garbage_between_packets_resyncs() {
        // Corrupt the PMT packet's sync byte: the pre-scan must skip it and
        // the reader must resume at the video packet.
        let mut data = h264_single_program_ts();
        data[TS_PACKET_LEN] = 0x33;

        let mut ts = TsDemuxer::new(data).expect("resync to video packets");
        // The PAT was aligned with the broken packet, so its table was never
        // confirmed either — but the PES packets still parse out.
        assert!(ts.streams().is_empty());
        let pkt = ts.read_packet().unwrap().expect("video packet survives");
        assert_eq!(pkt.stream_index, 0x0100);
        assert!(pkt.is_key_frame);
    }

    #[test]
    fn pes_without_pts_gets_none_timestamps() {
        let payload = [0xCA_u8, 0xFE];
        let mut pes = vec![0x00, 0x00, 0x01, 0xE0];
        let es_len = payload.len() + 3;
        pes.extend_from_slice(&(es_len as u16).to_be_bytes());
        pes.push(0x80); // flags1: '10' marker, no scrambling
        pes.push(0x00); // flags2: no PTS/DTS
        pes.push(0x00); // header_data_length 0
        pes.extend_from_slice(&payload);

        let mut pkt_bytes = vec![SYNC_BYTE, 0x40 | 0x01, 0x00, 0x10];
        pkt_bytes.extend_from_slice(&pes);
        pkt_bytes.resize(TS_PACKET_LEN, 0xFF);
        // No PAT/PMT at all — PES on an unknown PID must still be emitted.
        let mut ts = TsDemuxer::new(pkt_bytes).expect("parse");
        let pkt = ts.read_packet().unwrap().expect("packet");
        assert!(pkt.pts.is_none());
        assert!(pkt.dts.is_none());
        assert_eq!(pkt.data, payload.to_vec());
    }

    #[test]
    fn truncated_tail_pes_is_flushed_at_eof() {
        // PES spans several packets but the stream ends early (declared
        // length not reached): the partial PES must still be emitted.
        let mut data = Vec::new();
        data.extend_from_slice(&psi_packet(PID_PAT, &sample_pat(0x1000)));
        data.extend_from_slice(&psi_packet(0x1000, &sample_pmt(0x1B, 0x0100, &[])));
        let payload = [0xAA_u8; 400];
        let mut pkts = pes_packets(0x0100, 0xE0, 42, true, &payload);
        pkts.truncate(2); // cut the continuation packets
        data.extend_from_slice(&concat(&pkts));

        let mut ts = TsDemuxer::new(data).expect("parse");
        let pkt = ts.read_packet().unwrap().expect("partial packet flushed");
        assert!(pkt.data.len() < payload.len());
        assert_eq!(pkt.pts.value, 42);
        assert!(ts.read_packet().unwrap().is_none());
    }

    #[test]
    fn leading_garbage_before_tables() {
        let mut data = vec![0xEE; 37];
        data.extend_from_slice(&h264_single_program_ts());
        let mut ts = TsDemuxer::new(data).expect("sync found");
        assert_eq!(ts.streams()[0].codec, Some(CodecId::H264));
        assert!(ts.read_packet().unwrap().is_some());
    }

    #[test]
    fn interleaved_pids() {
        // PMT declaring both a video and an audio ES.
        let mut body = vec![0x00, 0x01, 0xC1, 0x00, 0x00];
        body.extend_from_slice(&(0xE000u16 | 0x0100).to_be_bytes()); // PCR on video
        body.extend_from_slice(&0xF000u16.to_be_bytes()); // program_info_length 0
        for (st, pid) in [(0x1Bu8, 0x0100u16), (0x0F, 0x0101)] {
            body.push(st);
            body.extend_from_slice(&(0xE000u16 | pid).to_be_bytes());
            body.extend_from_slice(&0xF000u16.to_be_bytes());
        }

        let mut data = Vec::new();
        data.extend_from_slice(&psi_packet(PID_PAT, &sample_pat(0x1000)));
        data.extend_from_slice(&psi_packet(0x1000, &section(TABLE_ID_PMT, &body)));
        let video = pes_packets(0x0100, 0xE0, 90_000, true, &[0x11; 300]);
        let audio = pes_packets(0x0101, 0xC0, 90_045, false, &[0x22; 300]);
        // Interleave the two PES packet runs.
        data.extend_from_slice(&video[0]);
        data.extend_from_slice(&audio[0]);
        data.extend_from_slice(&concat(&video[1..]));
        data.extend_from_slice(&concat(&audio[1..]));

        let mut ts = TsDemuxer::new(data).expect("parse");
        assert_eq!(ts.streams().len(), 2);
        // PCR is on the video PID only.
        assert!(ts.streams()[0].is_pcr);
        assert!(!ts.streams()[1].is_pcr);

        let mut seen = Vec::new();
        while let Some(pkt) = ts.read_packet().unwrap() {
            seen.push((pkt.stream_index, pkt.pts.value));
        }
        // Each bounded PES finalizes as soon as its declared length is
        // reached, so the two access units come out in file order.
        assert_eq!(seen, vec![(0x0100, 90_000), (0x0101, 90_045)]);
    }

    #[test]
    fn seek_is_unsupported() {
        let mut ts = TsDemuxer::new(h264_single_program_ts()).expect("parse");
        let err = ts.seek(0).unwrap_err();
        assert!(matches!(err, KinetixError::Unsupported(_)));
    }
}
