//! `#[ignore]` diagnostic: find the frame + element where `al22_chCfg0PCE_44`
//! (ISO LC 7.1, channel_config 0 / PCE) fails to parse.

use std::path::{Path, PathBuf};
use tpt_kinetix_aac::syntax::Element;
use tpt_kinetix_aac::{AdtsHeader, RawDataBlock};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/iso")
}

fn split_adts(adts: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 7 <= adts.len() {
        if adts[i] != 0xFF || (adts[i + 1] & 0xF0) != 0xF0 {
            break;
        }
        let len = (((adts[i + 3] as usize & 0x03) << 11)
            | ((adts[i + 4] as usize) << 3)
            | ((adts[i + 5] as usize & 0xE0) >> 5))
            & 0x1FFF;
        if len == 0 || i + len > adts.len() {
            break;
        }
        out.push(adts[i..i + len].to_vec());
        i += len;
    }
    out
}

fn dump(f: &[u8]) -> String {
    let hdr = AdtsHeader::parse(f).unwrap();
    match RawDataBlock::parse(
        &f[hdr.header_len..hdr.frame_length.min(f.len())],
        hdr.sampling_frequency_index as usize,
    ) {
        Ok(b) => {
            let mut s = String::new();
            for el in &b.elements {
                match el {
                    Element::Sce(e) => s.push_str(&format!(
                        "SCE(t{},{:?},msf{}) ",
                        e.instance_tag, e.stream.ics.window_sequence, e.stream.ics.max_sfb
                    )),
                    Element::Cpe(e) => s.push_str(&format!(
                        "CPE(t{},ms{},{:?}) ",
                        e.instance_tag, e.ms_mask_present, e.left.ics.window_sequence
                    )),
                    Element::Lfe(e) => s.push_str(&format!("LFE(t{}) ", e.instance_tag)),
                    Element::Cce(e) => s.push_str(&format!("CCE(t{}) ", e.instance_tag)),
                    Element::Fil(_) => s.push_str("FIL "),
                    Element::End => s.push_str("END"),
                }
            }
            format!("OK: {s}")
        }
        Err(e) => format!("ERR: {e:?}  (payload {} bytes)", f.len() - hdr.header_len),
    }
}

#[test]
#[ignore = "diagnostic"]
fn probe_frame() {
    // AAC_DBG_ICS=1 AAC_DBG_TARGET=am00_88:17
    let Ok(spec) = std::env::var("AAC_DBG_TARGET") else {
        eprintln!("set AAC_DBG_TARGET=<name>:<frame>");
        return;
    };
    let (name, fno) = spec.split_once(':').unwrap();
    let fno: usize = fno.parse().unwrap();
    let adts = std::fs::read(root().join(format!("{name}.adts"))).unwrap();
    let frames = split_adts(&adts);
    let f = &frames[fno];
    let hdr = AdtsHeader::parse(f).unwrap();
    eprintln!(
        "frame {fno}: sf_index={} chcfg={} payload={} bytes",
        hdr.sampling_frequency_index,
        hdr.channel_configuration,
        f.len() - hdr.header_len
    );
    let r = RawDataBlock::parse(
        &f[hdr.header_len..hdr.frame_length.min(f.len())],
        hdr.sampling_frequency_index as usize,
    );
    eprintln!("result: {r:?}");
}

#[test]
#[ignore = "diagnostic"]
fn probe_al22() {
    for name in ["al22_chCfg0PCE_44", "al17_44", "am00_88", "am05_44"] {
        let Ok(adts) = std::fs::read(root().join(format!("{name}.adts"))) else {
            continue;
        };
        let frames = split_adts(&adts);
        eprintln!("=== {name}: {} frames", frames.len());
        let mut shown = 0;
        for (fi, f) in frames.iter().enumerate() {
            let d = dump(f);
            let is_err = d.starts_with("ERR");
            if is_err || fi < 6 || (shown < 30 && is_err) {
                eprintln!("  f{fi:4}: {d}");
                shown += 1;
            }
            if shown > 40 {
                break;
            }
        }
    }
}
