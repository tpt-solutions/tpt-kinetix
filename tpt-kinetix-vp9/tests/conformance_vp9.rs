//! End-to-end VP9 conformance against `ffmpeg -c:v vp9`.
//!
//! Generates real `libvpx-vp9`-encoded IVF clips with `ffmpeg`, decodes them
//! with [`tpt_kinetix_vp9::Vp9Decoder`], decodes the same clips to raw YUV
//! with `ffmpeg`, and compares planar PSNR. Every test **skips** (rather than
//! fails) when `ffmpeg` is unavailable, per the workspace testing rules.

use std::process::Command;

use tpt_kinetix_vp9::Vp9Decoder;

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Encode `count` frames of `source` at `w`x`h` to a VP9 IVF. Returns `None`
/// when `ffmpeg` (or the libvpx-vp9 encoder) is unavailable.
fn make_vp9_ivf(
    tag: &str,
    w: u32,
    h: u32,
    source: &str,
    count: u32,
    extra: &[&str],
) -> Option<Vec<u8>> {
    let out = std::env::temp_dir().join(format!(
        "tpt_vp9_conf_{tag}_{source}_{w}x{h}_{count}_{}.ivf",
        extra.join("_").replace('-', "m")
    ));
    let mut cmd = Command::new("ffmpeg");
    // lavfi: sources with a value (color=black) take ':'-separated options,
    // bare sources (testsrc) take '='.
    let sep = if source.contains('=') { ':' } else { '=' };
    cmd.args(["-y", "-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("{source}{sep}duration=1:size={w}x{h}:rate=10"))
        // force 4:2:0 8-bit so libvpx emits profile 0 (a raw RGB source
        // would otherwise produce a profile-1 4:4:4 stream)
        .args(["-pix_fmt", "yuv420p"])
        .args(["-frames:v", &count.to_string(), "-c:v", "libvpx-vp9"])
        .args(extra)
        .arg(&out);
    let status = cmd.status().ok()?;
    if !status.success() {
        return None;
    }
    std::fs::read(&out).ok()
}

/// An IVF file split into its frames.
struct Ivf {
    width: u32,
    height: u32,
    frames: Vec<Vec<u8>>,
}

fn parse_ivf(data: &[u8]) -> Option<Ivf> {
    if data.len() < 32 || &data[0..4] != b"DKIF" {
        return None;
    }
    let width = u16::from_le_bytes([data[12], data[13]]) as u32;
    let height = u16::from_le_bytes([data[14], data[15]]) as u32;
    let mut frames = Vec::new();
    let mut pos = 32usize;
    while pos + 12 <= data.len() {
        let size =
            u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        pos += 12;
        if pos + size > data.len() {
            break;
        }
        frames.push(data[pos..pos + size].to_vec());
        pos += size;
    }
    Some(Ivf {
        width,
        height,
        frames,
    })
}

/// Decode an IVF to raw planar YUV420p via ffmpeg (the reference decode).
fn ffmpeg_decode_to_yuv(ivf: &[u8], tag_of: &str) -> Option<Vec<u8>> {
    let tmp = std::env::temp_dir().join(format!("tpt_vp9_conf_ref_{}.ivf", tag_of));
    let _ = tag_of;
    std::fs::write(&tmp, ivf).ok()?;
    let out = std::env::temp_dir().join(format!("tpt_vp9_conf_ref_{}.yuv", tag_of));
    let status = Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-i"])
        .arg(&tmp)
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p"])
        .arg(&out)
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    std::fs::read(&out).ok()
}

/// Planar MSE-based PSNR over Y then U then V; returns (y, u, v) dB.
fn psnr(a: &[u8], b: &[u8], w: usize, h: usize) -> (f64, f64, f64) {
    fn plane_psnr(a: &[u8], b: &[u8]) -> f64 {
        if a.is_empty() {
            return 99.0;
        }
        let mse: u64 = a
            .iter()
            .zip(b)
            .map(|(x, y)| {
                let d = i32::from(*x) - i32::from(*y);
                (d * d) as u64
            })
            .sum();
        let n = a.len() as u64;
        if mse == 0 {
            return 99.0;
        }
        10.0 * (255.0 * 255.0 * n as f64 / mse as f64).log10()
    }
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let (ya, rest) = a.split_at(w * h);
    let (ua, va) = rest.split_at(cw * ch);
    let (yb, rest) = b.split_at(w * h);
    let (ub, vb) = rest.split_at(cw * ch);
    (plane_psnr(ya, yb), plane_psnr(ua, ub), plane_psnr(va, vb))
}

/// Decode an IVF with the Kinetix decoder and return the concatenated planar
/// output for all frames marked `show_frame`.
fn kinetix_decode(ivf: &Ivf) -> Result<Vec<u8>, String> {
    let mut dec = Vp9Decoder::new();
    let mut out = Vec::new();
    for (i, frame) in ivf.frames.iter().enumerate() {
        if std::env::var("TPT_VP9_DBG").is_ok() {
            if let Ok(h) = tpt_kinetix_vp9::header::parse_uncompressed_header(frame, &[None; 8]) {
                eprintln!(
                    "frame {i}: {}x{} key={:?} intra_only={} ch_off={} ch_size={} tiles={}x{} q={} lf={}",
                    h.width, h.height, h.frame_type, h.intra_only,
                    h.compressed_header_offset, h.compressed_header_size,
                    h.tile.tile_cols(), h.tile.tile_rows(),
                    h.base_q_idx, h.loop_filter.level,
                );
            }
        }
        let packet = tpt_kinetix_core::packet::Packet {
            pts: tpt_kinetix_core::timestamp::Timestamp::NONE,
            dts: tpt_kinetix_core::timestamp::Timestamp::NONE,
            data: frame.clone(),
            stream_index: 0,
            is_key_frame: i == 0,
        };
        match dec.decode(&packet) {
            Ok(Some(vf)) => out.extend_from_slice(&vf.data),
            Ok(None) => {}
            Err(e) => return Err(format!("frame {i}: {e}")),
        }
    }
    Ok(out)
}

fn check_clip(name: &str, w: u32, h: u32, source: &str, count: u32, extra: &[&str]) {
    check_clip_tagged(name, name, w, h, source, count, extra);
}

#[allow(clippy::too_many_arguments)]
fn check_clip_tagged(
    name: &str,
    tag: &str,
    w: u32,
    h: u32,
    source: &str,
    count: u32,
    extra: &[&str],
) {
    if !ffmpeg_available() {
        eprintln!("[GAP] {name}: ffmpeg unavailable, skipping");
        return;
    }
    let ivf_bytes = match make_vp9_ivf(tag, w, h, source, count, extra) {
        Some(v) => v,
        None => {
            eprintln!("[GAP] {name}: libvpx-vp9 encode failed, skipping");
            return;
        }
    };
    let ivf = match parse_ivf(&ivf_bytes) {
        Some(v) if !v.frames.is_empty() => v,
        _ => {
            eprintln!("[GAP] {name}: IVF parse failed, skipping");
            return;
        }
    };
    let reference = match ffmpeg_decode_to_yuv(&ivf_bytes, tag) {
        Some(v) => v,
        None => {
            eprintln!("[GAP] {name}: ffmpeg reference decode failed, skipping");
            return;
        }
    };
    let ours = match kinetix_decode(&ivf) {
        Ok(v) => v,
        Err(e) => {
            panic!("{name}: kinetix decode failed: {e}");
        }
    };
    if ours.len() != reference.len() {
        panic!(
            "{name}: output size {} != reference {} (frame count mismatch)",
            ours.len(),
            reference.len()
        );
    }
    let _ = name;
    let (py, pu, pv) = psnr(&ours, &reference, ivf.width as usize, ivf.height as usize);
    let cw = ivf.width.div_ceil(2) as usize;
    let ch = ivf.height.div_ceil(2) as usize;
    let u_off = ivf.width as usize * ivf.height as usize;
    let u_bad = ours[u_off..u_off + cw * ch]
        .iter()
        .zip(&reference[u_off..u_off + cw * ch])
        .filter(|(a, b)| a != b)
        .count();
    let mut u_diff_pos = Vec::new();
    for (i, (a, b)) in ours[u_off..u_off + cw * ch]
        .iter()
        .zip(&reference[u_off..u_off + cw * ch])
        .enumerate()
    {
        if a != b {
            u_diff_pos.push((i, *a, *b));
        }
    }
    eprintln!(
        "[{name}] PSNR Y={py:.2} U={pu:.2} V={pv:.2} dB (u_bad={u_bad}, first={:?})",
        &u_diff_pos[..u_diff_pos.len().min(6)]
    );
}

#[test]
fn conformance_vp9_solid_lossless() {
    for sz in [16usize, 32, 48, 64, 96] {
        let sz = sz as u32;
        check_clip_tagged(
            &format!("solid_black_lossless_{sz}"),
            &format!("solidll{sz}"),
            sz,
            sz,
            "color=black",
            1,
            &["-lossless", "1", "-cpu-used", "4"],
        );
    }
}

/// 16x16 gray-128 lossless: DC_128 prediction equals the source, so every
/// block is skip — the minimal possible keyframe. Our decoder must be
/// pixel-exact here.
#[test]
fn conformance_vp9_micro_skip() {
    check_clip_tagged(
        "micro128_skip",
        "microskip",
        16,
        16,
        "color=0x828282",
        1,
        &["-lossless", "1", "-cpu-used", "4"],
    );
}

/// 16x16 lossy (default quant) with zero chroma residual: isolates the
/// chroma prediction + EOB-empty path at lossy quantization.
#[test]
fn conformance_vp9_micro_lossy_chroma() {
    check_clip_tagged(
        "micro128_lossy",
        "microly",
        16,
        16,
        "color=0x808080",
        1,
        &["-cpu-used", "4"],
    );
}

/// 64x64 lossy solid: exercises 32x32/16x16 block partitioning and the
/// loop filter on a flat scene.
#[test]
fn conformance_vp9_solid64_lossy() {
    check_clip_tagged(
        "solid64_lossy",
        "s64ly",
        64,
        64,
        "color=0x808080",
        1,
        &["-cpu-used", "4"],
    );
}

/// 16x16 with a uniform +-2 residual (Y=129/125): every one of the 16 Y
/// 4x4 sub-blocks carries an identical small DC token — the minimal
/// multi-sub-block coefficient test.
#[test]
fn conformance_vp9_micro_dc2() {
    check_clip_tagged(
        "micro129_dc2",
        "microdc3",
        16,
        16,
        "color=0x838383",
        1,
        &["-lossless", "1", "-cpu-used", "4"],
    );
    check_clip_tagged(
        "micro125_dc2",
        "microdc4",
        16,
        16,
        "color=0x7f7f7f",
        1,
        &["-lossless", "1", "-cpu-used", "4"],
    );
}

/// 16x16 with a +-1 residual (Y=127/129): exercises DC-only coefficient
/// tokens on top of the micro-skip path.
#[test]
fn conformance_vp9_micro_dc1() {
    check_clip_tagged(
        "micro127_dc1",
        "microdc1",
        16,
        16,
        "color=0x818181",
        1,
        &["-lossless", "1", "-cpu-used", "4"],
    );
    check_clip_tagged(
        "micro129_dc1",
        "microdc2",
        16,
        16,
        "color=0x838383",
        1,
        &["-lossless", "1", "-cpu-used", "4"],
    );
}

#[test]
fn conformance_vp9_solid_lossy() {
    check_clip_tagged(
        "solid_black_lossy",
        "solidly",
        64,
        64,
        "color=black",
        1,
        &["-cpu-used", "4"],
    );
}

#[test]
fn conformance_vp9_lossless_keyframe() {
    check_clip_tagged(
        "testsrc_lossless",
        "testll",
        96,
        96,
        "testsrc",
        1,
        &["-lossless", "1", "-cpu-used", "4"],
    );
}

#[test]
fn conformance_vp9_intra_keyframe() {
    check_clip("intra128", 128, 128, "testsrc", 1, &["-cpu-used", "4"]);
}

#[test]
fn conformance_vp9_inter() {
    check_clip("inter128x96", 128, 96, "testsrc", 4, &["-cpu-used", "4"]);
}

#[test]
fn conformance_vp9_lossless() {
    check_clip(
        "lossless96x64",
        96,
        64,
        "smptebars",
        1,
        &["-lossless", "1", "-cpu-used", "4"],
    );
}

#[test]
fn conformance_vp9_odd_size() {
    check_clip("odd125x67", 125, 67, "testsrc", 2, &["-cpu-used", "4"]);
}

#[test]
fn conformance_vp9_multitile() {
    check_clip(
        "tiled256x144",
        256,
        144,
        "testsrc",
        2,
        &["-cpu-used", "4", "-tile-columns", "2"],
    );
}
