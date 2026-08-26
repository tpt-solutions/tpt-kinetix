// Session #32k: compare the orchestrator's effective edge set (KINETIX_DBG_BS
// traces: BSV/BSV-INT/BSH/BSH-INT lines) against the plain path's (KINETIX_BINTRACE
// traces: "DEBLOCK L v-edge/h-edge" lines) on the same decoded stream.
//
// Run:
//   $env:KINETIX_DBG_BS='1'; $env:KINETIX_BINTRACE='1'
//   cargo test -p tpt-kinetix-h264 --test dbg_g6_mbaff_deblock -- --nocapture > t6.log
//   cargo test -p tpt-kinetix-h264 --test dbg_edge_diff -- --nocapture
#[test]
fn diff_orchestrator_vs_plain_edge_sets() {
    let log = {
        let candidates = [
            std::path::PathBuf::from("t6.log"),
            std::path::PathBuf::from("../t6.log"),
            std::path::PathBuf::from("../edge_log.txt"),
        ];
        let mut found = None;
        for c in &candidates {
            // Windows PowerShell's Out-File defaults to UTF-16LE; decode
            // accordingly when a BOM is present, else fall back to UTF-8.
            if let Ok(bytes) = std::fs::read(c) {
                let text = if bytes.starts_with(&[0xFF, 0xFE]) {
                    let units: Vec<u16> = bytes[2..]
                        .chunks(2)
                        .map(|c| u16::from_le_bytes([c[0], *c.get(1).unwrap_or(&0)]))
                        .collect();
                    String::from_utf16_lossy(&units)
                } else {
                    String::from_utf8_lossy(&bytes).into_owned()
                };
                found = Some(text);
                break;
            }
        }
        match found {
            Some(l) => l,
            None => {
                eprintln!("diff: no t6.log found; run the g6 harness with both trace envs first");
                return;
            }
        }
    };
    // Normalised edge key: "D|mb_x,mb_y|edge_index|b1,b2,b3,b4".
    fn norm(kind: &str, mb: &str, ei: &str, bs: &str) -> String {
        let vals: Vec<String> = bs
            .split(',')
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .collect();
        assert_eq!(vals.len(), 4, "bad bs in {kind} {mb} ei={ei}: {bs}");
        let mb = mb.replace(['(', ')', ' '], "");
        format!("{kind}|{mb}|{ei}|{}", vals.join("|"))
    }
    let mut orch: std::collections::HashSet<String> = Default::default();
    let mut plain: std::collections::HashSet<String> = Default::default();
    for line in log.lines() {
        if let Some(rest) = line.strip_prefix("BSV mb=") {
            // "BSV mb=(x,y) bs=[a, b, c, d] ..."
            if let Some(i) = rest.find("bs=[") {
                let mb = rest[..i].trim().trim_end_matches("bs=").trim();
                let bs = &rest[i + 4..].split(']').next().unwrap_or("");
                orch.insert(norm("V", mb.trim(), "0", bs));
            }
        } else if let Some(rest) = line.strip_prefix("BSV-INT mb=") {
            if let Some(i) = rest.find("ei=") {
                let mb = rest[..i].trim().trim_end_matches("ei=").trim();
                let tail = &rest[i + 3..];
                if let Some(j) = tail.find("bs=[") {
                    let ei = tail[..j].trim();
                    let bs = &tail[j + 4..].split(']').next().unwrap_or("");
                    orch.insert(norm("V", mb.trim(), ei, bs));
                }
            }
        } else if let Some(rest) = line.strip_prefix("BSH mb=") {
            if let Some(i) = rest.find("bs=[") {
                let mb = rest[..i].trim().trim_end_matches("bs=").trim();
                let bs = &rest[i + 4..].split(']').next().unwrap_or("");
                orch.insert(norm("H", mb.trim(), "0", bs));
            }
        } else if let Some(rest) = line.strip_prefix("BSH-INT mb=") {
            if let Some(i) = rest.find("ei=") {
                let mb = rest[..i].trim().trim_end_matches("ei=").trim();
                let tail = &rest[i + 3..];
                if let Some(j) = tail.find("bs=[") {
                    let ei = tail[..j].trim();
                    let bs = &tail[j + 4..].split(']').next().unwrap_or("");
                    orch.insert(norm("H", mb.trim(), ei, bs));
                }
            }
        } else if line.contains("DEBLOCK L v-edge MB(") || line.contains("DEBLOCK L h-edge MB(") {
            let kind = if line.contains("v-edge") { "V" } else { "H" };
            // "DEBLOCK L v-edge MB(x,y) idxN bs=[...] ..."
            if let Some(i) = line.find("MB(") {
                let rest = &line[i + 3..];
                if let Some(j) = rest.find(')') {
                    let mb = &rest[..j];
                    let after = &rest[j + 1..];
                    if let Some(ei_i) = after.find("idx") {
                        let tail = &after[ei_i + 3..];
                        let ei_end = tail.find(' ').unwrap_or(tail.len());
                        let ei = &tail[..ei_end];
                        if let Some(bs_i) = tail.find("bs=[") {
                            let bs = &tail[bs_i + 4..].split(']').next().unwrap_or("");
                            let nonzero = bs
                                .split(',')
                                .any(|v| !v.trim().is_empty() && v.trim() != "0");
                            if nonzero {
                                plain.insert(norm(kind, mb, ei, bs));
                            }
                        }
                    }
                }
            }
        }
    }

    let mut only_orch: Vec<&String> = orch.difference(&plain).collect();
    let mut only_plain: Vec<&String> = plain.difference(&orch).collect();
    only_orch.sort();
    only_plain.sort();
    println!(
        "edge sets: orchestrator={} plain={} only-orch={} only-plain={}",
        orch.len(),
        plain.len(),
        only_orch.len(),
        only_plain.len()
    );
    for k in only_orch.iter().take(20) {
        println!("  ONLY-IN-ORCHESTRATOR: {k}");
    }
    for k in only_plain.iter().take(20) {
        println!("  ONLY-IN-PLAIN:        {k}");
    }
}
