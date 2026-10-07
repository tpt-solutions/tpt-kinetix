//! Mutation hardening for the parallel tile-column path: take a real
//! multi-tile frame (encoded by ffmpeg/libvpx when available) and corrupt it
//! in many ways — bit flips, truncation, garbled tile-size fields. Every
//! mutant must decode or return `Err`, never panic (including inside the
//! rayon workers).

use std::process::{Command, Stdio};

use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};
use tpt_kinetix_vp9::Vp9Decoder;

fn first_frames(n: usize) -> Option<Vec<Vec<u8>>> {
    let out = std::env::temp_dir().join("kinetix-vp9-tile-mutation.ivf");
    let ok = Command::new("ffmpeg")
        .args(["-y", "-hide_banner", "-v", "error", "-f", "lavfi", "-i"])
        .arg("testsrc=size=1024x128:rate=30")
        .args([
            "-frames:v",
            &n.to_string(),
            "-pix_fmt",
            "yuv420p",
            "-c:v",
            "libvpx-vp9",
        ])
        .args(["-deadline", "good", "-cpu-used", "5", "-lag-in-frames", "0"])
        .args(["-tile-columns", "2", "-tile-rows", "1"])
        .arg(&out)
        .stdin(Stdio::null())
        .status()
        .ok()?
        .success();
    if !ok {
        return None;
    }
    let d = std::fs::read(&out).ok()?;
    let mut frames = Vec::new();
    let mut p = 32;
    while p + 12 <= d.len() {
        let sz = u32::from_le_bytes(d[p..p + 4].try_into().ok()?) as usize;
        frames.push(d.get(p + 12..p + 12 + sz)?.to_vec());
        p += 12 + sz;
    }
    Some(frames)
}

fn decode(frames: &[Vec<u8>]) {
    let mut dec = Vp9Decoder::new();
    for (i, f) in frames.iter().enumerate() {
        let _ = dec.decode(&Packet {
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            data: f.clone(),
            stream_index: 0,
            is_key_frame: i == 0,
        });
    }
}

#[test]
fn mutated_multitile_frames_never_panic() {
    let Some(frames) = first_frames(3) else {
        eprintln!("skipping: ffmpeg/libvpx unavailable");
        return;
    };
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for _ in 0..1500 {
        let mut m = frames.clone();
        let fi = (rnd() % m.len() as u64) as usize;
        let len = m[fi].len();
        match rnd() % 4 {
            0 => {
                for _ in 0..1 + rnd() % 8 {
                    let at = (rnd() % len as u64) as usize;
                    m[fi][at] ^= 1 << (rnd() % 8);
                }
            }
            1 => m[fi].truncate((rnd() % len as u64) as usize),
            2 => {
                // Garble a 4-byte window anywhere in the second half (where the
                // big-endian tile-size fields live).
                let at = len / 2 + (rnd() % (len as u64 / 2 - 4)) as usize;
                for b in &mut m[fi][at..at + 4] {
                    *b = rnd() as u8;
                }
            }
            _ => {
                let at = (rnd() % len as u64) as usize;
                m[fi][at..].fill(rnd() as u8);
            }
        }
        decode(&m);
    }
}
