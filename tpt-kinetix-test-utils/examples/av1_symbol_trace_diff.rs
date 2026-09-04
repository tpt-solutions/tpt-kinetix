//! AV1 differential-trace harness (Phase G.0 tooling, 2026-08-20).
//!
//! Replaces the four one-off `dbg_av1_*.rs` test files (`dbg_av1_smptebars`,
//! `dbg_av1_testsrc128`, `dbg_av1_mandelbrot128`, `dbg_av1_mandelbrot_diffmap`)
//! that each hand-rolled "decode with dav1d reference + decode with Kinetix +
//! compare a crop" and got abandoned once their specific crop stopped being
//! useful. This is the reusable version: point it at any corpus entry (or
//! extend `corpus_entry` below to accept an arbitrary OBU file) and it:
//!
//! 1. Decodes with `dav1d` (via `ffmpeg -c:v libdav1d`, or a standalone
//!    `dav1d` binary if present) as ground truth.
//! 2. Decodes with `Av1Decoder`, with `tpt_kinetix_av1::entropy`'s symbol
//!    trace enabled — every `read_symbol`/`read_bool`/`read_literal` call is
//!    captured with its bit position, alphabet size, decoded value, and
//!    *source call site* (via `#[track_caller]`, so no per-call-site edits
//!    were needed in `coeff.rs`/`reconstruct/*.rs`), plus block-level
//!    markers (`mode_info mi=(...)`, `coeffs plane=... px=(...)`) pushed
//!    from `decode_intra_block`/`reconstruct_tx_block`.
//! 3. Diffs the two decoded luma/chroma planes and finds the first pixel
//!    whose absolute difference exceeds a small threshold.
//! 4. Also re-decodes with `KINETIX_AV1_NOFILTER=1` (disables deblock/CDEF)
//!    to report whether the divergence survives with post-filters off, which
//!    tells you whether to look in `reconstruct/` (intra prediction /
//!    transform / coefficient decode) or `loop_filter.rs`/CDEF.
//! 5. Reports the first-diverging pixel's containing block (via the nearest
//!    preceding block marker) and prints the last N symbol-trace entries
//!    leading up to it — source location, alphabet size, value — so a human
//!    can jump straight to the relevant code instead of re-deriving where to
//!    look from spec text.
//!
//! Run: `cargo run -p tpt-kinetix-test-utils --example av1_symbol_trace_diff -- [label]`
//! (or `just av1-trace-diff [label]`). `label` defaults to `mandelbrot`; pass
//! `--all` to run every entry in [`av1_intra_corpus`] and print a one-line
//! summary per entry.
//!
//! # Known limitations (see `todo-av1.md` Phase G.0 session note for detail)
//!
//! * Pixel-level diffing only covers the *final* decoded frame plus the
//!   NOFILTER bracket above; there is no public API yet to pull pre-filter /
//!   post-deblock / post-CDEF / post-restoration intermediate buffers
//!   separately (restoration isn't implemented at all yet), so the "walk the
//!   pipeline stage by stage" ask from the Phase G.0 spec is only partially
//!   met — extending `Av1Decoder`/`TileDecodeState` to optionally snapshot
//!   each stage (mirroring `tpt-kinetix-test-utils::trace_dump::MapTracer`'s
//!   `DecodeTracer` pattern already used for H.264) is the natural next step.
//! * The symbol trace's "context used" is only indirect (source file:line,
//!   not the numeric CDF context index) — a human still has to open that
//!   line to see which context table/index was selected. Threading the
//!   context value itself through would need touching every `read_*_cdf`
//!   helper's signature; deferred as a possible follow-up.
//! * **No independent symbol-level oracle is wired in yet** (the Phase G.0
//!   spec's "Part 1"). `ffmpeg -v trace`/its AVOptions expose no per-symbol
//!   AV1 decode log (checked this session: `ffmpeg -h decoder=av1` only
//!   lists `operating_point`; no verbose/trace flag reaches per-symbol
//!   `libdav1d` internals through the AVOption surface), and building
//!   `dav1d` from source with a debug/trace feature flag was judged
//!   impractical within this session's time budget. The documented fallback
//!   — extending `coeff.rs`'s existing Python-cross-checked `coeffs()`
//!   oracle test methodology to cover the full `intra_frame_mode_info()` +
//!   `coeffs()` sequence driven off real captured bitstream bytes — was
//!   *not* attempted this session either (that existing oracle still only
//!   runs against synthetic `ramp()` buffers, not real bytes); it remains
//!   open for a future session. Without it, this harness's divergence
//!   reports are only as good as "kinetix disagrees with dav1d starting
//!   here", not "here is the exact symbol read that is numerically wrong
//!   per an independent re-derivation" — still a large time savings over
//!   the previous fully-manual process, but not the full two-part ask.

use std::collections::HashMap;

use tpt_kinetix_av1::entropy::{
    enable_symbol_trace, symbol_trace_enabled, take_block_markers, take_symbol_trace, BlockMarker,
    SymbolTraceEntry,
};
use tpt_kinetix_av1::Av1Decoder;
use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};
use tpt_kinetix_test_utils::{reference::decode_av1_obu_with_dav1d, synthetic::av1_intra_corpus};

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let n = a.len().min(b.len());
    if n == 0 {
        return 0.0;
    }
    let mut sse = 0.0f64;
    for i in 0..n {
        let d = a[i] as f64 - b[i] as f64;
        sse += d * d;
    }
    if sse == 0.0 {
        return 99.0;
    }
    10.0 * (255.0f64 * 255.0 / (sse / n as f64)).log10()
}

/// Decode `obu` with `Av1Decoder`, optionally with the post-filter chain
/// disabled (`KINETIX_AV1_NOFILTER`), returning the raw frame data.
fn decode_kinetix(obu: &[u8], nofilter: bool) -> Result<Vec<u8>, String> {
    if nofilter {
        std::env::set_var("KINETIX_AV1_NOFILTER", "1");
    } else {
        std::env::remove_var("KINETIX_AV1_NOFILTER");
    }
    let mut dec = Av1Decoder::new();
    let packet = Packet {
        pts: Timestamp::NONE,
        dts: Timestamp::NONE,
        data: obu.to_vec(),
        stream_index: 0,
        is_key_frame: true,
    };
    let frame = dec
        .decode(&packet)
        .map_err(|e| format!("kinetix decode error: {e}"))?
        .ok_or_else(|| "kinetix produced no frame".to_string())?;
    Ok(frame.data)
}

/// Find the first raster-order pixel (across Y then U then V, each plane
/// checked independently against `w`/`h` luma geometry) whose absolute diff
/// exceeds `threshold`. Returns `(plane_name, x, y, got, want)`.
fn first_divergence(
    got: &[u8],
    want: &[u8],
    w: usize,
    h: usize,
    threshold: i32,
) -> Option<(&'static str, usize, usize, u8, u8)> {
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    let planes: [(&str, usize, usize, usize); 3] = [
        ("Y", w, h, 0),
        ("U", cw, ch, w * h),
        ("V", cw, ch, w * h + cw * ch),
    ];
    for (name, pw, ph, off) in planes {
        for y in 0..ph {
            for x in 0..pw {
                let idx = off + y * pw + x;
                if idx >= got.len() || idx >= want.len() {
                    continue;
                }
                let a = got[idx] as i32;
                let b = want[idx] as i32;
                if (a - b).abs() > threshold {
                    return Some((name, x, y, got[idx], want[idx]));
                }
            }
        }
    }
    None
}

/// Run the full trace + diff for one corpus entry; prints a report.
fn run_one(label: &str, width: u32, height: u32, obu: &[u8]) {
    println!("\n=== {label} ({width}x{height}) ===");

    if let Ok(dir) = std::env::var("KINETIX_AV1_DUMP_OBU") {
        let path = format!("{dir}/{label}.obu");
        let _ = std::fs::write(&path, obu);
        println!("  dumped raw OBU to {path}");
    }

    let ref_frames = match decode_av1_obu_with_dav1d(obu, width, height) {
        Ok(f) => f,
        Err(e) => {
            println!("  SKIP: dav1d reference decode unavailable/failed: {e}");
            return;
        }
    };
    let ref_data = &ref_frames[0].data;

    // Pass 1: with the symbol trace enabled, decode once so we capture the
    // trace + block markers alongside the actual output used for diffing.
    enable_symbol_trace();
    let got = match decode_kinetix(obu, false) {
        Ok(d) => d,
        Err(e) => {
            println!("  Kinetix decode FAILED: {e}");
            let _ = take_symbol_trace();
            let _ = take_block_markers();
            return;
        }
    };
    let trace = take_symbol_trace();
    let markers = take_block_markers();
    debug_assert!(
        !symbol_trace_enabled(),
        "take_symbol_trace should clear the session"
    );
    if std::env::var("KINETIX_AV1_DBG_FIRST_READS").is_ok() {
        println!("  First reads (mode-parsing sequence before first coeffs()):");
        for e in trace.iter().take(20) {
            println!(
                "    #{:>4} n_symbols={:<3} value={:<6} bits=[{},{}) at {}",
                e.seq, e.n_symbols, e.value, e.bit_pos_before, e.bit_pos_after, e.location
            );
        }
        for m in markers.iter().take(6) {
            println!("    marker[{}] {}", m.trace_seq, m.label);
        }
    }

    let w = width as usize;
    let h = height as usize;
    if std::env::var("KINETIX_AV1_DBG_ROWS").is_ok() {
        let (c0, c1) = std::env::var("KINETIX_AV1_DBG_COLS")
            .ok()
            .and_then(|s| {
                let mut it = s.split(',');
                Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
            })
            .unwrap_or((0usize, 24usize));
        let (r0, r1) = std::env::var("KINETIX_AV1_DBG_RROWS")
            .ok()
            .and_then(|s| {
                let mut it = s.split(',');
                Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
            })
            .unwrap_or((0usize, 4usize));
        for row in r0..r1.min(h) {
            let a = row * w + c0.min(w);
            let b = row * w + c1.min(w);
            let g: Vec<u8> = got[a..b].to_vec();
            let r: Vec<u8> = ref_data[a..b].to_vec();
            println!("  row{row} c{c0}..{c1} kinetix={g:?}");
            println!("  row{row} c{c0}..{c1} dav1d  ={r:?}");
        }
    }
    let y_n = w * h;
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    let c_n = cw * ch;
    let psnr_y = psnr(
        &got[..y_n.min(got.len())],
        &ref_data[..y_n.min(ref_data.len())],
    );
    let psnr_u = psnr(
        &got[y_n..(y_n + c_n).min(got.len())],
        &ref_data[y_n..(y_n + c_n).min(ref_data.len())],
    );
    let psnr_v = psnr(
        &got[(y_n + c_n)..(y_n + 2 * c_n).min(got.len())],
        &ref_data[(y_n + c_n)..(y_n + 2 * c_n).min(ref_data.len())],
    );
    println!("  PSNR Y/U/V = {psnr_y:.2}/{psnr_u:.2}/{psnr_v:.2} dB  (symbol trace: {} reads, {} block markers)", trace.len(), markers.len());

    let div_threshold: i32 = std::env::var("KINETIX_AV1_DIV_THRESHOLD")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);
    let Some((plane, x, y, got_v, want_v)) = first_divergence(&got, ref_data, w, h, div_threshold)
    else {
        println!("  No pixel diverges by more than 3 (effectively pixel-exact at this threshold).");
        return;
    };
    println!(
        "  First divergence: plane {plane} px=({x},{y}) kinetix={got_v} dav1d={want_v} (delta={})",
        got_v as i32 - want_v as i32
    );

    // Bracket: does the divergence survive with post-filters disabled?
    // Always capture the symbol trace on this pass so we can show block
    // context for whichever pixel diverges pre-filter.
    enable_symbol_trace();
    match decode_kinetix(obu, true) {
        Ok(nofilter) => {
            let nofilter_trace = take_symbol_trace();
            let nofilter_markers = take_block_markers();
            if std::env::var("KINETIX_AV1_DBG_ROWS").is_ok() {
                let (r0, r1) = std::env::var("KINETIX_AV1_DBG_RROWS")
                    .ok()
                    .and_then(|s| {
                        let mut it = s.split(',');
                        Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
                    })
                    .unwrap_or((0usize, 4usize));
                let (c0, c1) = std::env::var("KINETIX_AV1_DBG_COLS")
                    .ok()
                    .and_then(|s| {
                        let mut it = s.split(',');
                        Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
                    })
                    .unwrap_or((0usize, 24usize));
                for row in r0..r1.min(h) {
                    let a = row * w + c0.min(w);
                    let b = row * w + c1.min(w);
                    println!("  NF row{row} c{c0}..{c1} kinetix={:?}", &nofilter[a..b]);
                    println!("  NF row{row} c{c0}..{c1} dav1d  ={:?}", &ref_data[a..b]);
                }
            }
            if let Some((p2, x2, y2, g2, w2)) =
                first_divergence(&nofilter, ref_data, w, h, div_threshold)
            {
                if (p2, x2, y2) == (plane, x, y) {
                    println!(
                        "  With KINETIX_AV1_NOFILTER=1: same first-divergence pixel ({p2},{x2},{y2}) kinetix={g2} dav1d={w2} -> deblock/CDEF is NOT the cause; look in reconstruct/ (prediction/transform/coeffs)."
                    );
                    print_trace_context(plane, x, y, &trace, &markers);
                } else {
                    println!(
                        "  With KINETIX_AV1_NOFILTER=1: first divergence MOVED to plane {p2} px=({x2},{y2}) kinetix={g2} dav1d={w2} -> filters propagate a pre-filter bug from here to {plane}/({x},{y})."
                    );
                    println!("  Block context for NOFILTER-diverging pixel (pre-filter reconstruction bug):");
                    print_trace_context(p2, x2, y2, &nofilter_trace, &nofilter_markers);
                    println!("  Block context for original filtered divergence {plane}/({x},{y}):");
                    print_trace_context(plane, x, y, &trace, &markers);
                }
            } else {
                println!("  With KINETIX_AV1_NOFILTER=1: no divergence found within threshold -> the post-filter chain is implicated in the original divergence.");
            }
        }
        Err(e) => {
            let _ = take_symbol_trace();
            let _ = take_block_markers();
            println!("  KINETIX_AV1_NOFILTER decode failed: {e}");
        }
    }
    std::env::remove_var("KINETIX_AV1_NOFILTER");
}

/// Print the nearest preceding block marker and surrounding symbol trace entries
/// for a diverging pixel at `(x, y)` in `plane`.
fn print_trace_context(
    plane: &str,
    x: usize,
    y: usize,
    trace: &[SymbolTraceEntry],
    markers: &[BlockMarker],
) {
    let want_plane: Option<u32> = match plane {
        "Y" => Some(0),
        "U" => Some(1),
        "V" => Some(2),
        _ => None,
    };
    let mut best: Option<&BlockMarker> = None;
    for m in markers {
        let Some(px) = parse_px(&m.label) else {
            continue;
        };
        let marker_plane = parse_plane(&m.label);
        if let (Some(wp), Some(mp)) = (want_plane, marker_plane) {
            if wp != mp {
                continue;
            }
        }
        let (tw, th) = parse_tx_size(&m.label).unwrap_or((usize::MAX, usize::MAX));
        let in_range = if tw != usize::MAX {
            px.0 <= x && x < px.0 + tw && px.1 <= y && y < px.1 + th
        } else {
            px.0 <= x && px.1 <= y
        };
        if in_range {
            best = Some(m);
        }
    }
    if let Some(m) = best {
        println!(
            "  Nearest preceding block marker: [{}] \"{}\"",
            m.trace_seq, m.label
        );
        let ctx_start = m.trace_seq.saturating_sub(2);
        let ctx_end = (m.trace_seq + 50).min(trace.len());
        println!("  Symbol trace around that marker (seq {ctx_start}..{ctx_end}):");
        for e in &trace[ctx_start..ctx_end] {
            println!(
                "    #{:>4} n_symbols={:<3} value={:<6} bits=[{},{}) at {}",
                e.seq, e.n_symbols, e.value, e.bit_pos_before, e.bit_pos_after, e.location
            );
        }
    } else {
        println!(
            "  No block marker found before the divergence point (unexpected — check marker wiring)."
        );
    }
}

fn parse_px(label: &str) -> Option<(usize, usize)> {
    let idx = label.find("px=(")?;
    let rest = &label[idx + 4..];
    let end = rest.find(')')?;
    let (xs, ys) = rest[..end].split_once(',')?;
    Some((xs.trim().parse().ok()?, ys.trim().parse().ok()?))
}

/// Parse `plane=N` out of a `coeffs` block-marker label; `mode_info` markers
/// have no plane tag and return `None` (treated as "covers all planes").
fn parse_plane(label: &str) -> Option<u32> {
    let idx = label.find("plane=")?;
    let rest = &label[idx + 6..];
    let end = rest.find(' ').unwrap_or(rest.len());
    rest[..end].trim().parse().ok()
}

/// Parse `tx=WxH` out of a `coeffs` block-marker label.
fn parse_tx_size(label: &str) -> Option<(usize, usize)> {
    let idx = label.find("tx=")?;
    let rest = &label[idx + 3..];
    let end = rest.find(' ').unwrap_or(rest.len());
    let (ws, hs) = rest[..end].split_once('x')?;
    Some((ws.trim().parse().ok()?, hs.trim().parse().ok()?))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let corpus = av1_intra_corpus();
    if corpus.is_empty() {
        println!("no corpus entries produced (ffmpeg missing or encode failed) — nothing to do");
        return;
    }
    let by_label: HashMap<&str, _> = corpus.iter().map(|e| (e.label, e)).collect();

    if args.first().map(String::as_str) == Some("--all") {
        for e in &corpus {
            run_one(e.label, e.width, e.height, &e.obu);
        }
        return;
    }

    let label = args.first().map(String::as_str).unwrap_or("mandelbrot");
    match by_label.get(label) {
        Some(e) => run_one(e.label, e.width, e.height, &e.obu),
        None => {
            println!(
                "no corpus entry named '{label}'; available: {:?}",
                corpus.iter().map(|e| e.label).collect::<Vec<_>>()
            );
        }
    }
}
