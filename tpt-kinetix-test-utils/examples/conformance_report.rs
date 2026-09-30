//! Run every conformance suite and write a public results table.
//!
//! Usage:
//! `cargo run --release -p tpt-kinetix-test-utils --example conformance_report [-- --out docs]`
//!
//! Writes `CONFORMANCE.md`, `conformance.json` and one shields.io endpoint
//! file per codec under `badges/` into the output directory (default `docs`).
//!
//! Each suite is run as a subprocess and its summary line parsed, so the
//! numbers come from the same tests CI gates on. A suite whose fixtures or
//! reference decoder are missing is reported as *not run*; it is never
//! reported as passing. Environment:
//! - `KINETIX_AV1_FATE_DIR` — directory of FFmpeg FATE AV1 samples
//!   (see `tools/fetch-av1-fate.sh`).
//! - ITU H.264 fixtures come from `just fetch-h264-conformance`.

use std::{
    fmt::Write as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::json;

use out_kinetix_h264::H264Decoder;
use tpt_kinetix_av1::Av1Decoder;
use tpt_kinetix_vp9::Vp9Decoder;

enum Outcome {
    /// Suite ran; `exact` of `total` items matched the reference bit-for-bit.
    Ran { exact: u32, total: u32 },
    /// Suite could not run (missing fixtures / reference decoder).
    NotRun(String),
}

struct Suite {
    codec: &'static str,
    name: &'static str,
    reference: String,
    outcome: Outcome,
}

fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".into())
}

/// Run a command, returning stdout + stderr concatenated (and exit success).
fn run(cmd: &mut Command) -> (String, bool) {
    match cmd.stdin(Stdio::null()).output() {
        Ok(o) => {
            let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
            s.push_str(&String::from_utf8_lossy(&o.stderr));
            (s, o.status.success())
        }
        Err(e) => (format!("failed to spawn: {e}"), false),
    }
}

fn first_line(cmd: &mut Command) -> Option<String> {
    let (out, ok) = run(cmd);
    ok.then(|| out.lines().next().unwrap_or("").trim().to_string())
}

/// Parse the integer that precedes `word` on the first line containing `marker`.
fn number_before(text: &str, marker: &str, word: &str) -> Option<u32> {
    let line = text.lines().find(|l| l.contains(marker))?;
    let idx = line.find(word)?;
    line[..idx].split_whitespace().last()?.parse().ok()
}

/// Parse `test result: ok. N passed; M failed; ...` (summed over all binaries).
fn cargo_test_counts(text: &str) -> (u32, u32) {
    let (mut passed, mut failed) = (0, 0);
    for l in text.lines().filter(|l| l.starts_with("test result:")) {
        for part in l.split(';').map(str::trim) {
            let n = |w: &str| {
                part.strip_suffix(w)
                    .and_then(|p| p.split_whitespace().last()?.parse::<u32>().ok())
            };
            if let Some(v) = n("passed") {
                passed += v;
            } else if let Some(v) = n("failed") {
                failed += v;
            }
        }
    }
    (passed, failed)
}

fn vp9_suite(ffmpeg: &str) -> Suite {
    let (out, _) = run(Command::new(cargo()).args([
        "test",
        "--release",
        "-p",
        "tpt-kinetix-vp9",
        "--test",
        "conformance_vp9",
        "--",
        "--nocapture",
    ]));
    let (passed, failed) = cargo_test_counts(&out);
    let gaps = out.matches("[GAP]").count() as u32;
    let outcome = if passed + failed == 0 {
        Outcome::NotRun("test binary did not run".into())
    } else if gaps >= passed {
        Outcome::NotRun("ffmpeg/libvpx unavailable, every clip skipped".into())
    } else {
        Outcome::Ran {
            exact: passed.saturating_sub(gaps),
            total: passed + failed,
        }
    };
    Suite {
        codec: "VP9",
        name: "Clip corpus (lossless/lossy, intra/inter, odd size, multitile)",
        reference: format!("ffmpeg + libvpx-vp9 ({ffmpeg})"),
        outcome,
    }
}

fn h264_suite() -> Suite {
    let (out, _) = run(Command::new(cargo()).args([
        "test",
        "--release",
        "-p",
        "out-kinetix-h264",
        "--test",
        "itu_conformance",
        "--",
        "--nocapture",
    ]));
    let outcome = match (
        number_before(&out, "ITU conformance:", "clip(s) present"),
        number_before(&out, "ITU conformance:", "hard-checked"),
        number_before(&out, "ITU conformance:", "failure(s)"),
    ) {
        (Some(present), Some(exact), Some(failures)) if present > 0 => Outcome::Ran {
            exact: exact.saturating_sub(failures),
            total: exact,
        },
        _ => Outcome::NotRun("ITU fixtures not fetched (`just fetch-h264-conformance`)".into()),
    };
    Suite {
        codec: "H.264",
        name: "ITU-T H.264.1 curated conformance clips",
        reference: "ITU-T reference YUV (normative)".into(),
        outcome,
    }
}

fn av1_fate_suite(dav1d: &str) -> Suite {
    let outcome = if std::env::var_os("KINETIX_AV1_FATE_DIR").is_none() {
        Outcome::NotRun("KINETIX_AV1_FATE_DIR unset (`just fetch-av1-fate`)".into())
    } else {
        let (out, _) = run(Command::new(cargo()).args([
            "run",
            "--release",
            "-q",
            "-p",
            "tpt-kinetix-av1",
            "--example",
            "av1_fate_score",
        ]));
        match out.lines().find(|l| l.starts_with("AGGREGATE:")) {
            Some(l) => {
                let frac = l.split_whitespace().nth(1).unwrap_or("");
                let mut it = frac.split('/').map(|v| v.parse::<u32>());
                match (it.next(), it.next()) {
                    (Some(Ok(exact)), Some(Ok(total))) if total > 0 => {
                        Outcome::Ran { exact, total }
                    }
                    _ => Outcome::NotRun("could not parse scorer output".into()),
                }
            }
            None => Outcome::NotRun("scorer skipped (needs ffmpeg with libdav1d)".into()),
        }
    };
    Suite {
        codec: "AV1",
        name: "FFmpeg FATE AV1 samples (frames)",
        reference: format!("libdav1d ({dav1d})"),
        outcome,
    }
}

fn av1_crosscheck_suite(dav1d: &str) -> Suite {
    let (out, _) = run(Command::new(cargo()).args([
        "test",
        "--release",
        "-p",
        "tpt-kinetix-av1",
        "--test",
        "libaom_crosscheck",
        "--",
        "--nocapture",
    ]));
    let (passed, failed) = cargo_test_counts(&out);
    let outcome = if passed + failed == 0 {
        Outcome::NotRun("test binary did not run".into())
    } else if out.contains("skipping: ffmpeg with libaom-av1") {
        Outcome::NotRun("needs ffmpeg with libaom-av1 and libdav1d".into())
    } else {
        Outcome::Ran {
            exact: passed,
            total: passed + failed,
        }
    };
    Suite {
        codec: "AV1",
        name: "libaom-encode crosscheck (tests)",
        reference: format!("libdav1d ({dav1d})"),
        outcome,
    }
}

/// `YYYY-MM-DD` (UTC) from the system clock, without a date crate.
fn today() -> String {
    let days = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() / 86_400)
        .unwrap_or(0) as i64;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

fn write(path: &Path, contents: &str) {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).expect("create output dir");
    }
    std::fs::write(path, contents).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out_dir: PathBuf = args
        .iter()
        .position(|a| a == "--out")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("docs"));

    let ffmpeg = first_line(Command::new("ffmpeg").arg("-version"))
        .map(|l| l.split_whitespace().take(3).collect::<Vec<_>>().join(" "))
        .unwrap_or_else(|| "ffmpeg not found".into());
    // The AV1 reference is libdav1d as linked into that ffmpeg build.
    let dav1d = {
        let (out, _) = run(Command::new("ffmpeg").args(["-hide_banner", "-decoders"]));
        if out.contains("libdav1d") {
            "via ffmpeg".to_string()
        } else {
            "unavailable".to_string()
        }
    };
    let commit = first_line(Command::new("git").args(["rev-parse", "--short", "HEAD"]))
        .unwrap_or_else(|| "unknown".into());

    let caps = [
        ("H.264", H264Decoder::new().capabilities().pixel_exact),
        ("VP9", Vp9Decoder::new().capabilities().pixel_exact),
        ("AV1", Av1Decoder::new().capabilities().pixel_exact),
    ];

    eprintln!("running suites (release build; this can take several minutes)...");
    let suites = vec![
        h264_suite(),
        vp9_suite(&ffmpeg),
        av1_fate_suite(&dav1d),
        av1_crosscheck_suite(&dav1d),
    ];

    let date = today();
    let mut md = String::new();
    let _ = writeln!(md, "# Conformance results\n");
    let _ = writeln!(
        md,
        "Generated by `just conformance-report` on {date} at commit `{commit}`. Do not edit by hand.\n"
    );
    let _ = writeln!(
        md,
        "\"Exact\" means byte-identical output to the reference decoder on every plane. \
         A suite that could not run is shown as *not run*, never as a pass.\n"
    );
    let _ = writeln!(md, "| Codec | Suite | Exact | Reference |");
    let _ = writeln!(md, "| --- | --- | --- | --- |");
    let mut json_suites = Vec::new();
    for s in &suites {
        let (cell, exact, total) = match &s.outcome {
            Outcome::Ran { exact, total } => {
                let mark = if exact == total { "✅" } else { "❌" };
                (
                    format!("{mark} {exact}/{total}"),
                    Some(*exact),
                    Some(*total),
                )
            }
            Outcome::NotRun(why) => (format!("⚪ not run ({why})"), None, None),
        };
        let _ = writeln!(
            md,
            "| {} | {} | {} | {} |",
            s.codec, s.name, cell, s.reference
        );
        json_suites.push(json!({
            "codec": s.codec, "suite": s.name, "reference": s.reference,
            "exact": exact, "total": total,
        }));
    }
    let _ = writeln!(md, "\n## Decoder `pixel_exact` flags\n");
    let _ = writeln!(md, "| Codec | `capabilities().pixel_exact` |");
    let _ = writeln!(md, "| --- | --- |");
    for (name, exact) in caps {
        let _ = writeln!(md, "| {name} | {exact} |");
    }
    let _ = writeln!(
        md,
        "\nH.264 is patent-encumbered and is **not published**; its results are for local/reference work only. \
         See [PATENTS.md](../PATENTS.md)."
    );

    write(&out_dir.join("CONFORMANCE.md"), &md);
    write(
        &out_dir.join("conformance.json"),
        &serde_json::to_string_pretty(&json!({
            "generated": date, "commit": commit, "suites": json_suites,
        }))
        .unwrap(),
    );

    // shields.io endpoint badges, one per codec: aggregate over suites that ran.
    for codec in ["H.264", "VP9", "AV1"] {
        let (mut e, mut t) = (0, 0);
        for s in suites.iter().filter(|s| s.codec == codec) {
            if let Outcome::Ran { exact, total } = s.outcome {
                e += exact;
                t += total;
            }
        }
        let (message, color) = if t == 0 {
            ("not run".to_string(), "lightgrey")
        } else if e == t {
            (format!("{e}/{t} exact"), "brightgreen")
        } else {
            (format!("{e}/{t} exact"), "red")
        };
        let badge = json!({
            "schemaVersion": 1,
            "label": format!("{codec} conformance"),
            "message": message,
            "color": color,
        });
        let file = format!("badges/{}.json", codec.to_lowercase().replace('.', ""));
        write(
            &out_dir.join(file),
            &serde_json::to_string_pretty(&badge).unwrap(),
        );
    }

    println!("{md}");
    eprintln!("wrote {}", out_dir.display());
}
