//! Randomized libaom -> Kinetix vs libdav1d sweep with PER-FRAME diff counts.
//!
//! Unlike `tests/libaom_crosscheck.rs` (which reports only a total byte count),
//! this prints the number of differing bytes for every decoded frame, so a
//! cascade starting at frame 1 is immediately visible as opposed to a bulk
//! mismatch.
//!
//! Usage:
//!   cargo run -p tpt-kinetix-av1 --example dbg_inter_sweep -- <seed> [count]
//!
//! Env:
//!   KINETIX_SWEEP_MAXW / _MAXH  cap the generated frame size
//!   KINETIX_SWEEP_SAVE=<dir>    write each encoded .ivf there

use std::{
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
};

use tpt_kinetix_av1::Av1Decoder;
use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};

/// Minimal xorshift64 so the sweep is reproducible from a seed alone.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn ffmpeg_has(name: &str, kind: &str) -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", kind])
        .stdin(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(name))
        .unwrap_or(false)
}

fn split_ivf(ivf: &[u8]) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    if ivf.len() < 32 || &ivf[0..4] != b"DKIF" {
        return frames;
    }
    let mut off = 32usize;
    while off + 12 <= ivf.len() {
        let sz = u32::from_le_bytes([ivf[off], ivf[off + 1], ivf[off + 2], ivf[off + 3]]) as usize;
        if off + 12 + sz > ivf.len() {
            break;
        }
        frames.push(ivf[off + 12..off + 12 + sz].to_vec());
        off += 12 + sz;
    }
    frames
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

const SOURCES: &[&str] = &[
    "testsrc",
    "testsrc2",
    "mandelbrot",
    "smptebars",
    "rgbtestsrc",
    "life",
];
const PIX_FMTS: &[&str] = &[
    "yuv420p",
    "yuv420p10le",
    "yuv420p12le",
    "yuv422p",
    "yuv444p",
];

/// Decode every IVF frame with Kinetix, returning one `Vec<u8>` per output
/// frame (in the decoder's output order).
fn decode_frames(ivf: &[u8]) -> Vec<Vec<u8>> {
    let mut dec = Av1Decoder::new();
    let mut out = Vec::new();
    for (i, payload) in split_ivf(ivf).into_iter().enumerate() {
        let packet = Packet {
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            data: payload,
            stream_index: 0,
            is_key_frame: i == 0,
        };
        if let Ok(Some(frame)) = dec.decode(&packet) {
            out.push(frame.data);
        }
    }
    out
}

/// Reference decode through ffmpeg + libdav1d to planar rawvideo.
fn reference(ivf: &[u8], pix_fmt: &str) -> Option<Vec<u8>> {
    let mut child = Command::new("ffmpeg")
        .args(["-loglevel", "error", "-i", "pipe:0", "-pix_fmt", pix_fmt])
        .args(["-noautoscale", "-f", "rawvideo", "pipe:1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take()?;
    let owned = ivf.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&owned);
    });
    let out = child.wait_with_output().ok()?;
    let _ = writer.join();
    out.status.success().then_some(out.stdout)
}

fn encode(lavfi: &str, pix_fmt: &str, cpu: u64, out: &std::path::Path) -> Option<Vec<u8>> {
    let ok = Command::new("ffmpeg")
        .args(["-loglevel", "error", "-y", "-f", "lavfi", "-i", lavfi])
        .args(["-t", "1.2", "-pix_fmt", pix_fmt, "-c:v", "libaom-av1"])
        .args(["-cpu-used", &cpu.to_string()])
        .args(["-f", "ivf"])
        .arg(out)
        .stdin(Stdio::null())
        .status()
        .ok()
        .is_some_and(|s| s.success());
    ok.then(|| std::fs::read(out).ok()).flatten()
}

fn main() {
    if !ffmpeg_has("libaom-av1", "-encoders") || !ffmpeg_has("libdav1d", "-decoders") {
        eprintln!("skipping: ffmpeg with libaom-av1 and libdav1d not available");
        return;
    }

    let arg: Vec<String> = std::env::args().skip(1).collect();
    // `--fixed <file.ivf>` re-checks one already-encoded stream (useful for
    // long, multi-keyframe inter clips) instead of generating new vectors.
    if arg.first().map(String::as_str) == Some("--fixed") {
        let path = PathBuf::from(arg.get(1).expect("--fixed needs a path"));
        let ivf = std::fs::read(&path).expect("read ivf");
        let Some(ref_bytes) = reference(&ivf, "yuv420p") else {
            eprintln!("reference decode failed");
            return;
        };
        let ours = decode_frames(&ivf);
        let fsize = ours[0].len();
        let out: Vec<String> = ours
            .iter()
            .enumerate()
            .map(|(fi, a)| {
                let b = &ref_bytes[fi * fsize..(fi + 1) * fsize];
                format!("{}:{}", fi, a.iter().zip(b).filter(|(x, y)| x != y).count())
            })
            .collect();
        println!("{}: per-frame diffs [{}]", path.display(), out.join(" "));
        return;
    }

    let seed: u64 = arg.first().and_then(|s| s.parse().ok()).unwrap_or(7);
    let count: usize = arg.get(1).and_then(|s| s.parse().ok()).unwrap_or(24);

    let max_w = env_u64("KINETIX_SWEEP_MAXW", 480);
    let max_h = env_u64("KINETIX_SWEEP_MAXH", 400);
    let save = std::env::var("KINETIX_SWEEP_SAVE").ok().map(PathBuf::from);
    if let Some(d) = &save {
        std::fs::create_dir_all(d).expect("create save dir");
    }

    let dir = std::env::temp_dir().join("tpt_kinetix_inter_sweep");
    std::fs::create_dir_all(&dir).expect("create scratch dir");

    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1) | 1);
    let mut bad_streams = 0usize;
    let mut total_frames = 0usize;
    let mut exact_frames = 0usize;

    for n in 0..count {
        let w = 16 + rng.below(max_w / 8) * 8;
        let h = 16 + rng.below(max_h / 8) * 8;
        let src = SOURCES[rng.below(SOURCES.len() as u64) as usize];
        let pix = PIX_FMTS[rng.below(PIX_FMTS.len() as u64) as usize];
        let cpu = rng.below(7);
        let lavfi = format!("{src}=size={w}x{h}:rate=10");

        let Some(ivf) = encode(&lavfi, pix, cpu, &dir.join(format!("sw{n}.ivf"))) else {
            eprintln!("[{n}] encode failed: {lavfi} {pix} cpu{cpu}");
            continue;
        };
        if let Some(d) = &save {
            let name = format!("s{seed}_{n}_{w}x{h}_{src}_{pix}_cpu{cpu}.ivf");
            std::fs::write(d.join(name), &ivf).expect("save ivf");
        }
        let Some(ref_bytes) = reference(&ivf, pix) else {
            eprintln!("[{n}] reference decode failed: {lavfi} {pix} cpu{cpu}");
            continue;
        };

        let ours = decode_frames(&ivf);
        if ours.is_empty() {
            eprintln!("[{n}] no frames decoded: {lavfi} {pix} cpu{cpu}");
            continue;
        }

        // Both sides emit planar frames of identical size, so walk them in
        // lockstep and attribute each difference to a specific frame index.
        let fsize = ours[0].len();
        let ref_n = ref_bytes.len() / fsize.max(1);
        if ours.len() != ref_n {
            println!(
                "[{n}] {lavfi} {pix} cpu{cpu}: FRAME COUNT ours={} ref={ref_n}",
                ours.len()
            );
        }
        let nframes = ours.len().min(ref_n);

        let mut per_frame = Vec::new();
        for (fi, a) in ours.iter().take(nframes).enumerate() {
            let b = &ref_bytes[fi * fsize..(fi + 1) * fsize];
            let d = a.iter().zip(b).filter(|(x, y)| x != y).count();
            total_frames += 1;
            if d == 0 {
                exact_frames += 1;
            }
            per_frame.push(d);
        }

        if let Some(fb) = per_frame.iter().position(|&d| d != 0) {
            bad_streams += 1;
            let tail: Vec<String> = per_frame
                .iter()
                .enumerate()
                .map(|(i, &d)| format!("{i}:{d}"))
                .collect();
            println!("[{n}] {lavfi} {pix} cpu{cpu}: first bad frame {fb}  per-frame {tail:?}");
        }
    }

    println!(
        "seed {seed}: {bad_streams}/{count} streams diverge; {exact_frames}/{total_frames} frames bit-exact"
    );
}
