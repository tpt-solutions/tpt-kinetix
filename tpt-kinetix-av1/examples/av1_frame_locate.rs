//! Localize a single AV1 frame's divergence against a dav1d/ffmpeg reference
//! YUV dump. Decodes an IVF up to `KINETIX_AV1_LOC_FRAME`, then reports every
//! differing sample for that frame as (plane, x, y, ours, reference, delta),
//! plus a per-plane bounding box so a small localized delta is obvious.
//!
//! Env:
//!   `KINETIX_AV1_LOC_IVF`   path to a .ivf sample (required)
//!   `KINETIX_AV1_LOC_REF`   path to a planar YUV420p reference dump (required)
//!   `KINETIX_AV1_LOC_FRAME` frame index to localize (default 2)
//!   `KINETIX_AV1_LOC_W`/`_H` frame dimensions (default: read the IVF header)
//!   `KINETIX_AV1_LOC_MAX`   max differing samples to print (default 200)
//!
//! Run: `KINETIX_AV1_LOC_IVF=... KINETIX_AV1_LOC_REF=... cargo run -p tpt-kinetix-av1 --example av1_frame_locate`

use std::path::PathBuf;

use tpt_kinetix_av1::Av1Decoder;
use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};

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

fn env_usize(key: &str) -> Option<usize> {
    std::env::var(key).ok()?.trim().parse().ok()
}

/// Geometry of one planar YUV420p plane within a frame, and its byte offset.
struct Plane {
    name: &'static str,
    w: usize,
    h: usize,
    offset: usize,
}

fn planes(w: usize, h: usize) -> [Plane; 3] {
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    [
        Plane {
            name: "Y",
            w,
            h,
            offset: 0,
        },
        Plane {
            name: "U",
            w: cw,
            h: ch,
            offset: w * h,
        },
        Plane {
            name: "V",
            w: cw,
            h: ch,
            offset: w * h + cw * ch,
        },
    ]
}

fn main() {
    let (Some(ivf_path), Some(ref_path)) = (
        std::env::var("KINETIX_AV1_LOC_IVF").ok().map(PathBuf::from),
        std::env::var("KINETIX_AV1_LOC_REF").ok().map(PathBuf::from),
    ) else {
        eprintln!("skipping: set KINETIX_AV1_LOC_IVF and KINETIX_AV1_LOC_REF");
        return;
    };
    let target = env_usize("KINETIX_AV1_LOC_FRAME").unwrap_or(2);
    let max_print = env_usize("KINETIX_AV1_LOC_MAX").unwrap_or(200);

    let ivf = std::fs::read(&ivf_path).expect("read ivf");
    let (mut w, mut h) = (
        u16::from_le_bytes([ivf[12], ivf[13]]) as usize,
        u16::from_le_bytes([ivf[14], ivf[15]]) as usize,
    );
    if let (Some(ew), Some(eh)) = (
        env_usize("KINETIX_AV1_LOC_W"),
        env_usize("KINETIX_AV1_LOC_H"),
    ) {
        w = ew;
        h = eh;
    }
    let payloads = split_ivf_frames(&ivf);
    if payloads.len() <= target {
        eprintln!(
            "skipping: stream has {} frames, cannot reach frame {target}",
            payloads.len()
        );
        return;
    }

    let mut dec = Av1Decoder::new();
    let mut ours = None;
    for (i, payload) in payloads.iter().enumerate().take(target + 1) {
        let packet = Packet {
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            data: payload.clone(),
            stream_index: 0,
            is_key_frame: i == 0,
        };
        match dec.decode(&packet) {
            Ok(Some(f)) => {
                if i == target {
                    ours = Some(f.data);
                }
            }
            Ok(None) => {}
            Err(e) => {
                eprintln!("decode error at frame {i}: {e}");
                return;
            }
        }
    }
    let Some(ours) = ours else {
        eprintln!("skipping: frame {target} is not a shown frame");
        return;
    };

    let reference = std::fs::read(&ref_path).expect("read reference yuv");
    let frame_bytes = w * h + 2 * (w.div_ceil(2) * h.div_ceil(2));
    let ref_off = target * frame_bytes;
    if reference.len() < ref_off + frame_bytes {
        eprintln!(
            "skipping: reference dump has {} bytes, need {} for frame {target}",
            reference.len(),
            ref_off + frame_bytes
        );
        return;
    }
    let reference = &reference[ref_off..ref_off + frame_bytes];
    if ours.len() != reference.len() {
        eprintln!(
            "skipping: size mismatch ours={} ref={}",
            ours.len(),
            reference.len()
        );
        return;
    }

    println!(
        "frame {target} of {} ({w}x{h}): localizing divergence",
        ivf_path.display()
    );
    let pl = planes(w, h);
    let mut total = 0usize;
    let mut printed = 0usize;
    for p in &pl {
        let mut plane_diff = 0usize;
        let mut min_x = usize::MAX;
        let mut max_x = 0usize;
        let mut min_y = usize::MAX;
        let mut max_y = 0usize;
        for y in 0..p.h {
            for x in 0..p.w {
                let idx = p.offset + y * p.w + x;
                let (a, b) = (ours[idx], reference[idx]);
                if a == b {
                    continue;
                }
                plane_diff += 1;
                min_x = min_x.min(x);
                max_x = max_x.max(x);
                min_y = min_y.min(y);
                max_y = max_y.max(y);
                if printed < max_print {
                    println!(
                        "  {} ({:>3},{:>3}) ours={:>3} ref={:>3} delta={:>+4}",
                        p.name,
                        x,
                        y,
                        a,
                        b,
                        a as i32 - b as i32
                    );
                    printed += 1;
                }
            }
        }
        total += plane_diff;
        if plane_diff > 0 {
            println!(
                "  -> plane {}: {plane_diff} differing samples, bbox x[{min_x}..{max_x}] y[{min_y}..{max_y}] (plane-local)",
                p.name
            );
        }
    }
    println!("total differing samples in frame {target}: {total}");
    if printed >= max_print && total > printed {
        println!(
            "({} further samples not printed; raise KINETIX_AV1_LOC_MAX)",
            total - printed
        );
    }
}
