// Dump a corpus entry's tile-group tile-0 payload + frame/sequence params +
// Kinetix's own symbol trace + block markers, as JSON, so the independent
// Python oracle (tools/av1_oracle/decode_intra.py) can re-decode the tile from
// scratch and diff its per-symbol trace against Kinetix's — the Part 1 oracle
// wiring (todo-av1.md Phase G.0). No OBU parsing or CDF serialization is needed
// in Python: the tile payload is fed to the Python SymbolDecoder at byte 0 and
// re-adapts exactly as Kinetix does, so a trace mismatch is a genuine bug.
//
// Run (single label):
//   cargo run -q -p tpt-kinetix-test-utils --example dump_tile_trace mandelbrot
// Run (all entries, JSONL):
//   cargo run -q -p tpt-kinetix-test-utils --example dump_tile_trace --all
use std::io::Write;

use tpt_kinetix_av1::entropy::{
    enable_symbol_trace, take_block_markers, take_symbol_trace,
};
use tpt_kinetix_av1::Av1Decoder;
use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};
use tpt_kinetix_test_utils::synthetic::av1_intra_corpus;

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// JSON-escape a string (quotes, backslashes, control chars).
fn jstr(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn dump_one(label: &str, width: u32, height: u32, obu: &[u8]) {
    let mut dec = Av1Decoder::new();
    enable_symbol_trace();
    let packet = Packet {
        pts: Timestamp::NONE,
        dts: Timestamp::NONE,
        data: obu.to_vec(),
        stream_index: 0,
        is_key_frame: true,
    };
    let _frame = match dec.decode(&packet) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("decode failed for {label}: {e}");
            let _ = take_symbol_trace();
            let _ = take_block_markers();
            return;
        }
    };
    let trace = take_symbol_trace();
    let markers = take_block_markers();
    let fh = match dec.last_frame_header() {
        Some(f) => f,
        None => {
            eprintln!("no frame header for {label}");
            return;
        }
    };
    let seq = dec.sequence_header();

    let payload = dec
        .tile_data()
        .first()
        .map(|t| t.payload.clone())
        .unwrap_or_default();

    // Trace entries: [n_symbols, value, bit_pos_before, bit_pos_after, location].
    let trace_json: Vec<String> = trace
        .iter()
        .map(|e| {
            format!(
                "[{},{},{},{},\"{}\"]",
                e.n_symbols, e.value, e.bit_pos_before, e.bit_pos_after, jstr(&e.location.to_string())
            )
        })
        .collect();
    let markers_json: Vec<String> = markers
        .iter()
        .map(|m| format!("[{},\"{}\"]", m.trace_seq, m.label.replace('"', "'")))
        .collect();

    let seg_feature_skip = fh.seg_feature_enabled.first().copied().unwrap_or(false);
    let use_128 = fh.use_128x128_superblock;

    println!(
        "{{\"label\":\"{label}\",\"width\":{width},\"height\":{height},\"tile_payload_hex\":\"{}\",\
\"frame\":{{\"frame_type\":{},\"subsampling_x\":{},\"subsampling_y\":{},\"bit_depth\":{},\
\"use_128x128_superblock\":{},\"allow_screen_content_tools\":{},\"allow_intrabc\":{},\
\"reduced_tx_set\":{},\"tx_mode_select\":{},\"frame_is_intra\":{},\"lossless\":{},\
\"coded_lossless\":{},\"segmentation_enabled\":{},\"seg_feature_skip\":{},\"delta_q_present\":{},\
\"base_q_idx\":{},\"enable_filter_intra\":{}}},\
\"markers\":[{}],\"trace\":[{}]}}",
        hex(&payload),
        fh.frame_type as u32,
        fh.subsampling_x as u32,
        fh.subsampling_y as u32,
        fh.bit_depth,
        use_128 as u32,
        fh.allow_screen_content_tools as u32,
        fh.allow_intrabc as u32,
        fh.reduced_tx_set as u32,
        fh.tx_mode_select as u32,
        fh.frame_is_intra as u32,
        fh.lossless as u32,
        fh.coded_lossless as u32,
        fh.segmentation_enabled as u32,
        seg_feature_skip as u32,
        fh.delta_q_present as u32,
        fh.base_q_idx,
        seq.map(|s| s.enable_filter_intra as u32).unwrap_or(0),
        markers_json.join(","),
        trace_json.join(","),
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let corpus = av1_intra_corpus();
    if corpus.is_empty() {
        eprintln!("no corpus (ffmpeg missing?)");
        return;
    }
    let all = args.first().map(String::as_str) == Some("--all");
    let mut out = std::io::stdout().lock();
    for e in &corpus {
        if all || args.first().map(String::as_str) == Some(e.label) {
            dump_one(e.label, e.width, e.height, &e.obu);
            let _ = out.flush();
        }
    }
}
