//! Run every Criterion bench in the workspace and print a **single consolidated
//! table** of mean throughput across all codecs (Phase 0 of `todo-perf.md`).
//!
//! Criterion's HTML reports are per-bench; this tool scrapes the `thrpt:` lines
//! out of `cargo bench` stdout and prints one row per benchmark so codecs can be
//! compared at a glance. Thin wrapper so `just bench-report` has a single entry
//! point; the heavy lifting is still `cargo bench`.
//!
//! Usage: `cargo run -p tpt-kinetix-test-utils --example bench_report
//!         [--release] [crate ...]` (defaults to every crate with a bench)

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

/// One scraped benchmark result.
struct Row {
    /// Full benchmark id, e.g. `lean_1920x1080/encode`.
    id: String,
    /// Criterion's mean throughput string, e.g. `10.418 Melem/s`.
    throughput: String,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let release = args.iter().any(|a| a == "--release");
    let positional: Vec<String> = args.into_iter().filter(|a| a != "--release").collect();
    let crates: Vec<String> = if positional.is_empty() {
        BENCH_CRATES.iter().map(|s| s.to_string()).collect()
    } else {
        positional
    };

    let mut rows: Vec<(String, Vec<Row>)> = Vec::new();

    for crate_name in &crates {
        let mut cmd = Command::new("cargo");
        cmd.args(["bench", "-p", crate_name, "--", "--quiet"]);
        if release {
            cmd.args(["--profile", "release"]);
        }
        let output = cmd.output();
        match output {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                let stderr = String::from_utf8_lossy(&o.stderr);
                // Criterion writes `Benchmarking <id>` headers to stderr and the
                // `thrpt:` result lines to stdout, so parse both streams and
                // zip them; a single concatenated stream pairs nothing up.
                let mut crate_rows = Vec::new();
                for (id, thrpt) in zip_criterion(&stderr, &stdout) {
                    crate_rows.push(Row {
                        id,
                        throughput: thrpt,
                    });
                }
                if !o.status.success() && crate_rows.is_empty() {
                    eprintln!("  (cargo bench for {crate_name} exited with {})", o.status);
                }
                if !crate_rows.is_empty() {
                    rows.push((crate_name.clone(), crate_rows));
                }
            }
            Err(e) => eprintln!("  failed to run cargo bench for {crate_name}: {e}"),
        }
    }

    print_table(&rows);
}

/// Pair Criterion's `Benchmarking <id>` headers (stderr) with its `thrpt:`
/// result lines (stdout), in emission order.
fn zip_criterion(stderr: &str, stdout: &str) -> Vec<(String, String)> {
    let mut ids: Vec<String> = Vec::new();
    for line in stderr.lines() {
        if let Some(rest) = line.trim_start().strip_prefix("Benchmarking ") {
            let id = rest.split(':').next().unwrap_or("").trim();
            if !id.is_empty() && !ids.last().is_some_and(|last| last == id) {
                ids.push(id.to_string());
            }
        }
    }

    let mut throughputs: Vec<String> = Vec::new();
    for line in stdout.lines() {
        if let Some(rest) = line.trim_start().strip_prefix("thrpt:") {
            if let Some(mean) = extract_mean(rest) {
                throughputs.push(mean);
            }
        }
    }

    ids.into_iter().zip(throughputs).collect()
}

/// Pull the middle (point-estimate) throughput out of a Criterion `[lo mid hi]`
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
    let mid = numbers.get(1)?;
    if unit.is_empty() {
        return None;
    }
    Some(format!("{mid} {unit}"))
}

fn print_table(rows: &[(String, Vec<Row>)]) {
    let total: usize = rows.iter().map(|(_, r)| r.len()).sum();
    println!();
    println!("# TPT Kinetix — consolidated benchmark report");
    println!();
    println!("Machine: {}", machine_summary());
    println!();
    if total == 0 {
        println!("No benchmark results were produced (was `cargo bench` run to completion?).");
        return;
    }
    println!("{:<56} {:>24}", "Benchmark", "Mean throughput");
    println!("{}", "-".repeat(82));
    for (crate_name, crate_rows) in rows {
        println!("[{}]", crate_name);
        for r in crate_rows {
            println!("  {:<54} {:>24}", r.id, r.throughput);
        }
    }
    println!();
}

/// A short machine identifier for the report header (CPU + core count).
fn machine_summary() -> String {
    if let Ok(s) = std::process::Command::new("cmd")
        .args(["/C", "echo %PROCESSOR_IDENTIFIER%"])
        .output()
    {
        let cpu = String::from_utf8_lossy(&s.stdout).trim().to_string();
        if !cpu.is_empty() {
            return cpu;
        }
    }
    "unknown".to_string()
}
