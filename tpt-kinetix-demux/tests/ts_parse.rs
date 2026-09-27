//! MPEG-TS demuxer integration tests.
//!
//! Streams are built here with an independent TS writer (a deliberate
//! cross-check against the parser: separate bit-packing code, public API
//! only). The ffmpeg-gated test additionally demuxes a real `libx264`
//! mpegts clip and cross-checks packet count and PTS against `ffprobe`.

use std::path::PathBuf;
use std::process::Command;

use tpt_kinetix_core::codec::{CodecId, MediaType};
use tpt_kinetix_demux::{Demuxer, TsDemuxer};

const TS_PACKET_LEN: usize = 188;
const SYNC: u8 = 0x47;

// ── independent TS writer ────────────────────────────────────────────────────

/// MPEG-2 CRC-32 (polynomial 0x04C11DB7, MSB-first).
fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= u32::from(b) << 24;
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

/// Build a PAT section mapping `programs` (program_number → PMT PID).
fn pat_section(programs: &[(u16, u16)]) -> Vec<u8> {
    let mut body = vec![0x00, 0x01, 0xC1, 0x00, 0x00]; // tsid 1, version 0, current
    for &(num, pid) in programs {
        body.extend_from_slice(&num.to_be_bytes());
        body.extend_from_slice(&(0xE000u16 | pid).to_be_bytes());
    }
    let mut sec = vec![0x00]; // table_id = PAT
    sec.extend_from_slice(&(0xB000u16 | (body.len() + 4) as u16).to_be_bytes());
    sec.extend_from_slice(&body);
    sec.extend_from_slice(&crc32(&sec).to_be_bytes());
    sec
}

/// Build a PMT section for one program: `pcr_pid` plus ES entries of
/// `(stream_type, pid)`. Descriptors are empty everywhere.
fn pmt_section(program_number: u16, pcr_pid: u16, streams: &[(u8, u16)]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&program_number.to_be_bytes());
    body.push(0xC1); // version 0, current_next 1
    body.push(0x00); // section_number
    body.push(0x00); // last_section_number
    body.extend_from_slice(&(0xE000u16 | pcr_pid).to_be_bytes());
    body.extend_from_slice(&0xF000u16.to_be_bytes()); // program_info_length 0
    for &(st, pid) in streams {
        body.push(st);
        body.extend_from_slice(&(0xE000u16 | pid).to_be_bytes());
        body.extend_from_slice(&0xF000u16.to_be_bytes()); // ES_info_length 0
    }
    let mut sec = vec![0x02]; // table_id = PMT
    sec.extend_from_slice(&(0xB000u16 | (body.len() + 4) as u16).to_be_bytes());
    sec.extend_from_slice(&body);
    sec.extend_from_slice(&crc32(&sec).to_be_bytes());
    sec
}

/// Simple TS packet: payload only, no adaptation field.
fn ts_packet(pid: u16, pusi: bool, payload: &[u8], cc: &mut u8) -> Vec<u8> {
    assert!(payload.len() <= TS_PACKET_LEN - 4);
    let mut p = Vec::with_capacity(TS_PACKET_LEN);
    p.push(SYNC);
    p.push(if pusi { 0x40 } else { 0x00 } | ((pid >> 8) as u8 & 0x1F));
    p.push(pid as u8);
    p.push(0x10 | *cc);
    *cc = cc.wrapping_add(1) & 0x0F;
    p.extend_from_slice(payload);
    p.resize(TS_PACKET_LEN, 0xFF);
    p
}

/// Ship a PSI section, splitting it across as many packets as needed
/// (first packet carries the pointer field).
fn psi_section_packets(pid: u16, sec: &[u8], cc: &mut u8) -> Vec<u8> {
    let mut stream = vec![0x00u8]; // pointer_field
    stream.extend_from_slice(sec);
    let mut out = Vec::new();
    for (i, chunk) in stream.chunks(TS_PACKET_LEN - 4).enumerate() {
        out.extend_from_slice(&ts_packet(pid, i == 0, chunk, cc));
    }
    out
}

/// PES-encapsulate `payload` (with PTS only) and split into TS packets. The
/// first packet carries an adaptation field with PCR (`pcr`) and the
/// random-access indicator (`key`).
fn pes_packets(pid: u16, stream_id: u8, pts: u64, pcr: bool, key: bool, payload: &[u8]) -> Vec<u8> {
    let mut pes = Vec::new();
    pes.extend_from_slice(&[0x00, 0x00, 0x01, stream_id]);
    let hdr = 5u16; // PTS only
    let pes_len = 3 + hdr + payload.len() as u16;
    pes.extend_from_slice(&pes_len.to_be_bytes());
    pes.push(0x80); // '10' marker
    pes.push(0x80); // PTS only
    pes.push(hdr as u8);
    let v = pts & 0x1_FFFF_FFFF;
    pes.push(0x21 | (((v >> 30) as u8 & 0x07) << 1));
    pes.push((v >> 22) as u8);
    pes.push(0x01 | (((v >> 15) as u8 & 0x7F) << 1));
    pes.push((v >> 7) as u8);
    pes.push(0x01 | ((v as u8 & 0x7F) << 1));
    pes.extend_from_slice(payload);

    let mut out = Vec::new();
    let mut cc = 0u8;
    let mut offset = 0usize;
    let mut first = true;
    while offset < pes.len() {
        let mut p = Vec::with_capacity(TS_PACKET_LEN);
        p.push(SYNC);
        p.push(if first { 0x40 } else { 0x00 } | ((pid >> 8) as u8 & 0x1F));
        p.push(pid as u8);
        let remaining = pes.len() - offset;
        if first || remaining < TS_PACKET_LEN - 5 {
            // adaptation field + payload
            p.push(0x30 | cc);
            let mut flags = 0u8;
            if first {
                if key {
                    flags |= 0x40; // random_access_indicator
                }
                if pcr {
                    flags |= 0x10; // PCR_flag
                }
            }
            let mut pcr_bytes = Vec::new();
            if first && pcr {
                let base = pts & 0x1_FFFF_FFFF;
                pcr_bytes.extend_from_slice(&[
                    (base >> 25) as u8,
                    (base >> 17) as u8,
                    (base >> 9) as u8,
                    (base >> 1) as u8,
                    (((base & 0x1) as u8) << 7) | 0x7E,
                    0x00,
                ]);
            }
            let space = TS_PACKET_LEN - 6 - pcr_bytes.len();
            let take = remaining.min(space);
            let stuffing = space - take;
            p.push((1 + pcr_bytes.len() + stuffing) as u8); // AF length
            p.push(flags);
            p.extend_from_slice(&pcr_bytes);
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
        cc = cc.wrapping_add(1) & 0x0F;
        out.extend_from_slice(&p);
        first = false;
    }
    out
}

// ── tests over synthetic streams ─────────────────────────────────────────────

#[test]
fn two_program_pat_with_two_streams() {
    let mut data = Vec::new();
    let mut cc = 0u8;
    // Two programs: PMTs on 0x1000 and 0x1001.
    data.extend_from_slice(&psi_section_packets(
        0x0000,
        &pat_section(&[(1, 0x1000), (2, 0x1001)]),
        &mut cc,
    ));
    // Program 1: H.264 video on 0x0100 (PCR there), AAC on 0x0101.
    data.extend_from_slice(&psi_section_packets(
        0x1000,
        &pmt_section(1, 0x0100, &[(0x1B, 0x0100), (0x0F, 0x0101)]),
        &mut cc,
    ));
    // Program 2: only declares a stream we don't map.
    data.extend_from_slice(&psi_section_packets(
        0x1001,
        &pmt_section(2, 0x0102, &[(0x02, 0x0102)]),
        &mut cc,
    ));

    let ts = TsDemuxer::new(data).expect("parse");
    assert_eq!(ts.programs().len(), 2);
    assert_eq!(ts.programs()[0].program_number, 1);
    assert_eq!(ts.programs()[0].pmt_pid, 0x1000);
    assert_eq!(ts.programs()[0].pcr_pid, Some(0x0100));
    assert_eq!(ts.programs()[1].program_number, 2);
    assert_eq!(ts.programs()[1].pmt_pid, 0x1001);

    let streams = ts.streams();
    assert_eq!(streams.len(), 3);
    let video = streams.iter().find(|s| s.pid == 0x0100).expect("video");
    assert_eq!(video.codec, Some(CodecId::H264));
    assert_eq!(video.media_type, MediaType::Video);
    assert!(video.is_pcr);
    let audio = streams.iter().find(|s| s.pid == 0x0101).expect("audio");
    assert_eq!(audio.codec, Some(CodecId::Aac));
    assert_eq!(audio.media_type, MediaType::Audio);
    assert!(!audio.is_pcr);
    let mpeg2 = streams.iter().find(|s| s.pid == 0x0102).expect("mpeg2");
    assert_eq!(mpeg2.codec, None);
    assert_eq!(mpeg2.media_type, MediaType::Other);
}

#[test]
fn pmt_section_spanning_multiple_packets() {
    // 40 ES entries → 9 + 200 = 209-byte body, far beyond one packet's
    // 184 payload bytes: the section must be reassembled across packets.
    let streams: Vec<(u8, u16)> = (0..40u16).map(|i| (0x1B, 0x0200 + i)).collect();
    let mut data = Vec::new();
    let mut cc = 0u8;
    data.extend_from_slice(&psi_section_packets(
        0x0000,
        &pat_section(&[(1, 0x1000)]),
        &mut cc,
    ));
    data.extend_from_slice(&psi_section_packets(
        0x1000,
        &pmt_section(1, 0x0200, &streams),
        &mut cc,
    ));

    let ts = TsDemuxer::new(data).expect("parse");
    assert_eq!(ts.streams().len(), 40);
    assert!(ts.streams().iter().all(|s| s.codec == Some(CodecId::H264)));
}

#[test]
fn two_sections_packed_in_one_packet() {
    // Two PAT sections back-to-back in a single payload; the parser must
    // consume both from one packet.
    let sec_a = pat_section(&[(1, 0x1000)]);
    let sec_b = pat_section(&[(2, 0x1002)]);
    let mut payload = vec![0x00u8]; // pointer_field
    payload.extend_from_slice(&sec_a);
    payload.extend_from_slice(&sec_b);

    let mut cc = 0u8;
    let data = ts_packet(0x0000, true, &payload, &mut cc);

    let ts = TsDemuxer::new(data).expect("parse");
    let nums: Vec<u16> = ts.programs().iter().map(|p| p.program_number).collect();
    assert_eq!(nums, vec![1, 2]);
}

#[test]
fn null_and_scrambled_packets_are_ignored() {
    let mut data = Vec::new();
    let mut cc = 0u8;
    data.extend_from_slice(&psi_section_packets(
        0x0000,
        &pat_section(&[(1, 0x1000)]),
        &mut cc,
    ));
    data.extend_from_slice(&psi_section_packets(
        0x1000,
        &pmt_section(1, 0x0100, &[(0x1B, 0x0100)]),
        &mut cc,
    ));
    // Null packet with junk payload.
    data.extend_from_slice(&ts_packet(0x1FFF, false, &[0xAA; 100], &mut cc));
    // Video packet with transport_scrambling_control = 2.
    let mut scrambled = ts_packet(0x0100, true, &[0x00, 0x00, 0x01, 0xE0, 0x00, 0x00], &mut cc);
    scrambled[3] |= 0x80; // tsc = '10'
    data.extend_from_slice(&scrambled);

    let mut ts = TsDemuxer::new(data).expect("parse");
    assert_eq!(ts.streams().len(), 1);
    assert!(ts.read_packet().unwrap().is_none());
}

#[test]
fn unknown_pes_pid_still_demuxes() {
    // A PES stream with no PAT/PMT at all (raw SP-style segment).
    let data = pes_packets(0x0200, 0xE0, 12_345, true, true, &[0u8; 32]);
    let mut ts = TsDemuxer::new(data).expect("parse");
    let pkt = ts.read_packet().unwrap().expect("packet");
    assert_eq!(pkt.stream_index, 0x0200);
    assert_eq!(pkt.pts.value, 12_345);
    assert_eq!(pkt.pts.time_base, (1, 90_000));
    assert!(pkt.is_key_frame);
}

// ── ffmpeg-gated real-clip test ──────────────────────────────────────────────

fn ffprobe_available() -> bool {
    Command::new("ffprobe")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Generate a short H.264 mpegts clip and return (path, ffprobe packet
/// count, ffprobe first video PTS in 90 kHz ticks).
fn generate_reference_clip() -> Option<(PathBuf, usize, i64)> {
    let dir = std::env::temp_dir().join("tpt-kinetix-ts-test");
    let _ = std::fs::create_dir_all(&dir);
    let clip = dir.join("ref.ts");

    let ok = Command::new("ffmpeg")
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=1:size=128x96:rate=10",
            "-c:v",
            "libx264",
            "-profile:v",
            "baseline",
            "-pix_fmt",
            "yuv420p",
            "-f",
            "mpegts",
        ])
        .arg(&clip)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok {
        return None;
    }

    // Video packet count.
    let count = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-count_packets",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=nb_read_packets",
            "-of",
            "csv=p=0",
        ])
        .arg(&clip)
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let count: usize = String::from_utf8_lossy(&count.stdout).trim().parse().ok()?;

    // Per-packet PTS values (90 kHz).
    let pts = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "packet=pts",
            "-of",
            "csv=p=0",
        ])
        .arg(&clip)
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let first_pts: i64 = String::from_utf8_lossy(&pts.stdout)
        .lines()
        .next()?
        .trim()
        .parse()
        .ok()?;

    Some((clip, count, first_pts))
}

#[test]
fn real_ffmpeg_h264_clip_matches_ffprobe() {
    if !ffprobe_available() {
        eprintln!("skipping: ffprobe not available");
        return;
    }
    let Some((clip, expected_packets, expected_first_pts)) = generate_reference_clip() else {
        eprintln!("skipping: could not generate reference clip (ffmpeg missing or failed)");
        return;
    };

    let data = std::fs::read(&clip).expect("read clip");
    let mut ts = TsDemuxer::new(data).expect("parse real mpegts");

    // Tables: one program, one H.264 video stream with PCR.
    assert_eq!(ts.programs().len(), 1);
    let streams = ts.streams();
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[0].codec, Some(CodecId::H264));
    assert_eq!(streams[0].media_type, MediaType::Video);
    assert!(streams[0].is_pcr);
    let video_pid = streams[0].pid;
    assert!(ts.pcr_90khz(video_pid).is_some());

    // Packets: count and first PTS must match ffprobe's view exactly.
    let mut packets = Vec::new();
    while let Some(pkt) = ts.read_packet().unwrap() {
        assert_eq!(pkt.stream_index, u32::from(video_pid));
        packets.push(pkt);
    }
    assert_eq!(packets.len(), expected_packets);
    assert_eq!(packets[0].pts.value, expected_first_pts);

    // Every packet is Annex-B framed; PTS is monotonically non-decreasing
    // (baseline profile → no B-frames → PTS order == demux order).
    let mut prev = i64::MIN;
    for pkt in &packets {
        assert!(pkt.data.len() > 4);
        assert_eq!(&pkt.data[..4], &[0x00, 0x00, 0x00, 0x01]);
        assert!(pkt.pts.value >= prev);
        prev = pkt.pts.value;
    }
    // The clip's first frame is an IDR: the demuxer must flag it as a
    // key frame (ffmpeg sets the random-access indicator on it).
    assert!(packets[0].is_key_frame);
}
