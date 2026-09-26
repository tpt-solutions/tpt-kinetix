//! Scratch: decode an IVF frame-by-frame with Kinetix and diff every frame
//! against dav1d. Usage: probe_tiles <ivf-path> [max-frames]
use tpt_kinetix_av1::Av1Decoder;
use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};
use tpt_kinetix_test_utils::{
    pixel_diff::within_tolerance,
    reference::{dav1d_available, decode_av1_with_dav1d, split_ivf_frames},
};

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: probe_tiles <ivf> [max]");
    let max: Option<usize> = std::env::args().nth(2).and_then(|s| s.parse().ok());
    if !dav1d_available() {
        eprintln!("dav1d not available");
        return;
    }
    let bytes = std::fs::read(&path).unwrap();
    let w = u16::from_le_bytes([bytes[12], bytes[13]]) as u32;
    let h = u16::from_le_bytes([bytes[14], bytes[15]]) as u32;
    let ref_frames = decode_av1_with_dav1d(&bytes, w, h).unwrap();
    let packets = split_ivf_frames(&bytes);
    let mut dec = Av1Decoder::new();
    let mut kframes = Vec::new();
    for (i, data) in packets.iter().enumerate() {
        let pk = Packet {
            pts: Timestamp::new(i as i64, (1, 30)),
            dts: Timestamp::new(i as i64, (1, 30)),
            data: data.clone(),
            stream_index: 0,
            is_key_frame: i == 0,
        };
        match dec.decode(&pk) {
            Ok(Some(f)) => kframes.push(f),
            Ok(None) => println!("packet {i}: no frame shown"),
            Err(e) => {
                println!("packet {i}: DECODE ERROR {e}");
                break;
            }
        }
        if let Some(m) = max {
            if kframes.len() >= m {
                break;
            }
        }
    }
    let n = kframes.len().min(ref_frames.len());
    let mut exact = 0;
    for i in 0..n {
        let kf = &kframes[i];
        let rf = &ref_frames[i];
        if within_tolerance(kf, rf, 0) {
            exact += 1;
            continue;
        }
        let diff: usize = kf
            .data
            .iter()
            .zip(rf.data.iter())
            .filter(|(a, b)| a != b)
            .count();
        let ysz = (kf.width as usize) * (kf.height as usize);
        let first = kf.data.iter().zip(rf.data.iter()).position(|(a, b)| a != b);
        let (comp, x, y) = match first {
            Some(o) if o < ysz => ("Y", o % kf.width as usize, o / kf.width as usize),
            Some(o) if o < ysz + ysz / 4 => (
                "U",
                (o - ysz) % (kf.width as usize / 2),
                (o - ysz) / (kf.width as usize / 2),
            ),
            Some(o) => (
                "V",
                (o - ysz - ysz / 4) % (kf.width as usize / 2),
                (o - ysz - ysz / 4) / (kf.width as usize / 2),
            ),
            None => ("?", 0, 0),
        };
        println!("frame {i}: MISMATCH {diff} bytes, first at {comp} ({x},{y})");
    }
    println!("{exact}/{n} frames exact vs dav1d");
}
