//! Throwaway: print direct_8x8_inference_flag / weighted_bipred_idc /
//! entropy_coding_mode for several fixtures to double check readme claims.
use std::path::{Path, PathBuf};

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/itu")
}

fn find_bitstream(dir: &Path) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let p = entry.path();
        match p.extension().and_then(|e| e.to_str()) {
            Some("264") | Some("jsv") | Some("h264") | Some("avc") | Some("26l") | Some("jvt")
            | Some("bits") => return Some(p),
            _ => {}
        }
    }
    None
}

fn sps_pps(data: &[u8]) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    let mut sps = None;
    let mut pps = None;
    let mut j = 0usize;
    while j + 4 < data.len() {
        if data[j] == 0 && data[j + 1] == 0 && data[j + 2] == 1 {
            let nal_type = data[j + 3] & 0x1F;
            if nal_type == 7 || nal_type == 8 {
                let mut k = j + 4;
                let mut rbsp = Vec::new();
                while k + 3 < data.len() && !(data[k] == 0 && data[k + 1] == 0 && data[k + 2] == 1)
                {
                    if data[k] == 0 && data[k + 1] == 0 && data[k + 2] == 3 {
                        rbsp.push(0);
                        rbsp.push(0);
                        k += 3;
                    } else {
                        rbsp.push(data[k]);
                        k += 1;
                    }
                }
                if nal_type == 7 && sps.is_none() {
                    sps = Some(rbsp);
                } else if nal_type == 8 && pps.is_none() {
                    pps = Some(rbsp);
                }
            }
        }
        j += 1;
    }
    (sps, pps)
}

#[test]
fn print_flags() {
    for name in [
        "CVBS3_Sony_C",
        "BA3_SVA_C",
        "CACQP3_Sony_D",
        "CABA3_Sony_C",
        "CANL3_Sony_C",
        "CABACI3_Sony_B",
    ] {
        let dir = fixtures_root().join(name);
        if !dir.exists() {
            println!("{name}: missing");
            continue;
        }
        let Some(bs) = find_bitstream(&dir) else {
            println!("{name}: no bitstream");
            continue;
        };
        let data = std::fs::read(&bs).unwrap();
        let (sps_rbsp, pps_rbsp) = sps_pps(&data);
        let sps_str = sps_rbsp
            .as_ref()
            .map(|r| match tpt_kinetix_h264::sps::SeqParameterSet::parse(r) {
                Ok(sp) => format!(
                    "direct_8x8_inference={} profile_idc={}",
                    sp.direct_8x8_inference_flag, sp.profile_idc
                ),
                Err(e) => format!("SPS parse err {e:?}"),
            })
            .unwrap_or_else(|| "no SPS".into());
        let pps_str = pps_rbsp
            .as_ref()
            .map(|r| match tpt_kinetix_h264::pps::PicParameterSet::parse(r, None) {
                Ok(p) => format!(
                    "entropy_coding_mode={} weighted_pred={} weighted_bipred_idc={}",
                    p.entropy_coding_mode_flag, p.weighted_pred_flag, p.weighted_bipred_idc
                ),
                Err(e) => format!("PPS parse err {e:?}"),
            })
            .unwrap_or_else(|| "no PPS".into());
        println!("{name}: {sps_str} | {pps_str}");
    }
}
