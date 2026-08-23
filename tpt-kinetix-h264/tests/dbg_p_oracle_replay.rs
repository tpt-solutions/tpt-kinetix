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

use std::process::Command;

#[rustfmt::skip]
const RT: [[u32; 4]; 64] = [
    [128,176,208,240],[128,167,197,227],[128,158,187,216],[123,150,178,205],
    [116,142,169,195],[111,135,160,185],[105,128,152,175],[100,122,144,166],
    [95,116,137,158],[90,110,130,150],[85,104,123,142],[81,99,117,135],
    [77,94,111,128],[73,89,105,122],[69,85,100,116],[66,80,95,110],
    [62,76,90,104],[59,72,86,99],[56,69,81,94],[53,65,77,89],
    [51,62,73,85],[48,59,69,80],[46,56,66,76],[43,53,63,72],
    [41,50,59,69],[39,48,56,65],[37,45,54,62],[35,43,51,59],
    [33,41,48,56],[32,39,46,53],[30,37,43,50],[29,35,41,48],
    [27,33,39,45],[26,31,37,43],[24,30,35,41],[23,28,33,39],
    [22,27,32,37],[21,26,30,35],[20,24,29,33],[19,23,27,31],
    [18,22,26,30],[17,21,25,28],[16,20,23,27],[15,19,22,25],
    [14,18,21,24],[14,17,20,23],[13,16,19,22],[12,15,18,21],
    [12,14,17,20],[11,14,16,19],[11,13,15,18],[10,12,15,17],
    [10,12,14,16],[9,11,13,15],[9,11,12,14],[8,10,12,14],
    [8,9,11,13],[7,9,11,12],[7,9,10,12],[7,8,10,11],
    [6,8,9,11],[6,7,9,10],[6,7,8,9],[2,2,2,2],
];
const TL: [u8; 64] = [
    0, 0, 1, 2, 2, 4, 4, 5, 6, 7, 8, 9, 9, 11, 11, 12, 13, 13, 15, 15, 16, 16,
    18, 18, 19, 19, 21, 21, 22, 22, 23, 24, 24, 25, 26, 26, 27, 27, 28, 29, 29,
    30, 30, 30, 31, 32, 32, 33, 33, 33, 34, 34, 35, 35, 35, 36, 36, 36, 37, 37,
    37, 38, 38, 63,
];
const TM: [u8; 64] = [
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21,
    22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40,
    41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59,
    60, 61, 62, 62, 63,
];

struct Eng<'a> {
    data: &'a [u8],
    pos: usize,
    low: u64,
    range: u32,
}

impl<'a> Eng<'a> {
    fn new(data: &'a [u8]) -> Self {
        let g = |i: usize| *data.get(i).unwrap_or(&0) as u64;
        Eng {
            data,
            pos: 24,
            low: (g(0) << 18) | (g(1) << 10) | ((g(2) & 0xC0) << 2),
            range: 0x1FE,
        }
    }
    fn bit(&mut self) -> u64 {
        let b = match self.data.get(self.pos / 8) {
            Some(&v) => ((v >> (7 - self.pos % 8)) & 1) as u64,
            None => 0,
        };
        self.pos += 1;
        b
    }
    fn get(&mut self, st: &mut [u8; 1024], idx: usize) -> u32 {
        let s = st[idx];
        let q = ((self.range >> 6) & 3) as usize;
        let rlps = RT[(s >> 1) as usize][q];
        self.range -= rlps;
        let bit;
        if self.low < (self.range as u64) * 131072 {
            bit = (s & 1) as u32;
            st[idx] = (TM[(s >> 1) as usize] << 1) | (s & 1);
        } else {
            self.low -= (self.range as u64) * 131072;
            self.range = rlps;
            let mps = if (s >> 1) == 0 { 1 - (s & 1) } else { s & 1 };
            st[idx] = (TL[(s >> 1) as usize] << 1) | mps;
            bit = 1 - (s & 1) as u32;
        }
        while self.range < 256 {
            self.low = (self.low << 1) | self.bit();
            self.range <<= 1;
        }
        bit
    }
    fn bypass(&mut self) -> u32 {
        self.low = (self.low << 1) | self.bit();
        if self.low >= (self.range as u64) << 17 {
            self.low -= (self.range as u64) << 17;
            1
        } else {
            0
        }
    }
    fn terminate(&mut self) -> u32 {
        self.range -= 2;
        if self.low >= (self.range as u64) * 131072 {
            1
        } else {
            while self.range < 256 {
                self.low = (self.low << 1) | self.bit();
                self.range <<= 1;
            }
            0
        }
    }
}

fn log(name: &str, idx: usize, v: u32) {
    if idx == 999 {
        eprintln!("ORACLE {name}: bypass bin={v}");
    } else {
        eprintln!("ORACLE {name}: ctx={idx} bin={v}");
    }
}

fn gen(dir: &std::path::Path, name: &str) -> Option<Vec<u8>> {
    let h264 = dir.join(format!("{name}.h264"));
    let ok = Command::new("ffmpeg")
        .args([
            "-hide_banner", "-loglevel", "error", "-y",
            "-f", "lavfi", "-i",
            "color=c=black:size=64x48:rate=1:duration=3",
            "-frames:v", "3",
            "-c:v", "libx264", "-profile:v", "main", "-pix_fmt", "yuv420p",
            "-x264-params",
            "cabac=1:ref=1:bframes=1:b-pyramid=0:b-adapt=0:8x8dct=0:weightp=0:weightb=0:aud=0:keyint=300:min-keyint=300:deblock=0:direct=none:partitions=none",
        ])
        .arg(h264.to_str()?)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok {
        return None;
    }
    std::fs::read(&h264).ok()
}

#[test]
fn p_slice_ffmpeg_engine_replay() {
    let dir = std::env::temp_dir().join("dbg_p_oracle");
    std::fs::create_dir_all(&dir).unwrap();
    let Some(annexb) = gen(&dir, "b_boxmv") else {
        eprintln!("ffmpeg unavailable; skipping");
        return;
    };
    let mut starts: Vec<usize> = Vec::new();
    for i in 0..annexb.len().saturating_sub(3) {
        if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
            starts.push(i + 3);
        }
    }
    // First NAL of type 1 after the IDR is the P slice (B NAL follows it).
    let mut p_nal: Option<Vec<u8>> = None;
    let mut seen_idr = false;
    for &s in &starts {
        let t = annexb[s] & 0x1F;
        if t == 5 {
            seen_idr = true;
        }
        if t == 1 && seen_idr {
            let e = starts
                .get(starts.iter().position(|&x| x == s).unwrap() + 1)
                .copied()
                .unwrap_or(annexb.len());
            p_nal = Some(annexb[s..e].to_vec());
            break;
        }
    }
    let nal = p_nal.expect("P NAL not found");
    // Strip emulation-prevention bytes -> RBSP.
    let mut rbsp: Vec<u8> = Vec::with_capacity(nal.len());
    let mut zeros = 0usize;
    for &b in &nal {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        rbsp.push(b);
    }
    // Sweep (start-offset, slice-qp) pairs to find the combination whose
    // first three bins reproduce the full parser's verified ground truth:
    //   MB0 skip=1, terminate=0, MB1 skip=0.
    for skip_bytes in [4usize, 5, 6, 7] {
        for qp in 0..52i32 {
            let payload = &rbsp[skip_bytes..];
            let mut st = [0u8; 1024];
            for i in 0..1024 {
                let (m, n) = tpt_kinetix_h264::cabac_tables::CABAC_CTX_INIT_PB0[i];
                let c = tpt_kinetix_h264::entropy::CabacContext::init(m as i32, n as i32, qp);
                st[i] = 2 * c.state + c.mps as u8;
            }
            let mut eng = Eng::new(payload);
            let s0 = eng.get(&mut st, 11);
            if s0 != 1 {
                continue;
            }
            let e0 = eng.terminate();
            if e0 != 0 {
                continue;
            }
            let s1 = eng.get(&mut st, 11);
            if s1 == 0 {
                eprintln!("MATCH offset={skip_bytes} qp={qp}: [skip=1, term=0, skip=0]");
            }
        }
    }

    // Slice header ends at bit 33 (SLICE_HDR trace); CABAC starts on the next
    // byte boundary => skip ceil(33/8) = 5 bytes.
    const HEADER_BYTES: usize = (33 + 7) / 8;
    let payload = &rbsp[HEADER_BYTES..];
    eprintln!("replay payload {} bytes", payload.len());

    // Slice QP (matches the crate's own CBF_INIT trace: qp=2).
    let slice_qp: i32 = 2;

    // Context init: spec formula (verified == ffmpeg's packed pre-state).
    let mut st = [0u8; 1024];
    for i in 0..1024 {
        let (m, n) = tpt_kinetix_h264::cabac_tables::CABAC_CTX_INIT_PB0[i];
        let c = tpt_kinetix_h264::entropy::CabacContext::init(m as i32, n as i32, slice_qp);
        st[i] = 2 * c.state + c.mps as u8; // packed: bit0 = MPS
    }

    let mut eng = Eng::new(payload);

    // ── MB(0,0): skip (both neighbours unavailable) ──
    let v = eng.get(&mut st, 11);
    log("MB0 skip", 11, v);
    assert_eq!(v, 1, "MB0 expected skip");
    log("MB0 eos", 0, eng.terminate());

    // ── MB(1,0): skip_flag (left=MB0 skipped, top unavailable) ──
    let v = eng.get(&mut st, 11);
    log("MB1 skip", 11, v);
    assert_eq!(v, 0, "MB1 expected non-skip");

    // mb_type prefix: get(ctx14)==1 -> intra-in-P.
    let v = eng.get(&mut st, 14);
    log("MB1 mbtype.prefix", 14, v);
    assert_eq!(v, 1, "MB1 expected intra-in-P");

    // decode_cabac_intra_mb_type(sl, 17, 0) -- `state` never advances.
    let b0 = eng.get(&mut st, 17);
    log("MB1 intra.b0", 17, b0);
    let mut intra_t: u32 = 0;
    if b0 != 0 {
        let pcm = eng.terminate();
        log("MB1 intra.pcm", 17, pcm);
        assert_eq!(pcm, 0, "unexpected I_PCM");
        intra_t = 1;
        let bl = eng.get(&mut st, 18);
        log("MB1 intra.cbpluma", 18, bl);
        intra_t += 12 * bl;
        let bc = eng.get(&mut st, 19);
        log("MB1 intra.cbpchroma.presence", 19, bc);
        if bc == 1 {
            let bv = eng.get(&mut st, 19);
            log("MB1 intra.cbpchroma.value", 19, bv);
            intra_t += 4 + 4 * bv;
        }
        let ph = eng.get(&mut st, 20);
        log("MB1 intra.predmode.hi", 20, ph);
        intra_t += 2 * ph;
        let pl = eng.get(&mut st, 20);
        log("MB1 intra.predmode.lo", 20, pl);
        intra_t += pl;
    }
    eprintln!("ORACLE MB1 intra_t={intra_t} (0=I4x4, 1..=24=I16x16)");
    let is_i16 = intra_t != 0;

    // Intra_4x4 prediction modes (only when I_NxN).
    if !is_i16 {
        for blk in 0..16u32 {
            let f = eng.get(&mut st, 68);
            log(&format!("MB1 mode[{blk}].prevflag"), 68, f);
            if f == 0 {
                for k in 0..3 {
                    let r = eng.get(&mut st, 69);
                    log(&format!("MB1 mode[{blk}].rem{k}"), 69, r);
                }
            }
        }
    }

    // intra_chroma_pred_mode: neighbours unavailable => ctx 64.
    let b = eng.get(&mut st, 64);
    log("MB1 chroma.b0", 64, b);
    if b == 1 {
        let b1 = eng.get(&mut st, 67);
        log("MB1 chroma.b1", 67, b1);
        if b1 == 1 {
            let b2 = eng.get(&mut st, 67);
            log("MB1 chroma.b2", 67, b2);
        }
    }

    // I_NxN: coded_block_pattern here (neighbours unavailable + intra current
    // => ffmpeg sentinel 0x7CF for left/top cbp).
    let mut cbp_l: u32 = 0;
    if !is_i16 {
        let sent: u32 = 0x7CF;
        // decode_cabac_mb_cbp_luma ctx sequence with cbp_a=sentinel-left,
        // cbp_b=sentinel-top (identical values).
        let mut cbp_a = sent;
        let mut cbp_b = sent;
        // ffmpeg: ctx = !(cbp_a&0x02)+2*!(cbp_b&0x04); bins accumulate into
        // `cbp` with same-MB feedback.
        let mut cur: u32 = 0;
        let specs: [(u32, u32, u32); 4] = [
            (0x02, 0x04, 1),
            (0x01, 0x08, 2),
            (0x08, 0x01, 4),
            (0x04, 0x02, 8),
        ];
        for (i, (ma, mb, shift)) in specs.iter().enumerate() {
            let ctx = (!(cbp_a & ma != 0)) as usize + 2 * (!(cbp_b & mb != 0)) as usize;
            let b = eng.get(&mut st, 73 + ctx);
            log(&format!("MB1 cbpluma[{i}]"), 73 + ctx, b);
            cur += b << shift.trailing_zeros();
            cbp_a = (cur & 0b1010) | (sent & !0b1010);
            cbp_b = (cur & 0b0101) << 1 | (sent & !0b11110);
            let _ = i;
        }
        cbp_l = cur;
        eprintln!("ORACLE MB1 cbp_luma={cbp_l:#x}");
    }

    // mb_qp_delta (context-coded first bin at 60+ctx, remaining bins context-
    // coded too per ffmpeg's decode_cabac_mb_dqp loop).
    let need_qp = is_i16 || cbp_l != 0;
    if need_qp {
        let mut ctx = 2usize;
        let mut val = 0u32;
        loop {
            let b = eng.get(&mut st, 60 + ctx);
            log("MB1 qpdelta.bin", 60 + ctx, b);
            if b == 0 {
                break;
            }
            ctx = if ctx < 2 { 2 } else { 3 };
            val += 1;
            if val > 1024 {
                break;
            }
        }
        eprintln!("ORACLE MB1 qpdelta_unary_val={val}");
    }

    // Residual luma-DC coded_block_flag for Intra_16x16 (cat 0, base 85).
    if is_i16 {
        // Neighbours unavailable + intra current => treated as coded => ctxInc
        // 2 (matches the crate trace's CBF cat=0 ctx_idx=2).
        let b = eng.get(&mut st, 87);
        log("MB1 dc.cbf", 87, b);
        if b == 1 {
            eprintln!("ORACLE MB1 DC block NONZERO -- extend replay for sig maps");
        }
    }

    log("MB1 eos", 0, eng.terminate());

    // ── MB(2,0): first coded inter MB ──
    let v = eng.get(&mut st, 12);
    log("MB2 skip", 12, v);
    assert_eq!(v, 0, "MB2 expected non-skip");

    let b = eng.get(&mut st, 14);
    log("MB2 mbtype.intra?", 14, b);
    assert_eq!(b, 0, "MB2 expected inter");
    let shape;
    let b15 = eng.get(&mut st, 15);
    log("MB2 mbtype.16x16vs8x8", 15, b15);
    if b15 == 0 {
        let bb = eng.get(&mut st, 16);
        log("MB2 mbtype.16x16vs8x8b", 16, bb);
        shape = 3 * bb; // 0 => P_L0_16x16
    } else {
        let bb = eng.get(&mut st, 17);
        log("MB2 mbtype.16x8bit", 17, bb);
        shape = 2 - bb;
    }
    eprintln!("ORACLE MB2 shape={shape}");

    let mvd_component = |eng: &mut Eng, st: &mut [u8; 1024], base: usize, comp: char| -> i32 {
        let idx = base + 2; // amvd=0 => ctxbase+2
        let b = eng.get(st, idx);
        log(&format!("MB2 mvd{comp}.first"), idx, b);
        if b == 0 {
            return 0;
        }
        let mut mvd = 1i32;
        let mut cb = base + 3;
        while mvd < 9 {
            let b = eng.get(st, cb);
            log(&format!("MB2 mvd{comp}.unary{mvd}"), cb, b);
            if b == 0 {
                break;
            }
            if mvd < 4 {
                cb += 1;
            }
            mvd += 1;
        }
        if mvd >= 9 {
            let mut k = 3u32;
            loop {
                let b = eng.bypass();
                log(&format!("MB2 mvd{comp}.egk{k}"), 999, b);
                if b == 0 {
                    break;
                }
                mvd += 1 << k;
                k += 1;
                if k > 24 {
                    break;
                }
            }
            let mut kk = k;
            while kk > 0 {
                kk -= 1;
                let b = eng.bypass();
                log(&format!("MB2 mvd{comp}.suffixbit{kk}"), 999, b);
                mvd += (b as i32) << kk;
            }
        }
        let sign = eng.bypass();
        log(&format!("MB2 mvd{comp}.sign"), 999, sign);
        if sign == 1 {
            -mvd
        } else {
            mvd
        }
    };
    let mx = mvd_component(&mut eng, &mut st, 40, 'x');
    let my = mvd_component(&mut eng, &mut st, 47, 'y');
    eprintln!("ORACLE RESULT MB(2,0) shape={shape} mvd_l0=({mx},{my})");
}
