//! Evidence for the I/O layer (todo-io.md M6): Kinetix's CLI vs ffprobe on the
//! things the project actually claims — startup cost, memory, and survival on
//! hostile input.
//!
//! Sections (default: all):
//!   `--startup`  wall time and peak RSS of `probe` on a real MP4, best of N.
//!   `--hostile`  a deterministic corpus of mutated MP4s fed to both tools with
//!                a timeout; counts clean results, rejected inputs, crashes and
//!                hangs. A rejection (non-zero exit with a message) is a *good*
//!                outcome; only crashes and hangs are failures.
//!
//! Usage: `cargo run --release -p tpt-kinetix-test-utils --example io_compare --
//!         [--startup|--hostile] [--seed <mp4>] [--variants <n>] [--runs <n>]`
//!
//! Needs `ffprobe` on PATH and a release `tpt-kinetix` binary (built on demand).
//! Peak RSS is measured on Windows only (the child's `PeakWorkingSetSize`).

use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const CORPUS_DIR: &str = "target/io-corpus";
const SEED_CANDIDATES: [&str; 2] = [
    "target/perf-corpus/vp9mp4_1280x720.mp4",
    "target/perf-corpus/vp9mp4_320x240.mp4",
];
const HANG_AFTER: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Outcome {
    /// Exit 0.
    Ok,
    /// Non-zero exit that the tool chose (error message, no crash).
    Rejected,
    /// Killed by a signal / Windows exception code, or panicked (exit 101).
    Crash,
    /// Still running after `HANG_AFTER`.
    Hang,
}

struct Tool {
    name: &'static str,
    program: PathBuf,
    args: fn(&Path) -> Vec<String>,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |f: &str| args.iter().any(|a| a == f);
    let value = |f: &str| {
        args.iter()
            .position(|a| a == f)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let all = !flag("--startup") && !flag("--hostile") && !flag("--density") && !flag("--remote");
    let variants: usize = value("--variants")
        .and_then(|v| v.parse().ok())
        .unwrap_or(300);
    let runs: usize = value("--runs").and_then(|v| v.parse().ok()).unwrap_or(15);

    let Some(seed) = value("--seed").map(PathBuf::from).or_else(|| {
        SEED_CANDIDATES
            .iter()
            .map(PathBuf::from)
            .find(|p| p.exists())
    }) else {
        eprintln!("no seed MP4: run `just bench-ffmpeg --e2e` first or pass --seed <mp4>");
        std::process::exit(2);
    };
    let Some(cli) = ensure_cli_binary() else {
        eprintln!("could not build tpt-kinetix-cli");
        std::process::exit(2);
    };
    if Command::new("ffprobe")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
    {
        eprintln!("ffprobe not found on PATH");
        std::process::exit(2);
    }

    let tools = [
        Tool {
            name: "tpt-kinetix probe",
            program: cli,
            args: |p| vec!["probe".into(), p.to_string_lossy().into_owned()],
        },
        Tool {
            name: "ffprobe -show_streams -show_format",
            program: PathBuf::from("ffprobe"),
            args: |p| {
                vec![
                    "-v".into(),
                    "error".into(),
                    "-show_streams".into(),
                    "-show_format".into(),
                    p.to_string_lossy().into_owned(),
                ]
            },
        },
    ];

    println!("seed: {}", seed.display());
    if all || flag("--startup") {
        startup(&tools, &seed, runs);
    }
    if all || flag("--hostile") {
        hostile(&tools, &seed, variants);
    }
    if all || flag("--remote") {
        let rtts: Vec<u64> = value("--rtts")
            .map(|v| v.split(',').filter_map(|n| n.trim().parse().ok()).collect())
            .unwrap_or_else(|| vec![0, 20, 50, 100]);
        remote_probe(&tools[0].program, &rtts, runs.min(5));
    }
    if all || flag("--density") {
        let streams: Vec<usize> = value("--streams")
            .map(|v| v.split(',').filter_map(|n| n.trim().parse().ok()).collect())
            .unwrap_or_else(|| vec![1, 10, 40]);
        let secs: u64 = value("--secs").and_then(|v| v.parse().ok()).unwrap_or(20);
        let viewers: usize = value("--viewers").and_then(|v| v.parse().ok()).unwrap_or(0);
        density(&tools[0].program, &streams, secs, viewers);
    }
}

fn ensure_cli_binary() -> Option<PathBuf> {
    let path = PathBuf::from("target/release")
        .join(format!("tpt-kinetix{}", std::env::consts::EXE_SUFFIX));
    if path.exists() {
        return Some(path);
    }
    let ok = Command::new("cargo")
        .args(["build", "--release", "-p", "tpt-kinetix-cli"])
        .status()
        .ok()?
        .success();
    (ok && path.exists()).then_some(path)
}

// ── Running a tool ───────────────────────────────────────────────────────────

struct Run {
    outcome: Outcome,
    secs: f64,
    peak_rss_kb: Option<u64>,
}

fn run_tool(tool: &Tool, input: &Path) -> Run {
    let t = Instant::now();
    let mut child = match Command::new(&tool.program)
        .args((tool.args)(input))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => {
            return Run {
                outcome: Outcome::Crash,
                secs: 0.0,
                peak_rss_kb: None,
            }
        }
    };
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let secs = t.elapsed().as_secs_f64();
                let outcome = match status.code() {
                    Some(0) => Outcome::Ok,
                    // 101 is Rust's panic exit; Windows exception codes
                    // (0xC0000005 ...) and unix signals (None) are crashes.
                    Some(101) | None => Outcome::Crash,
                    Some(c) if (c as u32) >= 0xC000_0000 => Outcome::Crash,
                    Some(_) => Outcome::Rejected,
                };
                return Run {
                    outcome,
                    secs,
                    peak_rss_kb: peak_rss_kb(&child),
                };
            }
            Ok(None) if t.elapsed() > HANG_AFTER => {
                child.kill().ok();
                child.wait().ok();
                return Run {
                    outcome: Outcome::Hang,
                    secs: t.elapsed().as_secs_f64(),
                    peak_rss_kb: None,
                };
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(1)),
            Err(_) => {
                return Run {
                    outcome: Outcome::Crash,
                    secs: t.elapsed().as_secs_f64(),
                    peak_rss_kb: None,
                }
            }
        }
    }
}

#[cfg(windows)]
fn peak_rss_kb(child: &Child) -> Option<u64> {
    use std::os::windows::io::AsRawHandle;

    #[repr(C)]
    struct ProcessMemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn K32GetProcessMemoryInfo(
            process: *mut core::ffi::c_void,
            counters: *mut ProcessMemoryCounters,
            cb: u32,
        ) -> i32;
    }
    let mut c = ProcessMemoryCounters {
        cb: std::mem::size_of::<ProcessMemoryCounters>() as u32,
        page_fault_count: 0,
        peak_working_set_size: 0,
        working_set_size: 0,
        quota_peak_paged_pool_usage: 0,
        quota_paged_pool_usage: 0,
        quota_peak_non_paged_pool_usage: 0,
        quota_non_paged_pool_usage: 0,
        pagefile_usage: 0,
        peak_pagefile_usage: 0,
    };
    // SAFETY: the handle is the live `Child` handle; `c` is a correctly sized,
    // writable PROCESS_MEMORY_COUNTERS.
    let ok = unsafe { K32GetProcessMemoryInfo(child.as_raw_handle().cast(), &mut c, c.cb) };
    (ok != 0).then_some((c.peak_working_set_size / 1024) as u64)
}

#[cfg(not(windows))]
fn peak_rss_kb(_child: &Child) -> Option<u64> {
    None
}

/// User + kernel CPU seconds the child has consumed so far (Windows only).
#[cfg(windows)]
fn cpu_secs(child: &Child) -> Option<f64> {
    use std::os::windows::io::AsRawHandle;
    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct FileTime {
        lo: u32,
        hi: u32,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetProcessTimes(
            process: *mut core::ffi::c_void,
            creation: *mut FileTime,
            exit: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
    }
    let (mut c, mut e, mut k, mut u) = (
        FileTime::default(),
        FileTime::default(),
        FileTime::default(),
        FileTime::default(),
    );
    // SAFETY: live child handle; four writable FILETIMEs.
    let ok =
        unsafe { GetProcessTimes(child.as_raw_handle().cast(), &mut c, &mut e, &mut k, &mut u) };
    let t = |f: FileTime| (((f.hi as u64) << 32) | f.lo as u64) as f64 / 1e7;
    (ok != 0).then(|| t(k) + t(u))
}

#[cfg(not(windows))]
fn cpu_secs(_child: &Child) -> Option<f64> {
    None
}

// ── Startup ──────────────────────────────────────────────────────────────────

fn startup(tools: &[Tool], seed: &Path, runs: usize) {
    println!("\n== Startup: probe of one MP4, best of {runs} ==");
    let size = std::fs::metadata(seed).map(|m| m.len()).unwrap_or(0);
    println!("file: {} KiB", size / 1024);
    for tool in tools {
        let mut best = f64::INFINITY;
        let mut rss = None;
        for _ in 0..runs {
            let r = run_tool(tool, seed);
            if r.outcome == Outcome::Ok && r.secs < best {
                best = r.secs;
                rss = r.peak_rss_kb;
            }
        }
        println!(
            "  {:<38} {:>8.1} ms   peak RSS {}",
            tool.name,
            best * 1000.0,
            rss.map(|k| format!("{:.1} MiB", k as f64 / 1024.0))
                .unwrap_or_else(|| "n/a".into())
        );
    }
}

// ── Hostile input ────────────────────────────────────────────────────────────

struct XorShift(u64);
impl XorShift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// Offsets of the `[size][type]` headers of the top-level and `moov`-nested
/// boxes, so mutations can target structure and not only random bytes.
fn box_header_offsets(data: &[u8]) -> Vec<usize> {
    fn walk(data: &[u8], mut pos: usize, end: usize, depth: u32, out: &mut Vec<usize>) {
        while pos + 8 <= end {
            let size = u32::from_be_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
            if size < 8 || pos + size > end {
                break;
            }
            out.push(pos);
            let kind = &data[pos + 4..pos + 8];
            if depth < 6 && matches!(kind, b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl") {
                walk(data, pos + 8, pos + size, depth + 1, out);
            }
            pos += size;
        }
    }
    let mut out = Vec::new();
    walk(data, 0, data.len(), 0, &mut out);
    out
}

fn mutate(seed: &[u8], headers: &[usize], rng: &mut XorShift, i: usize) -> Vec<u8> {
    let mut d = seed.to_vec();
    match i % 6 {
        // Flip a handful of random bytes.
        0 => {
            for _ in 0..1 + rng.below(16) {
                let p = rng.below(d.len());
                d[p] ^= 1 << rng.below(8);
            }
        }
        // Truncate anywhere.
        1 => d.truncate(rng.below(d.len())),
        // Corrupt a box size field with an extreme or off-by-N value.
        2 if !headers.is_empty() => {
            let h = headers[rng.below(headers.len())];
            let v: u32 = match rng.below(4) {
                0 => 0,
                1 => u32::MAX,
                2 => 7,
                _ => rng.next() as u32,
            };
            d[h..h + 4].copy_from_slice(&v.to_be_bytes());
        }
        // Corrupt a box type.
        3 if !headers.is_empty() => {
            let h = headers[rng.below(headers.len())];
            for b in &mut d[h + 4..h + 8] {
                *b = rng.next() as u8;
            }
        }
        // Zero-fill a window inside the file (hits sample tables).
        4 => {
            let p = rng.below(d.len());
            let n = (1 + rng.below(512)).min(d.len() - p);
            d[p..p + n].fill(0);
        }
        // Overwrite a window with 0xFF (huge counts / offsets).
        _ => {
            let p = rng.below(d.len());
            let n = (1 + rng.below(64)).min(d.len() - p);
            d[p..p + n].fill(0xFF);
        }
    }
    d
}

fn hostile(tools: &[Tool], seed: &Path, variants: usize) {
    println!("\n== Hostile input: {variants} mutated MP4s, {HANG_AFTER:?} hang limit ==");
    let seed_bytes = match std::fs::read(seed) {
        Ok(b) if b.len() > 64 => b,
        _ => {
            eprintln!("cannot read seed");
            return;
        }
    };
    let dir = Path::new(CORPUS_DIR);
    std::fs::create_dir_all(dir).expect("create corpus dir");
    let headers = box_header_offsets(&seed_bytes);
    let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);

    let mut counts = vec![[0usize; 4]; tools.len()];
    let mut failures: Vec<String> = Vec::new();
    for i in 0..variants {
        let path = dir.join(format!("hostile_{i:04}.mp4"));
        std::fs::write(&path, mutate(&seed_bytes, &headers, &mut rng, i)).expect("write variant");
        for (t, tool) in tools.iter().enumerate() {
            let r = run_tool(tool, &path);
            counts[t][r.outcome as usize] += 1;
            if matches!(r.outcome, Outcome::Crash | Outcome::Hang) {
                failures.push(format!(
                    "{:?}: {} on {}",
                    r.outcome,
                    tool.name,
                    path.display()
                ));
            } else if i >= 20 {
                // Keep the directory small: only failures are worth keeping.
                if t == tools.len() - 1
                    && !failures
                        .iter()
                        .any(|f| f.ends_with(&*path.to_string_lossy()))
                {
                    std::fs::remove_file(&path).ok();
                }
            }
        }
    }
    println!(
        "  {:<38} {:>6} {:>9} {:>7} {:>6}",
        "tool", "ok", "rejected", "CRASH", "HANG"
    );
    for (t, tool) in tools.iter().enumerate() {
        let c = counts[t];
        println!(
            "  {:<38} {:>6} {:>9} {:>7} {:>6}",
            tool.name,
            c[Outcome::Ok as usize],
            c[Outcome::Rejected as usize],
            c[Outcome::Crash as usize],
            c[Outcome::Hang as usize]
        );
    }
    for f in &failures {
        println!("  {f}");
    }
}

// ── Density ──────────────────────────────────────────────────────────────────

fn quiet(cmd: &mut Command) -> &mut Command {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
}

/// One bounded GET; returns the HTTP status (0 on failure).
fn http_get_status(port: u16, path: &str) -> u16 {
    use std::io::{Read, Write};
    let Ok(mut s) = std::net::TcpStream::connect(("127.0.0.1", port)) else {
        return 0;
    };
    s.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let req = format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    if s.write_all(req.as_bytes()).is_err() {
        return 0;
    }
    let mut head = [0u8; 16];
    let n = s.read(&mut head).unwrap_or(0);
    String::from_utf8_lossy(&head[..n])
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0)
}

struct Density {
    streams: usize,
    live: usize,
    cpu_secs: f64,
    peak_rss_mib: Option<f64>,
    viewers: Arc<ViewerStats>,
}

fn density(cli: &Path, counts: &[usize], secs: u64, viewers: usize) {
    println!(
        "\n== Density: N concurrent live streams, {secs} s window, 320x240 VP9+Opus passthrough =="
    );
    let dir = Path::new(CORPUS_DIR);
    std::fs::create_dir_all(dir).expect("create corpus dir");
    let src = dir.join("density_src.webm");
    if !src.exists() {
        let ok = quiet(
            Command::new("ffmpeg")
                .args([
                    "-y",
                    "-loglevel",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=320x240:rate=25",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=440:sample_rate=48000",
                    "-t",
                    "120",
                    "-c:v",
                    "libvpx-vp9",
                    "-deadline",
                    "realtime",
                    "-cpu-used",
                    "8",
                    "-b:v",
                    "300k",
                    "-g",
                    "50",
                    "-pix_fmt",
                    "yuv420p",
                    "-c:a",
                    "libopus",
                    "-b:a",
                    "48k",
                ])
                .arg(&src),
        )
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
        if !ok {
            println!("  could not create the source WebM (needs ffmpeg with libvpx-vp9/libopus); skipping");
            return;
        }
    }
    let src_s = src.to_string_lossy().into_owned();
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);

    let mut rows: Vec<(Density, Density)> = Vec::new();
    for (round, &n) in counts.iter().enumerate() {
        // Kinetix: one server, N ffmpeg publishers (their cost belongs to the
        // remote encoder and is not counted).
        let port = 18_700 + round as u16;
        let mut server = quiet(Command::new(cli).args([
            "live",
            "--port",
            &port.to_string(),
            "--segment-seconds",
            "2",
            "--part-seconds",
            "0",
        ]))
        .spawn()
        .expect("spawn tpt-kinetix live");
        std::thread::sleep(Duration::from_millis(800));
        let mut pubs: Vec<Child> = (0..n)
            .map(|i| {
                quiet(Command::new("ffmpeg").args([
                    "-re",
                    "-i",
                    &src_s,
                    "-c",
                    "copy",
                    "-f",
                    "webm",
                    "-method",
                    "POST",
                    &format!("http://127.0.0.1:{port}/ingest/s{i}"),
                ]))
                .spawn()
                .expect("spawn publisher")
            })
            .collect();
        let stop = Arc::new(AtomicBool::new(false));
        let kv = Arc::new(ViewerStats::default());
        let viewer_threads: Vec<_> = (0..n * viewers)
            .map(|v| {
                let (stop, st) = (stop.clone(), kv.clone());
                let entry = format!("/s{}/master.m3u8", v % n);
                std::thread::spawn(move || viewer(port, entry, stop, st))
            })
            .collect();
        std::thread::sleep(Duration::from_secs(secs));
        let live = (0..n)
            .filter(|i| http_get_status(port, &format!("/s{i}/master.m3u8")) == 200)
            .count();
        let k = Density {
            streams: n,
            live,
            cpu_secs: cpu_secs(&server).unwrap_or(f64::NAN),
            peak_rss_mib: peak_rss_kb(&server).map(|k| k as f64 / 1024.0),
            viewers: kv.clone(),
        };
        stop.store(true, Ordering::Relaxed);
        for t in viewer_threads {
            t.join().ok();
        }
        for p in &mut pubs {
            p.kill().ok();
            p.wait().ok();
        }
        server.kill().ok();
        server.wait().ok();

        // ffmpeg: one HLS-writing process per stream.
        let out_root = dir.join(format!("density_hls_{n}"));
        std::fs::remove_dir_all(&out_root).ok();
        let mut procs: Vec<(Child, PathBuf)> = (0..n)
            .map(|i| {
                let d = out_root.join(format!("s{i}"));
                std::fs::create_dir_all(&d).expect("create hls dir");
                let playlist = d.join("index.m3u8");
                let child = quiet(
                    Command::new("ffmpeg")
                        .args([
                            "-re",
                            "-i",
                            &src_s,
                            "-c",
                            "copy",
                            "-f",
                            "hls",
                            "-hls_time",
                            "2",
                            "-hls_list_size",
                            "6",
                            "-hls_segment_type",
                            "fmp4",
                            "-hls_flags",
                            "delete_segments",
                        ])
                        .arg(&playlist),
                )
                .spawn()
                .expect("spawn ffmpeg hls");
                (child, playlist)
            })
            .collect();
        let stop = Arc::new(AtomicBool::new(false));
        let fv = Arc::new(ViewerStats::default());
        let static_port = spawn_static_server(out_root.clone());
        let viewer_threads: Vec<_> = (0..n * viewers)
            .map(|v| {
                let (stop, st) = (stop.clone(), fv.clone());
                let entry = format!("/s{}/index.m3u8", v % n);
                std::thread::spawn(move || viewer(static_port, entry, stop, st))
            })
            .collect();
        std::thread::sleep(Duration::from_secs(secs));
        let f = Density {
            streams: n,
            live: procs.iter().filter(|(_, p)| p.exists()).count(),
            cpu_secs: procs.iter().filter_map(|(c, _)| cpu_secs(c)).sum(),
            peak_rss_mib: procs
                .iter()
                .map(|(c, _)| peak_rss_kb(c).map(|k| k as f64 / 1024.0))
                .sum::<Option<f64>>(),
            viewers: fv.clone(),
        };
        stop.store(true, Ordering::Relaxed);
        for t in viewer_threads {
            t.join().ok();
        }
        for (c, _) in &mut procs {
            c.kill().ok();
            c.wait().ok();
        }
        std::fs::remove_dir_all(&out_root).ok();
        rows.push((k, f));
    }

    println!("  ({cores} cores; CPU % = process CPU seconds over the window, of one core)");
    println!(
        "  {:>7}  {:<28} {:>6} {:>9} {:>13} {:>14}",
        "streams", "setup", "live", "CPU %", "peak RSS MiB", "streams/core*"
    );
    for (k, f) in &rows {
        for (name, d) in [
            ("tpt-kinetix live (1 proc)", k),
            ("ffmpeg -f hls (N procs)", f),
        ] {
            let cpu_pct = d.cpu_secs / secs as f64 * 100.0;
            println!(
                "  {:>7}  {:<28} {:>3}/{:<2} {:>9.1} {:>13} {:>14.0}",
                d.streams,
                name,
                d.live,
                d.streams,
                cpu_pct,
                d.peak_rss_mib
                    .map(|m| format!("{m:.1}"))
                    .unwrap_or_else(|| "n/a".into()),
                100.0 * d.streams as f64 / cpu_pct.max(0.01),
            );
        }
    }
    println!("  * streams one core could carry at this CPU cost (extrapolated).");
    if viewers > 0 {
        println!("  {viewers} viewer(s) per stream; viewer requests / MiB served / errors:");
        for (k, f) in &rows {
            for (name, d) in [("kinetix", k), ("ffmpeg ", f)] {
                println!(
                    "    {:>3} streams {name}: {} req, {:.1} MiB, {} errors",
                    d.streams,
                    d.viewers.requests.load(Ordering::Relaxed),
                    d.viewers.bytes.load(Ordering::Relaxed) as f64 / 1_048_576.0,
                    d.viewers.errors.load(Ordering::Relaxed),
                );
            }
        }
        println!("  (ffmpeg arm is served by this harness static file server, whose CPU is not counted: favourable to ffmpeg)");
    }
}

// ── Viewers (HLS clients) and a static server for the ffmpeg arm ─────────────

use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc,
    },
};

#[derive(Default)]
struct ViewerStats {
    requests: AtomicU64,
    bytes: AtomicU64,
    errors: AtomicU64,
}

/// GET a path on localhost; `Some(body)` only for a 200.
fn http_get(port: u16, path: &str) -> Option<Vec<u8>> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(5))).ok();
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").as_bytes())
        .ok()?;
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).ok()?;
    let split = buf.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&buf[..split]);
    let status: u16 = head.split_whitespace().nth(1)?.parse().ok()?;
    (status == 200).then(|| buf[split + 4..].to_vec())
}

fn resolve(base: &str, rel: &str) -> String {
    if rel.starts_with('/') {
        return rel.to_string();
    }
    let dir = base.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    format!("{dir}/{rel}")
}

/// Playlist URIs worth fetching: `EXT-X-MAP` init, parts and segments.
fn playlist_uris(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(i) = line.find("URI=\"") {
            let rest = &line[i + 5..];
            if let Some(j) = rest.find('"') {
                out.push(rest[..j].to_string());
            }
        } else if !line.is_empty() && !line.starts_with('#') {
            out.push(line.to_string());
        }
    }
    out
}

/// A minimal HLS client: reload the playlist twice a second and fetch whatever
/// it has not seen yet, like a player staying at the live edge.
fn viewer(port: u16, entry: String, stop: Arc<AtomicBool>, stats: Arc<ViewerStats>) {
    let mut seen = std::collections::HashSet::new();
    let get = |path: &str| {
        stats.requests.fetch_add(1, Ordering::Relaxed);
        match http_get(port, path) {
            Some(b) => {
                stats.bytes.fetch_add(b.len() as u64, Ordering::Relaxed);
                Some(b)
            }
            None => {
                stats.errors.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    };
    // Playlists (master, video, audio) are re-polled; media files are fetched once.
    fn walk(
        path: &str,
        depth: u32,
        seen: &mut std::collections::HashSet<String>,
        get: &dyn Fn(&str) -> Option<Vec<u8>>,
    ) {
        let Some(body) = get(path) else { return };
        for uri in playlist_uris(&String::from_utf8_lossy(&body)) {
            let p = resolve(path, &uri);
            if uri.contains(".m3u8") {
                if depth < 3 {
                    walk(&p, depth + 1, seen, get);
                }
            } else if seen.insert(p.clone()) {
                get(&p);
            }
        }
    }
    while !stop.load(Ordering::Relaxed) {
        walk(&entry, 0, &mut seen, &get);
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Serve `root` over one-shot HTTP connections (the ffmpeg arm has no server of
/// its own). Runs until the process exits.
fn spawn_static_server(root: PathBuf) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind static server");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            let root = root.clone();
            std::thread::spawn(move || {
                let mut conn = conn;
                let mut req = [0u8; 2048];
                let n = conn.read(&mut req).unwrap_or(0);
                let line = String::from_utf8_lossy(&req[..n]);
                let path = line.split_whitespace().nth(1).unwrap_or("/");
                let rel = path.trim_start_matches('/').split('?').next().unwrap_or("");
                let body = if rel.contains("..") {
                    None
                } else {
                    std::fs::read(root.join(rel)).ok()
                };
                match body {
                    Some(b) => {
                        let _ = conn.write_all(
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                b.len()
                            )
                            .as_bytes(),
                        );
                        let _ = conn.write_all(&b);
                    }
                    None => {
                        let _ = conn.write_all(
                            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        );
                    }
                }
            });
        }
    });
    port
}

// ── Remote probe through a latency-injecting proxy ───────────────────────────

/// Counters for the Range server: requests and body bytes actually written.
#[derive(Default)]
struct RangeCounters {
    requests: AtomicU64,
    bytes: AtomicU64,
}

/// A small keep-alive HTTP server for one in-memory file with `Range` support.
fn spawn_range_server(data: Arc<Vec<u8>>, counters: Arc<RangeCounters>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind range server");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            let (data, counters) = (data.clone(), counters.clone());
            std::thread::spawn(move || range_conn(conn, &data, &counters));
        }
    });
    port
}

fn range_conn(mut conn: TcpStream, data: &[u8], counters: &RangeCounters) {
    let mut pending: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let end = loop {
            if let Some(p) = pending.windows(4).position(|w| w == b"\r\n\r\n") {
                break p + 4;
            }
            match conn.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => pending.extend_from_slice(&buf[..n]),
            }
        };
        let head = String::from_utf8_lossy(&pending[..end]).into_owned();
        pending.drain(..end);
        counters.requests.fetch_add(1, Ordering::Relaxed);
        let is_head = head.starts_with("HEAD ");
        let total = data.len();
        let range = head
            .lines()
            .find_map(|l| {
                l.to_ascii_lowercase()
                    .strip_prefix("range: bytes=")
                    .map(str::to_string)
            })
            .and_then(|r| {
                let (a, b) = r.trim().split_once('-')?;
                let a: usize = a.parse().ok()?;
                let b: usize = b.parse().unwrap_or(total - 1).min(total - 1);
                (a <= b).then_some((a, b))
            });
        let (status, a, b) = match range {
            Some((a, b)) => ("206 Partial Content", a, b),
            None => ("200 OK", 0, total.saturating_sub(1)),
        };
        let len = b + 1 - a;
        let mut resp = format!(
            "HTTP/1.1 {status}\r\nAccept-Ranges: bytes\r\nContent-Type: video/mp4\r\nContent-Length: {len}\r\n"
        );
        if range.is_some() {
            resp.push_str(&format!("Content-Range: bytes {a}-{b}/{total}\r\n"));
        }
        resp.push_str("\r\n");
        if conn.write_all(resp.as_bytes()).is_err() {
            return;
        }
        if !is_head {
            for chunk in data[a..=b].chunks(65536) {
                if conn.write_all(chunk).is_err() {
                    return;
                }
                counters
                    .bytes
                    .fetch_add(chunk.len() as u64, Ordering::Relaxed);
            }
        }
    }
}

/// Forward bytes `from` -> `to`, releasing each read `delay` after it arrived
/// (latency without throttling throughput).
fn pump(mut from: TcpStream, mut to: TcpStream, delay: Duration) {
    let (tx, rx) = mpsc::channel::<(Instant, Vec<u8>)>();
    let writer = std::thread::spawn(move || {
        for (t, b) in rx {
            let due = t + delay;
            let now = Instant::now();
            if due > now {
                std::thread::sleep(due - now);
            }
            if to.write_all(&b).is_err() {
                break;
            }
        }
        let _ = to.shutdown(std::net::Shutdown::Write);
    });
    let mut buf = [0u8; 65536];
    loop {
        match from.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if tx.send((Instant::now(), buf[..n].to_vec())).is_err() {
                    break;
                }
            }
        }
    }
    drop(tx);
    writer.join().ok();
}

/// A TCP proxy adding `one_way` delay in each direction (and one round trip for
/// the connection setup). Returns the proxy port.
fn spawn_delay_proxy(backend: u16, one_way: Duration) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind proxy");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for client in listener.incoming().flatten() {
            std::thread::spawn(move || {
                std::thread::sleep(one_way * 2);
                let Ok(server) = TcpStream::connect(("127.0.0.1", backend)) else {
                    return;
                };
                let (c2, s2) = (client.try_clone().unwrap(), server.try_clone().unwrap());
                let up = std::thread::spawn(move || pump(client, server, one_way));
                pump(s2, c2, one_way);
                up.join().ok();
            });
        }
    });
    port
}

fn remote_probe(cli: &Path, rtts_ms: &[u64], runs: usize) {
    println!("\n== Remote probe: MP4 with moov at the end, over a link with added RTT ==");
    let dir = Path::new(CORPUS_DIR);
    std::fs::create_dir_all(dir).expect("create corpus dir");
    let big = dir.join("remote_big.mp4");
    if !big.exists() {
        let ok = quiet(
            Command::new("ffmpeg")
                .args([
                    "-y",
                    "-loglevel",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=1280x720:rate=30",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=440:sample_rate=48000",
                    "-t",
                    "90",
                    "-c:v",
                    "libvpx-vp9",
                    "-deadline",
                    "realtime",
                    "-cpu-used",
                    "8",
                    "-b:v",
                    "1500k",
                    "-g",
                    "60",
                    "-pix_fmt",
                    "yuv420p",
                    "-c:a",
                    "libopus",
                    "-b:a",
                    "64k",
                ])
                .arg(&big),
        )
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
        if !ok {
            println!("  could not create the test MP4; skipping");
            return;
        }
    }
    let data = Arc::new(std::fs::read(&big).expect("read test mp4"));
    println!("file: {:.1} MiB", data.len() as f64 / 1_048_576.0);
    let counters = Arc::new(RangeCounters::default());
    let backend = spawn_range_server(data, counters.clone());

    println!(
        "  {:>7}  {:<36} {:>9} {:>9} {:>11}",
        "RTT ms", "tool", "time ms", "requests", "KiB moved"
    );
    for &rtt in rtts_ms {
        let proxy = spawn_delay_proxy(backend, Duration::from_micros(rtt * 500));
        let url = format!("http://127.0.0.1:{proxy}/remote_big.mp4");
        let kinetix_url = url.clone();
        let tools: [(&str, PathBuf, Vec<String>); 2] = [
            (
                "tpt-kinetix probe",
                cli.to_path_buf(),
                vec!["probe".into(), kinetix_url],
            ),
            (
                "ffprobe -show_streams -show_format",
                PathBuf::from("ffprobe"),
                vec![
                    "-v".into(),
                    "error".into(),
                    "-show_streams".into(),
                    "-show_format".into(),
                    url,
                ],
            ),
        ];
        for (name, program, args) in tools {
            let mut best = f64::INFINITY;
            let (mut reqs, mut bytes) = (0, 0);
            let mut failed = false;
            for _ in 0..runs {
                counters.requests.store(0, Ordering::Relaxed);
                counters.bytes.store(0, Ordering::Relaxed);
                let t = Instant::now();
                let ok = quiet(Command::new(&program).args(&args))
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false);
                let secs = t.elapsed().as_secs_f64();
                if !ok {
                    failed = true;
                    continue;
                }
                if secs < best {
                    best = secs;
                    reqs = counters.requests.load(Ordering::Relaxed);
                    bytes = counters.bytes.load(Ordering::Relaxed);
                }
            }
            if best.is_finite() {
                println!(
                    "  {:>7}  {:<36} {:>9.0} {:>9} {:>11}{}",
                    rtt,
                    name,
                    best * 1000.0,
                    reqs,
                    bytes / 1024,
                    if failed { "  (some runs failed)" } else { "" }
                );
            } else {
                println!("  {rtt:>7}  {name:<36} FAILED");
            }
        }
    }
}
