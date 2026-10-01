//! Record a committed performance baseline: run every Criterion bench target
//! in the workspace, capture its mean throughput, and write
//!
//! * `docs/perf/baseline-<label>.json` — machine + toolchain metadata plus one
//!   entry per benchmark (the machine-readable snapshot `bench-compare` diffs
//!   against), and
//! * `docs/PERFORMANCE.md` — the human-readable table regenerated from the
//!   same data.
//!
//! This is Phase 0 of `todo-perf.md`: a number with no committed baseline is
//! not a measurement.
//!
//! Usage: `cargo run --release -p tpt-kinetix-test-utils --example bench_baseline
//!         [--label <label>] [--bench-crate <crate> ...]`
//!
//! `--label` defaults to today's date (`YYYY-MM-DD`). With no `--bench-crate`
//! every crate with a bench target is run.

use std::collections::BTreeMap;
use std::process::Command;

/// Every crate in the workspace that ships a Criterion bench target.
const BENCH_CRATES: &[&str] = &[
    "out-kinetix-h264",
    "tpt-kinetix-av1",
    "tpt-kinetix-vp9",
    "tpt-kinetix-bitstream",
    "tpt-kinetix-lean",
    "tpt-kinetix-lossless",
    "tpt-kinetix-realtime",
    "tpt-kinetix-screen",
    "tpt-kinetix-vision",
    "tpt-kinetix-face",
    "tpt-kinetix-volumetric",
    "tpt-kinetix-demux",
    "tpt-kinetix-mux",
    "tpt-kinetix-pipeline",
];

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let mut label = today();
    let mut crates: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--label" => {
                i += 1;
                label = args.get(i).cloned().unwrap_or_else(today);
            }
            other if other.starts_with("--label=") => {
                label = other.trim_start_matches("--label=").to_string();
            }
            "--bench-crate" => {
                i += 1;
                if let Some(c) = args.get(i) {
                    crates.push(c.clone());
                }
            }
            other if other.starts_with("--bench-crate=") => {
                crates.push(other.trim_start_matches("--bench-crate=").to_string());
            }
            _ => {}
        }
        i += 1;
    }
    if crates.is_empty() {
        crates = BENCH_CRATES.iter().map(|s| s.to_string()).collect();
    }

    eprintln!(
        "Recording performance baseline '{label}' across {} crates...",
        crates.len()
    );

    // Crate -> benchmark id -> mean throughput string.
    let mut results: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for crate_name in &crates {
        eprintln!("  {crate_name} ...");
        let mut cmd = Command::new("cargo");
        cmd.args(["bench", "-p", crate_name, "--", "--quiet"]);
        let output = match cmd.output() {
            Ok(o) => o,
            Err(e) => {
                eprintln!("    skipped: {e}");
                continue;
            }
        };
        if !output.status.success() {
            eprintln!("    cargo bench exited with {}", output.status);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let per_crate = parse_benches(&stderr, &stdout);
        if !per_crate.is_empty() {
            results.insert(crate_name.clone(), per_crate);
        }
    }

    if results.is_empty() {
        eprintln!("No benchmark results were produced; nothing written.");
        std::process::exit(1);
    }

    let json_path = format!("docs/perf/baseline-{label}.json");
    write_file(&json_path, &render_json(&label, &results));
    eprintln!("Wrote {json_path}");

    write_file("docs/PERFORMANCE.md", &render_markdown(&label, &results));
    eprintln!("Wrote docs/PERFORMANCE.md");
}

/// Parse Criterion's output into `{benchmark id: mean throughput}`.
///
/// Criterion writes its progress headers (`Benchmarking <id>: ...`) to
/// **stderr** and its result lines (`time:` / `thrpt:`) to **stdout**, so the
/// two streams must be parsed separately and zipped in emission order —
/// concatenating them would put every header *after* every result and pair
/// nothing up.
///
/// A benchmark only emits a `thrpt:` line when its group declared a
/// `Throughput`. Those that did not (e.g. `av1_encode`) still emit a `time:`
/// line, so those are recorded as a duration instead — otherwise the flagship
/// AV1 numbers would be silently absent from the baseline.
fn parse_benches(stderr: &str, stdout: &str) -> BTreeMap<String, String> {
    let ids = bench_ids(stderr);
    let throughputs = result_values(stdout, "thrpt:");
    let times = result_values(stdout, "time:");

    let mut out = BTreeMap::new();
    for (i, id) in ids.iter().enumerate() {
        if let Some(t) = throughputs.get(i) {
            out.insert(id.clone(), t.clone());
        } else if let Some(t) = times.get(i) {
            out.insert(id.clone(), t.clone());
        }
    }
    out
}

/// The benchmark ids Criterion started, in emission order (stderr). Repeated
/// `Benchmarking <id>: Warming up / Collecting / Analyzing` continuations of the
/// same id are collapsed.
fn bench_ids(stderr: &str) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for line in stderr.lines() {
        if let Some(rest) = line.trim_start().strip_prefix("Benchmarking ") {
            let id = rest.split(':').next().unwrap_or("").trim();
            if !id.is_empty() && !ids.last().is_some_and(|last| last == id) {
                ids.push(id.to_string());
            }
        }
    }
    ids
}

/// The middle estimate of every line containing a `<prefix>` result marker, in
/// emission order. The marker is matched anywhere in the line because Criterion
/// prints `thrpt:` indented but `<id>     time:` with the id in front of it.
fn result_values(stdout: &str, prefix: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        let Some(idx) = line.find(prefix) else {
            continue;
        };
        if let Some(mean) = extract_mean(&line[idx + prefix.len()..]) {
            out.push(mean);
        }
    }
    out
}

/// Pull the middle (point-estimate) throughput out of a `[lo mid hi]`
/// interval, preserving the unit token.
///
/// Criterion's line looks like one of:
///
/// ```text
/// thrpt:  [10.418 Melem/s 10.487 Melem/s 10.561 Melem/s]   <- value/unit pairs
/// thrpt:  [+1.0915% +1.9489% +2.9378%]                     <- change, no units
/// ```
///
/// So the numeric tokens are collected and the *second* one is the point
/// estimate, and the first non-numeric token is the unit. A line whose tokens
/// are all percentages carries no throughput and is rejected.
fn extract_mean(rest: &str) -> Option<String> {
    let inner = rest.trim().trim_start_matches('[').trim_end_matches(']');
    let mut numbers: Vec<&str> = Vec::new();
    let mut unit = String::new();
    for token in inner.split_whitespace() {
        let t = token.trim_start_matches('[').trim_end_matches(']');
        if t.parse::<f64>().is_ok() {
            numbers.push(t);
        } else if unit.is_empty() {
            unit = t.to_string();
        }
    }
    // Need lo + mid at minimum; unit-less (percentage) lines are skipped.
    let mid = numbers.get(1)?;
    if unit.is_empty() {
        return None;
    }
    Some(format!("{mid} {unit}"))
}

fn render_json(label: &str, results: &BTreeMap<String, BTreeMap<String, String>>) -> String {
    let mut s = String::new();
    s.push_str("{\n");
    s.push_str(&format!("  \"label\": \"{label}\",\n"));
    s.push_str(&format!("  \"date\": \"{}\",\n", today()));
    s.push_str(&format!("  \"cpu\": \"{}\",\n", cpu_model()));
    s.push_str(&format!("  \"logical_cores\": {},\n", logical_cores()));
    s.push_str(&format!("  \"os\": \"{}\",\n", os_summary()));
    s.push_str(&format!("  \"rustc\": \"{}\",\n", tool_version("rustc")));
    s.push_str(&format!("  \"cargo\": \"{}\",\n", tool_version("cargo")));
    s.push_str("  \"profile\": \"release\",\n");
    s.push_str(&format!("  \"ffmpeg\": \"{}\",\n", ffmpeg_version()));
    s.push_str("  \"results\": {\n");
    let crate_entries: Vec<(&String, &BTreeMap<String, String>)> = results.iter().collect();
    for (ci, (crate_name, benches)) in crate_entries.iter().enumerate() {
        s.push_str(&format!("    \"{crate_name}\": {{\n"));
        let bench_entries: Vec<(&String, &String)> = benches.iter().collect();
        for (bi, (id, thrpt)) in bench_entries.iter().enumerate() {
            let comma = if bi + 1 == bench_entries.len() {
                ""
            } else {
                ","
            };
            s.push_str(&format!("      \"{id}\": \"{thrpt}\"{comma}\n"));
        }
        let comma = if ci + 1 == crate_entries.len() {
            ""
        } else {
            ","
        };
        s.push_str(&format!("    }}{comma}\n"));
    }
    s.push_str("  }\n");
    s.push_str("}\n");
    s
}

fn render_markdown(label: &str, results: &BTreeMap<String, BTreeMap<String, String>>) -> String {
    let mut s = String::new();
    s.push_str("# TPT Kinetix — performance baseline\n\n");
    s.push_str(&format!(
        "Generated by `just bench-baseline` (see `todo-perf.md`, Phase 0). \
         Snapshot label: **{label}**.\n\n"
    ));
    s.push_str("## Recording environment\n\n");
    s.push_str("| Field | Value |\n|:---|:---|\n");
    s.push_str(&format!("| Date | {} |\n", today()));
    s.push_str(&format!("| CPU | {} |\n", cpu_model()));
    s.push_str(&format!("| Logical cores | {} |\n", logical_cores()));
    s.push_str(&format!("| OS | {} |\n", os_summary()));
    s.push_str(&format!("| rustc | {} |\n", tool_version("rustc")));
    s.push_str(&format!("| cargo | {} |\n", tool_version("cargo")));
    s.push_str("| Profile | release (`--release`, workspace default) |\n");
    s.push_str(&format!("| ffmpeg | {} |\n\n", ffmpeg_version()));
    s.push_str(
        "> Numbers are Criterion mean throughput. They are **not** comparable across \
         machines or toolchains — re-record on the target hardware before drawing \
         conclusions, and re-run after any optimisation change to see the delta \
         against this snapshot.\n\n",
    );
    s.push_str("## Throughput by crate\n\n");
    s.push_str("| Crate | Benchmark | Mean throughput |\n|:---|:---|---:|\n");
    for (crate_name, benches) in results {
        for (id, thrpt) in benches {
            s.push_str(&format!("| `{crate_name}` | `{id}` | {thrpt} |\n"));
        }
    }
    s.push('\n');
    s.push_str("## Reading the units\n\n");
    s.push_str(
        "A value with an `elem/s` or `B/s` unit is **throughput** (higher is better); a value \
         with a time unit (`ms`, `us`, `s`) is a **duration** (lower is better). Benchmarks whose \
         Criterion group declared no `Throughput` are recorded as durations, so compare those \
         against each other by inverting the direction.\n\n",
    );
    s.push_str(
        "- `Melem/s` / `Kelem/s` — megapixels (resp. kilopixels) of luma processed per \
         second for the 2D codecs, and megapixels of sample count per second for the \
         lossless codec.\n\
         - `elem/s` on the realtime bench counts **slices** (64 per frame at the 8x8 grid), \
         which is the latency-relevant unit rather than frames.\n\
         - `MiB/s` on the bitstream bench is raw payload bytes through `BitReader` / rANS.\n\
         - `Melem/s` on the volumetric bench counts 6 values per point (3 position \
         components plus 3 colour samples).\n",
    );
    s
}

fn write_file(path: &str, contents: &str) {
    if let Some(parent) = std::path::Path::new(path).parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("could not create {}: {e}", parent.display());
            std::process::exit(1);
        }
    }
    if let Err(e) = std::fs::write(path, contents) {
        eprintln!("could not write {path}: {e}");
        std::process::exit(1);
    }
}

fn today() -> String {
    // RFC 3339 UTC date from the system clock. No `chrono` dependency: read the
    // seconds since the Unix epoch and do the civil-date arithmetic inline.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    // Howard Hinnant's civil_from_days algorithm.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

fn tool_version(tool: &str) -> String {
    Command::new(tool)
        .arg("--version")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "unavailable".to_string())
}

fn ffmpeg_version() -> String {
    Command::new("ffmpeg")
        .args(["-version"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string()
        })
        .unwrap_or_else(|| "not installed (ffmpeg-gated benches were skipped)".to_string())
}

fn cpu_model() -> String {
    // Windows: %PROCESSOR_IDENTIFIER%. Elsewhere: `uname -m` is a weak signal but
    // is better than nothing for a baseline header.
    if let Ok(o) = Command::new("cmd")
        .args(["/C", "echo %PROCESSOR_IDENTIFIER%"])
        .output()
    {
        let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
        if !s.is_empty() {
            return s;
        }
    }
    Command::new("uname")
        .arg("-m")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

fn logical_cores() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(0)
}

fn os_summary() -> String {
    let out = Command::new("cmd")
        .args(["/C", "ver"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty());
    out.unwrap_or_else(|| std::env::consts::OS.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_mean_takes_the_middle_estimate_and_its_unit() {
        assert_eq!(
            extract_mean("  [10.418 Melem/s 10.487 Melem/s 10.561 Melem/s]").as_deref(),
            Some("10.487 Melem/s")
        );
    }

    #[test]
    fn extract_mean_handles_byte_units() {
        assert_eq!(
            extract_mean("  [97.884 MiB/s 98.529 MiB/s 99.088 MiB/s]").as_deref(),
            Some("98.529 MiB/s")
        );
    }

    #[test]
    fn extract_mean_rejects_a_unitless_change_line() {
        // Criterion's per-benchmark change line carries percentages, not a
        // throughput; scraping it would poison the baseline.
        assert_eq!(extract_mean("  [+1.0915% +1.9489% +2.9378%]"), None);
    }

    #[test]
    fn parse_benches_zips_ids_from_stderr_with_throughput_from_stdout() {
        // Criterion writes `Benchmarking <id>` to stderr and `thrpt:` to stdout,
        // so a single-stream parse pairs nothing up.
        let stderr = "\
Benchmarking lean_320x240/encode: Warming up for 3.0000 s
Benchmarking lean_320x240/encode: Collecting 10 samples
Benchmarking lean_320x240/encode: Analyzing
Benchmarking lean_320x240/decode: Warming up for 3.0000 s
Benchmarking lean_320x240/decode: Analyzing
";
        let stdout = "\
lean_320x240/encode     time:   [164.08 ms 164.65 ms 165.65 ms]
                        thrpt:  [463.63 Kelem/s 466.46 Kelem/s 468.06 Kelem/s]
lean_320x240/decode     time:   [21.4 ms 21.5 ms 21.6 ms]
                        thrpt:  [14.8 Melem/s 14.9 Melem/s 15.0 Melem/s]
";
        let parsed = parse_benches(stderr, stdout);
        assert_eq!(
            parsed.get("lean_320x240/encode").map(String::as_str),
            Some("466.46 Kelem/s")
        );
        assert_eq!(
            parsed.get("lean_320x240/decode").map(String::as_str),
            Some("14.9 Melem/s")
        );
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn parse_benches_falls_back_to_duration_when_no_throughput_is_declared() {
        // `av1_encode` declares no `Throughput`, so Criterion prints only a
        // `time:` line for it; it must still appear in the baseline.
        let stderr = "\
Benchmarking av1_encode/kinetix: Warming up for 3.0000 s
Benchmarking av1_encode/kinetix: Analyzing
";
        let stdout = "\
av1_encode/kinetix     time:   [1.2345 ms 1.3000 ms 1.4110 ms]
";
        let parsed = parse_benches(stderr, stdout);
        assert_eq!(
            parsed.get("av1_encode/kinetix").map(String::as_str),
            Some("1.3000 ms")
        );
    }

    #[test]
    fn today_is_a_well_formed_iso_date() {
        let d = today();
        assert_eq!(d.len(), 10, "{d} should be YYYY-MM-DD");
        assert_eq!(d.as_bytes()[4], b'-');
        assert_eq!(d.as_bytes()[7], b'-');
        assert!(d[..4].chars().all(|c| c.is_ascii_digit()));
    }
}
