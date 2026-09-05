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
    let mut printed_sps = false;
    for nal in parse_nal_units_from_annexb(&annexb) {
        match nal.nal_unit_type as u8 {
            7 => {
                let sps = Sps::parse(&nal.rbsp).unwrap();
                eprintln!(
                    "{clip} SPS id={} pic_width_in_mbs_minus1={} pic_height_in_map_units_minus1={} log2_max_frame_num_minus4={}",
                    sps.seq_parameter_set_id,
                    sps.pic_width_in_mbs_minus1,
                    sps.pic_height_in_map_units_minus1,
                    sps.log2_max_frame_num_minus4
                );
                if !printed_sps {
                    eprintln!("{clip} first SPS full: {sps:#?}");
                    printed_sps = true;
                }
            }
            8 => {
                eprintln!("{clip} PPS nal seen (rbsp len {})", nal.rbsp.len());
            }
            1 | 5 => {
                if let Some(id) = tpt_kinetix_h264::slice::peek_pic_parameter_set_id(&nal.rbsp) {
                    eprintln!(
                        "{clip} slice type={} pps_id={id}",
                        if nal.nal_unit_type as u8 == 5 {
                            "IDR"
                        } else {
                            "non-IDR"
                        }
                    );
                }
            }
            _ => {}
        }
    }
}
