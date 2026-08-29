//! Crate-side replay of the failing IBP P slice (cabac_b cell) over the SAME
//! hardcoded payload bytes used by `dbg_p_oracle_replay.rs`, so the
//! `KINETIX_BINTRACE=1` per-bin traces of both walks start at the same byte.
//!
//! Run: cargo test -p tpt-kinetix-h264 --test dbg_bintrace_replay -- --nocapture
//! (set KINETIX_BINTRACE=1 to get one `BIN n ...` line per decoded bin).

use tpt_kinetix_h264::{slice_data::parse_p_slice_cabac, NoopTracer};

#[rustfmt::skip]
const PAYLOAD: [u8; 64] = [
    0xFA, 0xB5, 0x69, 0xA6, 0x09, 0x1F, 0xD4, 0x00, 0x6C, 0xB7, 0xFF, 0xE0, 0xBA, 0x7F, 0x05,
    0x4E, 0xD6, 0x40, 0x1B, 0xCB, 0x61, 0xBF, 0xDB, 0x81, 0x3F, 0xFA, 0x36, 0x51, 0xD8, 0x8F,
    0x37, 0xA8, 0x26, 0x32, 0xEF, 0xFD, 0x8F, 0x6D, 0xC2, 0x4E, 0xA8, 0x64, 0x4C, 0x24, 0x93,
    0x58, 0xC3, 0x60, 0x2C, 0x73, 0xD8, 0xEF, 0x32, 0x9E, 0x9C, 0x02, 0xAE, 0xD6, 0xCE, 0x02,
    0x1D, 0xE5, 0x37, 0x0A,
];

#[test]
fn p_slice_crate_replay_same_payload_as_oracle() {
    // Same parameters the oracle assumes: slice QP 2, cabac_init_idc 0,
    // 64x48 clip => 4x3 macroblocks, ref list of one entry.
    let result = parse_p_slice_cabac(
        &PAYLOAD,
        4,
        3,
        2,
        false,
        false,
        0,
        1,
        0,
        false,
        true,
        &mut NoopTracer,
    );
    match result {
        Ok(parsed) => {
            for (i, mb) in parsed.macroblocks.iter().enumerate() {
                eprintln!(
                    "CRATE MB{i} skip={} cbp={:#04x} qp={}",
                    mb.skip, mb.cbp, mb.qp
                );
            }
        }
        Err(e) => eprintln!("CRATE parse error: {e:?}"),
    }
}
