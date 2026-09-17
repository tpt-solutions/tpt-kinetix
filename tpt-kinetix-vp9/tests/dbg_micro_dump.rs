//! Temporary debug: dump our decoder's micro-clip output values.

use std::path::PathBuf;
use std::process::Command;

#[test]
fn dbg_micro_dump() {
    let ivf_path = match std::env::var("TPT_VP9_IVF") {
        Ok(p) => PathBuf::from(p),
        Err(_) => return, // only runs when explicitly pointed at a clip
    };
    let data = std::fs::read(&ivf_path).unwrap();
    // skip IVF header
    let size = u32::from_le_bytes([data[32], data[33], data[34], data[35]]) as usize;
    let frame = data[44..44 + size].to_vec();
    let mut dec = tpt_kinetix_vp9::Vp9Decoder::new();
    let packet = tpt_kinetix_core::packet::Packet {
        pts: tpt_kinetix_core::timestamp::Timestamp::NONE,
        dts: tpt_kinetix_core::timestamp::Timestamp::NONE,
        data: frame,
        stream_index: 0,
        is_key_frame: true,
    };
    let vf = dec.decode(&packet).unwrap().unwrap();
    let w = vf.width as usize;
    let h = vf.height as usize;
    println!("frame {}x{}", w, h);
    println!("Y[0..8]: {:?}", &vf.data[..8]);
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    let u = &vf.data[w * h..w * h + cw * ch];
    println!("U[0..8]: {:?}", &u[..8]);
    println!("U row0: {:?}", &u[..cw]);
    println!("U row1: {:?}", &u[cw..2 * cw]);
    let v = &vf.data[w * h + cw * ch..];
    println!("V row0: {:?}", &v[..cw]);
}
