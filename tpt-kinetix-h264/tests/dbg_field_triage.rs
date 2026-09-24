//! Scratch: decode the first N frames of an ITU clip in strict mode and print
//! the exact per-NAL error, so a scaffold fallback surfaces its reason.
//!
//! Run: FIELD_CLIP=CI1_FT_B cargo test -p tpt-kinetix-h264 \
//!   --test dbg_field_triage -- --nocapture

use std::path::PathBuf;

use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::H264Decoder;

fn nal_starts(annexb: &[u8]) -> Vec<usize> {
    let mut starts = Vec::new();
    for i in 0..annexb.len().saturating_sub(3) {
        if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
            starts.push(i + 3);
        }
    }
    starts
}

#[test]
fn field_triage_strict() {
    let name = std::env::var("FIELD_CLIP").unwrap_or_else(|_| "CI1_FT_B".into());
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/itu")
        .join(&name);
    if !dir.exists() {
        eprintln!(
            "skipping: ITU fixture dir not found at {dir:?} (run `just fetch-h264-conformance`)"
        );
        return;
    }
    let mut stream = None;
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        let path = e.path();
        let ext = path.extension().and_then(|x| x.to_str()).unwrap_or("");
        if matches!(ext, "264" | "jsv" | "h264" | "avc" | "jvt" | "26l" | "bits") {
            stream = Some(path);
            break;
        }
    }
    let annexb = std::fs::read(stream.expect("bitstream")).unwrap();
    let starts = nal_starts(&annexb);

    // Optional reference YUV for pixel comparison.
    let refyuv = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            matches!(
                p.extension().and_then(|x| x.to_str()),
                Some("yuv") | Some("qcif") | Some("cif") | Some("4cif")
            )
        })
        .and_then(|p| std::fs::read(p).ok());

    let mut dec = H264Decoder::new().with_strict(true);
    if std::env::var_os("FIELD_DISPLAY_ORDER").is_some() {
        dec = dec.with_display_order();
    }
    let mut emitted_count = 0usize;
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
        match dec.decode(&pkt) {
            Ok(Some(f)) => {
                eprintln!(
                    "NAL {n}: frame {}x{} len={}",
                    f.width,
                    f.height,
                    f.data.len()
                );
                if std::env::var_os("FIELD_MATCH_SEARCH").is_some() {
                    if let Some(refy) = &refyuv {
                        let fl = f.width as usize * f.height as usize * 3 / 2;
                        if fl > 0 && f.data.len() == fl {
                            let mut matched = None;
                            let nframes = refy.len() / fl;
                            for ri in 0..nframes {
                                if refy[ri * fl..(ri + 1) * fl] == f.data[..] {
                                    matched = Some(ri);
                                    break;
                                }
                            }
                            eprintln!(
                                "        emitted#{emitted_count} (NAL {n}) matches ref index {matched:?}"
                            );
                        }
                    }
                }
                if let Some(refy) = &refyuv {
                    let fl = f.width as usize * f.height as usize * 3 / 2;
                    if refy.len() >= fl {
                        let md = f.data[..fl]
                            .iter()
                            .zip(&refy[..fl])
                            .map(|(a, b)| (*a as i32 - *b as i32).abs())
                            .max()
                            .unwrap_or(0);
                        let nd = f.data[..fl]
                            .iter()
                            .zip(&refy[..fl])
                            .filter(|(a, b)| a != b)
                            .count();
                        eprintln!("        vs ref frame 0: max_diff={md} ndiff={nd}/{fl}");
                    }
                }
                // Full per-frame comparison + field-parity split (luma only).
                if let Some(refy) = &refyuv {
                    let w = f.width as usize;
                    let h = f.height as usize;
                    let yl = w * h;
                    let idx = emitted_count;
                    if refy.len() >= yl * (idx + 1) {
                        let r = &refy[yl * idx..yl * (idx + 1)];
                        let (mut top_n, mut bot_n, mut tmax, mut bmax) =
                            (0usize, 0usize, 0i32, 0i32);
                        for y in 0..h {
                            for x in 0..w {
                                let d = (f.data[y * w + x] as i32 - r[y * w + x] as i32).abs();
                                if d != 0 {
                                    if y % 2 == 0 {
                                        top_n += 1;
                                        tmax = tmax.max(d);
                                    } else {
                                        bot_n += 1;
                                        bmax = bmax.max(d);
                                    }
                                }
                            }
                        }
                        eprintln!(
                            "        frame {idx} luma: top-field ndiff={top_n} max={tmax} | bottom-field ndiff={bot_n} max={bmax}"
                        );
                        // Target frame for the row-band map / luma dump.
                        let want: usize = std::env::var("DIFF_FRAME")
                            .ok()
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(1);
                        // Dump luma of our frame `want` plus the first few
                        // reference frames for offline shift correlation.
                        if idx == want {
                            let dump = |tag: &str, src: &[u8]| {
                                let p = std::env::temp_dir().join(tag);
                                std::fs::write(&p, src).unwrap();
                                eprintln!("          dumped {p:?}");
                            };
                            dump("triage_ours_f1.y", &f.data[..yl]);
                            for rf in 0..4usize {
                                if refy.len() >= yl * (rf + 1) {
                                    dump(
                                        std::format!("triage_ref_f{rf}.y").leak(),
                                        &refy[yl * rf..yl * (rf + 1)],
                                    );
                                }
                            }
                        }
                        // Row-band diff map (per-field-row counts, first 40 rows).
                        if idx == want {
                            // ASCII per-MB error map of each field: 45x30
                            // grid, '.' <=8 wrong samples, '0'-'9' tens,
                            // '#' >99. Field-row r of MB row R is 16R+r.
                            for (_parity, tag) in [(0usize, "TOP"), (1usize, "BOT")] {
                                eprintln!("          MB map {tag}:");
                                for my in 0..(h / 16) {
                                    let mut line = String::new();
                                    for mx in 0..(w / 16) {
                                        let mut wrong = 0usize;
                                        for fr in 0..16 {
                                            // 16*my+fr is a FRAME row: even rows
                                            // belong to the top field, odd to
                                            // the bottom.
                                            let y = 16 * my + fr;
                                            for x in (16 * mx)..(16 * mx + 16) {
                                                if f.data[y * w + x] != r[y * w + x] {
                                                    wrong += 1;
                                                }
                                            }
                                        }
                                        let c = if wrong <= 8 {
                                            '.'
                                        } else if wrong > 99 {
                                            '#'
                                        } else {
                                            std::char::from_digit((wrong / 10) as u32, 10)
                                                .unwrap_or('?')
                                        };
                                        line.push(c);
                                    }
                                    eprintln!("            {line}");
                                }
                            }
                        }
                    }
                }
                emitted_count += 1;
            }
            Ok(None) => eprintln!("NAL {n}: no frame"),
            Err(err) => {
                eprintln!("NAL {n}: ERR {err}");
                break;
            }
        }
    }
}
