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

use tpt_kinetix_test_utils::bench_parse::parse_benches;

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

    // Preserve the `<!-- ffmpeg-compare -->` section that
    // `just bench-ffmpeg` maintains (todo-perf.md Phase 1); regenerating the
    // Phase 0 table must not wipe the Phase 1 comparison.
    let previous = std::fs::read_to_string("docs/PERFORMANCE.md").ok();
    write_file(
        "docs/PERFORMANCE.md",
        &render_markdown(&label, &results, previous.as_deref()),
    );
    eprintln!("Wrote docs/PERFORMANCE.md");
}

// Criterion's output is parsed by [`tpt_kinetix_test_utils::bench_parse`],
// shared with `bench_report` and `bench_compare` so the three cannot drift.

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

/// The marked section `ffmpeg_compare` maintains in PERFORMANCE.md, carried
/// over verbatim when this tool regenerates the Phase 0 baseline table.
pub(crate) const FFMPEG_SECTION_MARKERS: (&str, &str) = (
    "<!-- ffmpeg-compare:start -->",
    "<!-- ffmpeg-compare:end -->",
);

/// Extract the text between (and including) the ffmpeg-compare markers, if the
/// previous PERFORMANCE.md contains a complete pair.
pub(crate) fn ffmpeg_section(previous: Option<&str>) -> String {
    let Some(text) = previous else {
        return String::new();
    };
    let Some(start) = text.find(FFMPEG_SECTION_MARKERS.0) else {
        return String::new();
    };
    let Some(end) = text[start..].find(FFMPEG_SECTION_MARKERS.1) else {
        return String::new();
    };
    let end = start + end + FFMPEG_SECTION_MARKERS.1.len();
    let mut section = text[start..end].to_string();
    section.push('\n');
    section
}

/// The marked section carrying hand-written notes about *this specific*
/// snapshot, carried over verbatim when this tool regenerates PERFORMANCE.md.
///
/// The rest of PERFORMANCE.md is derived from the benchmark run, so anything
/// written by hand into it is lost on the next `just bench-baseline` — which is
/// exactly when it would matter most (a note explaining why an older snapshot
/// is not comparable). These markers make the notes survive regeneration, the
/// same way [`FFMPEG_SECTION_MARKERS`] preserves the ffmpeg comparison.
pub(crate) const NOTES_SECTION_MARKERS: (&str, &str) =
    ("<!-- perf-notes:start -->", "<!-- perf-notes:end -->");

/// Extract the text between (and including) the perf-notes markers, if the
/// previous PERFORMANCE.md contains a complete pair.
pub(crate) fn notes_section(previous: Option<&str>) -> String {
    let Some(text) = previous else {
        return String::new();
    };
    let Some(start) = text.find(NOTES_SECTION_MARKERS.0) else {
        return String::new();
    };
    let Some(end) = text[start..].find(NOTES_SECTION_MARKERS.1) else {
        return String::new();
    };
    let end = start + end + NOTES_SECTION_MARKERS.1.len();
    let mut section = text[start..end].to_string();
    section.push('\n');
    section
}

fn render_markdown(
    label: &str,
    results: &BTreeMap<String, BTreeMap<String, String>>,
    previous: Option<&str>,
) -> String {
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
    s.push_str(&notes_section(previous));
    s.push_str(&ffmpeg_section(previous));
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
    fn today_is_a_well_formed_iso_date() {
        let d = today();
        assert_eq!(d.len(), 10, "{d} should be YYYY-MM-DD");
        assert_eq!(d.as_bytes()[4], b'-');
        assert_eq!(d.as_bytes()[7], b'-');
        assert!(d[..4].chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn markdown_preserves_a_marked_ffmpeg_compare_section() {
        let mut results = BTreeMap::new();
        results.insert(
            "tpt-kinetix-lean".to_string(),
            BTreeMap::from([("lean/decode".to_string(), "1.0 Melem/s".to_string())]),
        );
        let previous =
            "preamble\n<!-- ffmpeg-compare:start -->\nkept\n<!-- ffmpeg-compare:end -->\n";
        let rendered = render_markdown("t", &results, Some(previous));
        assert!(rendered.contains("kept"), "marked section must survive");
        assert!(rendered.starts_with("# TPT Kinetix — performance baseline"));
        assert!(
            !rendered.contains("preamble"),
            "old preamble must be dropped"
        );
    }

    #[test]
    fn markdown_without_markers_renders_without_a_section() {
        let rendered = render_markdown("t", &BTreeMap::new(), None);
        assert!(!rendered.contains("ffmpeg-compare:start"));
    }

    /// The notes section explains why a given snapshot is or is not comparable
    /// to another. It is the one hand-written part of PERFORMANCE.md, and the
    /// rest of the file is rewritten from the benchmark run -- so if this
    /// regresses, the explanation silently disappears on the next re-record,
    /// precisely when someone is about to trust a bad cross-baseline delta.
    #[test]
    fn markdown_preserves_a_marked_notes_section() {
        let previous =
            "preamble\n<!-- perf-notes:start -->\nnot comparable\n<!-- perf-notes:end -->\n";
        let rendered = render_markdown("t", &BTreeMap::new(), Some(previous));
        assert!(rendered.contains("not comparable"), "notes must survive");
        assert!(
            !rendered.contains("preamble"),
            "old preamble must be dropped"
        );
    }

    /// Both carried-over sections must coexist, and the notes must precede the
    /// ffmpeg table so a reader hits the caveat before the numbers.
    #[test]
    fn notes_and_ffmpeg_sections_both_survive_in_order() {
        let previous = "<!-- perf-notes:start -->\nnote text\n<!-- perf-notes:end -->\n\
                         <!-- ffmpeg-compare:start -->\nffmpeg text\n<!-- ffmpeg-compare:end -->\n";
        let rendered = render_markdown("t", &BTreeMap::new(), Some(previous));
        assert!(rendered.contains("note text"));
        assert!(rendered.contains("ffmpeg text"));
        let n = rendered.find("note text").expect("notes present");
        let f = rendered
            .find("ffmpeg text")
            .expect("ffmpeg section present");
        assert!(n < f, "notes must be rendered before the ffmpeg table");
    }

    /// A half-written notes section (start without end) must not be carried
    /// over, or the truncation would be treated as content.
    #[test]
    fn an_unterminated_notes_section_is_dropped() {
        let previous = "<!-- perf-notes:start -->\ntruncated text\n";
        let rendered = render_markdown("t", &BTreeMap::new(), Some(previous));
        assert!(!rendered.contains("truncated text"));
    }
}
