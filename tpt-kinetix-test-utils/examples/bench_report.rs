//! Run every Criterion bench in the workspace and print a consolidated report
//! of the resulting mean timings. Thin wrapper so `just bench-report` has a
//! single entry point; the heavy lifting is still `cargo bench`.
//!
//! Usage: `cargo run -p tpt-kinetix-test-utils --example bench_report
//!         [crate ...]` (defaults to the known bench crates)

use std::process::Command;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let release = args.iter().any(|a| a == "--release");
    let crates: Vec<String> = if args.is_empty() || release && args.len() == 1 {
        vec![
            "tpt-kinetix-h264".to_string(),
            "tpt-kinetix-av1".to_string(),
            "tpt-kinetix-pipeline".to_string(),
        ]
    } else {
        args.into_iter().filter(|a| a != "--release").collect()
    };

    for crate_name in &crates {
        println!("=== {crate_name} ===");
        let mut cmd = Command::new("cargo");
        cmd.args(["bench", "-p", crate_name, "--", "--quiet"]);
        if release {
            cmd.arg("--profile").arg("release");
        }
        let output = cmd.output();
        match output {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                let stderr = String::from_utf8_lossy(&o.stderr);
                for line in stdout.lines().chain(stderr.lines()) {
                    if line.contains("time:") {
                        println!("  {line}");
                    }
                }
                if !o.status.success() {
                    eprintln!("  (cargo bench exited with status {})", o.status);
                }
            }
            Err(e) => eprintln!("  failed to run cargo bench for {crate_name}: {e}"),
        }
    }
}
