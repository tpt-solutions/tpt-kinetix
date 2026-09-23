//! Throwaway: decode exactly the `testsrc_160x90` entry from `av1_inter_corpus()`
//! (the same bytes the real conformance test uses) and dump per-decoded-frame
//! grids via `KINETIX_AV1_DUMP_GRID`, to inspect the hidden alt-ref frame's
//! own reconstruction (never independently checked against dav1d since it's
//! never shown). Delete before final commit.

use tpt_kinetix_av1::Av1Decoder;
use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};
use tpt_kinetix_test_utils::synthetic::av1_inter_corpus;

#[test]
fn dbg_av1_160_grid_dump() {
    let corpus = av1_inter_corpus();
    let Some(entry) = corpus.iter().find(|e| e.label == "testsrc_160x90") else {
        eprintln!("skipping: testsrc_160x90 not in corpus (no ffmpeg?)");
        return;
    };

    let spans = obu_spans(&entry.obu);
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
        tu_spans.push((cs, entry.obu.len()));
    }

    let mut dec = Av1Decoder::new();
    let mut shown = 0usize;
    for (i, (start, end)) in tu_spans.iter().enumerate() {
        let mut data = Vec::new();
        if let Some((ss, se)) = seq_span {
            if !(*start <= ss && ss < *end) {
                data.extend_from_slice(&entry.obu[ss..se]);
            }
        }
        data.extend_from_slice(&entry.obu[*start..*end]);
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
