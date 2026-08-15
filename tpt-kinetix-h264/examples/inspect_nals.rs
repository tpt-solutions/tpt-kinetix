//! Inspect NAL units (types, rbsp lengths) of an Annex B H.264 file.
//! Usage: `cargo run -p tpt-kinetix-h264 --example inspect_nals -- <file.h264>`

use std::env;

use tpt_kinetix_h264::nal::parse_nal_units_from_annexb;

fn main() {
    let path = env::args().nth(1).expect("need a file path");
    let data = std::fs::read(&path).expect("read file");
    let units = parse_nal_units_from_annexb(&data);
    println!("file={path} nals={}", units.len());
    for u in &units {
        println!(
            "  type={:?} nal_ref_idc={} rbsp_len={}",
            u.nal_unit_type,
            u.nal_ref_idc,
            u.rbsp.len()
        );
    }
}
