//! Generate a small `testsrc_WxH.h264` CAVLC corpus into
//! `$TMPDIR/h264_corpus/` (or a directory given as the first CLI arg), using the
//! same `ffmpeg` invocation as `tests/cavlc_conformance.rs::generate()`. The
//! generated files are consumed by the `corpus_check` example.
//!
//! Usage: `cargo run -p tpt-kinetix-h264 --example gen_corpus [dir]`

use std::path::PathBuf;
use std::process::Command;

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .args(["-version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run(cmd: &mut Command) -> bool {
    match cmd.status() {
        Ok(s) => s.success(),
        Err(e) => {
            eprintln!("failed to spawn {}: {e}", cmd.get_program().to_string_lossy());
            false
        }
    }
}

fn generate(dir: &std::path::Path, w: u32, h: u32) {
    let out = dir.join(format!("testsrc_{w}x{h}.h264"));
    let input_spec = format!("testsrc=size={w}x{h}:rate=1:duration=1");
    let mut args: Vec<&str> = vec![
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "lavfi",
        "-i",
        &input_spec,
        "-frames:v",
        "1",
        "-c:v",
        "libx264",
        "-profile:v",
        "baseline",
        "-g",
        "1",
        "-bf",
        "0",
        "-pix_fmt",
        "yuv420p",
        "-x264-params",
        "cabac=0:ref=1:bframes=0:8x8dct=0:weightp=0:aud=0",
    ];
    args.push(out.to_str().unwrap());
    if !run(Command::new("ffmpeg").args(&args)) {
        eprintln!("ffmpeg failed to generate {}", out.display());
        return;
    }
    println!("wrote {}", out.display());
}

fn main() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not available on PATH; cannot generate corpus");
        return;
    }

    let dir = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("h264_corpus"));
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("create_dir_all {dir:?}: {e}"));

    for (w, h) in [
        (48, 32),
        (64, 48),
        (96, 64),
        (128, 96),
    ] {
        generate(&dir, w, h);
    }
}
