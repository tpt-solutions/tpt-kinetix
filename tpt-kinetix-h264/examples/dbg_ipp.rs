use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::H264Decoder;

fn split_nals(annexb: &[u8]) -> Vec<Vec<u8>> {
    let mut starts = Vec::new();
    let mut i = 0usize;
    while i + 3 <= annexb.len() {
        if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
            starts.push((i, i + 3));
            i += 3;
        } else if i + 4 <= annexb.len()
            && annexb[i] == 0
            && annexb[i + 1] == 0
            && annexb[i + 2] == 0
            && annexb[i + 3] == 1
        {
            starts.push((i, i + 4));
            i += 4;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::new();
    for (idx, &(_, payload_start)) in starts.iter().enumerate() {
        let end = starts
            .get(idx + 1)
            .map(|&(next_start, _)| next_start)
            .unwrap_or(annexb.len());
        let mut unit = vec![0, 0, 0, 1];
        unit.extend_from_slice(&annexb[payload_start..end]);
        out.push(unit);
    }
    out
}

fn main() {
    let dir = std::env::temp_dir().join("dbg_ipp");
    let arg = std::env::args().nth(1);
    let (annexb, refyuv) = if let Some(ref a) = arg {
        let p = std::path::PathBuf::from(a);
        (std::fs::read(&p).unwrap(), std::fs::read(p.with_extension("yuv")).unwrap())
    } else {
        (
            std::fs::read(dir.join("ipp.h264")).unwrap(),
            std::fs::read(dir.join("ipp.yuv")).unwrap(),
        )
    };
    let (w, h) = if arg.as_deref().map_or(false, |a| a.contains("x16")) {
        (64u32, 16u32)
    } else if arg.as_deref().map_or(false, |a| a.contains("x32") || a.contains("dbg_mid")) {
        (64u32, 32u32)
    } else if arg.as_deref().map_or(false, |a| a.contains("small")) {
        (32u32, 16u32)
    } else {
        (64u32, 48u32)
    };
    let fl = (w as usize * h as usize * 3) / 2;
    let nals = split_nals(&annexb);
    let types: Vec<u8> = nals
        .iter()
        .map(|n| {
            let p = &n[4..];
            (p[0] >> 3) & 0x1f
        })
        .collect();
    println!("nal types: {:?}", types);

    let mut dec = H264Decoder::new();
    let mut decoded = Vec::new();
    for unit in nals {
        let pkt = Packet {
            pts: Timestamp::new(decoded.len() as i64, (1, 30)),
            dts: Timestamp::new(decoded.len() as i64, (1, 30)),
            data: unit,
            stream_index: 0,
            is_key_frame: decoded.is_empty(),
        };
        if let Some(frame) = dec.decode(&pkt).expect("decode") {
            decoded.push(frame);
        }
    }
    println!("decoded {} frames", decoded.len());
    for (i, frame) in decoded.iter().enumerate() {
        let refd = &refyuv[i * fl..(i + 1) * fl];
        let mut maxd = 0i32;
        let mut nd = 0usize;
        for j in 0..fl {
            let d = (frame.data[j] as i32 - refd[j] as i32).abs();
            if d != 0 {
                nd += 1;
                maxd = maxd.max(d);
            }
        }
        println!("frame {i}: max={maxd} ndiff={nd}/{fl}");

        let mbw = w / 16;
        let mbh = h / 16;
        let mut line = String::new();
        for my in 0..mbh {
            for mx in 0..mbw {
                let mut n = 0usize;
                for yy in 0..16u32 {
                    for xx in 0..16u32 {
                        let px = mx * 16 + xx;
                        let py = my * 16 + yy;
                        if frame.data[(py * w + px) as usize] != refd[(py * w + px) as usize] {
                            n += 1;
                        }
                    }
                }
                line.push_str(&format!("{n:4}"));
            }
            line.push('\n');
        }
        println!("  MB diff map:\n{line}");
    }
}
