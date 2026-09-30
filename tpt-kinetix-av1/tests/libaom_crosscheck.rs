//! Cross-check the AV1 decoder against ffmpeg's libdav1d on streams freshly
//! encoded by libaom.
//!
//! Each case encodes a short synthetic clip with `ffmpeg -c:v libaom-av1`,
//! decodes it with both this crate and `ffmpeg -c:v libdav1d`, and requires the
//! raw output to match byte for byte. The cases are the ones that exposed real
//! bugs (screen-content IntraBC keyframes, 128-wide blocks straddling the frame
//! edge, partial-width deblocking, loop-restoration unit offsets and frame
//! edges, high-bit-depth filters, film grain, global motion, clamped compound
//! MV candidates, ...), so a regression in any of them fails here.
//!
//! The test skips (passes with a message) when `ffmpeg` with both `libaom-av1`
//! and `libdav1d` is not on `PATH`.

use std::{
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
};

use tpt_kinetix_av1::Av1Decoder;
use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};

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

struct Case {
    name: &'static str,
    lavfi: &'static str,
    pix_fmt: &'static str,
    aom: &'static [&'static str],
}

fn scratch_dir() -> PathBuf {
    let dir = std::env::temp_dir().join("tpt_kinetix_libaom_crosscheck");
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Encode `case`, returning the IVF bytes (or `None` if the encode failed).
fn encode(case: &Case, out: &std::path::Path) -> Option<Vec<u8>> {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-loglevel", "error", "-y", "-f", "lavfi", "-i", case.lavfi])
        .args(["-t", "1.5", "-pix_fmt", case.pix_fmt, "-c:v", "libaom-av1"])
        .args(case.aom)
        .args(["-f", "ivf"])
        .arg(out)
        .stdin(Stdio::null());
    if !cmd.status().ok()?.success() {
        return None;
    }
    std::fs::read(out).ok()
}

/// Reference decode through ffmpeg + libdav1d (grain applied, as dav1d does).
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
    Some(out.stdout)
}

fn decode_all(ivf: &[u8]) -> Vec<u8> {
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
            out.extend_from_slice(&frame.data);
        }
    }
    out
}

#[test]
fn libaom_streams_match_libdav1d() {
    if !ffmpeg_has("libaom-av1", "-encoders") || !ffmpeg_has("libdav1d", "-decoders") {
        eprintln!("skipping: ffmpeg with libaom-av1 and libdav1d not available");
        return;
    }
    let cases = [
        Case {
            name: "testsrc2 192x128 4:2:2 (chroma deblock/CDEF/LR grids, sub-8x8 chroma MC)",
            lavfi: "testsrc2=size=192x128:rate=10",
            pix_fmt: "yuv422p",
            aom: &["-cpu-used", "4"],
        },
        Case {
            // 64x64 blocks carry 32x32-max chroma: 4:2:2 32x64 (two tx blocks)
            // and 4:4:4 64x64 (four). Spec `residual()` codes all of U's
            // transform blocks before any of V's; interleaving them desyncs.
            name: "testsrc 548x388 4:2:2 (multi-tx chroma blocks: U-before-V order)",
            lavfi: "testsrc=size=548x388:rate=10",
            pix_fmt: "yuv422p",
            aom: &["-cpu-used", "1"],
        },
        Case {
            name: "testsrc 320x240 4:4:4 (multi-tx chroma blocks: U-before-V order)",
            lavfi: "testsrc=size=320x240:rate=10",
            pix_fmt: "yuv444p",
            aom: &["-cpu-used", "1"],
        },
        Case {
            // Profile-2 10-bit (`twelve_bit == 0`) was mis-read as 12-bit.
            name: "testsrc2 320x240 4:2:2 10-bit (profile 2, twelve_bit=0)",
            lavfi: "testsrc2=size=320x240:rate=10",
            pix_fmt: "yuv422p10le",
            aom: &["-cpu-used", "2"],
        },
        Case {
            // 12-bit SGR: alpha*sum*one_by_n reaches ~4.28e9 (i32 overflow).
            name: "testsrc2 332x210 4:4:4 12-bit (SGR overflow)",
            lavfi: "testsrc2=size=332x210:rate=10",
            pix_fmt: "yuv444p12le",
            aom: &["-cpu-used", "4"],
        },
        Case {
            // Segmentation: alt-Q features, spatially-predicted segment ids.
            name: "testsrc2 384x256 4:2:0 aq-mode=1 (segmentation, alt-Q)",
            lavfi: "testsrc2=size=384x256:rate=10",
            pix_fmt: "yuv420p",
            aom: &["-cpu-used", "4", "-aom-params", "aq-mode=1"],
        },
        Case {
            // Cyclic refresh: temporal segment-id prediction + PrevSegmentIds.
            name: "testsrc2 384x256 4:2:2 aq-mode=3 (temporal segmentation)",
            lavfi: "testsrc2=size=384x256:rate=10",
            pix_fmt: "yuv422p",
            aom: &["-cpu-used", "4", "-aom-params", "aq-mode=3"],
        },
        Case {
            // Lossless: WHT, forced TX_4X4, CFL only for 4x4 chroma, and inter
            // blocks overhanging the frame edge (200 is not a multiple of 64).
            name: "mandelbrot 200x202 4:2:0 lossless",
            lavfi: "mandelbrot=size=200x202:rate=10",
            pix_fmt: "yuv420p",
            aom: &["-cpu-used", "4", "-aom-params", "lossless=1"],
        },
        Case {
            name: "testsrc2 320x240 4:4:4 lossless",
            lavfi: "testsrc2=size=320x240:rate=10",
            pix_fmt: "yuv444p",
            aom: &["-cpu-used", "4", "-aom-params", "lossless=1"],
        },
        Case {
            // Film grain synthesis with non-4:2:0 chroma (previously skipped).
            name: "testsrc2 192x128 4:2:2 10-bit film grain",
            lavfi: "testsrc2=size=192x128:rate=10",
            pix_fmt: "yuv422p10le",
            aom: &["-cpu-used", "4", "-aom-params", "film-grain-test=8"],
        },
        Case {
            // Quantizer matrices (rectangular sizes are stored transposed).
            name: "testsrc2 320x240 4:2:0 quantizer matrices",
            lavfi: "testsrc2=size=320x240:rate=10",
            pix_fmt: "yuv420p",
            aom: &[
                "-cpu-used",
                "4",
                "-aom-params",
                "enable-qm=1:qm-min=0:qm-max=15",
            ],
        },
        Case {
            name: "testsrc2 192x128 4:4:4 (unsubsampled chroma planes and refs)",
            lavfi: "testsrc2=size=192x128:rate=10",
            pix_fmt: "yuv444p",
            aom: &["-cpu-used", "4"],
        },
        Case {
            name: "testsrc 548x388 cpu0 (128x128 inter blocks: per-64x64-chunk residual order)",
            lavfi: "testsrc=size=548x388:rate=10",
            pix_fmt: "yuv420p",
            aom: &["-cpu-used", "0"],
        },
        Case {
            // SVT-AV1 encodes superres (libaom via ffmpeg cannot): downscaled
            // frames upscaled between CDEF and LR, per-frame denominators, and
            // motion fields that are size-incompatible across frames (which
            // must disable temporal MV candidates).
            name: "testsrc2 200x150 svt superres d9 (superres upscale + LR + refs)",
            lavfi: "testsrc2=size=200x150:rate=10",
            pix_fmt: "yuv420p",
            aom: &[
                "-c:v",
                "libsvtav1",
                "-cpu-used",
                "8",
                "-svtav1-params",
                "superres-mode=2:superres-denominator=9",
            ],
        },
        Case {
            // SVT adaptive mode ends up coding NO superres here, but the
            // 260-wide frame is not 8-aligned and its all-intra keyframe is
            // palette-heavy — this exact combination once ran every frame
            // through the superres resampler (the superres gate must compare
            // the CODED width, not the 8-aligned grid extent).
            name: "testsrc2 260x200 svt adaptive superres (palette keyframe, width%8 != 0)",
            lavfi: "testsrc2=size=260x200:rate=10",
            pix_fmt: "yuv420p",
            aom: &[
                "-c:v",
                "libsvtav1",
                "-cpu-used",
                "8",
                "-svtav1-params",
                "superres-mode=1",
            ],
        },
        Case {
            // Monochrome (`mono_chrome` sequence flag): ffmpeg feeds gray
            // input and libaom codes a 1-plane stream. Non-skip odd/odd 4×4
            // inter leaves once read uv coefficients here (the inter
            // residual path's has_chroma gate missed the monochrome check),
            // desyncing every inter frame. Also exercises the Gray output
            // pixel format end to end.
            name: "testsrc2 160x120 monochrome (mono has_chroma gate, Gray output)",
            lavfi: "testsrc2=size=160x120:rate=10",
            pix_fmt: "gray",
            aom: &["-cpu-used", "4"],
        },
        Case {
            name: "testsrc2 352x288 cpu0 (IntraBC keyframe, 128 blocks, dmv CDFs)",
            lavfi: "testsrc2=size=352x288:rate=10",
            pix_fmt: "yuv420p",
            aom: &["-cpu-used", "0"],
        },
        Case {
            name: "testsrc2 352x288 cpu2 (MvCtx=1 CDFs carried across frames)",
            lavfi: "testsrc2=size=352x288:rate=10",
            pix_fmt: "yuv420p",
            aom: &["-cpu-used", "2"],
        },
        Case {
            name: "testsrc2 350x286 (LR at the right frame edge)",
            lavfi: "testsrc2=size=350x286:rate=10",
            pix_fmt: "yuv420p",
            aom: &["-cpu-used", "4"],
        },
        Case {
            name: "testsrc2 100x60 (partial-width deblocking)",
            lavfi: "testsrc2=size=100x60:rate=10",
            pix_fmt: "yuv420p",
            aom: &["-cpu-used", "5"],
        },
        Case {
            name: "testsrc2 184x210 (IntraBC secondary MV scan)",
            lavfi: "testsrc2=size=184x210:rate=10",
            pix_fmt: "yuv420p",
            aom: &["-cpu-used", "2"],
        },
        Case {
            name: "rgbtestsrc 284x206 (LR unit rows offset by 8)",
            lavfi: "rgbtestsrc=size=284x206:rate=10",
            pix_fmt: "yuv420p",
            aom: &["-cpu-used", "3"],
        },
        Case {
            name: "mandelbrot 320x240 (global motion, 4x4 GLOBALMV)",
            lavfi: "mandelbrot=size=320x240:rate=10",
            pix_fmt: "yuv420p",
            aom: &["-cpu-used", "4"],
        },
        Case {
            name: "testsrc2 484x256 (compound MV stack clamp)",
            lavfi: "testsrc2=size=484x256:rate=10",
            pix_fmt: "yuv420p",
            aom: &["-cpu-used", "1"],
        },
        Case {
            name: "rgbtestsrc 182x74 10-bit (high-bit-depth Wiener)",
            lavfi: "rgbtestsrc=size=182x74:rate=10",
            pix_fmt: "yuv420p10le",
            aom: &["-cpu-used", "4"],
        },
        Case {
            name: "testsrc2 352x288 10-bit + film grain on inter frames",
            lavfi: "testsrc2=size=352x288:rate=10",
            pix_fmt: "yuv420p10le",
            aom: &["-cpu-used", "4", "-aom-params", "film-grain-test=9"],
        },
        Case {
            name: "testsrc2 352x288 8-bit + film grain on inter frames",
            lavfi: "testsrc2=size=352x288:rate=10",
            pix_fmt: "yuv420p",
            aom: &["-cpu-used", "4", "-aom-params", "film-grain-test=4"],
        },
    ];

    let dir = scratch_dir();
    let mut failures = Vec::new();
    for (i, case) in cases.iter().enumerate() {
        let path = dir.join(format!("case{i}.ivf"));
        let Some(ivf) = encode(case, &path) else {
            eprintln!("skipping case (encode failed): {}", case.name);
            continue;
        };
        let Some(expected) = reference(&ivf, case.pix_fmt) else {
            eprintln!("skipping case (reference decode failed): {}", case.name);
            continue;
        };
        let actual = decode_all(&ivf);
        let differing = if actual.len() == expected.len() {
            actual.iter().zip(&expected).filter(|(a, b)| a != b).count()
        } else {
            usize::MAX
        };
        if differing != 0 {
            failures.push(format!(
                "{}: {} differing bytes (len ours={} ref={})",
                case.name,
                differing,
                actual.len(),
                expected.len()
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "decoder diverged from libdav1d:\n{}",
        failures.join("\n")
    );
}
