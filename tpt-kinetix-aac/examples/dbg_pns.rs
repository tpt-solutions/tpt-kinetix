//! Scratch: decode a mono-noise AAC file with both the native decoder and
//! ffmpeg, and compare per-1024-frame band energy to localize the PNS mismatch.
//! Run: cargo +stable test --example dbg_pns -- --nocapture (then invoke via main)
//! Place adts at C:\Users\phill\AppData\Local\Temp\kilo\mn.aac

use std::process::Command;
use tpt_kinetix_aac::AacDecoder;
use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;

fn split_adts(data: &[u8]) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    let mut i = 0;
    while i + 7 <= data.len() {
        if data[i] == 0xFF && (data[i + 1] & 0xF0) == 0xF0 {
            let fl = (((data[i + 3] & 0x03) as usize) << 11)
                | ((data[i + 4] as usize) << 3)
                | ((data[i + 5] as usize) >> 5);
            if fl == 0 || i + fl > data.len() { break; }
            frames.push(data[i..i + fl].to_vec());
            i += fl;
        } else { i += 1; }
    }
    frames
}

fn f32le(buf: &[u8]) -> Vec<f32> {
    buf.chunks_exact(4).map(|c| f32::from_le_bytes([c[0],c[1],c[2],c[3]])).collect()
}

fn main() {
    let adts = std::fs::read(r"C:\Users\phill\AppData\Local\Temp\kilo\mn.aac").unwrap();
    let frames = split_adts(&adts);
    eprintln!("frames={}", frames.len());

    // native, also capture per-frame window_sequence
    let mut dec = AacDecoder::new();
    let mut native = Vec::new();
    for f in &frames {
        let pkt = Packet { pts: Timestamp::NONE, dts: Timestamp::NONE, data: f.clone(), stream_index: 0, is_key_frame: true };
        if let Ok(Some(fr)) = dec.decode(&pkt) {
            native.extend_from_slice(&fr.data);
        }
    }
    let native = f32le(&native);

    // ffmpeg reference pcm (f32le)
    let out = Command::new("ffmpeg")
        .args(["-loglevel","error","-i",r"C:\Users\phill\AppData\Local\Temp\kilo\mn.aac","-f","f32le","-ac","1","-"])
        .output().unwrap();
    let reference = f32le(&out.stdout);
    eprintln!("native samples={} ref={}", native.len(), reference.len());

    // Compare energy in overlapping 1024-sample windows.
    let n = native.len().min(reference.len());
    for w in (0..n).step_by(1024) {
        let ne: f64 = native[w..w+1024.min(n-w)].iter().map(|x| (*x as f64)*(*x as f64)).sum();
        let re: f64 = reference[w..w+1024.min(n-w)].iter().map(|x| (*x as f64)*(*x as f64)).sum();
        let ratio = if re > 1e-9 { ne/re } else { f64::NAN };
        eprintln!("frame@{} native_e={:.4e} ref_e={:.4e} ratio={:.3}", w, ne, re, ratio);
    }
    // Detailed first-frame region: print native vs reference sample values for
    // frames 0 and 1, first 64 samples, to see the actual shape.
    eprintln!("--- frame 0 samples 400..600 native vs ref ---");
    for i in 400..600 {
        eprintln!("  i={i:3}  native={:+.4}  ref={:+.4}", native[i], reference[i]);
    }
    // peak
    let nm = native.iter().map(|x| x.abs()).fold(0.0f32, f32::max);
    let rm = reference.iter().map(|x| x.abs()).fold(0.0f32, f32::max);
    eprintln!("native peak={:.4} ref peak={:.4}", nm, rm);
}
