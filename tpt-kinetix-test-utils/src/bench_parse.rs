//! Scrape Criterion bench results out of `cargo bench` output.
//!
//! One implementation for the three tools that need it (`bench_report`,
//! `bench_baseline`, `bench_compare`), so their parsers cannot drift apart —
//! they already had: the per-example copies disagreed on whether benchmarks
//! that declare no `Throughput` (e.g. `av1_encode`, which only prints a
//! `time:` line) are captured at all.
//!
//! Criterion writes its progress headers (`Benchmarking <id>: ...`) to
//! **stderr** and its result lines (`time:` / `thrpt:`) to **stdout**, so the
//! two streams must be parsed separately and zipped in emission order —
//! concatenating them would put every header *after* every result and pair
//! nothing up.

use std::collections::BTreeMap;

/// Parse Criterion's output into `{benchmark id: mean value}`.
///
/// The value is the middle (point) estimate of the `thrpt:` line when the
/// benchmark's group declared a `criterion::Throughput`, otherwise the
/// `time:` line, so duration-only benchmarks still appear in a scraped table.
pub fn parse_benches(stderr: &str, stdout: &str) -> BTreeMap<String, String> {
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
pub fn bench_ids(stderr: &str) -> Vec<String> {
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
pub fn result_values(stdout: &str, prefix: &str) -> Vec<String> {
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
pub fn extract_mean(rest: &str) -> Option<String> {
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
    fn result_values_matches_markers_that_are_not_line_prefixes() {
        // The `time:` marker is preceded by the benchmark id on the same line.
        let stdout = "lean_320x240/encode     time:   [164.08 ms 164.65 ms 165.65 ms]\n";
        assert_eq!(
            result_values(stdout, "time:").first().map(String::as_str),
            Some("164.65 ms")
        );
    }
}
