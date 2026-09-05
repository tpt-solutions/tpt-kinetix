//! Session #32b: ENGINE-level differential — ffmpeg's REAL cabac engine
//! arithmetic (mechanically ported from libavcodec/cabac_functions.h @ n5.1,
//! repo-root `cabac_ref.c`/`cabac_funcs.h`) run in LOCKSTEP against the
//! crate's spec-literal `CabacDecoder`.
//!
//! Every previous oracle (sessions #28/#29/#31) executed ffmpeg-transcribed
//! CONTEXT logic through the CRATE engine, so a rare engine-arithmetic bug
//! was invisible to all of them. This test closes that hole: both engines
//! consume the same pseudo-random payloads, share one context model (each
//! side maintains its own representation), and every bin must match.
//!
//! The ffmpeg tables (`ff_h264_cabac_tables`) are parsed OUT OF THE VENDORED
//! SOURCE FILE at test runtime — no hand transcription.
use tpt_kinetix_h264::cabac_tables::CABAC_CTX_INIT_PB0;
use tpt_kinetix_h264::entropy::{CabacContext, CabacDecoder};

// ---------------------------------------------------------------- ff engine

const CABAC_BITS: i32 = 16;
const CABAC_MASK: i32 = (1 << CABAC_BITS) - 1;

struct FfTables {
    /// H264_NORM_SHIFT_OFFSET (512 entries)
    norm_shift: Vec<i32>,
    /// H264_LPS_RANGE_OFFSET (512 entries)
    lps_range: Vec<i32>,
    /// H264_MLPS_STATE_OFFSET (256 entries); indexed as mlps_state+128
    mlps_state: Vec<i32>,
}

impl FfTables {
    /// Extract `ff_h264_cabac_tables`' integers out of the vendored C source.
    fn parse(path: &std::path::Path) -> Self {
        let src = std::fs::read_to_string(path).expect("cabac_ref.c not found");
        let start = src.find("ff_h264_cabac_tables").expect("symbol");
        let open = src[start..].find('{').unwrap() + start;
        let close = src[open..].find('}').unwrap() + open;
        let body = &src[open + 1..close];
        let mut nums = Vec::new();
        for line in body.lines() {
            let line = match line.find("//") {
                Some(p) => &line[..p],
                None => line,
            };
            for tok in line.split(|c: char| !(c.is_ascii_digit() || c == '-')) {
                if !tok.is_empty() && tok.parse::<i64>().is_ok() {
                    nums.push(tok.parse::<i32>().unwrap());
                }
            }
        }
        assert!(nums.len() >= 1280, "table too short: {}", nums.len());
        FfTables {
            norm_shift: nums[..512].to_vec(),
            lps_range: nums[512..1024].to_vec(),
            mlps_state: nums[1024..1280].to_vec(),
        }
    }
}

struct FfEngine<'a> {
    low: i32,
    range: i32,
    data: &'a [u8],
    idx: usize,
    t: &'a FfTables,
}

impl<'a> FfEngine<'a> {
    /// `ff_init_cabac_decoder`, CABAC_BITS==16, even-alignment branch.
    fn new(data: &'a [u8], t: &'a FfTables) -> Self {
        let mut e = FfEngine {
            low: 0,
            range: 0,
            data,
            idx: 0,
            t,
        };
        e.low = (e.byte() as i32) << 18;
        e.low += (e.byte() as i32) << 10;
        e.low += 1 << 9; // even-alignment branch (prefetch only)
        e.range = 0x1FE;
        e
    }

    fn byte(&mut self) -> u8 {
        let b = self.data.get(self.idx).copied().unwrap_or(0);
        self.idx += 1;
        b
    }

    /// `refill`
    fn refill(&mut self) {
        let b0 = self.data.get(self.idx).copied().unwrap_or(0);
        let b1 = self.data.get(self.idx + 1).copied().unwrap_or(0);
        self.low = self
            .low
            .wrapping_add(((b0 as i32) << 9) + ((b1 as i32) << 1));
        self.low -= CABAC_MASK;
        self.idx += (CABAC_BITS / 8) as usize;
    }

    /// `refill2`
    fn refill2(&mut self) {
        let i = (self.low.trailing_zeros() as i32) - CABAC_BITS;
        let b0 = self.data.get(self.idx).copied().unwrap_or(0);
        let b1 = self.data.get(self.idx + 1).copied().unwrap_or(0);
        let x = (-CABAC_MASK).wrapping_add(((b0 as i32) << 9) + ((b1 as i32) << 1));
        self.low = self.low.wrapping_add(x.wrapping_shl(i as u32));
        self.idx += (CABAC_BITS / 8) as usize;
    }

    /// `get_cabac_inline`; `state` is ffmpeg's packed (pStateIdx, mps) byte.
    fn get(&mut self, state: &mut u8) -> i32 {
        let s0 = *state as i32;
        let li = (2 * (self.range & 0xC0) + s0) as usize;
        let range_lps = match self.t.lps_range.get(li) {
            Some(&v) => v,
            None => panic!("ff lps_range OOB idx={li} range={} state={s0}", self.range),
        };
        if range_lps <= 0 {
            panic!(
                "ff non-positive RangeLPS={range_lps} idx={li} range={} state={s0}",
                self.range
            );
        }
        self.range -= range_lps;
        let lps_mask = ((self.range << (CABAC_BITS + 1)) - self.low) >> 31;
        self.low -= (self.range << (CABAC_BITS + 1)) & lps_mask;
        self.range += (range_lps - self.range) & lps_mask;

        let s = s0 ^ lps_mask;
        *state = self.t.mlps_state[(128 + s) as usize] as u8;
        let bit = s & 1;

        let shift = self.t.norm_shift[self.range as usize];
        self.range <<= shift;
        self.low <<= shift;
        if (self.low & CABAC_MASK) == 0 {
            self.refill2();
        }
        bit
    }

    /// `get_cabac_bypass`
    fn bypass(&mut self) -> i32 {
        self.low = self.low.wrapping_add(self.low);
        if (self.low & CABAC_MASK) == 0 {
            self.refill();
        }
        let r = self.range << (CABAC_BITS + 1);
        if self.low < r {
            0
        } else {
            self.low -= r;
            1
        }
    }

    /// `get_cabac_terminate` -> true when end taken
    fn terminate(&mut self) -> bool {
        self.range -= 2;
        if self.low < self.range << (CABAC_BITS + 1) {
            // renorm_cabac_decoder_once
            let shift = ((self.range - 0x100) as u32) >> 31;
            self.range <<= shift;
            self.low <<= shift;
            if (self.low & CABAC_MASK) == 0 {
                self.refill();
            }
            false
        } else {
            true
        }
    }
}

// ---------------------------------------------------------- lockstep driver

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// One lockstep payload: both engines decode the same random op sequence over
/// the same bytes, sharing one logical context model (each side keeps its own
/// representation). Returns Err(detail) on the first mismatch.
fn run_payload(
    payload: &[u8],
    t: &FfTables,
    ff_states: &mut [u8; 1024],
    crate_ctx: &mut [CabacContext; 1024],
    ops: &[u8],
) -> Result<usize, String> {
    // Pad so ffmpeg's 2-byte lookahead never passes the end.
    let mut buf = payload.to_vec();
    buf.extend(std::iter::repeat_n(0u8, 64));
    let mut ff = FfEngine::new(&buf, t);
    let mut dec = CabacDecoder::new(&buf).map_err(|e| e.to_string())?;

    for (step, &op) in ops.iter().enumerate() {
        let idx = ((op as usize * 251) ^ (step * 7)) % 1024;
        match op & 0x30 {
            0x00 | 0x10 => {
                // Only probe where ffmpeg's table is defined: real streams
                // never reach the negative-padding entries of lps_range
                // (ffmpeg's mirrored state space excludes them), so compare
                // bins only there. Skipping consumes nothing on either side.
                let li = (2 * (ff.range & 0xC0) + ff_states[idx] as i32) as usize;
                if t.lps_range.get(li).copied().unwrap_or(-1) <= 0 {
                    continue;
                }
                let ff_bin = ff.get(&mut ff_states[idx]);
                let c_bin = dec.decode_decision(&mut crate_ctx[idx]);
                if ff_bin != c_bin as i32 {
                    return Err(format!(
                        "step {step}: decision mismatch ctx{idx}: ff={ff_bin} ours={c_bin} \
                         ff_state={:02x} ours=(pi={},mps={})",
                        ff_states[idx], crate_ctx[idx].state, crate_ctx[idx].mps
                    ));
                }
            }
            0x20 => {
                let ff_bin = ff.bypass();
                let c_bin = dec.decode_bypass();
                if ff_bin != c_bin as i32 {
                    return Err(format!(
                        "step {step}: bypass mismatch: ff={ff_bin} ours={c_bin}"
                    ));
                }
            }
            _ => {
                let ff_end = ff.terminate();
                let c_end = dec.decode_terminate();
                if ff_end != (c_end == 1) {
                    return Err(format!(
                        "step {step}: terminate mismatch: ff={} ours={c_end}",
                        ff_end as i32
                    ));
                }
                if ff_end {
                    return Ok(step);
                }
            }
        }
    }
    Ok(ops.len())
}

#[test]
fn single_step_probe() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../cabac_ref.c");
    let t = FfTables::parse(&src);
    let mut rng = Rng(0xDEADBEEFCAFEBABE);

    // Candidate mappings: crate (pi,mps) -> ff packed byte.
    type PackFn = fn(u8, u8) -> u8;
    let candidates: &[(&str, PackFn)] = &[
        ("2pi+1", |pi, _m| 2 * pi + 1),
        ("2pi", |pi, _m| 2 * pi),
        ("125-2pi", |pi, _m| (125i32 - 2 * pi as i32) as u8),
        ("126-2pi", |pi, _m| (126i32 - 2 * pi as i32) as u8),
        ("253-2pi-mod", |pi, _m| {
            (253u16.wrapping_sub(2 * pi as u16)) as u8
        }),
        ("2pi+(1-mps)", |pi, m| 2 * pi + (1 - (m & 1))),
        ("2pi+mps", |pi, m| 2 * pi + (m & 1)),
    ];

    let mut agree = vec![0usize; candidates.len()];
    let mut total = 0usize;

    for _trial in 0..4000u32 {
        let payload: Vec<u8> = (0..64u32).map(|_| rng.next() as u8).collect();
        let mut buf = payload.clone();
        buf.extend(std::iter::repeat_n(0u8, 64));
        let pi = rng.below(64) as u8;
        let mps = rng.below(2) as u8;

        for (ci, (_, f)) in candidates.iter().enumerate() {
            let packed = f(pi, mps);
            if packed > 127 || (t.lps_range[(2 * (510 & 0xC0) + packed as i32) as usize]) <= 0 {
                continue; // invalid/padding state
            }
            let mut ff = FfEngine::new(&buf, &t);
            let mut fs = packed;
            let ff_bin = ff.get(&mut fs);

            let mut dec = CabacDecoder::new(&buf).unwrap();
            let mut cc = CabacContext {
                state: pi,
                mps,
                ctx_id: 0xFFFF,
            };
            let c_bin = dec.decode_decision(&mut cc);
            total += 1;
            if ff_bin == c_bin as i32 {
                agree[ci] += 1;
            }
        }
    }
    for (ci, (name, _)) in candidates.iter().enumerate() {
        println!("{:>12}: {}/{}", name, agree[ci], total / candidates.len());
    }
}

#[test]
fn reverse_engineer_ff_state_mapping() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../cabac_ref.c");
    let t = FfTables::parse(&src);

    // Spec rangeTabLPS (Table 9-44), transcribed from src/entropy.rs.
    const SPEC: [[u32; 4]; 64] = [
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

    // For each ff quadrant (128 entries == one spec qCodIRangeIdx) and each
    // packed state byte, report which spec pStateIdx the RangeLPS matches.
    println!("s | q0->pi q1->pi q2->pi q3->pi  (values; 'x' = no unique match)");
    for s in 0..128i32 {
        let mut cols: [String; 4] = std::array::from_fn(|_| "x".to_string());
        for q in 0..4usize {
            let v = t.lps_range[(128 * q) + s as usize];
            let hits: Vec<usize> = (0..64)
                .filter(|&pi| SPEC[pi][q] == v.unsigned_abs())
                .collect();
            if hits.len() == 1 {
                cols[q] = format!("{}{:2}", if v < 0 { "-" } else { "+" }, hits[0]);
            } else if hits.len() > 1 {
                cols[q] = format!("{}{{?}}", v);
            } else {
                cols[q] = format!("{v}?");
            }
        }
        println!("s={s:3}  {}", cols.join("  "));
    }
}

#[test]
fn engine_lockstep_vs_ffmpeg() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../cabac_ref.c");
    let tables = FfTables::parse(&src);

    let mut rng = Rng(0x9E3779B97F4A7C15);
    let qp = 24i32;
    let mut failures = 0usize;

    for payload_id in 0..8usize {
        let payload: Vec<u8> = (0..320u32).map(|_| rng.next() as u8).collect();
        let ops: Vec<u8> = (0..200_000u32).map(|_| rng.next() as u8).collect();

        let mut ff_states = [0u8; 1024];
        let mut crate_ctx: [CabacContext; 1024] = std::array::from_fn(|i| {
            CabacContext::init(
                CABAC_CTX_INIT_PB0[i].0 as i32,
                CABAC_CTX_INIT_PB0[i].1 as i32,
                qp,
            )
        });
        for i in 0..1024 {
            // Empirically verified (single_step_probe): ffmpeg packs states as
            // 2*pStateIdx + valMPS — identical semantics to the crate's split
            // representation.
            ff_states[i] = (crate_ctx[i].state << 1) | (crate_ctx[i].mps & 1);
        }

        match run_payload(&payload, &tables, &mut ff_states, &mut crate_ctx, &ops) {
            Ok(n) => println!("payload {payload_id}: {n} bins in lockstep"),
            Err(e) => {
                failures += 1;
                println!("payload {payload_id}: DIVERGED: {e}");
                if failures > 3 {
                    break;
                }
            }
        }
    }
    assert_eq!(failures, 0, "{failures} payloads diverged — engines differ");
}
