//! Throwaway chroma-desync probe for the 128x96 testsrc inter clip.
//! With `KINETIX_AV1_CHROMA_OBU=<path>` the generated OBU stream is saved
//! (for dav1d CLI stage isolation / internal dumps); with
//! `KINETIX_AV1_DUMP_FRAMES` the decoder dumps every decoded frame. Delete
//! before final commit.

use tpt_kinetix_av1::Av1Decoder;
use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};
use tpt_kinetix_test_utils::{reference::dav1d_available, synthetic::av1_multiframe_obu};

#[test]
fn dbg_av1_chroma_dump() {
    if !dav1d_available() {
        eprintln!("skipping: dav1d not available");
        return;
    }
    let width: u32 = std::env::var("KINETIX_AV1_CHROMA_W")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(128);
    let height: u32 = std::env::var("KINETIX_AV1_CHROMA_H")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(96);
    let frames: u32 = std::env::var("KINETIX_AV1_CHROMA_N")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8);
    // Pin the bitstream: when KINETIX_AV1_CHROMA_OBU points at an existing
    // file it is REUSED (ffmpeg's av1 encoder is nondeterministic across
    // processes for some sizes, and dav1d must decode the exact same bytes
    // kinetix does); otherwise the freshly encoded stream is saved there.
    let obu = if let Ok(path) = std::env::var("KINETIX_AV1_CHROMA_OBU") {
        if std::path::Path::new(&path).exists() {
            let obu = std::fs::read(&path).expect("read obu");
            eprintln!("loaded pinned OBU ({}) from {path}", obu.len());
            obu
        } else {
            let Some(obu) = av1_multiframe_obu(width, height, frames) else {
                eprintln!("skipping: no ffmpeg");
                return;
            };
            std::fs::write(&path, &obu).expect("write obu");
            eprintln!("saved OBU ({}) to {path}", obu.len());
            obu
        }
    } else {
        let Some(obu) = av1_multiframe_obu(width, height, frames) else {
            eprintln!("skipping: no ffmpeg");
            return;
        };
        obu
    };

    // Split into temporal units (TD OBU, type 2) and feed one packet per TU,
    // prefixing the sequence header when it isn't part of the TU.
    let spans = obu_spans(&obu);
    let seq_span: Option<(usize, usize)> = spans.iter().find(|s| s.0 == 1).map(|s| (s.1, s.2));
    let mut tu_spans: Vec<(usize, usize)> = Vec::new();
    let mut cur: Option<usize> = None;
    for (t, s, _e) in &spans {
        if *t == 2 {
            if let Some(cs) = cur.take() {
                tu_spans.push((cs, *s));
            }
            cur = Some(*s);
        }
    }
    if let Some(cs) = cur {
        tu_spans.push((cs, obu.len()));
    }
    if tu_spans.is_empty() {
        tu_spans = spans
            .iter()
            .filter(|s| s.0 == 6)
            .map(|s| (s.1, s.2))
            .collect();
    }

    let mut dec = Av1Decoder::new();
    let mut shown = 0usize;
    for (i, (start, end)) in tu_spans.iter().enumerate() {
        let mut data = Vec::new();
        if let Some((ss, se)) = seq_span {
            if i > 0 {
                data.extend_from_slice(&obu[ss..se]);
            }
        }
        data.extend_from_slice(&obu[*start..*end]);
        let pk = Packet {
            pts: Timestamp::new(i as i64, (1, 90_000)),
            dts: Timestamp::new(i as i64, (1, 90_000)),
            data,
            stream_index: 0,
            is_key_frame: i == 0,
        };
        if dec.decode(&pk).expect("decode").is_some() {
            shown += 1;
        }
    }
    eprintln!("decoded {shown} shown frames");
}

/// Parse OBU spans (type, start, end) from a raw OBU stream (same logic as
/// `dbg_av1_warp` / `conformance.rs`'s local helper).
fn obu_spans(data: &[u8]) -> Vec<(u8, usize, usize)> {
    let mut spans = Vec::new();
    let mut pos = 0usize;
    while pos < data.len() {
        let header = data[pos];
        let obu_type = (header >> 3) & 0x0F;
        let has_size = (header >> 1) & 1 != 0;
        let mut off = pos + 1;
        if header & 0x80 != 0 {
            break;
        }
        let mut payload_len = 0usize;
        if has_size {
            let mut shift = 0u32;
            loop {
                if off >= data.len() {
                    return spans;
                }
                let b = data[off];
                off += 1;
                payload_len |= ((b & 0x7F) as usize) << shift;
                shift += 7;
                if b & 0x80 == 0 {
                    break;
                }
            }
        } else {
            payload_len = data.len() - off;
        }
        let end = (off + payload_len).min(data.len());
        spans.push((obu_type, pos, end));
        pos = end;
    }
    spans
}
