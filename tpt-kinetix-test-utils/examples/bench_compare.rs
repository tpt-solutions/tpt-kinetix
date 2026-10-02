//! Compare a fresh bench run against the committed baseline snapshot and fail
//! if any benchmark regressed by more than a threshold (Phase 0 of
//! `todo-perf.md`).
//!
//! Both sides are Criterion **mean throughput strings** like `"10.418 Melem/s"`,
//! parsed into a `(value, unit)` pair and normalised to base units, so
//! `10.4 Melem/s` and `10400 Kelem/s` compare correctly. Only benchmarks present
//! in both snapshots with comparable units are checked. A *regression* is lower
//! throughput than the baseline by more than `--threshold` percent (default 5%).
//!
//! Comparing against a committed JSON snapshot (rather than Criterion's own
//! `"time: [lo mid hi]"` change lines) is deliberate: those lines vanish when
//! `target/criterion` is cleared or the baseline came from another machine,
//! which is exactly when a regression is easiest to miss.
//!
//! Usage:
//! ```text
//! cargo run --release -p tpt-kinetix-test-utils --example bench_compare \
//!     -- --baseline docs/perf/baseline-2026-10-02.json [--threshold 5]
//! ```

use std::collections::BTreeMap;

use serde_json::Value;
use tpt_kinetix_test_utils::bench_parse::parse_benches;

/// Default maximum tolerated throughput regression, in percent.
const DEFAULT_THRESHOLD: f64 = 5.0;

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

    let mut baseline_path = String::new();
    let mut threshold = DEFAULT_THRESHOLD;
    let mut write_to: Option<String> = None;

    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--baseline" => {
                i += 1;
                baseline_path = args.get(i).cloned().unwrap_or_default();
            }
            other if other.starts_with("--baseline=") => {
                baseline_path = other.trim_start_matches("--baseline=").to_string();
            }
            // Record the current run to this path instead of diffing.
            "--write-current" => {
                i += 1;
                write_to = Some(
                    args.get(i)
                        .cloned()
                        .unwrap_or_else(|| "docs/perf/current.json".to_string()),
                );
            }
            other if other.starts_with("--write-current=") => {
                write_to = Some(other.trim_start_matches("--write-current=").to_string());
            }
            "--threshold" => {
                i += 1;
                threshold = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(DEFAULT_THRESHOLD);
            }
            other if other.starts_with("--threshold=") => {
                threshold = other
                    .trim_start_matches("--threshold=")
                    .parse()
                    .unwrap_or(DEFAULT_THRESHOLD);
            }
            _ => {}
        }
        i += 1;
    }

    eprintln!("Running benches...");
    let Some(current) = run_current_benches() else {
        eprintln!("error: no benchmark results were produced by the current run.");
        std::process::exit(1);
    };

    if let Some(path) = write_to {
        write_snapshot(&path, &current);
        println!("Wrote current snapshot to {path}");
        return;
    }

    if baseline_path.is_empty() {
        eprintln!(
            "error: --baseline <path> is required.\n\
             Record one first with: just bench-baseline <label>"
        );
        std::process::exit(2);
    }

    let Some(baseline) = read_snapshot(&baseline_path) else {
        eprintln!("error: could not read baseline snapshot {baseline_path}");
        std::process::exit(1);
    };

    compare(&baseline, &current, threshold);
}

/// Run every bench crate and scrape `{crate/bench id: throughput}`.
fn run_current_benches() -> Option<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for crate_name in BENCH_CRATES {
        eprintln!("  {crate_name} ...");
        let mut cmd = std::process::Command::new("cargo");
        cmd.args(["bench", "-p", crate_name, "--", "--quiet"]);
        if let Ok(output) = cmd.output() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            for (id, thrpt) in parse_benches(&stderr, &stdout) {
                out.insert(format!("{crate_name}/{id}"), thrpt);
            }
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

// Criterion's output is parsed by [`tpt_kinetix_test_utils::bench_parse`],
// shared with `bench_report` and `bench_baseline` so the three cannot drift
// (the previous per-example copy here missed duration-only benchmarks such as
// `av1_encode`, silently excluding them from the comparison).

/// Read a snapshot JSON (as written by `bench_baseline`) into a flat
/// `{crate/bench id: throughput}` map.
fn read_snapshot(path: &str) -> Option<BTreeMap<String, String>> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    let results = value.get("results")?.as_object()?;
    let mut out = BTreeMap::new();
    for (crate_name, benches) in results {
        let Some(benches) = benches.as_object() else {
            continue;
        };
        for (id, thrpt) in benches {
            if let Some(s) = thrpt.as_str() {
                out.insert(format!("{crate_name}/{id}"), s.to_string());
            }
        }
    }
    Some(out)
}

fn write_snapshot(path: &str, results: &BTreeMap<String, String>) {
    // Group by crate so the file matches `bench_baseline`'s shape.
    let mut by_crate: BTreeMap<String, serde_json::Map<String, Value>> = BTreeMap::new();
    for (id, thrpt) in results {
        let (crate_name, bench) = id.split_once('/').unwrap_or(("unknown", id.as_str()));
        by_crate
            .entry(crate_name.to_string())
            .or_default()
            .insert(bench.to_string(), Value::String(thrpt.clone()));
    }
    let mut results = serde_json::Map::new();
    for (crate_name, benches) in by_crate {
        results.insert(crate_name, Value::Object(benches));
    }
    let mut root = serde_json::Map::new();
    root.insert("results".to_string(), Value::Object(results));
    if let Some(parent) = std::path::Path::new(path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let json =
        serde_json::to_string_pretty(&Value::Object(root)).unwrap_or_else(|_| "{}".to_string());
    if let Err(e) = std::fs::write(path, format!("{json}\n")) {
        eprintln!("error: could not write {path}: {e}");
        std::process::exit(1);
    }
}

/// Split `"10.418 Melem/s"` into `(10.418, "Melem/s")`, and report whether the
/// unit is a **duration** (lower is better) or a **rate** (higher is better).
fn parse_measurement(s: &str) -> Option<(f64, String, bool)> {
    let mut parts = s.split_whitespace();
    let value: f64 = parts.next()?.parse().ok()?;
    let unit = parts.next().unwrap_or("").to_string();
    // Time units: the mean *duration*, so a *larger* value is a regression.
    // Rates (`/s`) go the other way.
    let is_duration = matches!(unit.as_str(), "ns" | "us" | "ms" | "s")
        || unit.ends_with("sec")
        || unit.ends_with("secs");
    Some((value, unit, is_duration))
}

/// Normalise a value into base units so `10.4 Melem/s` and `10400 Kelem/s`
/// compare correctly. Byte-rate units are binary (1 KiB = 1024 B) because
/// Criterion formats them with the `KiB`/`MiB`/`GiB` prefixes. Time units are
/// decimal (1 ms = 1e-6 s, 1 us = 1e-9 s).
fn to_base(value: f64, unit: &str) -> f64 {
    let kib = 1024.0;
    let factor = match unit {
        "ns" => 1e-9,
        "us" => 1e-6,
        "ms" => 1e-3,
        "s" => 1.0,
        "elem/s" | "elem" | "B/s" => 1.0,
        "Kelem/s" | "Kelem" => 1e3,
        "Melem/s" | "Melem" => 1e6,
        "Gelem/s" | "Gelem" => 1e9,
        "KiB/s" => kib,
        "MiB/s" => kib * kib,
        "GiB/s" => kib * kib * kib,
        // Unknown unit: treat the numeric value as already-base so the
        // comparison still runs, at the cost of a possible unit mismatch.
        _ => 1.0,
    };
    value * factor
}

/// Whether two unit strings denote the same dimension, so a codec's `Melem/s`
/// is never compared against the bitstream bench's `MiB/s`.
fn same_dimension(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    /// The dimension family a unit belongs to: `0` when unrecognised.
    fn family(u: &str) -> u8 {
        // Byte rates: `B/s`, `KiB/s`, `MiB/s`, `GiB/s`. Check the binary
        // prefixes first so `MiB/s` is not mistaken for an element unit.
        if u.ends_with("iB/s") || u.ends_with("B/s") {
            return 2;
        }
        // Element counts: pixels, samples, slices, points.
        if u.ends_with("elem/s") || u.ends_with("elem") {
            return 1;
        }
        // Durations.
        if matches!(u, "ns" | "us" | "ms" | "s") {
            return 3;
        }
        0
    }
    let fa = family(a);
    let fb = family(b);
    // An unrecognised unit on either side must not veto the comparison.
    fa == fb || fa == 0 || fb == 0
}

fn compare(
    baseline: &BTreeMap<String, String>,
    current: &BTreeMap<String, String>,
    threshold: f64,
) {
    println!();
    println!("# Bench comparison (regression threshold: -{threshold:.1}%)");
    println!();

    let mut regressions: Vec<(String, f64, String, String)> = Vec::new();
    let mut improvements: Vec<(String, f64, String, String)> = Vec::new();
    let mut compared = 0usize;
    let mut skipped = 0usize;

    for (id, base_str) in baseline {
        let Some(cur_str) = current.get(id) else {
            skipped += 1;
            continue;
        };
        let (Some((bv, bu, b_is_dur)), Some((cv, cu, c_is_dur))) =
            (parse_measurement(base_str), parse_measurement(cur_str))
        else {
            skipped += 1;
            continue;
        };
        if !same_dimension(&bu, &cu) || bv <= 0.0 || b_is_dur != c_is_dur {
            skipped += 1;
            continue;
        }
        compared += 1;
        let base_base = to_base(bv, &bu);
        let cur_base = to_base(cv, &cu);
        // Normalise to a "higher is better" score: invert durations, so a
        // regression is always a negative delta regardless of unit kind.
        let score = |v: f64| if b_is_dur { 1.0 / v } else { v };
        let delta_pct = (score(cur_base) - score(base_base)) / score(base_base) * 100.0;
        if delta_pct < -threshold {
            regressions.push((id.clone(), delta_pct, base_str.clone(), cur_str.clone()));
        } else if delta_pct > threshold {
            improvements.push((id.clone(), delta_pct, base_str.clone(), cur_str.clone()));
        }
    }

    println!(
        "{:<54} {:>9}  {:>14} -> {:<14}",
        "Benchmark", "Delta", "Baseline", "Current"
    );
    println!("{}", "-".repeat(98));
    if regressions.is_empty() && improvements.is_empty() {
        println!("(no benchmark changed by more than the threshold)");
    }
    for (id, pct, base, cur) in &regressions {
        println!(
            "{:<54} {:>8.1}%  {:>14} -> {:<14}  REGRESSION",
            id, pct, base, cur
        );
    }
    for (id, pct, base, cur) in &improvements {
        println!(
            "{:<54} {:>8.1}%  {:>14} -> {:<14}  improved",
            id, pct, base, cur
        );
    }
    println!();
    println!(
        "{compared} compared, {} regressed, {} improved, {skipped} skipped \
         (absent from this run, or unit mismatch).",
        regressions.len(),
        improvements.len()
    );
    println!();

    if !regressions.is_empty() {
        eprintln!(
            "FAIL: {} benchmark(s) regressed by more than {threshold:.1}%.",
            regressions.len()
        );
        std::process::exit(1);
    }
    println!("OK: no regression beyond the threshold.");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_base_normalises_across_units() {
        assert!((to_base(10.4, "Melem/s") - to_base(10_400.0, "Kelem/s")).abs() < 1.0);
        assert!((to_base(1.0, "MiB/s") - to_base(1024.0, "KiB/s")).abs() < 1.0);
        assert!((to_base(1.0, "ms") - to_base(1000.0, "us")).abs() < 1e-9);
    }

    #[test]
    fn parse_measurement_flags_duration_units() {
        let (_, _, is_dur) = parse_measurement("1.3 ms").expect("parse");
        assert!(is_dur);
        let (_, _, is_dur) = parse_measurement("10.4 Melem/s").expect("parse");
        assert!(!is_dur);
    }

    #[test]
    fn slower_duration_is_reported_as_a_regression() {
        // The AV1 benches declare no `Throughput`, so they are stored as
        // durations. A *larger* duration must count as a regression, i.e. the
        // comparison must not blindly treat "bigger number" as an improvement.
        let mut baseline = BTreeMap::new();
        baseline.insert("c/av1_encode".to_string(), "1.00 ms".to_string());
        let mut slower = BTreeMap::new();
        slower.insert("c/av1_encode".to_string(), "1.50 ms".to_string());
        let mut faster = BTreeMap::new();
        faster.insert("c/av1_encode".to_string(), "0.50 ms".to_string());

        // Inverting durations makes the delta the relative *change in speed*: 1.0 ms
        // -> 1.5 ms is a 1/1.5 = 0.667x speedup, i.e. -33.3%.
        let d = delta_pct(&baseline, &slower, "c/av1_encode").expect("comparable");
        assert!((d - (-33.333)).abs() < 0.01, "got {d}");
        // 1.0 ms -> 0.5 ms is a 2x speedup: +100%.
        let d = delta_pct(&baseline, &faster, "c/av1_encode").expect("comparable");
        assert!((d - 100.0).abs() < 0.01, "got {d}");
    }

    #[test]
    fn lower_throughput_is_reported_as_a_regression() {
        let mut baseline = BTreeMap::new();
        baseline.insert("c/decode".to_string(), "10.0 Melem/s".to_string());
        let mut slower = BTreeMap::new();
        slower.insert("c/decode".to_string(), "9.0 Melem/s".to_string());
        assert_eq!(delta_pct(&baseline, &slower, "c/decode"), Some(-10.0));
    }

    /// The signed percentage a benchmark moved, as `compare` computes it.
    fn delta_pct(
        baseline: &BTreeMap<String, String>,
        current: &BTreeMap<String, String>,
        id: &str,
    ) -> Option<f64> {
        let (bv, bu, b_is_dur) = parse_measurement(baseline.get(id)?)?;
        let (cv, cu, c_is_dur) = parse_measurement(current.get(id)?)?;
        if !same_dimension(&bu, &cu) || bv <= 0.0 || b_is_dur != c_is_dur {
            return None;
        }
        let base_base = to_base(bv, &bu);
        let cur_base = to_base(cv, &cu);
        let score = |v: f64| if b_is_dur { 1.0 / v } else { v };
        Some((score(cur_base) - score(base_base)) / score(base_base) * 100.0)
    }

    #[test]
    fn same_dimension_separates_pixels_from_bytes() {
        assert!(same_dimension("Melem/s", "Kelem/s"));
        assert!(same_dimension("Melem/s", "Melem/s"));
        assert!(!same_dimension("Melem/s", "MiB/s"));
        // An unrecognised unit must not veto the comparison.
        assert!(same_dimension("Melem/s", "widgets"));
    }

    #[test]
    fn read_snapshot_flattens_the_baseline_json() {
        let json = r#"{
  "label": "x",
  "results": {
    "tpt-kinetix-lean": {
      "lean_320x240/encode": "466.46 Kelem/s"
    },
    "tpt-kinetix-bitstream": {
      "bitstream_rans/encode_static": "100.49 MiB/s"
    }
  }
}
"#;
        let path = std::env::temp_dir().join("tpt_bench_compare_snapshot_test.json");
        std::fs::write(&path, json).expect("write temp snapshot");
        let parsed = read_snapshot(path.to_str().unwrap()).expect("parse snapshot");
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            parsed
                .get("tpt-kinetix-lean/lean_320x240/encode")
                .map(String::as_str),
            Some("466.46 Kelem/s")
        );
        assert_eq!(
            parsed
                .get("tpt-kinetix-bitstream/bitstream_rans/encode_static")
                .map(String::as_str),
            Some("100.49 MiB/s")
        );
    }

    #[test]
    fn identical_snapshots_report_no_regression() {
        let mut bench = BTreeMap::new();
        bench.insert("c/bench".to_string(), "10.0 Melem/s".to_string());
        compare(&bench, &bench, 5.0);
        // `compare` exits non-zero only on a regression; reaching here is the
        // assertion that no regression was found.
    }
}
