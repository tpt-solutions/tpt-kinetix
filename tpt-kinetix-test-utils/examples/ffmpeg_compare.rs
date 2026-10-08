//! Kinetix vs ffmpeg — decode/encode comparison harness (todo-perf.md Phase 1).
//!
//! For a shared corpus of clips (synthetic `testsrc` clips encoded by ffmpeg's
//! libaom/libvpx/libx264, plus the committed AV1 FATE fixtures when present)
//! this tool:
//!
//! 1. **verifies before timing** — decodes every clip with both Kinetix and the
//!    ffmpeg reference and byte-compares the raw planes; speed ratios are only
//!    reported for clips whose outputs match,
//! 2. times ffmpeg decode via `ffmpeg -benchmark -f null -` (both `-threads 1`,
//!    the apples-to-apples case for Kinetix's single-threaded decoders, and
//!    ffmpeg's default threading), and Kinetix in-process (best of N runs),
//! 3. compares AV1 encoding (Kinetix's `Av1Encoder` vs ffmpeg `libaom-av1`) for
//!    wall time, output size and Y-PSNR on identical raw source frames,
//! 4. compares the original codecs against the closest standard reference
//!    (lossless vs FFV1 / lossless-x264 / PNG / JPEG-LS, screen and lean /
//!    realtime vs x264 and libaom realtime presets) for speed, size and PSNR,
//! 5. times the end-to-end CLI transcode (VP9 MP4 → AV1) against the
//!    equivalent ffmpeg CLI invocation.
//!
//! Results land in `docs/perf/ffmpeg-compare-<label>.json` and in the marked
//! `<!-- ffmpeg-compare -->` section of `docs/PERFORMANCE.md` (which
//! `just bench-baseline` preserves).
//!
//! Usage: `cargo run --release -p tpt-kinetix-test-utils --example ffmpeg_compare
//!         [--decode|--encode-av1|--originals|--e2e] [--label <l>] [--quick]`
//!
//! With no section flag every section runs. `--quick` shrinks the corpus and
//! run counts for iteration. ffmpeg is required; every section degrades to a
//! `skipped` entry when a needed encoder/binary is absent.

use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Instant,
};

use serde_json::{json, Value};
use tpt_kinetix_core::{
    frame::VideoFrame, packet::Packet, pixel_format::PixelFormat, timestamp::Timestamp,
};
use tpt_kinetix_test_utils::{
    pixel_diff::psnr_yuv420p,
    reference::{decode_av1_with_dav1d_auto, ffmpeg_available, split_ivf_frames},
};

// ── Configuration ────────────────────────────────────────────────────────────

/// Synthetic-corpus resolutions and frame counts (frame count chosen so a
/// decode run takes long enough to amortise process start-up).
const CORPUS: [(u32, u32, u32); 3] = [(320, 240, 300), (1280, 720, 180), (1920, 1080, 120)];
/// Encode-comparison resolutions: (width, height, frames).
const ENCODE_AV1: [(u32, u32, u32); 2] = [(320, 240, 120), (1280, 720, 90)];
/// Original-codec comparison geometry.
const ORIGINALS_SIZE: (u32, u32, u32) = (1280, 720, 90);
/// End-to-end transcode inputs (VP9-in-MP4 clips from the corpus).
const E2E: [(u32, u32); 2] = [(320, 240), (1280, 720)];

const CORPUS_DIR: &str = "target/perf-corpus";
const FATE_DIR: &str = "fixtures/av1-fate";

const MARK_START: &str = "<!-- ffmpeg-compare:start -->";
const MARK_END: &str = "<!-- ffmpeg-compare:end -->";

#[derive(Clone, Copy, PartialEq, Eq)]
struct Sections {
    decode: bool,
    encode_av1: bool,
    originals: bool,
    e2e: bool,
}

impl Sections {
    fn all() -> Self {
        Self {
            decode: true,
            encode_av1: true,
            originals: true,
            e2e: true,
        }
    }
}

struct Config {
    label: String,
    quick: bool,
    sections: Sections,
    runs: usize,
    /// Where the marked section is spliced. Defaults to `docs/PERFORMANCE.md`;
    /// verification/iteration runs should redirect this so they cannot clobber
    /// the committed artifacts.
    performance_md: String,
}

// ── Report accumulation ──────────────────────────────────────────────────────

/// Mutable report state: rows are `serde_json` values so the JSON snapshot and
/// the markdown tables are rendered from exactly the same data.
#[derive(Default)]
struct Report {
    env: Value,
    decode: Vec<Value>,
    encode_av1: Vec<Value>,
    originals: Vec<Value>,
    e2e: Vec<Value>,
    skipped: Vec<(String, String)>,
    /// Wall-clock seconds spent by the harness itself, recorded for context.
    elapsed_s: f64,
}

fn main() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg is not available on PATH; the Phase 1 comparison needs it.");
        std::process::exit(1);
    }

    let cfg = parse_args();
    let started = Instant::now();
    let mut report = Report {
        env: probe_env(),
        ..Default::default()
    };

    let corpus = ensure_corpus(&cfg);
    let fate = list_fate_clips();

    if cfg.sections.decode {
        run_decode_section(&cfg, &corpus, &fate, &mut report);
    }
    if cfg.sections.encode_av1 {
        run_encode_av1_section(&cfg, &corpus, &mut report);
    }
    if cfg.sections.originals {
        run_originals_section(&cfg, &corpus.dir, &mut report);
    }
    if cfg.sections.e2e {
        run_e2e_section(&cfg, &corpus, &mut report);
    }

    report.elapsed_s = started.elapsed().as_secs_f64();

    let json_path = format!("docs/perf/ffmpeg-compare-{}.json", cfg.label);
    write_file(&json_path, &render_json(&cfg, &report));
    eprintln!("Wrote {json_path}");
    splice_markdown_section(&cfg.performance_md, &render_markdown(&cfg, &report));
    eprintln!("Updated docs/PERFORMANCE.md ({} s total)", report.elapsed_s);
}

fn parse_args() -> Config {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut cfg = Config {
        label: today(),
        quick: false,
        sections: Sections::all(),
        runs: 3,
        performance_md: "docs/PERFORMANCE.md".to_string(),
    };
    let mut any_section = false;
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--decode" => {
                cfg.sections = Sections {
                    decode: true,
                    encode_av1: false,
                    originals: false,
                    e2e: false,
                };
                any_section = true;
            }
            "--encode-av1" => {
                cfg.sections = Sections {
                    decode: false,
                    encode_av1: true,
                    originals: false,
                    e2e: false,
                };
                any_section = true;
            }
            "--originals" => {
                cfg.sections = Sections {
                    decode: false,
                    encode_av1: false,
                    originals: true,
                    e2e: false,
                };
                any_section = true;
            }
            "--e2e" => {
                cfg.sections = Sections {
                    decode: false,
                    encode_av1: false,
                    originals: false,
                    e2e: true,
                };
                any_section = true;
            }
            "--quick" => cfg.quick = true,
            "--label" => {
                i += 1;
                if let Some(l) = args.get(i) {
                    cfg.label = l.clone();
                }
            }
            other if other.starts_with("--label=") => {
                cfg.label = other.trim_start_matches("--label=").to_string();
            }
            "--performance-md" => {
                i += 1;
                if let Some(p) = args.get(i) {
                    cfg.performance_md = p.clone();
                }
            }
            other if other.starts_with("--performance-md=") => {
                cfg.performance_md = other.trim_start_matches("--performance-md=").to_string();
            }
            _ => {}
        }
        i += 1;
    }
    if any_section {
        // Individual sections are for iteration; keep them fast.
        cfg.runs = cfg.runs.min(2);
    }
    if cfg.quick {
        cfg.runs = 2;
    }
    cfg
}

// ── ffmpeg environment ───────────────────────────────────────────────────────

/// Probe and pin the tool versions Phase 1 asks to record. gyan-style ffmpeg
/// builds do not expose the *individual* external library versions through the
/// CLI, so the ffmpeg build line + libavcodec version is the pin; the
/// encoder/decoder presence flags say which libraries that build carries.
fn probe_env() -> Value {
    let version_out = ffmpeg_stdout(&["-version"]).unwrap_or_default();
    let first = version_out.lines().next().unwrap_or("").trim().to_string();
    let libavcodec = version_out
        .lines()
        .find(|l| l.trim_start().starts_with("libavcodec"))
        .unwrap_or("")
        .trim()
        .to_string();

    let mut decoders = BTreeMap::new();
    for d in ["libdav1d", "vp9", "h264", "ffv1", "png", "jpegls"] {
        decoders.insert(d.to_string(), ffmpeg_has(d, "-decoders"));
    }
    let mut encoders = BTreeMap::new();
    for e in [
        "libaom-av1",
        "librav1e",
        "libvpx-vp9",
        "libx264",
        "ffv1",
        "png",
        "jpegls",
    ] {
        encoders.insert(e.to_string(), ffmpeg_has(e, "-encoders"));
    }

    json!({
        "ffmpeg": first,
        "libavcodec": libavcodec,
        "decoders": decoders,
        "encoders": encoders,
        "rustc": tool_version("rustc"),
        "cargo": tool_version("cargo"),
        "cpu": cpu_model(),
        "logical_cores": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
        "os": os_summary(),
    })
}

fn tool_version(tool: &str) -> String {
    Command::new(tool)
        .arg("--version")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "unavailable".to_string())
}

fn cpu_model() -> String {
    if let Ok(o) = Command::new("cmd")
        .args(["/C", "echo %PROCESSOR_IDENTIFIER%"])
        .output()
    {
        let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
        if !s.is_empty() {
            return s;
        }
    }
    Command::new("uname")
        .arg("-m")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

fn os_summary() -> String {
    Command::new("cmd")
        .args(["/C", "ver"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| std::env::consts::OS.to_string())
}

// ── ffmpeg process helpers ───────────────────────────────────────────────────

/// One `ffmpeg -benchmark` run: wall time inside the transcode loop (excludes
/// process spawn + option parsing) and peak RSS when the build reports it.
struct Bench {
    rtime_s: f64,
    maxrss_kb: Option<u64>,
}

fn ffmpeg_stdout(args: &[&str]) -> Option<String> {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner"])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Whether ffmpeg's encoder/decoder list contains `name` (word-delimited).
fn ffmpeg_has(name: &str, list_flag: &str) -> bool {
    ffmpeg_stdout(&[list_flag])
        .map(|text| text.contains(&format!(" {name} ")))
        .unwrap_or(false)
}

/// Run ffmpeg with `-benchmark` and parse `bench: utime=… rtime=…` (plus
/// `bench: maxrss=…` when printed). `stdin_data` is fed to a piped stdin when
/// given, so short-lived inputs do not need a temp file.
fn ffmpeg_bench(args: &[&str], stdin_data: Option<&[u8]>) -> Option<Bench> {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-benchmark"]);
    cmd.args(args);
    if stdin_data.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }
    cmd.stdout(Stdio::null()).stderr(Stdio::piped());
    let mut child = cmd.spawn().ok()?;
    if let Some(data) = stdin_data {
        let mut stdin = child.stdin.take()?;
        let owned = data.to_vec();
        std::thread::spawn(move || {
            let _ = stdin.write_all(&owned);
        });
    }
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        eprintln!(
            "  ffmpeg failed ({}): {}",
            out.status,
            tail(&String::from_utf8_lossy(&out.stderr), 300)
        );
        return None;
    }
    parse_bench_stderr(&String::from_utf8_lossy(&out.stderr))
}

fn parse_bench_stderr(stderr: &str) -> Option<Bench> {
    let mut rtime = None;
    let mut maxrss = None;
    for line in stderr.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("bench: ") {
            for token in rest.split_whitespace() {
                if let Some(v) = token.strip_prefix("rtime=") {
                    rtime = v.trim_end_matches('s').parse().ok();
                } else if let Some(v) = token.strip_prefix("maxrss=") {
                    maxrss = v.trim_end_matches("kB").parse().ok();
                }
            }
        }
    }
    Some(Bench {
        rtime_s: rtime?,
        maxrss_kb: maxrss,
    })
}

fn tail(s: &str, n: usize) -> &str {
    if s.len() <= n {
        s
    } else {
        &s[s.len() - n..]
    }
}

/// Best (minimum) `rtime` over `runs` invocations — the least-noisy estimator
/// for wall-clock subprocess timing.
fn ffmpeg_bench_best(args: &[&str], stdin_data: Option<&[u8]>, runs: usize) -> Option<Bench> {
    let mut best: Option<Bench> = None;
    for _ in 0..runs {
        let b = ffmpeg_bench(args, stdin_data)?;
        best = Some(match best {
            Some(prev) if prev.rtime_s <= b.rtime_s => prev,
            _ => b,
        });
    }
    best
}

/// Decode a clip to a raw 8-bit 4:2:0 YUV temp file (used for verification
/// against Kinetix output). Returns (path, byte count); the caller removes it.
fn ffmpeg_decode_to_raw(clip: &Path) -> Option<PathBuf> {
    let out = std::env::temp_dir().join(format!("kinetix-ffcmp-{}.yuv", std::process::id()));
    let status = Command::new("ffmpeg")
        .args(["-hide_banner", "-v", "error", "-i"])
        .arg(clip)
        .args([
            "-map", "0:v:0", "-an", "-sn", "-dn", "-pix_fmt", "yuv420p", "-f", "rawvideo",
        ])
        .arg("-y")
        .arg(&out)
        .stdin(Stdio::null())
        .status()
        .ok()?;
    status.success().then_some(out)
}

// ── Peak-heap tracking ───────────────────────────────────────────────────────

/// Wraps the system allocator to track live and peak heap bytes. Tracking is
/// always on (two relaxed atomics per allocation, negligible next to decode);
/// [`peak_heap_of`] resets the peak, runs a closure and reads it back.
struct PeakAlloc;

static LIVE_BYTES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static PEAK_BYTES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

unsafe impl std::alloc::GlobalAlloc for PeakAlloc {
    unsafe fn alloc(&self, l: std::alloc::Layout) -> *mut u8 {
        use std::sync::atomic::Ordering::Relaxed;
        let p = std::alloc::System.alloc(l);
        if !p.is_null() {
            let now = LIVE_BYTES.fetch_add(l.size(), Relaxed) + l.size();
            PEAK_BYTES.fetch_max(now, Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: std::alloc::Layout) {
        LIVE_BYTES.fetch_sub(l.size(), std::sync::atomic::Ordering::Relaxed);
        std::alloc::System.dealloc(p, l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: std::alloc::Layout, new: usize) -> *mut u8 {
        use std::sync::atomic::Ordering::Relaxed;
        let q = std::alloc::System.realloc(p, l, new);
        if !q.is_null() {
            if new >= l.size() {
                let now = LIVE_BYTES.fetch_add(new - l.size(), Relaxed) + (new - l.size());
                PEAK_BYTES.fetch_max(now, Relaxed);
            } else {
                LIVE_BYTES.fetch_sub(l.size() - new, Relaxed);
            }
        }
        q
    }
}

#[global_allocator]
static ALLOC: PeakAlloc = PeakAlloc;

/// Peak extra heap (bytes above the level at entry) while `f` runs.
fn peak_heap_of<R>(f: impl FnOnce() -> R) -> (R, usize) {
    use std::sync::atomic::Ordering::Relaxed;
    let base = LIVE_BYTES.load(Relaxed);
    PEAK_BYTES.store(base, Relaxed);
    let r = f();
    (r, PEAK_BYTES.load(Relaxed).saturating_sub(base))
}

// ── Corpus ───────────────────────────────────────────────────────────────────

struct Corpus {
    dir: PathBuf,
    /// `av1_{w}x{h}.ivf`, `vp9_{w}x{h}.ivf`, `h264_{w}x{h}.h264`,
    /// `raw_{w}x{h}.yuv` (decode corpus) and `vp9mp4_{w}x{h}.mp4` (e2e input),
    /// plus raw sources for the encode sections.
    resolutions: Vec<(u32, u32, u32)>,
}

/// Generate (or reuse from `target/perf-corpus`) every synthetic clip. A clip
/// is regenerated when its file is missing or its ffmpeg argument signature in
/// `manifest.json` changed, so parameter tweaks never test stale data.
fn ensure_corpus(cfg: &Config) -> Corpus {
    let dir = PathBuf::from(CORPUS_DIR);
    std::fs::create_dir_all(&dir).expect("create corpus dir");

    let resolutions: Vec<(u32, u32, u32)> = if cfg.quick {
        vec![(320, 240, 120)]
    } else {
        CORPUS.to_vec()
    };

    let manifest_path = dir.join("manifest.json");
    let mut manifest: BTreeMap<String, String> = std::fs::read_to_string(&manifest_path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();

    let mut generate = |name: &str, args: Vec<String>| {
        let sig = args.join(" ");
        let path = dir.join(name);
        if manifest.get(name).map(|s| s.as_str()) == Some(sig.as_str()) && path.exists() {
            return;
        }
        eprintln!("  corpus: generating {name} ...");
        let out = Command::new("ffmpeg")
            .arg("-y")
            .args(["-hide_banner", "-v", "error"])
            .args(&args)
            .arg(&path)
            .stdin(Stdio::null())
            .status();
        if !out.map(|s| s.success()).unwrap_or(false) {
            eprintln!("  corpus: FAILED to generate {name}; clips depending on it will be skipped");
            return;
        }
        manifest.insert(name.to_string(), sig);
    };

    for (w, h, n) in &resolutions {
        let src = format!("testsrc=size={w}x{h}:rate=30");
        let frames = ["-frames:v", &n.to_string()];
        generate(
            &format!("av1_{w}x{h}.ivf"),
            vec_args(&[
                "-f",
                "lavfi",
                "-i",
                &src,
                "-pix_fmt",
                "yuv420p",
                "-c:v",
                "libaom-av1",
                "-cpu-used",
                "8",
                "-row-mt",
                "1",
            ])
            .merged(frames),
        );
        generate(
            &format!("vp9_{w}x{h}.ivf"),
            vec_args(&[
                "-f",
                "lavfi",
                "-i",
                &src,
                "-pix_fmt",
                "yuv420p",
                "-c:v",
                "libvpx-vp9",
                // good-deadline / cpu-used 4 is the envelope the VP9
                // conformance corpus covers. libvpx `-deadline realtime`
                // streams currently FAIL the byte-exact check against ffmpeg
                // (identical frame count and size, differing pixels) — a
                // decoder gap to investigate (see todo-perf.md Phase 1 notes),
                // so the perf corpus stays inside the verified subset.
                "-deadline",
                "good",
                "-cpu-used",
                "4",
                "-lag-in-frames",
                "0",
            ])
            .merged(frames),
        );
        generate(
            &format!("h264_{w}x{h}.h264"),
            vec_args(&[
                "-f", "lavfi", "-i", &src, "-pix_fmt", "yuv420p", "-c:v", "libx264", "-preset",
                "medium",
            ])
            .merged(frames),
        );
        generate(
            &format!("raw_{w}x{h}.yuv"),
            vec_args(&[
                "-f", "lavfi", "-i", &src, "-pix_fmt", "yuv420p", "-f", "rawvideo",
            ])
            .merged(frames),
        );
        generate(
            &format!("vp9mp4_{w}x{h}.mp4"),
            vec_args(&[
                "-f",
                "lavfi",
                "-i",
                &src,
                "-pix_fmt",
                "yuv420p",
                "-c:v",
                "libvpx-vp9",
                // good-deadline / cpu-used 4 is the envelope the VP9
                // conformance corpus covers. libvpx `-deadline realtime`
                // streams currently FAIL the byte-exact check against ffmpeg
                // (identical frame count and size, differing pixels) — a
                // decoder gap to investigate (see todo-perf.md Phase 1 notes),
                // so the perf corpus stays inside the verified subset.
                "-deadline",
                "good",
                "-cpu-used",
                "4",
                "-lag-in-frames",
                "0",
            ])
            .merged(frames),
        );
    }

    if let Ok(text) = serde_json::to_string_pretty(&manifest) {
        let _ = std::fs::write(&manifest_path, text);
    }
    Corpus { dir, resolutions }
}

/// Small argument-vector builder helpers: `vec_args` splits string literals,
/// `.merged` appends another slice.
fn vec_args(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

trait MergeArgs {
    fn merged(self, extra: [&str; 2]) -> Vec<String>;
}

impl MergeArgs for Vec<String> {
    fn merged(mut self, extra: [&str; 2]) -> Vec<String> {
        self.extend(extra.iter().map(|s| s.to_string()));
        self
    }
}

/// Committed AV1 FATE fixtures, used as real-world decode cases when present.
fn list_fate_clips() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(FATE_DIR) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "ivf") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

// ── Kinetix decode drivers ───────────────────────────────────────────────────

/// Decode a whole AV1 IVF with Kinetix, returning every produced frame.
fn av1_decode_all(ivf: &[u8]) -> (usize, Vec<u8>) {
    let mut dec = tpt_kinetix_av1::Av1Decoder::new();
    let mut frames = Vec::new();
    let mut bytes = Vec::new();
    for (i, payload) in split_ivf_frames(ivf).into_iter().enumerate() {
        let pkt = Packet {
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            data: payload,
            stream_index: 0,
            is_key_frame: i == 0,
        };
        if let Ok(Some(f)) = dec.decode(&pkt) {
            bytes.extend_from_slice(&f.data);
            frames.push(f);
        }
    }
    (frames.len(), bytes)
}

/// Decode a whole VP9 IVF with Kinetix.
fn vp9_decode_all(ivf: &[u8]) -> (usize, Vec<u8>) {
    let mut dec = tpt_kinetix_vp9::Vp9Decoder::new();
    let mut frames = 0usize;
    let mut bytes = Vec::new();
    for (i, payload) in split_ivf_frames(ivf).into_iter().enumerate() {
        let pkt = Packet {
            pts: Timestamp::new(i as i64, (1, 30)),
            dts: Timestamp::new(i as i64, (1, 30)),
            data: payload,
            stream_index: 0,
            is_key_frame: i == 0,
        };
        if let Ok(Some(f)) = dec.decode(&pkt) {
            bytes.extend_from_slice(&f.data);
            frames += 1;
        }
    }
    (frames, bytes)
}

/// Split an Annex B stream into one buffer per NAL (4-byte start codes), the
/// packet form `H264Decoder` consumes. Same algorithm as the h264 crate's
/// conformance tests (trailing-zero trim included — dense PPS NALs break
/// without it).
fn split_nals(annexb: &[u8]) -> Vec<Vec<u8>> {
    let mut starts = Vec::new();
    let mut i = 0usize;
    while i + 3 <= annexb.len() {
        if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else if i + 4 <= annexb.len()
            && annexb[i] == 0
            && annexb[i + 1] == 0
            && annexb[i + 2] == 0
            && annexb[i + 3] == 1
        {
            starts.push(i + 4);
            i += 4;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::with_capacity(starts.len());
    for (idx, &payload_start) in starts.iter().enumerate() {
        let mut end = starts.get(idx + 1).map(|&s| s - 3).unwrap_or(annexb.len());
        while end > payload_start && annexb[end - 1] == 0 {
            end -= 1;
        }
        let mut unit = vec![0u8, 0, 0, 1];
        unit.extend_from_slice(&annexb[payload_start..end]);
        out.push(unit);
    }
    out
}

/// Decode a whole H.264 Annex B stream with Kinetix in display order.
fn h264_decode_all(annexb: &[u8]) -> (usize, Vec<u8>) {
    let mut dec = out_kinetix_h264::H264Decoder::new().with_display_order();
    let mut frames = 0usize;
    let mut bytes = Vec::new();
    for (n, unit) in split_nals(annexb).into_iter().enumerate() {
        let pkt = Packet {
            pts: Timestamp::new(n as i64, (1, 25)),
            dts: Timestamp::new(n as i64, (1, 25)),
            data: unit,
            stream_index: 0,
            is_key_frame: n == 0,
        };
        if let Ok(Some(f)) = dec.decode(&pkt) {
            bytes.extend_from_slice(&f.data);
            frames += 1;
        }
    }
    if let Ok(rest) = dec.flush() {
        for f in rest {
            bytes.extend_from_slice(&f.data);
            frames += 1;
        }
    }
    (frames, bytes)
}

/// Best-of-N wall time for a closure that returns the number of elementary
/// units it processed (for the throughput denominator).
fn time_best<F>(runs: usize, mut f: F) -> (f64, usize)
where
    F: FnMut() -> usize,
{
    let mut best = f64::INFINITY;
    let mut elems = 0usize;
    for _ in 0..runs {
        let t = Instant::now();
        elems = f();
        let dt = t.elapsed().as_secs_f64();
        if dt < best {
            best = dt;
        }
    }
    (best, elems)
}

fn mpxs(elems: usize, secs: f64) -> String {
    if secs <= 0.0 {
        return "n/a".to_string();
    }
    let m = elems as f64 / secs / 1e6;
    if m >= 10.0 {
        format!("{m:.1} MPix/s")
    } else {
        format!("{m:.2} MPix/s")
    }
}

fn fmt_secs(s: f64) -> String {
    if s >= 10.0 {
        format!("{s:.1} s")
    } else if s >= 1.0 {
        format!("{s:.2} s")
    } else {
        format!("{s:.3} s")
    }
}

// ── Section: decode ──────────────────────────────────────────────────────────

fn run_decode_section(cfg: &Config, corpus: &Corpus, fate: &[PathBuf], report: &mut Report) {
    eprintln!("== Decode comparison (verify first, then time) ==");
    let mut targets: Vec<(String, String, PathBuf)> = Vec::new();

    for (w, h, _) in &corpus.resolutions {
        targets.push((
            "av1".into(),
            format!("testsrc {w}x{h} (libaom)"),
            corpus.dir.join(format!("av1_{w}x{h}.ivf")),
        ));
        targets.push((
            "vp9".into(),
            format!("testsrc {w}x{h} (libvpx)"),
            corpus.dir.join(format!("vp9_{w}x{h}.ivf")),
        ));
        targets.push((
            "h264".into(),
            format!("testsrc {w}x{h} (libx264)"),
            corpus.dir.join(format!("h264_{w}x{h}.h264")),
        ));
    }
    for p in fate {
        let name = p
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("fate")
            .to_string();
        targets.push(("av1".into(), format!("fate/{name}"), p.clone()));
    }

    for (codec, label, path) in targets {
        // One ffmpeg invocation decodes the clip `loops` times (-stream_loop);
        // a single short clip finishes in fewer milliseconds than the
        // `-benchmark` rtime timer can resolve on Windows (~15 ms), which
        // would report absurd throughputs. Kinetix decodes the same number of
        // passes per timed run so both sides do identical work.
        let loops = match codec.as_str() {
            "av1" if label.starts_with("fate/") => 10,
            _ if path.to_string_lossy().contains("320x240") => 20,
            _ if path.to_string_lossy().contains("1280x720") => 5,
            _ => 3,
        };
        let Some(data) = std::fs::read(&path).ok().filter(|d| !d.is_empty()) else {
            report.skipped.push((
                format!("decode {codec} {}", path.display()),
                "clip missing or empty".to_string(),
            ));
            continue;
        };
        eprintln!("  {codec} {} ...", path.display());

        // 1. Verify: ffmpeg reference decode vs Kinetix, byte-exact.
        let (k_frames, k_bytes, ref_bytes, verified) = match codec.as_str() {
            "av1" if label.starts_with("fate/") => {
                let (kf, kb) = av1_decode_all(&data);
                match decode_av1_with_dav1d_auto(&data, 0, 0) {
                    Ok(frames) => {
                        let rb: Vec<u8> =
                            frames.iter().flat_map(|f| f.data.iter().copied()).collect();
                        {
                            let same = kb == rb;
                            (kf, kb, rb, same)
                        }
                    }
                    Err(e) => {
                        report.skipped.push((
                            format!("decode {label}"),
                            format!("reference decode failed: {e}"),
                        ));
                        continue;
                    }
                }
            }
            "av1" | "vp9" => {
                let (kf, kb) = if codec == "av1" {
                    av1_decode_all(&data)
                } else {
                    vp9_decode_all(&data)
                };
                let Some(raw_path) = ffmpeg_decode_to_raw(&path) else {
                    report.skipped.push((
                        format!("decode {label}"),
                        "ffmpeg raw decode failed".to_string(),
                    ));
                    continue;
                };
                let rb = std::fs::read(&raw_path).unwrap_or_default();
                let _ = std::fs::remove_file(&raw_path);
                {
                    let same = kb == rb;
                    (kf, kb, rb, same)
                }
            }
            "h264" => {
                let (kf, kb) = h264_decode_all(&data);
                let Some(raw_path) = ffmpeg_decode_to_raw(&path) else {
                    report.skipped.push((
                        format!("decode {label}"),
                        "ffmpeg raw decode failed".to_string(),
                    ));
                    continue;
                };
                let rb = std::fs::read(&raw_path).unwrap_or_default();
                let _ = std::fs::remove_file(&raw_path);
                {
                    let same = kb == rb;
                    (kf, kb, rb, same)
                }
            }
            _ => unreachable!("unknown codec {codec}"),
        };
        if k_frames == 0 {
            report.skipped.push((
                format!("decode {label}"),
                "Kinetix produced no frames".to_string(),
            ));
            continue;
        }
        if !verified {
            eprintln!("    WARNING: output mismatch (kinetix {k_frames} frames, {} bytes vs ref {} bytes) — timings will be flagged", k_bytes.len(), ref_bytes.len());
            if let Some((idx, frame, off)) = first_diff(&k_bytes, &ref_bytes, k_frames) {
                eprintln!(
                    "    first difference at byte {idx} (frame {frame}, intra-frame offset {off})"
                );
            }
        }

        // 2. Time. `elems` is luma pixels; ref_bytes length / (3/2) per frame
        //    recovers it for 8-bit 4:2:0 without per-frame geometry bookkeeping.
        let elems_per_pass = k_bytes.len() * 2 / 3;
        let (k_best, _) = time_best(cfg.runs, || {
            let mut frames = 0usize;
            for _ in 0..loops {
                frames += match codec.as_str() {
                    "av1" => av1_decode_all(&data).0,
                    "vp9" => vp9_decode_all(&data).0,
                    _ => h264_decode_all(&data).0,
                };
            }
            frames
        });
        let elems = elems_per_pass * loops;
        // One untimed pass with the allocator's peak tracker (heap only; the
        // output frames Kinetix returns are included, as they are in use).
        let (_, peak_heap) = peak_heap_of(|| match codec.as_str() {
            "av1" => av1_decode_all(&data).0,
            "vp9" => vp9_decode_all(&data).0,
            _ => h264_decode_all(&data).0,
        });

        let clip = path.to_string_lossy().replace('\\', "/");
        // NOTE: no `-v error` here — ffmpeg prints the `bench:` line at info
        // level, so silencing info would hide the very number being parsed.
        //
        // `-stream_loop` is NOT used: it misbehaves on raw .h264 input (the
        // demuxer rewinds without re-decoding, producing absurd timings).
        // Instead the clip's bytes are simply concatenated `loops` times into
        // a temp file — valid for both IVF and Annex-B streams.
        let timed_clip: PathBuf = if loops > 1 {
            let p = std::env::temp_dir().join(format!(
                "kinetix-ffcmp-looped-{}x-{}",
                loops,
                path.file_name().and_then(|s| s.to_str()).unwrap_or("clip")
            ));
            let buf = build_looped_clip(&data, loops);
            std::fs::write(&p, &buf).ok();
            p
        } else {
            path.clone()
        };
        let timed = timed_clip.to_string_lossy().replace('\\', "/");
        let st_args = [
            "-nostats", "-threads", "1", "-i", &timed, "-map", "0:v:0", "-an", "-f", "null", "-",
        ];
        let mt_args = [
            "-nostats", "-i", &timed, "-map", "0:v:0", "-an", "-f", "null", "-",
        ];
        let (Some(st), Some(mt)) = (
            ffmpeg_bench_best(&st_args, None, cfg.runs),
            ffmpeg_bench_best(&mt_args, None, cfg.runs),
        ) else {
            report.skipped.push((
                format!("decode {label}"),
                "ffmpeg timing failed".to_string(),
            ));
            continue;
        };

        if timed_clip != path {
            let _ = std::fs::remove_file(&timed_clip);
        }

        let k_mpx = elems as f64 / k_best / 1e6;
        let st_mpx = elems as f64 / st.rtime_s / 1e6;
        let mt_mpx = elems as f64 / mt.rtime_s / 1e6;
        let ratio = if verified { k_mpx / st_mpx } else { f64::NAN };

        report.decode.push(json!({
            "codec": codec,
            "clip": label,
            "file": clip,
            "kinetix_frames": k_frames,
            "decode_passes": loops,
            "verified": verified,
            "kinetix_s": round3(k_best),
            "ffmpeg_1thread_s": round3(st.rtime_s),
            "ffmpeg_default_s": round3(mt.rtime_s),
            "kinetix_mpxs": round2(k_mpx),
            "ffmpeg_1thread_mpxs": round2(st_mpx),
            "ffmpeg_default_mpxs": round2(mt_mpx),
            "ratio_kinetix_over_ffmpeg_1thread": if verified { Some(round2(ratio)) } else { None },
            "ffmpeg_1thread_maxrss_kb": st.maxrss_kb,
            "kinetix_peak_heap_kb": peak_heap / 1024,
        }));

        println!(
            "  {:<28} verified={:<5} kinetix {:>10}  ffmpeg1T {:>10}  ffmpegMT {:>10}  ratio {:>6}",
            label,
            verified,
            mpxs(elems, k_best),
            mpxs(elems, st.rtime_s),
            mpxs(elems, mt.rtime_s),
            if verified {
                format!("{ratio:.2}x")
            } else {
                "UNVERIFIED".to_string()
            }
        );
    }
}

/// Build a clip that decodes `loops` times: raw Annex-B H.264 can simply be
/// byte-concatenated, but IVF has a 32-byte file header the demuxer would
/// trip over mid-stream, so for IVF the frames are re-packed under one header.
fn build_looped_clip(data: &[u8], loops: usize) -> Vec<u8> {
    let is_ivf = data.len() > 32 && &data[0..4] == b"DKIF";
    if !is_ivf {
        let mut out = Vec::with_capacity(data.len() * loops);
        for _ in 0..loops {
            out.extend_from_slice(data);
        }
        return out;
    }
    // Collect (12-byte frame header, payload) pairs, then emit header + N copies.
    let mut frames: Vec<&[u8]> = Vec::new();
    let mut pos = 32usize;
    while pos + 12 <= data.len() {
        let size =
            u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        let end = pos + 12 + size;
        if size == 0 || end > data.len() {
            break;
        }
        frames.push(&data[pos..end]);
        pos = end;
    }
    let mut out = Vec::with_capacity(32 + frames.len() * loops * 12);
    out.extend_from_slice(&data[..32]);
    for _ in 0..loops {
        for f in &frames {
            out.extend_from_slice(f);
        }
    }
    out
}

/// Locate the first differing byte between the Kinetix and reference plane
/// dumps: (byte index, frame index, offset within that frame).
fn first_diff(a: &[u8], b: &[u8], frames: usize) -> Option<(usize, usize, usize)> {
    let idx = a.iter().zip(b).position(|(x, y)| x != y)?;
    let frame_len = a.len().checked_div(frames).unwrap_or(0);
    Some((idx, idx / frame_len.max(1), idx % frame_len.max(1)))
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

// ── Section: AV1 encode ──────────────────────────────────────────────────────

/// Split a raw 8-bit 4:2:0 YUV file into `VideoFrame`s.
fn load_raw_frames(path: &Path, w: u32, h: u32) -> Option<Vec<VideoFrame>> {
    let data = std::fs::read(path).ok()?;
    let (uw, uh) = (w as usize, h as usize);
    let frame_len = uw * uh + 2 * (uw.div_ceil(2) * uh.div_ceil(2));
    if frame_len == 0 || data.len() % frame_len != 0 {
        return None;
    }
    Some(
        data.chunks_exact(frame_len)
            .enumerate()
            .map(|(i, c)| VideoFrame {
                pts: Timestamp::new(i as i64, (1, 30)),
                dts: Timestamp::new(i as i64, (1, 30)),
                data: c.to_vec(),
                width: w,
                height: h,
                pixel_format: PixelFormat::Yuv420p,
                is_key_frame: i == 0,
            })
            .collect(),
    )
}

/// Mean Y-PSNR of `decoded` against `source`, frame-aligned from the front.
fn mean_y_psnr(source: &[VideoFrame], decoded: &[VideoFrame]) -> Option<f64> {
    if source.is_empty() || decoded.is_empty() {
        return None;
    }
    let n = source.len().min(decoded.len());
    let mut acc = 0.0;
    let mut used = 0usize;
    for i in 0..n {
        if let Some((y, _, _)) = psnr_yuv420p(&source[i], &decoded[i]) {
            // Identical planes give infinite PSNR; cap at 100 dB so the
            // average (and the JSON) stay finite while still reading as
            // "no measurable error".
            acc += y.min(100.0);
            used += 1;
        }
    }
    (used > 0).then(|| acc / used as f64)
}

/// Decode AV1 packets produced by an encoder (OBU temporal units) with Kinetix.
fn av1_decode_packets(packets: &[Packet]) -> Vec<VideoFrame> {
    let mut dec = tpt_kinetix_av1::Av1Decoder::new();
    let mut frames = Vec::new();
    for p in packets {
        if let Ok(Some(f)) = dec.decode(p) {
            frames.push(f);
        }
    }
    frames
}

fn run_encode_av1_section(cfg: &Config, corpus: &Corpus, report: &mut Report) {
    eprintln!("== AV1 encode comparison (Kinetix vs ffmpeg libaom) ==");
    let env = report.env.as_object().cloned().unwrap_or_default();
    if !env
        .get("encoders")
        .and_then(|e| e.get("libaom-av1"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        report.skipped.push((
            "encode-av1".to_string(),
            "ffmpeg has no libaom-av1 encoder".to_string(),
        ));
        return;
    }

    let sizes: Vec<(u32, u32, u32)> = if cfg.quick {
        vec![(320, 240, 60)]
    } else {
        ENCODE_AV1.to_vec()
    };

    for (w, h, n) in sizes {
        let raw = corpus.dir.join(format!("raw_{w}x{h}.yuv"));
        let Some(frames) = load_raw_frames(&raw, w, h).map(|mut v| {
            v.truncate(n as usize);
            v
        }) else {
            report.skipped.push((
                format!("encode-av1 {w}x{h}"),
                "raw source missing or malformed".to_string(),
            ));
            continue;
        };
        if frames.is_empty() {
            continue;
        }
        eprintln!("  {w}x{h} x{} ...", frames.len());

        // Kinetix: CQP quantizer 100, swept over rav1e speed presets with
        // auto tiling (libaom below runs cpu-used 8 + row-mt, so speed 6 alone
        // is not an equal-effort comparison).
        let kinetix = |speed: u8| {
            let cfg = tpt_kinetix_av1::encoder::Av1EncoderConfig {
                width: w,
                height: h,
                bitrate: 0,
                quantizer: 100,
                speed,
                keyframe_interval: 240,
                ..Default::default()
            };
            let mut enc = match tpt_kinetix_av1::encoder::Av1Encoder::new(&cfg) {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("    kinetix encoder init failed: {e}");
                    return (f64::INFINITY, 0usize, Vec::<Packet>::new());
                }
            };
            let t = Instant::now();
            let mut bytes = 0usize;
            let mut packets = Vec::new();
            for f in &frames {
                if let Ok(Some(p)) = enc.encode_frame(f) {
                    bytes += p.data.len();
                    packets.push(p);
                }
            }
            for p in enc.flush().unwrap_or_default() {
                bytes += p.data.len();
                packets.push(p);
            }
            (t.elapsed().as_secs_f64(), bytes, packets)
        };
        // (speed, time, bytes, psnr)
        let mut sweep: Vec<(u8, f64, usize, Option<f64>)> = Vec::new();
        for speed in [6u8, 8, 10] {
            let (t1, b1, p1) = kinetix(speed);
            let (t2, b2, p2) = kinetix(speed);
            let (t, b, p) = if t1 <= t2 { (t1, b1, p1) } else { (t2, b2, p2) };
            let psnr = mean_y_psnr(&frames, &av1_decode_packets(&p));
            println!(
                "    kinetix speed {speed}: {} ({} KiB, PSNR {})",
                fmt_secs(t),
                b / 1024,
                psnr.map(|p| format!("{p:.2} dB"))
                    .unwrap_or_else(|| "n/a".into())
            );
            sweep.push((speed, t, b, psnr));
        }

        // libaom through ffmpeg, constant quality.
        let out = corpus.dir.join(format!("enc_libaom_{w}x{h}.ivf"));
        let clip = raw.to_string_lossy().replace('\\', "/");
        let outfile = out.to_string_lossy().replace('\\', "/");
        let aom_args = [
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-s",
            &format!("{w}x{h}"),
            "-r",
            "30",
            "-i",
            &clip,
            "-c:v",
            "libaom-av1",
            "-cpu-used",
            "8",
            "-crf",
            "30",
            "-b:v",
            "0",
            "-row-mt",
            "1",
            "-y",
            &outfile,
        ];
        let aom = ffmpeg_bench_best(&aom_args, None, cfg.runs);
        let aom_psnr = if out.exists() {
            let bytes = std::fs::read(&out).unwrap_or_default();
            let d = av1_decode_all(&bytes).1;
            let decoded = load_raw_frames_from(&d, w, h);
            mean_y_psnr(&frames, &decoded)
        } else {
            None
        };
        let aom_bytes = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0) as usize;

        // Headline row: the sweep entry whose PSNR is closest to libaom's.
        let (k_speed, k_time, k_bytes, k_psnr) = sweep
            .iter()
            .copied()
            .min_by(|x, y| {
                let d = |e: &(u8, f64, usize, Option<f64>)| match (e.3, aom_psnr) {
                    (Some(a), Some(b)) => (a - b).abs(),
                    _ => e.1,
                };
                d(x).total_cmp(&d(y))
            })
            .expect("sweep is non-empty");

        report.encode_av1.push(json!({
            "clip": format!("testsrc {w}x{h} x{}", frames.len()),
            "kinetix": {
                "enc_s": round3(k_time),
                "bytes": k_bytes,
                "y_psnr": k_psnr.map(round2),
                "settings": format!("CQP q=100 speed={k_speed} auto-tiles keyint=240"),
            },
            "libaom": {
                "enc_s": aom.as_ref().map(|b| round3(b.rtime_s)),
                "bytes": if aom.is_some() { aom_bytes } else { 0 },
                "y_psnr": aom_psnr.map(round2),
                "settings": "CRF 30 cpu-used=8 row-mt=1",
                "maxrss_kb": aom.as_ref().and_then(|b| b.maxrss_kb),
            },
        }));
        println!(
            "  {w}x{h}: kinetix {} ({} KiB, PSNR {})  libaom {} ({} KiB, PSNR {})",
            fmt_secs(k_time),
            k_bytes / 1024,
            k_psnr
                .map(|p| format!("{p:.2} dB"))
                .unwrap_or_else(|| "n/a".into()),
            aom.as_ref()
                .map(|b| fmt_secs(b.rtime_s))
                .unwrap_or_else(|| "failed".into()),
            aom_bytes / 1024,
            aom_psnr
                .map(|p| format!("{p:.2} dB"))
                .unwrap_or_else(|| "n/a".into()),
        );
    }
}

/// Wrap already-concatenated raw 4:2:0 bytes as frames (PSNR comparison path).
fn load_raw_frames_from(data: &[u8], w: u32, h: u32) -> Vec<VideoFrame> {
    let (uw, uh) = (w as usize, h as usize);
    let frame_len = uw * uh + 2 * (uw.div_ceil(2) * uh.div_ceil(2));
    data.chunks_exact(frame_len)
        .enumerate()
        .map(|(i, c)| VideoFrame {
            pts: Timestamp::new(i as i64, (1, 30)),
            dts: Timestamp::new(i as i64, (1, 30)),
            data: c.to_vec(),
            width: w,
            height: h,
            pixel_format: PixelFormat::Yuv420p,
            is_key_frame: i == 0,
        })
        .collect()
}

// ── Section: original codecs ─────────────────────────────────────────────────

/// A moving natural-image-like luma plane (the lean/realtime bench pattern),
/// with per-frame motion so reference encoders cannot cheat with skip blocks.
fn natural_luma(w: u32, h: u32, t: u32) -> Vec<u8> {
    let mut luma = vec![0u8; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            let v = (((x + t * 3) * 255) / w.max(1)) as u8 ^ (((y + t * 2) * 255) / h.max(1)) as u8;
            luma[(y * w + x) as usize] = v;
        }
    }
    luma
}

/// A UI-like frame (the screen bench pattern) with a moving highlight.
fn screen_luma(w: u32, h: u32, t: u32) -> Vec<u8> {
    let mut luma = vec![240u8; (w * h) as usize];
    let shift = (t * 7) % 64;
    for y in 0..h {
        for x in 0..w {
            let px = (y * w + x) as usize;
            if y % 32 == 0 || (x + shift) % 64 == 0 {
                luma[px] = 128;
                continue;
            }
            if (y % 32) / 8 == 2 && (x % 24) < 14 {
                luma[px] = 16;
            } else if (y % 128) < 8 && x < w / 2 {
                luma[px] = 64;
            }
        }
    }
    luma
}

/// Flat mid-grey chroma planes for the 4:2:0 originals.
fn flat_chroma(w: u32, h: u32) -> (Vec<u8>, Vec<u8>) {
    let cw = (w as usize).div_ceil(2);
    let ch = (h as usize).div_ceil(2);
    (vec![128u8; cw * ch], vec![128u8; cw * ch])
}

/// The 8-bit 4:2:0 source clip shared by an original codec and its reference
/// encoders: in-process patterns (so both sides see byte-identical input),
/// cached as raw YUV on disk for ffmpeg.
struct OrigSource {
    frames: Vec<VideoFrame>,
    raw_path: PathBuf,
    /// 10-bit 4:2:0 (`yuv420p10le`) source: PSNR against the reference output
    /// is skipped (the shared PSNR helper is 8-bit only); lossless rows carry
    /// a bit-exact flag instead.
    pix10: bool,
}

fn make_source(kind: &str, w: u32, h: u32, n: u32, corpus_dir: &Path) -> OrigSource {
    let raw_path = corpus_dir.join(format!("orig_{kind}_{w}x{h}.yuv"));
    let mut frames = Vec::new();
    let mut raw = Vec::new();
    for t in 0..n {
        let luma = match kind {
            "screen" => screen_luma(w, h, t),
            _ => natural_luma(w, h, t),
        };
        let (cb, cr) = flat_chroma(w, h);
        let mut data = luma.clone();
        data.extend_from_slice(&cb);
        data.extend_from_slice(&cr);
        raw.extend_from_slice(&data);
        frames.push(VideoFrame {
            pts: Timestamp::new(t as i64, (1, 30)),
            dts: Timestamp::new(t as i64, (1, 30)),
            data,
            width: w,
            height: h,
            pixel_format: PixelFormat::Yuv420p,
            is_key_frame: true,
        });
    }
    write_bytes_file(raw_path.to_str().expect("utf-8 path"), &raw);
    OrigSource {
        frames,
        raw_path,
        pix10: false,
    }
}

/// 10-bit variant for the lossless codec (which only accepts 10/12/16-bit
/// planes): the 8-bit patterns are scaled into the 10-bit range and cached as
/// `yuv420p10le` raw (u16 little-endian samples) for the reference encoders.
fn make_source10(kind: &str, w: u32, h: u32, n: u32, corpus_dir: &Path) -> OrigSource {
    let raw_path = corpus_dir.join(format!("orig_{kind}10_{w}x{h}.yuv"));
    let mut frames = Vec::new();
    let mut raw = Vec::new();
    let scale = |v: u8| u16::from(v) * 4; // 8 -> 10 bit range
    for t in 0..n {
        let luma = match kind {
            "screen" => screen_luma(w, h, t),
            _ => natural_luma(w, h, t),
        };
        let (cb, cr) = flat_chroma(w, h);
        let mut data = Vec::with_capacity(((w * h) * 2 + 2 * (w * h / 2) * 2) as usize);
        for b in &luma {
            data.extend_from_slice(&scale(*b).to_le_bytes());
        }
        for plane in [&cb, &cr] {
            for b in plane.iter() {
                data.extend_from_slice(&scale(*b).to_le_bytes());
            }
        }
        raw.extend_from_slice(&data);
        frames.push(VideoFrame {
            pts: Timestamp::new(t as i64, (1, 30)),
            dts: Timestamp::new(t as i64, (1, 30)),
            data,
            width: w,
            height: h,
            pixel_format: PixelFormat::Yuv420p10le,
            is_key_frame: true,
        });
    }
    write_bytes_file(raw_path.to_str().expect("utf-8 path"), &raw);
    OrigSource {
        frames,
        raw_path,
        pix10: true,
    }
}

fn write_bytes_file(path: &str, contents: &[u8]) {
    if let Some(parent) = Path::new(path).parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("could not create {}: {e}", parent.display());
            std::process::exit(1);
        }
    }
    if let Err(e) = std::fs::write(path, contents) {
        eprintln!("could not write {path}: {e}");
        std::process::exit(1);
    }
}

/// A standard encoder an original codec is measured against.
struct Reference {
    name: &'static str,
    /// ffmpeg args between the input and the output file.
    enc_args: Vec<String>,
    out_ext: &'static str,
    /// Still-image sequence (PNG / JPEG-LS): output is a `%04d` pattern.
    image2: bool,
    settings: &'static str,
}

impl Reference {
    fn video(name: &'static str, args: &[&str], ext: &'static str, settings: &'static str) -> Self {
        Self {
            name,
            enc_args: args.iter().map(|s| s.to_string()).collect(),
            out_ext: ext,
            image2: false,
            settings,
        }
    }

    fn image2(
        name: &'static str,
        args: &[&str],
        ext: &'static str,
        settings: &'static str,
    ) -> Self {
        Self {
            name,
            enc_args: args.iter().map(|s| s.to_string()).collect(),
            out_ext: ext,
            image2: true,
            settings,
        }
    }
}

/// Run one reference encoder over the raw source: encode (best-of-N
/// `-benchmark` rtime), decode (`-f null -`, best-of-N), output size, and mean
/// Y-PSNR of the decoded output against the source frames.
fn run_reference(
    cfg: &Config,
    src: &OrigSource,
    r: &Reference,
    corpus_dir: &Path,
) -> Option<(f64, f64, usize, Option<f64>)> {
    let w = src.frames[0].width;
    let h = src.frames[0].height;
    let src_s = src.raw_path.to_string_lossy().replace('\\', "/");
    let stem = r
        .name
        .to_lowercase()
        .replace([' ', '+', '/', '(', ')'], "_");
    // image2 still sequences need a printf pattern in the file name, video
    // muxers must not have one.
    let out = corpus_dir.join(if r.image2 {
        format!("orig_ref_{stem}_{w}x{h}_%04d.{}", r.out_ext)
    } else {
        format!("orig_ref_{stem}_{w}x{h}.{}", r.out_ext)
    });
    let out_s = out.to_string_lossy().replace('\\', "/");
    let geo = format!("{w}x{h}");

    let in_pix = if src.pix10 { "yuv420p10le" } else { "yuv420p" };
    let mut enc_args: Vec<&str> = vec![
        "-f", "rawvideo", "-pix_fmt", in_pix, "-s", &geo, "-r", "30", "-i", &src_s,
    ];
    enc_args.extend(r.enc_args.iter().map(|s| s.as_str()));
    if r.image2 {
        enc_args.extend_from_slice(&["-f", "image2", "-y", &out_s]);
    } else {
        enc_args.extend_from_slice(&["-y", &out_s]);
    }
    let enc = ffmpeg_bench_best(&enc_args, None, cfg.runs)?;

    let dec = if r.image2 {
        ffmpeg_bench_best(
            &[
                "-nostats", "-f", "image2", "-i", &out_s, "-map", "0:v:0", "-an", "-f", "null", "-",
            ],
            None,
            cfg.runs,
        )?
    } else {
        ffmpeg_bench_best(
            &[
                "-nostats", "-i", &out_s, "-map", "0:v:0", "-an", "-f", "null", "-",
            ],
            None,
            cfg.runs,
        )?
    };

    let bytes = if r.image2 {
        let prefix = format!("orig_ref_{stem}_{w}x{h}");
        let mut total = 0u64;
        for e in std::fs::read_dir(corpus_dir).ok()?.flatten() {
            if e.file_name()
                .to_str()
                .is_some_and(|n| n.starts_with(&prefix))
            {
                total += e.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
        total as usize
    } else {
        std::fs::metadata(&out)
            .map(|m| m.len() as usize)
            .unwrap_or(0)
    };

    // Quality of the reference output, measured on the decoded planes. Not
    // computable for 10-bit sources (the shared PSNR helper is 8-bit only),
    // which is exactly the lossless case where bit-exactness is the metric.
    let psnr = if src.pix10 {
        None
    } else {
        ffmpeg_decode_to_raw(&out).and_then(|raw_path| {
            let raw = std::fs::read(&raw_path).ok();
            let _ = std::fs::remove_file(&raw_path);
            let decoded = load_raw_frames_from(&raw?, w, h);
            mean_y_psnr(&src.frames, &decoded)
        })
    };

    Some((enc.rtime_s, dec.rtime_s, bytes, psnr))
}

/// Everything `push_original_rows` needs for one original codec (kept as a
/// struct: the flat argument list tripped clippy's too-many-arguments).
struct OrigCompare<'a> {
    cfg: &'a Config,
    codec: &'a str,
    src: &'a OrigSource,
    /// Produce the per-frame Kinetix packets.
    encode: &'a dyn Fn() -> Option<Vec<Vec<u8>>>,
    /// Re-decode packets back to frames, so decode cost and roundtrip quality
    /// are measured separately from encode.
    decode: &'a dyn Fn(&[Vec<u8>]) -> Vec<VideoFrame>,
    bit_exact: bool,
    references: &'a [Reference],
    corpus_dir: &'a Path,
}

/// Record one original codec's Kinetix rows plus one row per reference.
fn push_original_rows(c: &OrigCompare, report: &mut Report) {
    let cfg = c.cfg;
    let codec = c.codec;
    let src = c.src;
    let encode = c.encode;
    let decode = c.decode;
    let bit_exact = c.bit_exact;
    let references = c.references;
    let corpus_dir = c.corpus_dir;
    let (k_enc_s, k_packets) = {
        let mut best = f64::INFINITY;
        let mut packets = None;
        for _ in 0..cfg.runs.min(2) {
            let t = Instant::now();
            let p = encode();
            let dt = t.elapsed().as_secs_f64();
            if dt < best {
                best = dt;
            }
            if p.is_some() {
                packets = p;
            }
        }
        (best, packets)
    };
    let Some(k_packets) = k_packets else {
        report.skipped.push((
            format!("{codec} encode"),
            "Kinetix encoder failed".to_string(),
        ));
        return;
    };
    let k_bytes: usize = k_packets.iter().map(|p| p.len()).sum();
    let k_decoded = decode(&k_packets);
    let (k_dec_s, k_dec_frames) = time_best(cfg.runs, || decode(&k_packets).len());
    // `packets_bit_exact` is the **measurement**; `bit_exact` is the
    // **expectation**. They were previously collapsed with `&&` into a single
    // `bit_exact` field, which destroyed the distinction: a codec declared
    // lossless that failed looked identical to one that is lossy by design, and
    // the renderer then printed `screen`'s failure as "lossy" — a correctness
    // failure displayed as an expected quality setting. Both are recorded
    // separately now; `bit_exact` stays the conjunction so existing consumers
    // of the JSON keep their current meaning.
    let measured_exact = packets_bit_exact(&src.frames, &k_decoded);
    let exact = bit_exact && measured_exact;

    report.originals.push(json!({
        "codec": codec,
        "encoder": "kinetix (all-intra)",
        "enc_s": round3(k_enc_s),
        "dec_s": round3(k_dec_s),
        "bytes": k_bytes,
        "y_psnr": if exact { None } else { mean_y_psnr(&src.frames, &k_decoded).map(round2) },
        "bit_exact": exact,
        "lossless_expected": bit_exact,
        "bit_exact_measured": measured_exact,
        "decoded_frames": k_dec_frames,
        "source_frames": src.frames.len(),
        "settings": if codec == "screen" {
            "all-intra key frames, codec defaults (v1 codes luma only; chroma decodes as 0)"
        } else {
            "all-intra key frames, codec defaults"
        },
    }));
    println!(
        "  {codec}: kinetix enc {} dec {} ({} KiB, bit-exact={}, frames {}/{})",
        fmt_secs(k_enc_s),
        fmt_secs(k_dec_s),
        k_bytes / 1024,
        exact,
        k_dec_frames,
        src.frames.len(),
    );

    for r in references {
        match run_reference(cfg, src, r, corpus_dir) {
            Some((enc_s, dec_s, bytes, psnr)) => {
                report.originals.push(json!({
                    "codec": codec,
                    "encoder": r.name,
                    "enc_s": round3(enc_s),
                    "dec_s": round3(dec_s),
                    "bytes": bytes,
                    "y_psnr": psnr.map(round2),
                    "settings": r.settings,
                }));
                println!(
                    "  {codec}: {:<26} enc {} dec {} ({} KiB, PSNR {})",
                    r.name,
                    fmt_secs(enc_s),
                    fmt_secs(dec_s),
                    bytes / 1024,
                    psnr.map(|p| format!("{p:.2} dB"))
                        .unwrap_or_else(|| "n/a".into()),
                );
            }
            None => {
                report.skipped.push((
                    format!("{codec} vs {}", r.name),
                    "ffmpeg encode or decode failed".to_string(),
                ));
            }
        }
    }
}

fn packets_bit_exact(source: &[VideoFrame], decoded: &[VideoFrame]) -> bool {
    source.len() == decoded.len()
        && source
            .iter()
            .zip(decoded.iter())
            .all(|(a, b)| a.data == b.data)
}

/// Split a source `VideoFrame` into (luma, cb, cr) planes.
fn planes_of(f: &VideoFrame) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let w = f.width as usize;
    let h = f.height as usize;
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    let (luma, rest) = f.data.split_at(w * h);
    (
        luma.to_vec(),
        rest[..cw * ch].to_vec(),
        rest[cw * ch..cw * ch * 2].to_vec(),
    )
}

fn run_originals_section(cfg: &Config, corpus_dir: &Path, report: &mut Report) {
    eprintln!("== Original codecs vs standard references ==");
    let (w, h, n) = if cfg.quick {
        (320, 240, 60)
    } else {
        ORIGINALS_SIZE
    };

    compare_lean(cfg, w, h, n, corpus_dir, report);
    compare_realtime(cfg, w, h, n, corpus_dir, report);
    compare_screen(cfg, w, h, n, corpus_dir, report);
    compare_lossless(cfg, w, h, n, corpus_dir, report);

    report.skipped.push((
        "vision vs AV1/x264 at equal detector accuracy".to_string(),
        "needs a detector-accuracy ground-truth study; not automatable in this harness yet"
            .to_string(),
    ));
    report.skipped.push((
        "face vs AV1/x264 at matched quality".to_string(),
        "needs matched-quality talking-head clips; not automatable in this harness yet".to_string(),
    ));
    report.skipped.push((
        "volumetric vs Draco / TMC13".to_string(),
        "tpt-kinetix-volumetric is not yet byte-compatible with tmc3 (see \
         tpt-kinetix-test-utils::tmc13); a cross-decode comparison needs that alignment first"
            .to_string(),
    ));
}

fn compare_lean(cfg: &Config, w: u32, h: u32, n: u32, corpus_dir: &Path, report: &mut Report) {
    use tpt_kinetix_lean::{
        decoder::LeanDecoder,
        headers::ChromaFormat,
        reconstruct::{encode_frame, FrameBuffer},
        FrameHeader, FrameType, SequenceHeader,
    };
    let src = make_source("natural", w, h, n, corpus_dir);
    let seq = SequenceHeader {
        version: 1,
        max_width: 1920,
        max_height: 1080,
        max_ref_frames: 4,
        min_block_size_log2: 3,
        max_block_size_log2: 3,
        bit_depth: 8,
        chroma_format: ChromaFormat::Yuv420,
        num_rans_streams: 1,
    };
    let fhdr = FrameHeader {
        frame_type: FrameType::Key,
        width: w as u16,
        height: h as u16,
        base_qp: 0,
        ref_frame_count: 0,
        payload_len: 0,
    };

    let encode = || -> Option<Vec<Vec<u8>>> {
        let mut packets = Vec::new();
        for f in &src.frames {
            let (luma, cb, cr) = planes_of(f);
            let fb = FrameBuffer::from_yuv420(w, h, luma, cb, cr).ok()?;
            let payload = encode_frame(&seq, &fhdr, &fb, None).ok()?;
            let mut pkt = fhdr.to_bytes().to_vec();
            pkt.extend_from_slice(&payload);
            packets.push(pkt);
        }
        Some(packets)
    };
    let decode = |packets: &[Vec<u8>]| -> Vec<VideoFrame> {
        let mut dec = LeanDecoder::new();
        dec.set_sequence_header(seq);
        let mut out = Vec::new();
        for (i, data) in packets.iter().enumerate() {
            let pkt = Packet {
                pts: Timestamp::new(i as i64, (1, 30)),
                dts: Timestamp::new(i as i64, (1, 30)),
                data: data.clone(),
                stream_index: 0,
                is_key_frame: true,
            };
            if let Ok(Some(f)) = dec.decode(&pkt) {
                out.push(f);
            }
        }
        out
    };

    let references = vec![
        Reference::video(
            "x264 ultrafast+zerolatency",
            &[
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-tune",
                "zerolatency",
                "-crf",
                "18",
            ],
            "mkv",
            "CRF 18 ultrafast zerolatency",
        ),
        Reference::video(
            "libaom realtime",
            &[
                "-c:v",
                "libaom-av1",
                "-usage",
                "1",
                "-cpu-used",
                "8",
                "-crf",
                "20",
                "-b:v",
                "0",
                "-row-mt",
                "1",
            ],
            "ivf",
            "CRF 20 usage=realtime cpu-used=8",
        ),
    ];
    push_original_rows(
        &OrigCompare {
            cfg,
            codec: "lean",
            src: &src,
            encode: &encode,
            decode: &decode,
            bit_exact: true,
            references: &references,
            corpus_dir,
        },
        report,
    );
}

fn compare_realtime(cfg: &Config, w: u32, h: u32, n: u32, corpus_dir: &Path, report: &mut Report) {
    use tpt_kinetix_realtime::{
        decoder::RealtimeDecoder,
        headers::{ChromaFormat, FrameHeader, FrameType, ProfilePreset, SequenceHeader},
        reconstruct::{encode_frame_slices, FrameBuffer},
        SliceGrid,
    };
    let src = make_source("natural", w, h, n, corpus_dir);
    let seq = SequenceHeader {
        version: 1,
        max_width: 1920,
        max_height: 1080,
        profile: ProfilePreset::Conferencing,
        slice_grid_cols: 8,
        slice_grid_rows: 8,
        fec_overhead_pct: 20,
        foveation_enabled: false,
        min_block_size_log2: 3,
        max_block_size_log2: 3,
        bit_depth: 8,
        chroma_format: ChromaFormat::Yuv420,
        num_rans_streams: 64,
        max_ref_frames: 1,
        max_deadline_ms: 16,
    };
    let fhdr = FrameHeader {
        frame_type: FrameType::Key,
        width: w as u16,
        height: h as u16,
        base_qp: 0,
        ref_frame_count: 0,
        deadline_ms: 16,
        force_idr: true,
        foveation_center_x: (w / 2) as u16,
        foveation_center_y: (h / 2) as u16,
        // The parser always reads refresh_mask_len() mask bytes, so the writer must
        // emit a zero-filled mask of that length (0 bits = no intra refresh),
        // not an empty vector.
        intra_refresh_mask: vec![0; seq.refresh_mask_len()],
        payload_len: 0,
    };

    let encode = || -> Option<Vec<Vec<u8>>> {
        let grid = SliceGrid {
            cols: seq.slice_grid_cols,
            rows: seq.slice_grid_rows,
        };
        let mut packets = Vec::new();
        for f in &src.frames {
            let (luma, cb, cr) = planes_of(f);
            let fb = FrameBuffer::from_yuv420(w, h, luma, cb, cr).ok()?;
            let slices = encode_frame_slices(&seq, &fhdr, &fb, None).ok()?;
            let framed = grid.frame(&slices).ok()?;
            // The realtime wire format carries the payload length in the frame
            // header and the decoder slices the packet by it — it must be the
            // real framed length, not the placeholder 0 (the crate's own bench
            // shipped this bug for a while: its decode case ignored the
            // decoder's error and timed a failed decode).
            let fhdr = FrameHeader {
                payload_len: framed.len() as u32,
                ..fhdr.clone()
            };
            let mut pkt = fhdr.to_bytes().to_vec();
            pkt.extend_from_slice(&framed);
            packets.push(pkt);
        }
        Some(packets)
    };
    let decode = |packets: &[Vec<u8>]| -> Vec<VideoFrame> {
        let mut dec = RealtimeDecoder::new();
        dec.set_sequence_header(seq);
        let mut out = Vec::new();
        for (i, data) in packets.iter().enumerate() {
            let pkt = Packet {
                pts: Timestamp::new(i as i64, (1, 30)),
                dts: Timestamp::new(i as i64, (1, 30)),
                data: data.clone(),
                stream_index: 0,
                is_key_frame: true,
            };
            if let Ok(Some(f)) = dec.decode(&pkt) {
                out.push(f);
            }
        }
        out
    };

    let references = vec![
        Reference::video(
            "x264 ultrafast+zerolatency",
            &[
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-tune",
                "zerolatency",
                "-crf",
                "18",
            ],
            "mkv",
            "CRF 18 ultrafast zerolatency",
        ),
        Reference::video(
            "libaom realtime",
            &[
                "-c:v",
                "libaom-av1",
                "-usage",
                "1",
                "-cpu-used",
                "8",
                "-crf",
                "20",
                "-b:v",
                "0",
                "-row-mt",
                "1",
            ],
            "ivf",
            "CRF 20 usage=realtime cpu-used=8",
        ),
    ];
    push_original_rows(
        &OrigCompare {
            cfg,
            codec: "realtime",
            src: &src,
            encode: &encode,
            decode: &decode,
            bit_exact: true,
            references: &references,
            corpus_dir,
        },
        report,
    );
}

fn compare_screen(cfg: &Config, w: u32, h: u32, n: u32, corpus_dir: &Path, report: &mut Report) {
    use tpt_kinetix_screen::{
        decoder::ScreenDecoder,
        headers::{ChromaFormat, FrameHeader, FrameType, SequenceHeader},
        reconstruct::{encode_frame, FrameBuffer},
    };
    let src = make_source("screen", w, h, n, corpus_dir);
    let seq = SequenceHeader {
        version: 1,
        max_width: 1920,
        max_height: 1080,
        base_block_size_log2: 4,
        num_rans_streams: 4,
        dict_cap: 256,
        palette_cap: 64,
        glyph_max_dim: 32,
        bit_depth: 8,
        chroma_format: ChromaFormat::Yuv420,
        max_ref_frames: 1,
    };
    let fhdr = FrameHeader {
        frame_type: FrameType::Key,
        width: w as u16,
        height: h as u16,
        base_qp: 0,
        ref_frame_count: 0,
        dict_version: 0,
        dict_reset: true,
        payload_len: 0,
    };

    let encode = || -> Option<Vec<Vec<u8>>> {
        let mut packets = Vec::new();
        for f in &src.frames {
            let (luma, cb, cr) = planes_of(f);
            let fb = FrameBuffer::from_yuv420(w, h, luma, cb, cr).ok()?;
            let payload = encode_frame(&seq, &fhdr, &fb, None).ok()?;
            let mut pkt = fhdr.to_bytes().to_vec();
            pkt.extend_from_slice(&payload);
            packets.push(pkt);
        }
        Some(packets)
    };
    let decode = |packets: &[Vec<u8>]| -> Vec<VideoFrame> {
        let mut dec = ScreenDecoder::new();
        dec.set_sequence_header(seq);
        let mut out = Vec::new();
        for (i, data) in packets.iter().enumerate() {
            let pkt = Packet {
                pts: Timestamp::new(i as i64, (1, 30)),
                dts: Timestamp::new(i as i64, (1, 30)),
                data: data.clone(),
                stream_index: 0,
                is_key_frame: true,
            };
            if let Ok(Some(f)) = dec.decode(&pkt) {
                out.push(f);
            }
        }
        out
    };

    let references = vec![
        Reference::video(
            "x264 ultrafast",
            &["-c:v", "libx264", "-preset", "ultrafast", "-crf", "18"],
            "mkv",
            "CRF 18 ultrafast",
        ),
        Reference::video(
            "libaom realtime",
            &[
                "-c:v",
                "libaom-av1",
                "-usage",
                "1",
                "-cpu-used",
                "8",
                "-crf",
                "20",
                "-b:v",
                "0",
                "-row-mt",
                "1",
            ],
            "ivf",
            "CRF 20 usage=realtime cpu-used=8 (this ffmpeg build has no -tune-content)",
        ),
    ];
    push_original_rows(
        &OrigCompare {
            cfg,
            codec: "screen",
            src: &src,
            encode: &encode,
            decode: &decode,
            // `false`, not `true`: screen v1 codes **luma only** — `reconstruct.rs`
            // has no chroma write path at all (`fill_luma_block` /
            // `blit_luma_block` are the only writers), so `FrameBuffer::new`'s
            // zero-filled chroma survives decoding. A full-frame round-trip
            // therefore *cannot* reproduce a source with non-zero chroma, and
            // declaring `bit_exact: true` here only guaranteed a permanent false
            // "MISMATCH" that was previously being rendered as "lossy" by a
            // screen-specific special-case in the table renderer.
            //
            // The luma plane *is* bit-exact, which is the guarantee that actually
            // matters and is asserted by `tpt-kinetix-screen/tests/roundtrip.rs`.
            // Raise this to `true` when a later version codes chroma.
            bit_exact: false,
            references: &references,
            corpus_dir,
        },
        report,
    );
}

/// The lossless codec only accepts 10/12/16-bit planes, so the comparison runs
/// at 10-bit 4:2:0: Kinetix (3 planes, bit-exact roundtrip asserted in-process)
/// against FFV1 (10-bit, bit-exact by construction) and PNG stills (indicative
/// size/speed only — ffmpeg converts through RGB, which is not YUV-lossless).
fn compare_lossless(cfg: &Config, w: u32, h: u32, n: u32, corpus_dir: &Path, report: &mut Report) {
    use tpt_kinetix_lossless::{
        headers::{PlaneSpec, SequenceHeader as LlSeq},
        LosslessDecoder, LosslessEncoder, Plane,
    };
    let src = make_source10("natural", w, h, n, corpus_dir);
    let seq = LlSeq {
        version: 1,
        max_width: w as u16,
        max_height: h as u16,
        transform_id: 0,
        planes: vec![PlaneSpec { bit_depth: 10 }; 3],
    };
    let planes_of10 = |f: &VideoFrame| -> (Vec<u16>, Vec<u16>, Vec<u16>) {
        let read16 = |data: &[u8]| -> Vec<u16> {
            data.chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect()
        };
        let w = f.width as usize;
        let h = f.height as usize;
        let cw = w.div_ceil(2) * h.div_ceil(2);
        let all = read16(&f.data);
        (
            all[..w * h].to_vec(),
            all[w * h..w * h + cw].to_vec(),
            all[w * h + cw..].to_vec(),
        )
    };

    let encode = || -> Option<Vec<Vec<u8>>> {
        let mut packets = Vec::new();
        for f in &src.frames {
            let (luma, cb, cr) = planes_of10(f);
            let planes = vec![
                Plane {
                    width: w,
                    height: h,
                    bit_depth: 10,
                    data: luma,
                },
                Plane {
                    width: w.div_ceil(2),
                    height: h.div_ceil(2),
                    bit_depth: 10,
                    data: cb,
                },
                Plane {
                    width: w.div_ceil(2),
                    height: h.div_ceil(2),
                    bit_depth: 10,
                    data: cr,
                },
            ];
            packets.push(LosslessEncoder::new().encode_frame(&seq, &planes).ok()?);
        }
        Some(packets)
    };
    let decode = |packets: &[Vec<u8>]| -> Vec<VideoFrame> {
        let mut out = Vec::new();
        for (i, bytes) in packets.iter().enumerate() {
            let Ok(planes) = LosslessDecoder::new().decode_frame(&seq, bytes) else {
                continue;
            };
            // Reassemble the decoded planes in yuv420p10le layout so the
            // bit-exact check compares against the identically-laid-out source.
            let mut data = Vec::with_capacity(src.frames.first().map_or(0, |f| f.data.len()));
            for p in &planes {
                for s in &p.data {
                    data.extend_from_slice(&s.to_le_bytes());
                }
            }
            out.push(VideoFrame {
                pts: Timestamp::new(i as i64, (1, 30)),
                dts: Timestamp::new(i as i64, (1, 30)),
                data,
                width: w,
                height: h,
                pixel_format: PixelFormat::Yuv420p10le,
                is_key_frame: true,
            });
        }
        out
    };

    let mut references = vec![Reference::video(
        "FFV1",
        &["-c:v", "ffv1", "-level", "3", "-g", "1"],
        "mkv",
        "level 3, 10-bit, all-intra (-g 1)",
    )];
    if report.env["encoders"]["png"].as_bool().unwrap_or(false) {
        references.push(Reference::image2(
            "PNG",
            &["-c:v", "png"],
            "png",
            "image2 sequence (ffmpeg converts via RGB; size indicative only)",
        ));
    }
    push_original_rows(
        &OrigCompare {
            cfg,
            codec: "lossless",
            src: &src,
            encode: &encode,
            decode: &decode,
            bit_exact: true,
            references: &references,
            corpus_dir,
        },
        report,
    );
}

// ── Section: end-to-end CLI ──────────────────────────────────────────────────

fn run_e2e_section(cfg: &Config, corpus: &Corpus, report: &mut Report) {
    eprintln!("== End-to-end CLI transcode (VP9 MP4 → AV1) ==");
    let Some(cli) = ensure_cli_binary() else {
        report.skipped.push((
            "e2e".to_string(),
            "could not build tpt-kinetix-cli".to_string(),
        ));
        return;
    };

    let sizes: Vec<(u32, u32)> = if cfg.quick {
        vec![(320, 240)]
    } else {
        E2E.to_vec()
    };
    for (w, h) in sizes {
        let input = corpus.dir.join(format!("vp9mp4_{w}x{h}.mp4"));
        if !input.exists() {
            report.skipped.push((
                format!("e2e {w}x{h}"),
                "VP9 MP4 corpus clip missing".to_string(),
            ));
            continue;
        }
        let in_s = input.to_string_lossy().replace('\\', "/");

        // Kinetix CLI.
        let k_out = corpus.dir.join(format!("e2e_kinetix_{w}x{h}.mkv"));
        let k_args = vec![
            "transcode".to_string(),
            "--input".to_string(),
            in_s.clone(),
            "--output".to_string(),
            k_out.to_string_lossy().replace('\\', "/"),
        ];
        let mut k_best = f64::INFINITY;
        let mut k_ok = false;
        for _ in 0..cfg.runs {
            let t = Instant::now();
            let st = Command::new(&cli)
                .args(&k_args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            let dt = t.elapsed().as_secs_f64();
            if st.map(|s| s.success()).unwrap_or(false) {
                k_ok = true;
                k_best = k_best.min(dt);
            }
        }
        let k_bytes = std::fs::metadata(&k_out).map(|m| m.len()).unwrap_or(0);

        // ffmpeg: same VP9 input → AV1 in Matroska, to a real file like the CLI.
        let f_out = corpus.dir.join(format!("e2e_ffmpeg_{w}x{h}.mkv"));
        let fout_s = f_out.to_string_lossy().replace('\\', "/");
        let f_args = [
            "-nostats",
            "-i",
            &in_s,
            "-map",
            "0:v:0",
            "-an",
            "-c:v",
            "libaom-av1",
            "-cpu-used",
            "8",
            "-crf",
            "30",
            "-b:v",
            "0",
            "-row-mt",
            "1",
            "-y",
            &fout_s,
        ];
        let f_best = ffmpeg_bench_best(&f_args, None, cfg.runs);
        let f_bytes = std::fs::metadata(&f_out).map(|m| m.len()).unwrap_or(0);

        if !k_ok || f_best.is_none() {
            report.skipped.push((
                format!("e2e {w}x{h}"),
                "one side of the transcode failed".to_string(),
            ));
            continue;
        }
        let f = f_best.unwrap();
        report.e2e.push(json!({
            "input": format!("vp9 {w}x{h} mp4"),
            "kinetix_cli_s": round3(k_best),
            "kinetix_out_bytes": k_bytes,
            "ffmpeg_s": round3(f.rtime_s),
            "ffmpeg_out_bytes": f_bytes,
            "note": "kinetix CLI time includes process start-up; ffmpeg time is -benchmark rtime (excludes start-up)",
        }));
        println!(
            "  {w}x{h}: kinetix CLI {} ({} KiB)  ffmpeg {} ({} KiB)",
            fmt_secs(k_best),
            k_bytes / 1024,
            fmt_secs(f.rtime_s),
            f_bytes / 1024,
        );
    }
}

/// Locate (building once if needed) the release CLI binary.
fn ensure_cli_binary() -> Option<PathBuf> {
    let exe = format!("tpt-kinetix{}", std::env::consts::EXE_SUFFIX);
    let path = PathBuf::from("target/release").join(&exe);
    if path.exists() {
        return Some(path);
    }
    eprintln!("  building tpt-kinetix-cli (release) for the e2e section ...");
    let st = Command::new("cargo")
        .args(["build", "--release", "-p", "tpt-kinetix-cli"])
        .status()
        .ok()?;
    st.success().then_some(path).filter(|p| p.exists())
}

// ── Report rendering ─────────────────────────────────────────────────────────

fn render_json(cfg: &Config, report: &Report) -> String {
    let value = json!({
        "label": cfg.label,
        "date": today(),
        "phase": "todo-perf.md Phase 1 (Kinetix vs ffmpeg)",
        "quick": cfg.quick,
        "elapsed_s": round3(report.elapsed_s),
        "env": report.env,
        "decode": report.decode,
        "encode_av1": report.encode_av1,
        "originals": report.originals,
        "e2e": report.e2e,
        "skipped": report.skipped.iter().map(|(i, r)| json!({"item": i, "reason": r})).collect::<Vec<_>>(),
    });
    serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string())
}

fn md_env_table(env: &Value) -> String {
    let mut s = String::new();
    s.push_str("| Field | Value |\n|:---|:---|\n");
    for key in [
        "ffmpeg",
        "libavcodec",
        "rustc",
        "cargo",
        "cpu",
        "logical_cores",
        "os",
    ] {
        // `logical_cores` is a JSON number, the rest are strings.
        let cell = match &env[key] {
            Value::String(v) => v.clone(),
            Value::Number(n) => n.to_string(),
            other => other.to_string(),
        };
        s.push_str(&format!("| {key} | {cell} |\n"));
    }
    let list = |flag: &str, map: &Value| -> String {
        let mut acc: Vec<String> = Vec::new();
        if let Some(obj) = map.as_object() {
            for (k, v) in obj {
                if v.as_bool().unwrap_or(false) {
                    acc.push(k.clone());
                }
            }
        }
        format!("| {flag} | {} |\n", acc.join(", "))
    };
    s.push_str(&list("Reference decoders present", &env["decoders"]));
    s.push_str(&list("Reference encoders present", &env["encoders"]));
    s
}

fn render_markdown(cfg: &Config, report: &Report) -> String {
    let mut s = String::new();
    s.push_str(MARK_START);
    s.push_str("\n\n## Kinetix vs ffmpeg (todo-perf.md Phase 1)\n\n");
    s.push_str(&format!(
        "Generated by `just bench-ffmpeg` on **{}** (label `{}`{}). \
         Speeds are luma-MPix/s; ratios compare Kinetix against ffmpeg `-threads 1` \
         (the like-for-like single-thread case — Kinetix decoders are single-threaded); \
         >1.0× means Kinetix is faster.\n\n",
        today(),
        cfg.label,
        if cfg.quick { ", quick mode" } else { "" },
    ));

    s.push_str("### Reference toolchain\n\n");
    s.push_str(&md_env_table(&report.env));

    if !report.decode.is_empty() {
        s.push_str("### Decode: output verified identical, then timed\n\n");
        s.push_str("A checkmark means Kinetix's decoded planes were **byte-identical** to the reference decoder's before any timing was recorded.\n\n");
        s.push_str("| Codec | Clip | Frames | Verified | Kinetix | ffmpeg 1T | ffmpeg default | Kinetix/ffmpeg 1T | Peak mem Kinetix / ffmpeg 1T |\n");
        s.push_str("|:---|:---|---:|:---:|---:|---:|---:|---:|---:|\n");
        for r in &report.decode {
            let ratio = match r["ratio_kinetix_over_ffmpeg_1thread"].as_f64() {
                Some(v) => format!("{v:.2}×"),
                None => "n/a (unverified)".to_string(),
            };
            s.push_str(&format!(
                "| {} | {} | {} | {} | {} MPix/s | {} MPix/s | {} MPix/s | {} | {} |\n",
                r["codec"].as_str().unwrap_or("?").to_uppercase(),
                r["clip"].as_str().unwrap_or("?"),
                r["kinetix_frames"],
                if r["verified"].as_bool().unwrap_or(false) {
                    "✅"
                } else {
                    "❌"
                },
                r["kinetix_mpxs"],
                r["ffmpeg_1thread_mpxs"],
                r["ffmpeg_default_mpxs"],
                ratio,
                match (
                    r["kinetix_peak_heap_kb"].as_u64(),
                    r["ffmpeg_1thread_maxrss_kb"].as_u64(),
                ) {
                    (Some(k), Some(f)) =>
                        format!("{:.1} / {:.1} MiB", k as f64 / 1024.0, f as f64 / 1024.0),
                    (Some(k), None) => format!("{:.1} MiB / n/a", k as f64 / 1024.0),
                    _ => "n/a".to_string(),
                },
            ));
        }
        s.push_str("\nPeak memory: Kinetix = peak live heap during one decode pass, including the accumulated decoded output frames (excludes the input file); ffmpeg = process max RSS (includes the runtime), so the two are indicative, not like-for-like.\n");
        s.push('\n');
    }

    if !report.encode_av1.is_empty() {
        s.push_str("### AV1 encode: Kinetix vs libaom (through ffmpeg)\n\n");
        s.push_str("| Clip | Encoder | Encode time | Size (KiB) | Y-PSNR (dB) | Settings |\n");
        s.push_str("|:---|:---|---:|---:|---:|:---|\n");
        for r in &report.encode_av1 {
            for side in ["kinetix", "libaom"] {
                let e = &r[side];
                let time = e["enc_s"]
                    .as_f64()
                    .map(fmt_secs)
                    .unwrap_or_else(|| "failed".to_string());
                s.push_str(&format!(
                    "| {} | {} | {} | {} | {} | {} |\n",
                    r["clip"].as_str().unwrap_or("?"),
                    side,
                    time,
                    e["bytes"].as_u64().unwrap_or(0) / 1024,
                    e["y_psnr"]
                        .as_f64()
                        .map(|p| format!("{p:.2}"))
                        .unwrap_or_else(|| "n/a".into()),
                    e["settings"].as_str().unwrap_or(""),
                ));
            }
        }
        s.push('\n');
        s.push_str("> Quality is roughly matched (CQP 100 ≈ CRF 30 on this content) but not identical — compare the PSNR column before trusting a size ratio. VP9 has no Kinetix encoder (libvpx is the only VP9 reference).\n\n");
    }

    if !report.originals.is_empty() {
        s.push_str(
            "### Original codecs vs the closest standard reference

",
        );
        s.push_str("| Codec | Encoder | Enc time | Dec time | Size (KiB) | Y-PSNR (dB) | Roundtrip | Settings |
");
        s.push_str(
            "|:---|:---|---:|---:|---:|---:|:---|:---|
",
        );
        for r in &report.originals {
            let roundtrip = if r["encoder"].as_str() == Some("kinetix (all-intra)") {
                let frames = format!(
                    "{}/{}",
                    r["decoded_frames"].as_u64().unwrap_or(0),
                    r["source_frames"].as_u64().unwrap_or(0)
                );
                // The row carries both the expectation (`lossless_expected`) and the
                // measurement (`bit_exact_measured`), so all four combinations are
                // distinguishable. The screen special-case that used to live here
                // is gone: it rendered a *declared-lossless* codec's failure as
                // "lossy", which is exactly the case that must read as a
                // failure. `bit_exact` is the conjunction and is kept as the
                // fallback for rows written before those fields existed.
                let expected = r["lossless_expected"]
                    .as_bool()
                    .unwrap_or(r["bit_exact"].as_bool().unwrap_or(false));
                let measured = r["bit_exact_measured"]
                    .as_bool()
                    .unwrap_or(r["bit_exact"].as_bool().unwrap_or(false));
                match (expected, measured) {
                    (true, true) => format!("bit-exact ({frames})"),
                    // Declared lossless but the round-trip did not reproduce the
                    // source: a correctness bug, not a quality setting.
                    (true, false) => format!("**MISMATCH** ({frames} frames)"),
                    (false, true) => format!("bit-exact ({frames}, unexpected)"),
                    // Lossy by design: reproducing the source exactly is not
                    // required, so a non-bit-exact round-trip is the expected
                    // outcome rather than a defect.
                    (false, false) => format!("lossy ({frames} frames)"),
                }
            } else {
                "-".to_string()
            };
            s.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} |
",
                r["codec"].as_str().unwrap_or("?"),
                r["encoder"].as_str().unwrap_or("?"),
                r["enc_s"]
                    .as_f64()
                    .map(fmt_secs)
                    .unwrap_or_else(|| "n/a".into()),
                r["dec_s"]
                    .as_f64()
                    .map(fmt_secs)
                    .unwrap_or_else(|| "n/a".into()),
                r["bytes"].as_u64().unwrap_or(0) / 1024,
                r["y_psnr"]
                    .as_f64()
                    .map(|p| format!("{p:.2}"))
                    .unwrap_or_else(|| "n/a".into()),
                roundtrip,
                r["settings"].as_str().unwrap_or(""),
            ));
        }
        s.push('\n');
        s.push_str("> Kinetix originals encode all frames intra here; the reference encoders run their normal GOP, so size ratios are indicative, not like-for-like. `Roundtrip` compares Kinetix's decode against its own source: `MISMATCH` rows are correctness bugs (see todo-perf.md Phase 1 findings), not measurement noise.\n\n");
    }

    if !report.e2e.is_empty() {
        s.push_str("### End-to-end CLI transcode (VP9 MP4 → AV1)\n\n");
        s.push_str("| Input | tpt-kinetix CLI | Output KiB | ffmpeg CLI | Output KiB |\n");
        s.push_str("|:---|---:|---:|---:|---:|\n");
        for r in &report.e2e {
            s.push_str(&format!(
                "| {} | {} s | {} | {} s | {} |\n",
                r["input"].as_str().unwrap_or("?"),
                r["kinetix_cli_s"],
                r["kinetix_out_bytes"].as_u64().unwrap_or(0) / 1024,
                r["ffmpeg_s"],
                r["ffmpeg_out_bytes"].as_u64().unwrap_or(0) / 1024,
            ));
        }
        s.push('\n');
    }

    if !report.skipped.is_empty() {
        s.push_str("### Skipped\n\n");
        for (item, reason) in &report.skipped {
            s.push_str(&format!("- **{}** — {}\n", item.replace('\\', "/"), reason));
        }
        s.push('\n');
    }

    s.push_str("### Methodology\n\n");
    s.push_str(
        "- ffmpeg is timed with `-benchmark -f null -` (the `rtime=` figure: wall time inside \
         the transcode loop, excluding process spawn); Kinetix is timed in-process, best of \
         the recorded runs, fresh decoder per run.\n\
         - Decode correctness is verified **before** timing: Kinetix planes must be byte-identical \
         to the reference decoder's, otherwise the ratio column reads `n/a (unverified)`.\n\
         - Corpus: ffmpeg-generated `testsrc` clips (cached in `target/perf-corpus/`, regenerated \
         when the generator arguments change) plus the committed AV1 FATE fixtures.\n\
         - Re-run after any optimisation change; numbers are only comparable within one machine \
         and toolchain (see the Phase 0 caveat above).\n\n",
    );
    s.push_str(MARK_END);
    s.push('\n');
    s
}

/// Replace (or append) the marked ffmpeg-compare section of PERFORMANCE.md.
fn splice_markdown_section(path: &str, section: &str) {
    let path = Path::new(path);
    let existing = std::fs::read_to_string(path).ok();
    let next = match existing {
        Some(text) if text.contains(MARK_START) && text.contains(MARK_END) => {
            let start = text.find(MARK_START).expect("start marker");
            let end = text.find(MARK_END).expect("end marker") + MARK_END.len();
            format!("{}{}", &text[..start], &text[end..])
        }
        other => other.unwrap_or_default(),
    };
    let mut next = next.trim_end().to_string();
    next.push_str("\n\n");
    next.push_str(section.trim_end());
    next.push('\n');
    write_file(path.to_str().expect("utf-8 path"), &next);
}

fn write_file(path: &str, contents: &str) {
    if let Some(parent) = Path::new(path).parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("could not create {}: {e}", parent.display());
            std::process::exit(1);
        }
    }
    if let Err(e) = std::fs::write(path, contents) {
        eprintln!("could not write {path}: {e}");
        std::process::exit(1);
    }
}

fn today() -> String {
    // RFC 3339 UTC date from the system clock. No `chrono` dependency: read the
    // seconds since the Unix epoch and do the civil-date arithmetic inline.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    // Howard Hinnant's civil_from_days algorithm.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bench_stderr_parses_rtime_and_maxrss() {
        let stderr = "frame=  30 fps=0.0 speed=32.7x\n\
                      bench: utime=0.000s stime=0.000s rtime=0.031s\n\
                      bench: maxrss=34156kB\n";
        let b = parse_bench_stderr(stderr).expect("parse");
        assert!((b.rtime_s - 0.031).abs() < 1e-9);
        assert_eq!(b.maxrss_kb, Some(34156));
    }

    #[test]
    fn bench_stderr_without_bench_line_is_none() {
        assert!(parse_bench_stderr("nothing here\n").is_none());
    }

    #[test]
    fn ffmpeg_presence_check_is_word_delimited() {
        // "libvpx-vp9" must not match a hypothetical "vp9" check and vice versa;
        // exercised via the same substring rule ffmpeg_has uses.
        let list = " V....D libaom-av1           libaom AV1 (codec av1)\n V.S..D ffv1 FFmpeg video codec #1\n";
        assert!(list.contains(" libaom-av1 "));
        assert!(!list.contains(" vp9 "));
    }

    #[test]
    fn marker_splice_replaces_and_appends() {
        let existing =
            "phase 0 table\n<!-- ffmpeg-compare:start -->\nold\n<!-- ffmpeg-compare:end -->\n";
        // Simulate splice via the same logic the function uses.
        let start = existing.find(MARK_START).unwrap();
        let end = existing.find(MARK_END).unwrap() + MARK_END.len();
        let stripped = format!("{}{}", &existing[..start], &existing[end..]);
        assert_eq!(stripped.trim_end(), "phase 0 table");

        let no_marker = "only phase 0";
        assert!(!no_marker.contains(MARK_START));
    }

    #[test]
    fn today_is_a_well_formed_iso_date() {
        let d = today();
        assert_eq!(d.len(), 10);
        assert_eq!(d.as_bytes()[4], b'-');
        assert_eq!(d.as_bytes()[7], b'-');
    }
}
