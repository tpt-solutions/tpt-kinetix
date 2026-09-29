//! Throwaway repro harness for the t2.ivf chroma-only inter divergence
//! documented in todo-av1.md (session cont'd 23/24). NOT meant to be
//! committed as a permanent test — delete before finishing.
//!
//! Reproduces `ffmpeg -f lavfi -i testsrc2=size=128x96:rate=15:duration=2
//! -frames:v 2 -pix_fmt yuv420p -g 10 -c:v av1 -f ivf t2.ivf`, decodes both
//! frames with Kinetix and with real dav1d, and diffs frame 1's chroma.

use std::{
    io::Read,
    process::{Command, Stdio},
};

use tpt_kinetix_av1::Av1Decoder;
use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};
use tpt_kinetix_test_utils::reference::{decode_av1_with_dav1d, split_ivf_frames};

fn make_t2_ivf() -> Option<Vec<u8>> {
    let mut child = Command::new("ffmpeg")
        .args([
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=128x96:rate=15:duration=2",
            "-frames:v",
            "2",
            "-pix_fmt",
            "yuv420p",
            "-g",
            "10",
            "-c:v",
            "av1",
            "-f",
            "ivf",
            "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut out = Vec::new();
    let read = child.stdout.take()?.read_to_end(&mut out).is_ok();
    let _ = child.wait();
    if !read || out.is_empty() {
        None
    } else {
        Some(out)
    }
}

#[test]
fn dbg_t2_chroma_diff() {
    const W: usize = 128;
    const H: usize = 96;
    let Some(ivf) = make_t2_ivf() else {
        eprintln!("ffmpeg unavailable, skipping");
        return;
    };

    let ref_frames = match decode_av1_with_dav1d(&ivf, W as u32, H as u32) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("dav1d unavailable: {e:?}, skipping");
            return;
        }
    };
    assert!(ref_frames.len() >= 2, "need 2 ref frames");

    let payloads = split_ivf_frames(&ivf);
    assert!(payloads.len() >= 2, "need 2 ivf payloads");

    let mut dec = Av1Decoder::new();
    let mut kin_frames = Vec::new();
    for (i, payload) in payloads.iter().enumerate() {
        let packet = Packet {
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            data: payload.clone(),
            stream_index: 0,
            is_key_frame: i == 0,
        };
        match dec.decode(&packet) {
            Ok(Some(f)) => kin_frames.push(f),
            other => eprintln!("[frame {i}] kinetix: {other:?}"),
        }
    }
    eprintln!("kinetix decoded {} frames", kin_frames.len());
    assert!(kin_frames.len() >= 2, "need 2 kinetix frames");

    let uv_w = W / 2;
    let uv_h = H / 2;
    let frame = &kin_frames[1];
    let ref_frame = &ref_frames[1];

    let u_off = W * H;
    let v_off = u_off + uv_w * uv_h;

    for (plane_name, off) in [("U", u_off), ("V", v_off)] {
        let mut total = 0u64;
        let mut n = 0u32;
        eprintln!("=== {plane_name} diffs (frame 1) ===");
        for y in 0..uv_h {
            for x in 0..uv_w {
                let a = frame.data[off + y * uv_w + x] as i32;
                let b = ref_frame.data[off + y * uv_w + x] as i32;
                let d = a - b;
                if d != 0 {
                    n += 1;
                    total += d.unsigned_abs() as u64;
                    if n <= 100 {
                        eprintln!("  ({x},{y}) kin={a} dav1d={b} d={d:+}");
                    }
                }
            }
        }
        eprintln!("{plane_name}: {n} differing samples, total |diff|={total}");
    }

    let mut y_total = 0u64;
    for y in 0..H {
        for x in 0..W {
            let a = frame.data[y * W + x] as i32;
            let b = ref_frame.data[y * W + x] as i32;
            y_total += (a - b).unsigned_abs() as u64;
        }
    }
    eprintln!("Y total |diff| = {y_total}");

    // --- lead (a) check: is dav1d's own frame-0 V-plane row 19, x=26..41
    // (the region KINETIX_AV1_MCSUM says mi=(16,11)'s chroma MC reads from)
    // what Kinetix's MCSUM dump reports? Both decoders' frame 0 is supposed
    // to be bit-exact, so this directly tests whether Kinetix's *reference*
    // read is wrong (lead a) versus something later overwriting the region
    // (lead b).
    if std::env::var("KINETIX_AV1_DBG_LEAD_A").is_ok() {
        let f0 = &ref_frames[0];
        let v_off0 = W * H + uv_w * uv_h;
        for y in 19..23 {
            let row: Vec<i32> = (26..42)
                .map(|x| f0.data[v_off0 + y * uv_w + x] as i32)
                .collect();
            eprintln!("dav1d frame0 V row y={y} x=26..41 = {row:?}");
        }
        // Full 4x4 prediction block for mi=(16,11)'s chroma V write
        // (base_y=19, base_x=34, straight full-pel copy per KINMCSUM/
        // KINMCOUT): pred[r][c] = frame0[19+r][34+c].
        let mut pred = [[0i32; 4]; 4];
        for r in 0..4 {
            for c in 0..4 {
                pred[r][c] = f0.data[v_off0 + (19 + r) * uv_w + (34 + c)] as i32;
            }
        }
        eprintln!("pred 4x4 (from frame0, base=(34,19)) = {pred:?}");
        let frame1 = &kin_frames[1];
        let ref1 = &ref_frames[1];
        for r in 0..4 {
            let kin_row: Vec<i32> = (0..4)
                .map(|c| frame1.data[v_off + (20 + r) * uv_w + (32 + c)] as i32)
                .collect();
            let dav_row: Vec<i32> = (0..4)
                .map(|c| ref1.data[v_off + (20 + r) * uv_w + (32 + c)] as i32)
                .collect();
            let kin_resid: Vec<i32> = (0..4).map(|c| kin_row[c] - pred[r][c]).collect();
            let dav_resid_implied: Vec<i32> = (0..4).map(|c| dav_row[c] - pred[r][c]).collect();
            eprintln!(
                "row y={} kin_final={kin_row:?} dav1d_final={dav_row:?} kin_resid={kin_resid:?} dav1d_resid_implied={dav_resid_implied:?}",
                20 + r
            );
        }
    }
}
