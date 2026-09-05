//! FFmpeg-engine oracle replay of the failing IBP P slice (cabac_b cell).
//!
//! Walks FFmpeg's exact P-slice CABAC element sequence (transcribed from
//! `ff_h264_decode_mb_cabac` in the fetched `ff_h264_cabac.c`) over the SAME
//! payload bytes the crate parser reads, using the FFmpeg-convention engine
//! formulation proven in `dbg_cabac_skip_probe.rs`. Prints every bin with its
//! context index; diffing against the crate trace (`dbg_out.txt`) pinpoints
//! the first divergent read. See todo-h264.md session #11 next step.
//!
//! Run: cargo test -p tpt-kinetix-h264 --test dbg_p_oracle_replay -- --nocapture

//! FFmpeg-element-walk oracle replay of the failing IBP P slice (cabac_b cell).
//!
//! Walks FFmpeg's exact P-slice CABAC element sequence (transcribed from
//! `ff_h264_decode_mb_cabac`) over the SAME payload bytes the crate parser
//! reads -- but using THE CRATE'S OWN CABAC ENGINE and context variables
//! (flat 1024-entry table indexed by spec ctxIdx). Engine equivalence is
//! guaranteed by construction; any bin divergence between this walk and the
//! parser's own trace is definitively an element-order/context bug.
//!
//! Run: cargo test -p tpt-kinetix-h264 --test dbg_p_oracle_replay -- --nocapture

use tpt_kinetix_h264::entropy::{CabacContext, CabacDecoder};

struct Eng<'a> {
    dec: CabacDecoder<'a>,
    st: Vec<CabacContext>,
}

impl<'a> Eng<'a> {
    fn new(data: &'a [u8], slice_qp: i32) -> Self {
        let dec = CabacDecoder::new(data).unwrap();
        let st = (0..1024)
            .map(|i| {
                let (m, n) = tpt_kinetix_h264::cabac_tables::CABAC_CTX_INIT_PB0[i];
                CabacContext::init(m as i32, n as i32, slice_qp)
            })
            .collect();
        Eng { dec, st }
    }
    fn get(&mut self, idx: usize) -> u32 {
        self.dec.decode_decision(&mut self.st[idx]) as u32
    }
    #[allow(dead_code)] // used when the walk is extended past intra MBs
    fn bypass(&mut self) -> u32 {
        self.dec.decode_bypass() as u32
    }
    fn terminate(&mut self) -> u32 {
        self.dec.decode_terminate() as u32
    }
}

fn log(name: &str, idx: usize, v: u32) {
    if idx == 999 {
        eprintln!("ORACLE {name}: bypass bin={v}");
    } else {
        eprintln!("ORACLE {name}: ctx={idx} bin={v}");
    }
}

#[rustfmt::skip]
const PAYLOAD: [u8; 64] = [
    0xFA, 0xB5, 0x69, 0xA6, 0x09, 0x1F, 0xD4, 0x00, 0x6C, 0xB7, 0xFF, 0xE0, 0xBA, 0x7F, 0x05,
    0x4E, 0xD6, 0x40, 0x1B, 0xCB, 0x61, 0xBF, 0xDB, 0x81, 0x3F, 0xFA, 0x36, 0x51, 0xD8, 0x8F,
    0x37, 0xA8, 0x26, 0x32, 0xEF, 0xFD, 0x8F, 0x6D, 0xC2, 0x4E, 0xA8, 0x64, 0x4C, 0x24, 0x93,
    0x58, 0xC3, 0x60, 0x2C, 0x73, 0xD8, 0xEF, 0x32, 0x9E, 0x9C, 0x02, 0xAE, 0xD6, 0xCE, 0x02,
    0x1D, 0xE5, 0x37, 0x0A,
];

#[test]
fn p_slice_ffmpeg_element_walk_on_crate_engine() {
    // Slice QP 2 / cabac_init_idc 0, matching the crate parser parameters.
    let mut eng = Eng::new(&PAYLOAD, 2);

    // ── MB(0,0): skip (both neighbours unavailable) ──
    let v = eng.get(11);
    log("MB0 skip", 11, v);
    assert_eq!(v, 1, "MB0 expected skip");
    log("MB0 eos", 0, eng.terminate());

    // ── MB(1,0): skip_flag (left=MB0 skipped, top unavailable) ──
    let v = eng.get(11);
    log("MB1 skip", 11, v);
    assert_eq!(v, 0, "MB1 expected non-skip");

    // mb_type prefix: get(ctx14)==1 -> intra-in-P.
    let v = eng.get(14);
    log("MB1 mbtype.prefix", 14, v);
    assert_eq!(v, 1, "MB1 expected intra-in-P");

    // decode_cabac_intra_mb_type(ctx_base=17, intra_slice=0).
    let b0 = eng.get(17);
    log("MB1 intra.b0", 17, b0);
    let mut intra_t: u32 = 0;
    if b0 != 0 {
        let pcm = eng.terminate();
        log("MB1 intra.pcm", 17, pcm);
        assert_eq!(pcm, 0, "unexpected I_PCM");
        intra_t = 1;
        let bl = eng.get(18);
        log("MB1 intra.cbpluma", 18, bl);
        intra_t += 12 * bl;
        let bc = eng.get(19);
        log("MB1 intra.cbpchroma.presence", 19, bc);
        if bc == 1 {
            let bv = eng.get(19);
            log("MB1 intra.cbpchroma.value", 19, bv);
            intra_t += 4 + 4 * bv;
        }
        let ph = eng.get(20);
        log("MB1 intra.predmode.hi", 20, ph);
        intra_t += 2 * ph;
        let pl = eng.get(20);
        log("MB1 intra.predmode.lo", 20, pl);
        intra_t += pl;
    }
    eprintln!("ORACLE MB1 intra_t={intra_t} (0=I4x4, 1..=24=I16x16)");
    let is_i16 = intra_t != 0;

    // Intra_4x4 prediction modes (only when I_NxN).
    if !is_i16 {
        for blk in 0..16u32 {
            let f = eng.get(68);
            log(&format!("MB1 mode[{blk}].prevflag"), 68, f);
            if f == 0 {
                for k in 0..3 {
                    let r = eng.get(69);
                    log(&format!("MB1 mode[{blk}].rem{k}"), 69, r);
                }
            }
        }
    }

    // intra_chroma_pred_mode: neighbours unavailable => ctx 64.
    let b = eng.get(64);
    log("MB1 chroma.b0", 64, b);
    if b == 1 {
        let b1 = eng.get(67);
        log("MB1 chroma.b1", 67, b1);
        if b1 == 1 {
            let b2 = eng.get(67);
            log("MB1 chroma.b2", 67, b2);
        }
    }

    // I_NxN: coded_block_pattern (sentinel neighbours).
    let mut cbp_l: u32 = 0;
    if !is_i16 {
        let sent: u32 = 0x7CF;
        let mut cbp_a = sent;
        let mut cbp_b = sent;
        let mut cur: u32 = 0;
        let specs: [(u32, u32, u32); 4] = [
            (0x02, 0x04, 1),
            (0x01, 0x08, 2),
            (0x08, 0x01, 4),
            (0x04, 0x02, 8),
        ];
        for (i, (ma, mb, shift)) in specs.iter().enumerate() {
            let ctx = (cbp_a & ma == 0) as usize + 2 * (cbp_b & mb == 0) as usize;
            let b = eng.get(73 + ctx);
            log(&format!("MB1 cbpluma[{i}]"), 73 + ctx, b);
            cur += b << shift.trailing_zeros();
            cbp_a = (cur & 0b1010) | (sent & !0b1010);
            cbp_b = (cur & 0b0101) << 1 | (sent & !0b11110);
        }
        cbp_l = cur;
        eprintln!("ORACLE MB1 cbp_luma={cbp_l:#x}");
    }

    // mb_qp_delta: first bin at 60 + (last_qscale_diff != 0), then unary tail
    // at 60+2 / 60+3.
    let need_qp = is_i16 || cbp_l != 0;
    if need_qp {
        let b = eng.get(60);
        log("MB1 qpdelta.first", 60, b);
        let mut val = 0u32;
        if b == 1 {
            val = 1;
            let mut ctx = 2usize;
            loop {
                let bb = eng.get(60 + ctx);
                log("MB1 qpdelta.cont", 60 + ctx, bb);
                if bb == 0 {
                    break;
                }
                ctx = 3;
                val += 1;
                if val > 1024 {
                    break;
                }
            }
        }
        eprintln!("ORACLE MB1 qpdelta_unary_val={val}");
    }

    // Residual luma-DC coded_block_flag for Intra_16x16 (cat 0, base 85).
    if is_i16 {
        let b = eng.get(87);
        log("MB1 dc.cbf", 87, b);
        if b == 1 {
            eprintln!("ORACLE MB1 DC block NONZERO -- extend replay for sig maps");
        }
    }

    log("MB1 eos", 0, eng.terminate());
}
