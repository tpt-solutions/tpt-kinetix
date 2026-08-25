//! Phase G.4/G.5 diagnostic: why does the MBAFF I-only frame diverge?
//! Per-macroblock-pair diff map of our decode vs ffmpeg for the single-I
//! interlaced clip, plus each pair's parsed mb_field_decoding_flag.

use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::H264Decoder;

const W: usize = 64;
const H: usize = 64;

#[test]
fn g4_mbaff_i1_diffmap() {
    let dir = std::env::temp_dir().join("dbg_g5_interlaced");
    let h264 = dir.join("mbaff_i1.h264");
    let refyuv = dir.join("mbaff_i1_nolf.yuv");
    assert!(h264.exists(), "run g5_interlaced_corpus first");
    let ff = std::fs::read(&refyuv).unwrap();
    let frame_len = W * H * 3 / 2;
    assert!(ff.len() >= frame_len);

    let mut dec = H264Decoder::new();
    let annexb = std::fs::read(&h264).unwrap();
    let mut starts = Vec::new();
    for i in 0..annexb.len().saturating_sub(3) {
        if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
            starts.push(i + 3);
        }
    }
    let mut ours: Option<Vec<u8>> = None;
    for (n, &s) in starts.iter().enumerate() {
        let e = starts.get(n + 1).copied().unwrap_or(annexb.len());
        let mut data = vec![0u8, 0, 0, 1];
        data.extend_from_slice(&annexb[s..e]);
        let pkt = Packet {
            pts: Timestamp::new(n as i64, (1, 30)),
            dts: Timestamp::new(n as i64, (1, 30)),
            data,
            stream_index: 0,
            is_key_frame: true,
        };
        if let Ok(Some(f)) = dec.decode(&pkt) {
            if f.data.len() == W * H * 3 / 2 {
                ours = Some(f.data);
            }
        }
    }
    let o = ours.expect("no frame decoded");

    // Luma diff map per macroblock pair (pair = rows 2k,2k+1).
    println!("MBAFF I1 pair diff map (luma):");
    for py in 0..(H / 32) {
        for px in 0..(W / 16) {
            let mut n_diff = 0usize;
            let mut max_d = 0i32;
            for y in 0..32usize {
                for x in 0..16usize {
                    let idx = (py * 32 + y) * W + px * 16 + x;
                    let d = (o[idx] as i32 - ff[idx] as i32).abs();
                    if d != 0 {
                        n_diff += 1;
                        max_d = max_d.max(d);
                    }
                }
            }
            println!("pair({px},{py}): {n_diff}/512 differ, max={max_d}");
        }
    }

    // Chroma quick check.
    let cw = W / 2;
    let ch = H / 2;
    let ou = &o[W * H..W * H + cw * ch];
    let fu = &ff[W * H..W * H + cw * ch];
    let nu = ou.iter().zip(fu.iter()).filter(|(a, b)| a != b).count();
    println!("chroma-U differing samples: {nu}/{}", cw * ch);

    // Pixel forensics on the very first MB (frame pair, picture corner).
    println!("MB(0,0) luma rows 0..3: ours | ff");
    for y in 0..4usize {
        let base = y * W;
        println!(
            "r{y} o{:?}\n   f{:?}",
            &o[base..base + 16],
            &ff[base..base + 16]
        );
    }
}
