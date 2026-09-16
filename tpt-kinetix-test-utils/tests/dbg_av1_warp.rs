//! Throwaway per-frame diffmap for the 64x64 warp-affine regression clip.
//! `KINETIX_AV1_SAVE_OBU=<path>` saves the raw OBU stream (for dav1d CLI
//! stage isolation via `--inloopfilters`); `KINETIX_AV1_SAVE_OUT=<path>`
//! appends Kinetix's shown frames as raw yuv420p. Delete before final commit.

use tpt_kinetix_av1::Av1Decoder;
use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};
use tpt_kinetix_test_utils::{
    reference::{dav1d_available, decode_av1_obu_with_dav1d},
    synthetic::av1_multiframe_obu,
};

#[test]
fn dbg_av1_warp_diffmap() {
    if !dav1d_available() {
        eprintln!("skipping: dav1d not available");
        return;
    }
    const W: usize = 64;
    const H: usize = 64;
    let Some(obu) = av1_multiframe_obu(W as u32, H as u32, 6) else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    if let Ok(path) = std::env::var("KINETIX_AV1_SAVE_OBU") {
        std::fs::write(&path, &obu).expect("write obu");
        eprintln!("saved OBU to {path}");
    }
    let ref_frames = decode_av1_obu_with_dav1d(&obu, W as u32, H as u32).expect("dav1d");

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
    let mut kinetix_frames = Vec::new();
    for (i, (start, end)) in tu_spans.iter().enumerate() {
        let mut data = Vec::new();
        if let Some((ss, se)) = seq_span {
            if !(*start <= ss && ss < *end) {
                data.extend_from_slice(&obu[ss..se]);
            }
        }
        data.extend_from_slice(&obu[*start..*end]);
        let packet = Packet {
            pts: Timestamp::new(i as i64, (1, 90_000)),
            dts: Timestamp::new(i as i64, (1, 90_000)),
            data,
            stream_index: 0,
            is_key_frame: i == 0,
        };
        match dec.decode(&packet) {
            Ok(Some(f)) => kinetix_frames.push(f),
            Ok(None) => {
                eprintln!("TU {i}: no frame");
                break;
            }
            Err(e) => {
                eprintln!("TU {i}: errored: {e}");
                break;
            }
        }
    }
    if let Ok(path) = std::env::var("KINETIX_AV1_SAVE_OUT") {
        use std::io::Write;
        let mut out = std::fs::File::create(&path).expect("create save-out");
        for f in &kinetix_frames {
            out.write_all(&f.data).expect("write save-out");
        }
    }

    for (i, (kf, rf)) in kinetix_frames.iter().zip(ref_frames.iter()).enumerate() {
        let stride = W;
        let mut total = 0u64;
        let mut n = 0u64;
        let mut first_bad: Option<(usize, usize, i32, i32)> = None;
        for y in 0..H {
            for x in 0..W {
                let d = kf.data[y * stride + x] as i32 - rf.data[y * stride + x] as i32;
                if d != 0 {
                    total += d.unsigned_abs() as u64;
                    n += 1;
                    if first_bad.is_none() {
                        first_bad = Some((
                            x,
                            y,
                            kf.data[y * stride + x] as i32,
                            rf.data[y * stride + x] as i32,
                        ));
                    }
                }
            }
        }
        eprintln!(
            "frame {i}: {n} diff samples, total |diff| = {total}, first wrong = {first_bad:?}"
        );
        if i == 1 {
            eprintln!("=== frame 1 heatmap ===");
            for by in (0..H).step_by(8) {
                let mut row = String::new();
                for bx in (0..W).step_by(8) {
                    let mut sum = 0u32;
                    for y in by..by + 8 {
                        for x in bx..bx + 8 {
                            sum += (kf.data[y * stride + x] as i32
                                - rf.data[y * stride + x] as i32)
                                .unsigned_abs();
                        }
                    }
                    row.push_str(&format!("{:3} ", sum.min(999)));
                }
                eprintln!("y={by:3}: {row}");
            }
        }
    }
}

/// Parse OBU spans (type, start, end) from a raw OBU stream (same logic as
/// `conformance.rs`'s local helper).
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
