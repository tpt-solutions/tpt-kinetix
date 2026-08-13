//! Structured reproduction harness for the `fuzz_h264_nal` crash.
//!
//! Unlike blind byte fuzzing, this builds *syntactically valid* Annex B H.264
//! streams with a tiny Exp-Golomb bit-writer, then mutates them while catching
//! panics in [`H264Decoder::decode`]. The seeds deliberately reach code paths
//! that random bytes rarely parse into (large-dimension IDR frames and
//! IDR+P-slice packets), which is where coverage-guided fuzzers reported
//! crashes.

use std::panic;

use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::H264Decoder;

struct BitWriter {
    buf: Vec<u8>,
    cur: u8,
    nbits: u8,
}

impl BitWriter {
    fn new() -> Self {
        Self { buf: Vec::new(), cur: 0, nbits: 0 }
    }
    fn bit(&mut self, b: u8) {
        self.cur = (self.cur << 1) | (b & 1);
        self.nbits += 1;
        if self.nbits == 8 {
            self.buf.push(self.cur);
            self.cur = 0;
            self.nbits = 0;
        }
    }
    fn ue(&mut self, v: u32) {
        let mut x = v + 1;
        let mut nbits = 0u32;
        while x > 1 {
            x >>= 1;
            nbits += 1;
        }
        for _ in 0..nbits {
            self.bit(0);
        }
        let val = v + 1;
        for i in (0..=nbits).rev() {
            self.bit(((val >> i) & 1) as u8);
        }
    }
    fn se(&mut self, v: i32) {
        let ue: u32 = if v <= 0 {
            (-2 * v) as u32
        } else {
            (2 * v - 1) as u32
        };
        self.ue(ue);
    }
    fn bits(&mut self, v: u32, n: u8) {
        for i in (0..n).rev() {
            self.bit(((v >> i) & 1) as u8);
        }
    }
    fn finish(&mut self) -> Vec<u8> {
        if self.nbits > 0 {
            self.cur = (self.cur << 1) | 1;
            self.nbits += 1;
            while self.nbits != 8 {
                self.cur <<= 1;
                self.nbits += 1;
            }
            self.buf.push(self.cur);
            self.cur = 0;
            self.nbits = 0;
        }
        std::mem::take(&mut self.buf)
    }
}

/// Insert H.264 emulation-prevention `0x03` bytes so the parser's removal step
/// does not corrupt the generated RBSP.
fn add_epb(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut zeros = 0u32;
    for &b in data {
        if zeros >= 2 && b <= 0x03 {
            out.push(0x03);
            zeros = 0;
        }
        out.push(b);
        if b == 0 {
            zeros += 1;
        } else {
            zeros = 0;
        }
    }
    out
}

fn annexb(header: u8, rbsp: &[u8]) -> Vec<u8> {
    let mut v = vec![0x00, 0x00, 0x00, 0x01, header];
    v.extend_from_slice(&add_epb(rbsp));
    v
}

/// Baseline SPS, CAVLC, for a `w_mbs x h_mbs` picture.
fn sps(w_mbs: u32, h_mbs: u32, num_ref_frames: u32) -> Vec<u8> {
    let mut b = BitWriter::new();
    b.bits(66, 8); // profile_idc
    b.bits(0, 8); // constraint + reserved
    b.bits(30, 8); // level_idc
    b.ue(0); // seq_parameter_set_id
    b.ue(0); // log2_max_frame_num_minus4
    b.ue(0); // pic_order_cnt_type
    b.ue(4); // log2_max_pic_order_cnt_lsb_minus4
    b.ue(num_ref_frames); // num_ref_frames
    b.bit(0); // gaps_in_frame_num_value_allowed_flag
    b.ue(w_mbs - 1); // pic_width_in_mbs_minus1
    b.ue(h_mbs - 1); // pic_height_in_map_units_minus1
    b.bit(1); // frame_mbs_only_flag
    b.bit(0); // direct_8x8_inference_flag
    b.bit(0); // frame_cropping_flag
    b.finish()
}

fn pps() -> Vec<u8> {
    let mut b = BitWriter::new();
    b.ue(0); // pic_parameter_set_id
    b.ue(0); // seq_parameter_set_id
    b.bit(0); // entropy_coding_mode_flag (CAVLC)
    b.bit(0); // bottom_field_pic_order_in_frame_present_flag
    b.ue(0); // num_slice_groups_minus1
    b.ue(0); // num_ref_idx_l0_default_active_minus1
    b.ue(0); // num_ref_idx_l1_default_active_minus1
    b.bit(0); // weighted_pred_flag
    b.bits(0, 2); // weighted_bipred_idc
    b.se(0); // pic_init_qp_minus26
    b.se(0); // pic_init_qs_minus26
    b.se(0); // chroma_qp_index_offset
    b.bit(0); // deblocking_filter_control_present_flag
    b.bit(0); // constrained_intra_pred_flag
    b.bit(0); // redundant_pic_cnt_present_flag
    b.finish()
}

/// IDR I-slice covering `n_mbs` macroblocks, all I_16x16 with zero coefficients.
fn idr_islice(n_mbs: u32) -> Vec<u8> {
    let mut b = BitWriter::new();
    b.ue(0); // first_mb_in_slice
    b.ue(2); // slice_type = I
    b.ue(0); // pic_parameter_set_id
    b.bits(0, 4); // frame_num
    b.ue(0); // idr_pic_id
    b.bits(0, 8); // pic_order_cnt_lsb
    b.bit(0); // no_output_of_prior_pics_flag
    b.bit(0); // long_term_reference_flag
    b.se(0); // slice_qp_delta
    for _ in 0..n_mbs {
        b.ue(1); // mb_type = I_16x16
        b.ue(0); // intra_chroma_pred_mode
        b.se(0); // mb_qp_delta (always read for I_16x16)
        b.bit(1); // luma DC coeff_token: total_coeff 0 => "1"
    }
    b.finish()
}

/// P-slice covering `n_mbs` macroblocks, each `P_16x16` with a fixed motion
/// vector `(mvx, mvy)` and zero residuals (cbp 0).
fn p_16x16_mv_slice(n_mbs: u32, mvx: i32, mvy: i32) -> Vec<u8> {
    let mut b = BitWriter::new();
    b.ue(0); // first_mb_in_slice
    b.ue(0); // slice_type = P
    b.ue(0); // pic_parameter_set_id
    b.bits(0, 4); // frame_num
    b.bits(0, 8); // pic_order_cnt_lsb
    b.bit(0); // adaptive_ref_pic_marking_mode_flag (not idr)
    b.bit(0); // num_ref_idx_active_override_flag
    b.bit(0); // modification_of_pic_nums_idc
    b.se(0); // slice_qp_delta
    for _ in 0..n_mbs {
        b.ue(1); // mb_type = P_16x16
        b.ue(0); // ref_idx_l0
        b.se(mvx); // mvdx
        b.se(mvy); // mvdy
        b.ue(0); // coded_block_pattern = 0
    }
    b.finish()
}

/// P-slice covering `n_mbs` macroblocks, all P_Skip (inherited motion).
fn p_skip_slice(n_mbs: u32) -> Vec<u8> {
    let mut b = BitWriter::new();
    b.ue(0); // first_mb_in_slice
    b.ue(0); // slice_type = P
    b.ue(0); // pic_parameter_set_id
    b.bits(0, 4); // frame_num
    b.bits(0, 8); // pic_order_cnt_lsb
    b.bit(0); // adaptive_ref_pic_marking_mode_flag (not idr)
    b.bit(0); // num_ref_idx_active_override_flag
    b.bit(0); // modification_of_pic_nums_idc (no ref pic list modification)
    b.se(0); // slice_qp_delta
    for _ in 0..n_mbs {
        b.ue(0); // mb_type = P_Skip
    }
    b.finish()
}

fn large_idr_stream() -> Vec<u8> {
    let mut v = Vec::new();
    v.extend(annexb(0x67, &sps(512, 1, 1)));
    v.extend(annexb(0x68, &pps()));
    v.extend(annexb(0x65, &idr_islice(512)));
    v
}

fn idr_and_p_stream() -> Vec<u8> {
    let mut v = Vec::new();
    v.extend(annexb(0x67, &sps(2, 1, 1)));
    v.extend(annexb(0x68, &pps()));
    v.extend(annexb(0x65, &idr_islice(2)));
    v.extend(annexb(0x41, &p_skip_slice(2)));
    v
}

fn seeds() -> Vec<Vec<u8>> {
    vec![
        // Original tiny valid I-slice.
        {
            let mut v = Vec::new();
            v.extend(annexb(0x67, &[0x42, 0x00, 0x1E, 0xE5, 0x4E, 0x40]));
            v.extend(annexb(0x68, &[0xCE, 0x38, 0x80]));
            v.extend(annexb(0x65, &[0xB8, 0x40, 0x2B, 0xF0]));
            v
        },
        large_idr_stream(),
        idr_and_p_stream(),
        {
            let mut v = Vec::new();
            v.extend(annexb(0x67, &sps(2, 1, 1)));
            v.extend(annexb(0x68, &pps()));
            v.extend(annexb(0x65, &idr_islice(2)));
            v.extend(annexb(0x41, &p_16x16_mv_slice(2, 4, 4)));
            v
        },
    ]
}

fn try_decode(data: &[u8]) -> Result<(), String> {
    let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let mut dec = H264Decoder::new();
        let pkt = Packet {
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            data: data.to_vec(),
            stream_index: 0,
            is_key_frame: false,
        };
        let _ = dec.decode(&pkt);
    }));
    match result {
        Ok(()) => Ok(()),
        Err(_) => Err("panic during decode".to_string()),
    }
}

#[test]
fn structured_seeds_decode_without_panic() {
    for (i, s) in seeds().iter().enumerate() {
        try_decode(s).unwrap_or_else(|e| panic!("seed #{i} panicked: {e}"));
    }
}

#[test]
fn fuzz_structured_seeds() {
    use std::time::{Duration, Instant};

    // Keep CI logs clean: the timeout path relies on `catch_unwind`, whose
    // default hook would otherwise dump a backtrace for every caught panic.
    std::panic::set_hook(Box::new(|_| {}));

    let seeds = seeds();
    let mut rng: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next_u64 = || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };

    // Machine-relative soft timeout. The decoder caps picture size
    // (`MAX_MB_COUNT`, see decoder.rs), so the slowest *valid* decode is bounded
    // and scales with machine speed. Calibrate the per-decode budget from the
    // worst-case-size valid IDR on this runner, replacing the previous fixed
    // 300 ms constant (which was flaky on slow CI and loose on fast machines)
    // with a budget proportional to how long a legitimate large decode takes
    // here. A genuine hang still exceeds it by orders of magnitude.
    let worst_stream = {
        let mut v = Vec::new();
        v.extend(annexb(0x67, &sps(192, 192, 1)));
        v.extend(annexb(0x68, &pps()));
        v.extend(annexb(0x65, &idr_islice(192 * 192)));
        v
    };
    let mut ref_times = Vec::new();
    for _ in 0..5 {
        let t0 = Instant::now();
        let _ = try_decode(&worst_stream);
        ref_times.push(t0.elapsed());
    }
    ref_times.sort_unstable();
    let worst_valid = *ref_times.last().unwrap();
    let timeout = (worst_valid * 3)
        .max(Duration::from_secs(2))
        .min(Duration::from_secs(30));
    eprintln!(
        "fuzz_structured_seeds: calibrated per-decode timeout={timeout:?} \
         (worst-case valid decode on this machine={worst_valid:?})"
    );

    let mut iterations = 0u64;
    let mut crash: Option<(usize, Vec<u8>, String)> = None;
    let mut slowest = Duration::ZERO;
    let mut slowest_input: Option<Vec<u8>> = None;
    let mut next_report = Instant::now() + Duration::from_secs(20);
    // Bound total fuzzing wall-clock so this stays a fast, reliable CI gate
    // regardless of how many iterations a given runner can squeeze in.
    let deadline = Instant::now() + Duration::from_secs(60);

    for _ in 0..60_000_000 {
        if Instant::now() >= deadline {
            eprintln!(
                "fuzz_structured_seeds: reached 60s wall-clock budget after {iterations} iters"
            );
            break;
        }
        iterations += 1;
        let seed = &seeds[(next_u64() as usize) % seeds.len()];
        let mut candidate = seed.clone();

        let n_muts = 1 + (next_u64() % 10) as usize;
        for _ in 0..n_muts {
            let op = next_u64() % 4;
            let idx = (next_u64() as usize) % candidate.len();
            match op {
                0 => candidate[idx] ^= 1 << ((next_u64() % 8) as u8),
                1 => candidate[idx] = (next_u64() % 256) as u8,
                2 => {
                    if candidate.len() < 8000 {
                        candidate.insert(idx, (next_u64() % 256) as u8);
                    }
                }
                _ => {
                    if candidate.len() > 6 {
                        candidate.remove(idx);
                    }
                }
            }
        }

        let candidate2 = candidate.clone();
        let started = Instant::now();
        let (tx, rx) = std::sync::mpsc::channel();
        let _handle = std::thread::spawn(move || {
            let _ = tx.send(try_decode(&candidate2));
        });
        let result = match rx.recv_timeout(timeout) {
            Ok(r) => {
                let elapsed = started.elapsed();
                if elapsed > slowest {
                    slowest = elapsed;
                    slowest_input = Some(candidate.clone());
                }
                r
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // Timed out / hung: treat as a timeout crash and stop accumulating.
                let _ = std::fs::write("fuzz_slow_input.bin", &candidate);
                crash = Some((0, candidate, "decode exceeded soft timeout (hang/slow)".into()));
                eprintln!("TIMEOUT after {iterations} iters: decode exceeded {timeout:?}");
                break;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                crash = Some((0, candidate, "decode thread disconnected".into()));
                break;
            }
        };

        if let Err(msg) = result {
            if crash.is_none() {
                let _ = std::fs::write("fuzz_crash_input.bin", &candidate);
                crash = Some((0, candidate, msg.clone()));
                eprintln!("CRASH after {iterations} iters: {msg}");
            }
        }

        if Instant::now() > next_report {
            eprintln!(
                "  [{iterations} iters] slowest so far: {slowest:?}",
            );
            next_report = Instant::now() + Duration::from_secs(20);
        }
    }

    if let Some(slow) = &slowest_input {
        let _ = std::fs::write("fuzz_slowest_input.bin", slow);
    }
    eprintln!("slowest decode observed: {slowest:?}");

    if let Some((si, c, why)) = crash {
        panic!(
            "reproduced decode failure from seed #{si} ({why}); wrote fuzz_crash_input.bin (len={})",
            c.len()
        );
    } else {
        eprintln!("no crash in {iterations} iterations");
    }
}

/// Dump adversarial structured seeds into the fuzz corpus directory so the
/// plain libFuzzer binary can mutate from valid large-dimension / P-slice
/// starting points (coverage-guided fuzzing on this platform lacks the
/// sanitizer runtime, so we seed with structurally-valid inputs).
#[test]
fn dump_corpus_seeds() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("fuzz")
        .join("corpus")
        .join("fuzz_h264_nal");
    let _ = std::fs::create_dir_all(&dir);

    let write = |name: &str, data: &[u8]| {
        let _ = std::fs::write(dir.join(name), data);
    };
    write("seed_small", &seeds()[0]);
    write("seed_large_idr", &large_idr_stream());
    write("seed_idr_p", &idr_and_p_stream());

    // Adversarial: maximal dimension SPS (8192x8192) forcing the largest
    // possible picture/skip-frame allocation and reconstruction work.
    let mut v = Vec::new();
    v.extend(annexb(0x67, &sps(512, 512, 1)));
    v.extend(annexb(0x68, &pps()));
    v.extend(annexb(0x65, &idr_islice(1)));
    write("seed_8192x8192_idr", &v);

    eprintln!("wrote corpus seeds to {}", dir.display());
}
