//! Probe: decode ONLY mb_skip_flag bins from the static clip's P slice,
//! assuming an all-skip stream, to test the mb_skip_flag path in isolation.

#![allow(warnings)]

use std::process::Command;

use tpt_kinetix_h264::bitreader::BitReader;
use tpt_kinetix_h264::nal::{parse_nal_units_from_annexb, NalUnitType};
use tpt_kinetix_h264::pps::PicParameterSet;
use tpt_kinetix_h264::slice::SliceHeaderContext;
use tpt_kinetix_h264::sps::SeqParameterSet;

fn run(cmd: &mut Command) -> bool {
    cmd.output().map(|o| o.status.success()).unwrap_or(false)
}

fn main() {
    let dir = std::env::temp_dir().join("dbg_cabac_p_matrix");
    let annexb = std::fs::read(dir.join("static.h264")).unwrap();

    let units = parse_nal_units_from_annexb(&annexb);
    let sps = units
        .iter()
        .find(|u| u.nal_unit_type == NalUnitType::Sps)
        .and_then(|u| SeqParameterSet::parse(&u.rbsp).ok())
        .unwrap();
    let pps = units
        .iter()
        .find(|u| u.nal_unit_type == NalUnitType::Pps)
        .and_then(|u| PicParameterSet::parse(&u.rbsp, None).ok())
        .unwrap();
    let _ = &run;
    let p = units
        .iter()
        .find(|u| u.nal_unit_type == NalUnitType::NonIdrSlice)
        .unwrap();
    eprintln!("P rbsp ({} bytes): {:02X?}", p.rbsp.len(), &p.rbsp[..]);
    // Locate the P NAL's raw EBSP bytes in the annex-b file for manual audit.
    {
        let file = std::fs::read(dir.join("static.h264")).unwrap();
        if let Some(idx) = file.windows(4).rposition(|w| w == [0x41, 0x9A, 0x22, 0x0A]) {
            eprintln!(
                "raw around P NAL (start at {}): {:02X?}",
                idx,
                &file[idx.saturating_sub(6)..(idx + 16).min(file.len())]
            );
        } else {
            // find the last 00 00 01 start code
            let mut sc = None;
            for i in 0..file.len().saturating_sub(3) {
                if &file[i..i + 3] == b"\x00\x00\x01" {
                    sc = Some(i);
                }
            }
            if let Some(s) = sc {
                eprintln!(
                    "last start code at {s}: {:02X?}",
                    &file[s..(s + 20).min(file.len())]
                );
            }
        }
    }
    eprintln!(
        "P rbsp bits: {}",
        p.rbsp
            .iter()
            .map(|b| format!("{:08b}", b))
            .collect::<Vec<_>>()
            .join(" ")
    );

    let ctx = SliceHeaderContext {
        log2_max_frame_num_minus4: sps.log2_max_frame_num_minus4,
        pic_order_cnt_type: sps.pic_order_cnt_type,
        log2_max_pic_order_cnt_lsb_minus4: sps.log2_max_pic_order_cnt_lsb_minus4,
        frame_mbs_only_flag: sps.frame_mbs_only_flag,
        bottom_field_pic_order_in_frame_present_flag: pps
            .bottom_field_pic_order_in_frame_present_flag,
        delta_pic_order_always_zero_flag: false,
        num_ref_idx_l0_default_active_minus1: pps.num_ref_idx_l0_default_active_minus1,
        num_ref_idx_l1_default_active_minus1: pps.num_ref_idx_l1_default_active_minus1,
        weighted_pred_flag: pps.weighted_pred_flag,
        weighted_bipred_idc: pps.weighted_bipred_idc,
        entropy_coding_mode_flag: pps.entropy_coding_mode_flag,
        deblocking_filter_control_present_flag: pps.deblocking_filter_control_present_flag,
        redundant_pic_cnt_present_flag: pps.redundant_pic_cnt_present_flag,
        num_slice_groups_minus1: pps.num_slice_groups_minus1,
        chroma_array_type: sps.chroma_format_idc,
    };
    let header = tpt_kinetix_h264::slice::SliceHeader::parse_with_context(
        &p.rbsp,
        p.nal_unit_type,
        p.nal_ref_idc,
        &ctx,
    )
    .unwrap();
    eprintln!(
        "P slice: cabac_init_idc={} slice_qp_delta={} first_mb={} data_bit_offset={}",
        header.cabac_init_idc,
        header.slice_qp_delta,
        header.first_mb_in_slice,
        header.data_bit_offset
    );
    eprintln!(
        "SPS: log2_max_frame_num_minus4={} poc_type={} log2_poc_lsb_m4={}",
        sps.log2_max_frame_num_minus4,
        sps.pic_order_cnt_type,
        sps.log2_max_pic_order_cnt_lsb_minus4
    );
    eprintln!(
        "PPS: pic_init_qp_minus26={} deblock_pres={} ref_l0_def_m1={}",
        pps.pic_init_qp_minus26,
        pps.deblocking_filter_control_present_flag,
        pps.num_ref_idx_l0_default_active_minus1
    );
    let ppsu = units
        .iter()
        .find(|u| u.nal_unit_type == NalUnitType::Pps)
        .unwrap();
    eprintln!("PPS rbsp: {:02X?}", &ppsu.rbsp[..]);

    // Step-by-step header replication with bit positions.
    {
        struct Br<'a> {
            data: &'a [u8],
            pos: usize,
        }
        impl<'a> Br<'a> {
            fn bit(&mut self) -> u32 {
                let b = (self.data[self.pos / 8] >> (7 - self.pos % 8)) & 1;
                self.pos += 1;
                b as u32
            }
            fn bits(&mut self, n: usize) -> u32 {
                let mut v = 0;
                for _ in 0..n {
                    v = (v << 1) | self.bit();
                }
                v
            }
            fn ue(&mut self) -> u32 {
                let mut k = 0;
                while self.bit() == 0 {
                    k += 1;
                }
                ((1u32 << k) - 1) + self.bits(k)
            }
            fn se(&mut self) -> i32 {
                let u = self.ue();
                if u % 2 == 1 {
                    ((u + 1) / 2) as i32
                } else {
                    -((u / 2) as i32)
                }
            }
        }
        let mut b = Br {
            data: &p.rbsp,
            pos: 0,
        };
        eprintln!("first_mb={} @{}", b.ue(), b.pos);
        eprintln!("slice_type_raw={} @{}", b.ue(), b.pos);
        eprintln!("pps_id={} @{}", b.ue(), b.pos);
        eprintln!("frame_num={} @{}", b.bits(4), b.pos);
        eprintln!("override={} @{}", b.bit(), b.pos);
        eprintln!("modflag_l0={} @{}", b.bit(), b.pos);
        eprintln!("adaptive_marking={} @{}", b.bit(), b.pos);
        eprintln!("cabac_init_idc={} @{}", b.ue(), b.pos);
        eprintln!("qp_delta={} @{}", b.se(), b.pos);
        eprintln!("deblock_idc={} @{}", b.ue(), b.pos);
    }

    let mut r = BitReader::new(&p.rbsp);
    r.seek_to_bit(header.data_bit_offset);
    eprintln!(
        "after seek({}): bit_position={}",
        header.data_bit_offset,
        r.bit_position()
    );
    r.byte_align();
    eprintln!("after align: bit_position={}", r.bit_position());
    let data = r.remaining_bytes().to_vec();
    eprintln!(
        "cabac payload: {} bytes: {:02X?}",
        data.len(),
        &data[..data.len().min(24)]
    );

    let slice_qp = 26 + pps.pic_init_qp_minus26 + header.slice_qp_delta;

    // Build the same skip-flag context the parser uses.
    use tpt_kinetix_h264::entropy::MbSkipFlagContext;
    let mut skip = MbSkipFlagContext::new_p_slice(slice_qp, header.cabac_init_idc as usize);

    // Reconstruct the engine exactly like parse_p_slice_cabac does.
    let mut dec = tpt_kinetix_h264::entropy::CabacDecoder::new(&data).unwrap();

    // All-skip hypothesis: 12 MBs, ctx idx always 0 (all neighbours skipped).
    // Brute-force the starting bit position to test header-offset hypotheses.
    let full_bits: Vec<u32> = {
        let mut v = Vec::new();
        let mut r = BitReader::new(&p.rbsp);
        while let Some(b) = r.read_bit() {
            v.push(b as u32);
        }
        v
    };
    for shift in -20i32..24usize as i32 {
        let start = (header.data_bit_offset as i32 + shift).max(0) as usize;

        let mut sub = Vec::new();
        let mut acc = 0u32;
        let mut n = 0;
        let mut val = 0u32;
        for &b in &full_bits[start.min(full_bits.len())..] {
            acc = (acc << 1) | b;
            n += 1;
            if n == 8 {
                sub.push(acc as u8);
                acc = 0;
                n = 0;
            }
            let _ = val;
        }
        if n > 0 {
            acc <<= 8 - n;
            sub.push(acc as u8);
        }
        let Ok(mut d) = tpt_kinetix_h264::entropy::CabacDecoder::new(&sub) else {
            continue;
        };
        let mut sk = MbSkipFlagContext::new_p_slice(slice_qp, header.cabac_init_idc as usize);
        let mut flags = Vec::new();
        for _ in 0..12 {
            flags.push(sk.decode(&mut d, &Default::default()) as u8);
        }
        let term = d.decode_terminate();
        eprintln!("shift={shift}: flags={flags:?} terminate={term}");
    }
    let _ = (&dec, &skip);

    // All-skip hypothesis at nominal offset: 12 MBs.
    for i in 0..12 {
        let v = skip.decode(&mut dec, &Default::default());
        eprintln!("MB{i}: skip_flag={v}");
        if i == 11 {
            let t = dec.decode_terminate();
            eprintln!("terminate={t} (expect 1)");
        }
    }

    // ── Experiment: unaligned CABAC start ──────────────────────────────────
    // Build a byte buffer whose bit stream starts EXACTLY at
    // data_bit_offset (no alignment skip) and try parsing the P slice.
    let dir3 = dir.clone();
    let h264t = dir3.join("test_cabac.h264");
    let okt = run(&mut Command::new("ffmpeg").args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "lavfi",
        "-i",
        "testsrc=size=64x48:rate=1:duration=2",
        "-frames:v",
        "2",
        "-c:v",
        "libx264",
        "-profile:v",
        "main",
        "-pix_fmt",
        "yuv420p",
        "-x264-params",
        "cabac=1:ref=1:bframes=0:8x8dct=0:weightp=0:aud=0:keyint=2:min-keyint=2:deblock=0",
        h264t.to_str().unwrap(),
    ]));
    assert!(okt);
    let annexb_t = std::fs::read(&h264t).unwrap();
    let units_t = parse_nal_units_from_annexb(&annexb_t);
    let pt = units_t
        .iter()
        .find(|u| u.nal_unit_type == NalUnitType::NonIdrSlice)
        .unwrap();
    let ht = tpt_kinetix_h264::slice::SliceHeader::parse_with_context(
        &pt.rbsp,
        pt.nal_unit_type,
        pt.nal_ref_idc,
        &ctx,
    )
    .unwrap();
    let bits_t: Vec<u32> = {
        let mut v = Vec::new();
        let mut rr = BitReader::new(&pt.rbsp);
        while let Some(x) = rr.read_bit() {
            v.push(x as u32);
        }
        v
    };
    for shift in -12i32..=2 {
        let start = (ht.data_bit_offset as i32 + shift).max(0) as usize;
        let mut sub: Vec<u8> = Vec::new();
        let mut acc = 0u32;
        let mut n = 0;
        for &bb in &bits_t[start.min(bits_t.len())..] {
            acc = (acc << 1) | bb;
            n += 1;
            if n == 8 {
                sub.push(acc as u8);
                acc = 0;
                n = 0;
            }
        }
        if n > 0 {
            acc <<= 8 - n;
            sub.push(acc as u8);
        }
        match tpt_kinetix_h264::slice_data::parse_p_slice_cabac(
            &sub,
            4,
            3,
            26 + pps.pic_init_qp_minus26 + ht.slice_qp_delta,
            false,
            false,
            ht.cabac_init_idc as usize,
            1,
            pps.chroma_qp_index_offset,
            false,
            &mut tpt_kinetix_h264::trace::NoopTracer,
        ) {
            Ok(parsed) => {
                eprintln!(
                    "shift={shift}: PARSE OK ({} MBs, skips={:?})",
                    parsed.macroblocks.len(),
                    parsed
                        .macroblocks
                        .iter()
                        .map(|m| m.skip as u8)
                        .collect::<Vec<_>>()
                );
            }
            Err(e) => eprintln!("shift={shift}: parse error {e}"),
        }
    }

    // ── FFmpeg-exact engine transcription ──────────────────────────────────
    // Replicates ff_init_cabac_decoder + get_cabac_inline (low/range
    // conventions and packed mlps_state transitions included) using tables
    // derived from the crate's own verified RANGE_TAB_LPS / TRANS_IDX_*.
    {
        const RT: [[u32; 4]; 64] = [
            [128, 176, 208, 240],
            [128, 167, 197, 227],
            [128, 158, 187, 216],
            [123, 150, 178, 205],
            [116, 142, 169, 195],
            [111, 135, 160, 185],
            [105, 128, 152, 175],
            [100, 122, 144, 166],
            [95, 116, 137, 158],
            [90, 110, 130, 150],
            [85, 104, 123, 142],
            [81, 99, 117, 135],
            [77, 94, 111, 128],
            [73, 89, 105, 122],
            [69, 85, 100, 116],
            [66, 80, 95, 110],
            [62, 76, 90, 104],
            [59, 72, 86, 99],
            [56, 69, 81, 94],
            [53, 65, 77, 89],
            [51, 62, 73, 85],
            [48, 59, 69, 80],
            [46, 56, 66, 76],
            [43, 53, 63, 72],
            [41, 50, 59, 69],
            [39, 48, 56, 65],
            [37, 45, 54, 62],
            [35, 43, 51, 59],
            [33, 41, 48, 56],
            [32, 39, 46, 53],
            [30, 37, 43, 50],
            [29, 35, 41, 48],
            [27, 33, 39, 45],
            [26, 31, 37, 43],
            [24, 30, 35, 41],
            [23, 28, 33, 39],
            [22, 27, 32, 37],
            [21, 26, 30, 35],
            [20, 24, 29, 33],
            [19, 23, 27, 31],
            [18, 22, 26, 30],
            [17, 21, 25, 28],
            [16, 20, 23, 27],
            [15, 19, 22, 25],
            [14, 18, 21, 24],
            [14, 17, 20, 23],
            [13, 16, 19, 22],
            [12, 15, 18, 21],
            [12, 14, 17, 20],
            [11, 14, 16, 19],
            [11, 13, 15, 18],
            [10, 12, 15, 17],
            [10, 12, 14, 16],
            [9, 11, 13, 15],
            [9, 11, 12, 14],
            [8, 10, 12, 14],
            [8, 9, 11, 13],
            [7, 9, 11, 12],
            [7, 9, 10, 12],
            [7, 8, 10, 11],
            [6, 8, 9, 11],
            [6, 7, 9, 10],
            [6, 7, 8, 9],
            [2, 2, 2, 2],
        ];
        const TL: [u8; 64] = [
            0, 0, 1, 2, 2, 4, 4, 5, 6, 7, 8, 9, 9, 11, 11, 12, 13, 13, 15, 15, 16, 16, 18, 18, 19,
            19, 21, 21, 22, 22, 23, 24, 24, 25, 26, 26, 27, 27, 28, 29, 29, 30, 30, 30, 31, 32, 32,
            33, 33, 33, 34, 34, 35, 35, 35, 36, 36, 36, 37, 37, 37, 38, 38, 63,
        ];
        const TM: [u8; 64] = [
            1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
            25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46,
            47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 62, 63,
        ];

        struct FfEngine<'a> {
            data: &'a [u8],
            /// bit position of the NEXT unread bit (ffmpeg refills transparently)
            pos: usize,
            /// ffmpeg `low`: 17 fractional bits below the spec offset
            low: u64,
            range: u32,
        }
        impl<'a> FfEngine<'a> {
            fn new(data: &'a [u8]) -> Self {
                // ff_init_cabac_decoder:
                //   low  = (*by++ << 18) | (*by++ << 10) | ((*by & 0xC0) << 2)
                //   range = 0x1FE
                let g = |i: usize| *data.get(i).unwrap_or(&0) as u64;
                let low = (g(0) << 18) | (g(1) << 10) | ((g(2) & 0xC0) << 2);
                FfEngine {
                    data,
                    pos: 18,
                    low,
                    range: 0x1FE,
                }
            }
            fn refill_bit(&mut self) -> u64 {
                let b = match self.data.get(self.pos / 8) {
                    Some(&v) => ((v >> (7 - self.pos % 8)) & 1) as u64,
                    None => 0,
                };
                self.pos += 1;
                b
            }
            fn get_cabac(&mut self, st: &mut u32, mps: &mut u32) -> u32 {
                let q = ((self.range >> 6) & 3) as usize;
                let rlps = RT[*st as usize][q];
                self.range -= rlps;
                // ffmpeg compares low against range scaled by 2^17
                let bit;
                if self.low < (self.range as u64) * 131072 {
                    bit = *mps;
                    *st = TM[*st as usize] as u32;
                } else {
                    self.low -= (self.range as u64) * 131072;
                    self.range = rlps;
                    if *st == 0 {
                        *mps = 1 - *mps;
                    }
                    *st = TL[*st as usize] as u32;
                    bit = 1 - *mps;
                }
                // renorm: shift low left, pull bits
                while self.range < 256 {
                    self.low = (self.low << 1) | self.refill_bit();
                    self.range <<= 1;
                }
                bit
            }
            fn terminate(&mut self) -> u32 {
                self.range -= 2;
                if self.low >= (self.range as u64) * 131072 {
                    1
                } else {
                    while self.range < 256 {
                        self.low = (self.low << 1) | self.refill_bit();
                        self.range <<= 1;
                    }
                    0
                }
            }
        }

        // Trace the current clip's P payload under ctxIdx-11 init,
        // sweeping every plausible packed-transition index mapping against
        // the ground truth (12x skip_flag=1 + terminate=1).
        let payload: Vec<u8> = {
            let mut r = BitReader::new(&p.rbsp);
            r.seek_to_bit(header.data_bit_offset);
            r.byte_align();
            r.remaining_bytes().to_vec()
        };
        // lps_range lookup: RT[q][s>>1] under packed s=2*idx+mps.
        let lps_of =
            |range: u32, s: u32| RT[((s >> 1) as usize).min(63)][((range >> 6) & 3) as usize];

        /// Minimal ffmpeg-convention engine (17-bit fraction low register).
        struct FfEnginePacked<'a> {
            data: &'a [u8],
            pos: usize,
            low: u64,
            range: u32,
        }
        impl<'a> FfEnginePacked<'a> {
            fn new(data: &'a [u8]) -> Self {
                let g = |i: usize| *data.get(i).unwrap_or(&0) as u64;
                let low = (g(0) << 18) | (g(1) << 10) | ((g(2) & 0xC0) << 2);
                FfEnginePacked {
                    data,
                    pos: 18,
                    low,
                    range: 0x1FE,
                }
            }
            fn terminate(&mut self) -> u32 {
                self.range -= 2;
                if self.low >= (self.range as u64) * 131072 {
                    1
                } else {
                    while self.range < 256 {
                        self.low = (self.low << 1) | self.refill_bit();
                        self.range <<= 1;
                    }
                    0
                }
            }
            fn refill_bit(&mut self) -> u64 {
                let b = match self.data.get(self.pos / 8) {
                    Some(&v) => ((v >> (7 - self.pos % 8)) & 1) as u64,
                    None => 0,
                };
                self.pos += 1;
                b
            }
        }

        struct Variant {
            name: &'static str,
            mps: fn(u32) -> u32,
            lps: fn(u32) -> u32,
        }
        let variants = [
            Variant {
                name: "A mps[128+s] lps[255-s]",
                mps: |s| 128 + s,
                lps: |s| 255 - s,
            },
            Variant {
                name: "B mps[s] lps[255-s]",
                mps: |s| s,
                lps: |s| 255 - s,
            },
            Variant {
                name: "C mps[128+s] lps[127-s]",
                mps: |s| 128 + s,
                lps: |s| 127 - s,
            },
            Variant {
                name: "D mps[s] lps[127-s]",
                mps: |s| s,
                lps: |s| 127 - s,
            },
            Variant {
                name: "E mps[128+s] lps[128+255-s]",
                mps: |s| 128 + s,
                lps: |s| 383 - s,
            },
        ];
        // Build the flat 256-entry mlps section from the fetched source dump.
        const SECTION: [u32; 256] = [
            127, 126, 77, 76, 77, 76, 75, 74, 75, 74, 75, 74, 73, 72, 73, 72, 73, 72, 71, 70, 71,
            70, 71, 70, 69, 68, 69, 68, 67, 66, 67, 66, 67, 66, 65, 64, 65, 64, 63, 62, 61, 60, 61,
            60, 61, 60, 59, 58, 59, 58, 57, 56, 55, 54, 55, 54, 53, 52, 53, 52, 51, 50, 49, 48, 49,
            48, 47, 46, 45, 44, 45, 44, 43, 42, 43, 42, 39, 38, 39, 38, 37, 36, 37, 36, 33, 32, 33,
            32, 31, 30, 31, 30, 27, 26, 27, 26, 25, 24, 23, 22, 23, 22, 19, 18, 19, 18, 17, 16, 15,
            14, 13, 12, 11, 10, 9, 8, 9, 8, 5, 4, 5, 4, 3, 2, 1, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9,
            10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31,
            32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53,
            54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73, 74, 75,
            76, 77, 78, 79, 80, 81, 82, 83, 84, 85, 86, 87, 88, 89, 90, 91, 92, 93, 94, 95, 96, 97,
            98, 99, 100, 101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111, 112, 113, 114, 115,
            116, 117, 118, 119, 120, 121, 122, 123, 124, 125, 124, 125, 126, 127,
        ];

        // Head-to-head: our engine vs the ffmpeg-transcribed engine on the
        // aligned payload with the true init (idx=5, mps=0) for ctxIdx11@QP18.
        {
            let mut d = tpt_kinetix_h264::entropy::CabacDecoder::new(&payload).unwrap();
            let mut c = tpt_kinetix_h264::entropy::CabacContext {
                state: 5,
                mps: 0,
                ctx_id: 0xFFFF,
            };
            eprintln!(
                "--- our engine, init (5,0), payload {:02X?} ---",
                &payload[..]
            );
            for i in 0..12 {
                let (r0, o0) = d.debug_state();
                let b = d.decode_decision(&mut c);
                eprintln!(
                    "our[{i}]: bin={b} state={} mps={} range={r0:#06x} off={o0:#010x}",
                    c.state, c.mps
                );
            }
            eprintln!("our terminate = {}", d.decode_terminate());

            let mut eng = FfEnginePacked::new(&payload);
            let mut fst = 10u32; // packed (5,0)
            eprintln!("--- ffmpeg-engine (packed, corrected) ---");
            for i in 0..12 {
                let q = ((eng.range >> 6) & 3) as usize;
                let rlps = RT[(fst >> 1) as usize][q];
                eng.range -= rlps;
                let lps = eng.low >= (eng.range as u64) * 131072;
                let old_lsb = fst & 1;
                if lps {
                    eng.low -= (eng.range as u64) * 131072;
                    eng.range = rlps;
                    fst = SECTION[(127 - fst) as usize];
                } else {
                    fst = SECTION[(128 + fst) as usize];
                }
                let bin = if lps { 1 - old_lsb } else { old_lsb };
                while eng.range < 256 {
                    eng.low = (eng.low << 1) | eng.refill_bit();
                    eng.range <<= 1;
                }
                eprintln!(
                    "ff [{i}]: bin={bin} packed_st={fst} range={:06x} low={:010x}",
                    eng.range, eng.low
                );
            }
            eprintln!("ff terminate = {}", eng.terminate());
        }
    }
    // Encodes the all-skip hypothesis and compares with x264's bytes.
    {
        const RT: [[u32; 4]; 64] = [
            [128, 176, 208, 240],
            [128, 167, 197, 227],
            [128, 158, 187, 216],
            [123, 150, 178, 205],
            [116, 142, 169, 195],
            [111, 135, 160, 185],
            [105, 128, 152, 175],
            [100, 122, 144, 166],
            [95, 116, 137, 158],
            [90, 110, 130, 150],
            [85, 104, 123, 142],
            [81, 99, 117, 135],
            [77, 94, 111, 128],
            [73, 89, 105, 122],
            [69, 85, 100, 116],
            [66, 80, 95, 110],
            [62, 76, 90, 104],
            [59, 72, 86, 99],
            [56, 69, 81, 94],
            [53, 65, 77, 89],
            [51, 62, 73, 85],
            [48, 59, 69, 80],
            [46, 56, 66, 76],
            [43, 53, 63, 72],
            [41, 50, 59, 69],
            [39, 48, 56, 65],
            [37, 45, 54, 62],
            [35, 43, 51, 59],
            [33, 41, 48, 56],
            [32, 39, 46, 53],
            [30, 37, 43, 50],
            [29, 35, 41, 48],
            [27, 33, 39, 45],
            [26, 31, 37, 43],
            [24, 30, 35, 41],
            [23, 28, 33, 39],
            [22, 27, 32, 37],
            [21, 26, 30, 35],
            [20, 24, 29, 33],
            [19, 23, 27, 31],
            [18, 22, 26, 30],
            [17, 21, 25, 28],
            [16, 20, 23, 27],
            [15, 19, 22, 25],
            [14, 18, 21, 24],
            [14, 17, 20, 23],
            [13, 16, 19, 22],
            [12, 15, 18, 21],
            [12, 14, 17, 20],
            [11, 14, 16, 19],
            [11, 13, 15, 18],
            [10, 12, 15, 17],
            [10, 12, 14, 16],
            [9, 11, 13, 15],
            [9, 11, 12, 14],
            [8, 10, 12, 14],
            [8, 9, 11, 13],
            [7, 9, 11, 12],
            [7, 9, 10, 12],
            [7, 8, 10, 11],
            [6, 8, 9, 11],
            [6, 7, 9, 10],
            [6, 7, 8, 9],
            [2, 2, 2, 2],
        ];
        const TL: [u8; 64] = [
            0, 0, 1, 2, 2, 4, 4, 5, 6, 7, 8, 9, 9, 11, 11, 12, 13, 13, 15, 15, 16, 16, 18, 18, 19,
            19, 21, 21, 22, 22, 23, 24, 24, 25, 26, 26, 27, 27, 28, 29, 29, 30, 30, 30, 31, 32, 32,
            33, 33, 33, 34, 34, 35, 35, 35, 36, 36, 36, 37, 37, 37, 38, 38, 63,
        ];
        const TM: [u8; 64] = [
            1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
            25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46,
            47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 62, 63,
        ];
        struct E2 {
            low: u32,
            range: u32,
            outs: u32,
            bits: Vec<u32>,
            first: bool,
        }
        impl E2 {
            fn putbit(&mut self, b: u32) {
                if self.first {
                    self.first = false;
                    return;
                }
                self.bits.push(b);
                while self.outs > 0 {
                    self.outs -= 1;
                    let l = *self.bits.last().unwrap();
                    self.bits.push(1 - l);
                }
            }
            fn renorm(&mut self) {
                while self.range < 256 {
                    if self.low < 256 {
                        self.putbit(0);
                    } else if self.low >= 512 {
                        self.low -= 512;
                        self.putbit(1);
                    } else {
                        self.low -= 256;
                        self.outs += 1;
                    }
                    self.range <<= 1;
                    self.low <<= 1;
                }
            }
        }
        struct C2 {
            st: u32,
            mps: u32,
        }
        impl C2 {
            fn dec_like_bin(&mut self, e: &mut E2, bin: u32) {
                let q = ((e.range >> 6) & 3) as usize;
                let rl = RT[self.st as usize][q];
                e.range -= rl;
                if bin != self.mps {
                    e.low += e.range;
                    e.range = rl;
                    if self.st == 0 {
                        self.mps = 1 - self.mps;
                    }
                    self.st = TL[self.st as usize] as u32;
                } else {
                    self.st = TM[self.st as usize] as u32;
                }
                e.renorm();
            }
        }
        fn to_bytes(bits: &[u32]) -> Vec<u8> {
            let mut o = Vec::new();
            let mut a = 0u32;
            let mut n = 0;
            for &b in bits {
                a = (a << 1) | b;
                n += 1;
                if n == 8 {
                    o.push(a as u8);
                    a = 0;
                    n = 0;
                }
            }
            if n > 0 {
                a <<= 8 - n;
                o.push(a as u8);
            }
            o
        }

        // ── SYSTEMATIC TABLE DIFF: our RT/TL/TM vs ffmpeg's packed tables ──
        {
            // ffmpeg ff_h264_cabac_tables layout:
            //   [512..1024)  lps_range, indexed q*128 + (2*i+m)
            //   [1024..1280) mlps_state:
            //                MPS next = sec[128+s], LPS next = sec[127-s]
            // Extracted verbatim from libavcodec/cabac.c @ master.
            // lps_range section (512 entries, int8 as printed -> u8):
            const LR_RAW: [&str; 32] = [
                "-128,-128,-128,-128,-128,-128,123,123",
                "116,116,111,111,105,105,100,100",
                "95,95,90,90,85,85,81,81",
                "77,77,73,73,69,69,66,66",
                "62,62,59,59,56,56,53,53",
                "51,51,48,48,46,46,43,43",
                "41,41,39,39,37,37,35,35",
                "33,33,32,32,30,30,29,29",
                "27,27,26,26,24,24,23,23",
                "22,22,21,21,20,20,19,19",
                "18,18,17,17,16,16,15,15",
                "14,14,14,14,13,13,12,12",
                "12,12,11,11,11,11,10,10",
                "10,10,9,9,9,9,8,8",
                "8,8,7,7,7,7,6,6",
                "6,6,6,6,5,5,5,5",
                "5,5,5,5,4,4,4,4",
                "4,4,4,4,4,4,3,3",
                "3,3,3,3,3,3,3,3",
                "3,3,3,3,3,3,3,3",
                "2,2,2,2,2,2,2,2",
                "2,2,2,2,2,2,2,2",
                "2,2,2,2,2,2,2,2",
                "2,2,2,2,2,2,2,2",
                "2,2,2,2,2,2,2,2",
                "2,2,2,2,2,2,2,2",
                "2,2,2,2,2,2,2,2",
                "2,2,2,2,2,2,2,2",
                "1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1",
                "1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1",
                "1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1",
                "1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1",
            ];
            let _ = LR_RAW;
        }
        // (init derived from the actual slice QP) into the stream's payload
        // region and see whether ffmpeg accepts OUR model's encoding.
        {
            let slice_qp_v = 26 + pps.pic_init_qp_minus26 + header.slice_qp_delta;
            let raw = ((23 * slice_qp_v) >> 4) + 33;
            let (st0, mps0): (u32, u32) = if raw <= 63 {
                ((63 - raw) as u32, 0)
            } else {
                ((raw - 64) as u32, 1)
            };
            let payload: Vec<u8> = {
                let mut r = BitReader::new(&p.rbsp);
                r.seek_to_bit(header.data_bit_offset);
                r.byte_align();
                r.remaining_bytes().to_vec()
            };
            eprintln!(
                "splice test: SliceQPY={slice_qp_v} raw={raw} init=({st0},{mps0}) payload={:02X?}",
                &payload[..]
            );
            let mut e = E2 {
                low: 0,
                range: 510,
                outs: 0,
                bits: Vec::new(),
                first: true,
            };
            let mut c = C2 { st: st0, mps: mps0 };
            for _ in 0..12 {
                c.dec_like_bin(&mut e, 1);
            }
            e.range -= 2;
            e.low += e.range;
            e.range = 2;
            e.renorm();
            e.putbit((e.low >> 9) & 1);
            e.putbit((e.low >> 8) & 1);
            e.putbit(((e.low >> 7) & 3) | 1);
            let enc = to_bytes(&e.bits);
            eprintln!("our spec-model all-skip encoding: {:02X?}", &enc[..]);

            let mut file = std::fs::read(dir.join("static.h264")).unwrap();
            let n = file.len();
            let start = n - payload.len();
            for k in 0..payload.len() {
                file[start + k] = enc.get(k).copied().unwrap_or(0);
            }
            let out_path = dir.join("splice_spec.h264");
            std::fs::write(&out_path, &file).unwrap();
            let md5p = dir.join("splice_spec.md5");
            let errp = dir.join("splice_spec.err");
            let ok = run(&mut Command::new("ffmpeg").args([
                "-v",
                "5",
                "-y",
                "-i",
                out_path.to_str().unwrap(),
                "-f",
                "framemd5",
                md5p.to_str().unwrap(),
            ]));
            let errs = std::fs::read_to_string(&errp).unwrap_or_default();
            let has_err = errs.contains("error") || errs.contains("Error");
            eprintln!("ffmpeg decode of splice_spec: ok={ok} has_err={has_err}");
        }

        // Brute-force: which (pStateIdx, valMps) makes the CURRENT payload
        // (static.h264's P slice, byte-aligned) decode as twelve 1-bins +
        // terminate=1 from the byte-aligned start?
        let payload: Vec<u8> = {
            let mut r = BitReader::new(&p.rbsp);
            r.seek_to_bit(header.data_bit_offset);
            r.byte_align();
            r.remaining_bytes().to_vec()
        };
        eprintln!("sweep payload: {:02X?}", &payload[..]);
        // Also try the payload with the previous byte prepended (alternative
        // start-position hypotheses).
        let prev = p.rbsp[p.rbsp.len() - payload.len() - 1];
        let mut payload_with_prev = vec![prev];
        payload_with_prev.extend_from_slice(&payload);
        for (label, pay) in [("aligned", &payload), ("with-prev", &payload_with_prev)] {
            eprintln!("--- {label}: {:02X?}", &pay[..]);
            for st in 0..64u32 {
                for mpsv in 0..2u8 {
                    let mut d = tpt_kinetix_h264::entropy::CabacDecoder::new(pay).unwrap();
                    let mut c = tpt_kinetix_h264::entropy::CabacContext {
                        state: st as u8,
                        mps: mpsv,
                        ctx_id: 0xFFFF,
                    };
                    let mut all_ones = true;
                    for _ in 0..12 {
                        if d.decode_decision(&mut c) != 1 {
                            all_ones = false;
                            break;
                        }
                    }
                    if all_ones && d.decode_terminate() == 1 {
                        eprintln!("HIT [{label}]: pStateIdx={st} valMps={mpsv}");
                    }
                }
            }
        }
    }
    let dir2 = dir.clone();
    let h264c = dir2.join("static_cavlc.h264");
    let okc = run(&mut Command::new("ffmpeg").args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "lavfi",
        "-i",
        "color=c=gray:size=64x48:rate=1:duration=2",
        "-frames:v",
        "2",
        "-c:v",
        "libx264",
        "-profile:v",
        "baseline",
        "-pix_fmt",
        "yuv420p",
        "-x264-params",
        "cabac=0:ref=1:bframes=0:8x8dct=0:weightp=0:aud=0:keyint=2:min-keyint=2:deblock=0",
        h264c.to_str().unwrap(),
    ]));
    assert!(okc);
    let annexb_c = std::fs::read(&h264c).unwrap();
    let units_c = parse_nal_units_from_annexb(&annexb_c);
    let pc = units_c
        .iter()
        .find(|u| u.nal_unit_type == NalUnitType::NonIdrSlice)
        .unwrap();
    let header_c = tpt_kinetix_h264::slice::SliceHeader::parse_with_context(
        &pc.rbsp,
        pc.nal_unit_type,
        pc.nal_ref_idc,
        &ctx,
    )
    .unwrap();
    let mut rc = BitReader::new(&pc.rbsp);
    rc.seek_to_bit(header_c.data_bit_offset);
    let parsed_c = tpt_kinetix_h264::slice_data::parse_p_slice(
        &mut rc,
        4,
        3,
        26 + pps.pic_init_qp_minus26 + header_c.slice_qp_delta,
        1,
        pps.chroma_qp_index_offset,
        false,
        false,
        false,
        &mut tpt_kinetix_h264::trace::NoopTracer,
    )
    .unwrap();
    eprintln!(
        "CAVLC static P skip flags: {:?}",
        parsed_c
            .macroblocks
            .iter()
            .map(|m| m.skip as u8)
            .collect::<Vec<_>>()
    );
}
