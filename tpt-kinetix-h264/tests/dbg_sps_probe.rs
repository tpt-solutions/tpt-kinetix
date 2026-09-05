//! Scratch: print SPS fields (esp. `direct_8x8_inference_flag`) for an ITU clip.
use std::path::Path;
use tpt_kinetix_h264::nal::parse_nal_units_from_annexb;
use tpt_kinetix_h264::sps::SeqParameterSet as Sps;

#[test]
#[ignore = "diagnostic"]
fn print_sps() {
    let clip = std::env::var("ITU_CLIP").unwrap_or_else(|_| "BA3_SVA_C".to_string());
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/itu")
        .join(&clip);
    let bs = std::fs::read_dir(&dir)
        .ok()
        .and_then(|rd| {
            rd.flatten().map(|e| e.path()).find(|p| {
                p.extension()
                    .is_some_and(|x| matches!(x.to_str(), Some("264" | "jsv" | "h264" | "avc")))
            })
        })
        .expect("fixture");
    let annexb = std::fs::read(&bs).unwrap();
    for nal in parse_nal_units_from_annexb(&annexb) {
        if nal.nal_unit_type as u8 == 7 {
            let sps = Sps::parse(&nal.rbsp).unwrap();
            eprintln!("{clip} SPS: {sps:#?}");
            return;
        }
    }
}
