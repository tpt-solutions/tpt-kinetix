//! Benchmark: AV1 **decode** throughput with [`Av1Decoder`].
//!
//! The crate previously only benched encode, so none of the Phase 3 decode
//! work (deblock, inverse transforms, the reconstruct-path SIMD kernels) had a
//! criterion row to move. This adds one, on two corpora:
//!
//! * `av1_decode/fate_corpus` — every stream in `fixtures/av1-fate/`
//!   (overridable with `KINETIX_AV1_BENCH_DIR`). Small, but it is the corpus
//!   the bit-exactness gate (`av1_fate_score`) uses, so a regression here is a
//!   regression against a corpus we know is decoded correctly.
//! * `av1_decode/<w>x<h>` — `ffmpeg`-generated `testsrc` clips at 320x240 and
//!   1280x720, 60 frames each, the same shapes the AV1 phase timings in
//!   `todo-perf.md` quote. Skipped when `ffmpeg` is not on `PATH`.
//!
//! Each `iter` decodes every frame of the corpus with a fresh decoder, so the
//! measurement includes both entropy decode and reconstruction.
//!
//! Set `TPT_AV1_NO_SIMD=1` to measure the scalar oracle paths in the same
//! binary (see `tpt_kinetix_av1::simd`). The switch is intentionally not
//! `KINETIX_`-prefixed: such a name disables the decoder's `dbg_env` fast path
//! and would measure env-var lookups instead of the kernel.

use std::path::{Path, PathBuf};
use std::process::Command;

use criterion::{criterion_group, criterion_main, Criterion};
use tpt_kinetix_av1::Av1Decoder;
use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Split an IVF file into its per-frame OBU payloads.
fn split_ivf_frames(ivf: &[u8]) -> Vec<Vec<u8>> {
    if ivf.len() < 44 || &ivf[0..4] != b"DKIF" {
        return Vec::new();
    }
    let mut frames = Vec::new();
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

/// Encode `frames` frames of `testsrc` to an AV1 IVF via `ffmpeg`'s libaom
/// encoder, returning `(ivf bytes, per-frame OBU payloads)`.
fn make_testsrc_ivf(w: u32, h: u32, frames: u32) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut child = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            &format!(
                "testsrc=size={w}x{h}:rate=30:duration={}",
                frames as f32 / 30.0
            ),
            "-c:v",
            "libaom-av1",
            "-strict",
            "experimental",
            "-cpu-used",
            "8",
            "-pix_fmt",
            "yuv420p",
            "-y",
            "-f",
            "ivf",
            "-",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut out = Vec::new();
    child.stdout.take()?.read_to_end(&mut out).ok()?;
    let _ = child.wait();
    (out.len() > 44).then_some(out)
}

/// Decode every OBU payload with a fresh decoder, returning the number of
/// frames that actually produced a picture.
fn decode_all(frames: &[Vec<u8>]) -> usize {
    let mut dec = Av1Decoder::new();
    let mut decoded = 0usize;
    for (i, data) in frames.iter().enumerate() {
        let pts = Timestamp::new(i as i64, (1, 90_000));
        let packet = Packet {
            pts,
            dts: pts,
            data: data.clone(),
            is_key_frame: i == 0,
            stream_index: 0,
        };
        if let Ok(Some(_frame)) = dec.decode(&packet) {
            decoded += 1;
        }
    }
    decoded
}

/// Every `.ivf` under `dir`, sorted by path so the corpus is stable.
fn ivf_files(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "ivf"))
            .collect(),
        Err(_) => Vec::new(),
    };
    out.sort();
    out
}

fn bench_decode(c: &mut Criterion) {
    let mut group = c.benchmark_group("av1_decode");

    // Cargo runs a bench with the *package* directory as CWD, so the
    // workspace-level corpus is one level up. Try both.
    let dir = std::env::var("KINETIX_AV1_BENCH_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let local = PathBuf::from("fixtures/av1-fate");
            if local.is_dir() {
                local
            } else {
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("..")
                    .join("fixtures")
                    .join("av1-fate")
            }
        });
    let fate: Vec<Vec<Vec<u8>>> = ivf_files(&dir)
        .iter()
        .filter_map(|p| std::fs::read(p).ok())
        .map(|b| split_ivf_frames(&b))
        .filter(|f| !f.is_empty())
        .collect();
    if fate.is_empty() {
        eprintln!(
            "no AV1 IVF corpus under {}; skipping av1_decode/fate_corpus",
            dir.display()
        );
    } else {
        let total: usize = fate.iter().map(|s| s.len()).sum();
        eprintln!(
            "av1_decode/fate_corpus: {} streams, {} frames",
            fate.len(),
            total
        );
        group.bench_function("fate_corpus", |b| {
            b.iter(|| {
                for stream in &fate {
                    std::hint::black_box(decode_all(stream));
                }
            });
        });
    }

    if ffmpeg_available() {
        // 20 frames keeps the 720p case near a second per iteration; the 320x240
        // case uses 60. Both are whole GOPs of `testsrc` at libaom cpu-used 8.
        for (w, h, nframes) in [(320u32, 240u32, 60u32), (1280, 720, 20)] {
            let Some(ivf) = make_testsrc_ivf(w, h, nframes) else {
                eprintln!("ffmpeg failed to encode {w}x{h}; skipping that case");
                continue;
            };
            let frames = split_ivf_frames(&ivf);
            if frames.is_empty() {
                continue;
            }
            let id = format!("{w}x{h}");
            group.bench_function(id, |b| {
                b.iter(|| {
                    std::hint::black_box(decode_all(&frames));
                });
            });
        }
    } else {
        eprintln!("ffmpeg not available; skipping the generated 320x240 / 1280x720 cases");
    }

    group.finish();
}

criterion_group!(benches, bench_decode);
criterion_main!(benches);
