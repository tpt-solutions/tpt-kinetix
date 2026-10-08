# TPT Kinetix — contributor task runner
#
# Install `just` (https://github.com/casey/just), then run `just <recipe>`.
# `just` (no args) lists all recipes.

# List available recipes.
default:
    @just --list

# Format all crates.
fmt:
    cargo fmt --all

# Check formatting without modifying files (CI parity).
fmt-check:
    cargo fmt --all --check

# Lint the whole workspace, denying warnings (CI parity).
clippy:
    cargo clippy --workspace --all-targets -- -D warnings

# Build the whole workspace.
build:
    cargo build --workspace

# Build the published engine crates (pipeline + CLI); neither depends on the
# unpublished, patent-encumbered out-kinetix-h264. See PATENTS.md.
build-royalty-free:
    cargo build -p tpt-kinetix-pipeline -p tpt-kinetix-cli

# Fail if a published crate can reach a patent-encumbered `out-*` crate (see PATENTS.md).
check-publish-safe:
    python tools/check_no_encumbered.py

# Run the whole test suite. Prefers cargo-nextest when installed.
test:
    cargo nextest run --workspace --lib --bins --tests || cargo test --workspace

# Run doctests (nextest does not run them).
test-doc:
    cargo test --workspace --doc

# License / advisory / duplicate-dependency checks.
deny:
    cargo deny check

# Build API docs (denies rustdoc warnings, CI parity).
doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

# Code coverage report (requires cargo-llvm-cov).
coverage:
    cargo llvm-cov --workspace --lcov --output-path lcov.info

# Compile all fuzz targets (requires nightly + cargo-fuzz).
fuzz-build:
    cd tpt-kinetix-demux && cargo fuzz build fuzz_mp4_box && cargo fuzz build fuzz_mkv_ebml
    cd tpt-kinetix-av1 && cargo fuzz build fuzz_obu_parse && cargo fuzz build fuzz_av1_frame
    cd out-kinetix-h264 && cargo fuzz build fuzz_h264_nal
    cd tpt-kinetix-vp9 && cargo fuzz build fuzz_vp9_frame
    cd tpt-kinetix-stream && cargo fuzz build fuzz_rtmp_chunk && cargo fuzz build fuzz_rtmp_amf && cargo fuzz build fuzz_rtmp_flv && cargo fuzz build fuzz_hls_playlist

# Run a single fuzz target for N seconds: `just fuzz tpt-kinetix-demux fuzz_mp4_box 60`
fuzz crate target seconds="60":
    cd {{crate}} && cargo fuzz run {{target}} -- -max_total_time={{seconds}}

# Build the browser wasm demo and serve it locally (requires wasm-pack; see web-demo/README.md).
wasm-demo:
    cd tpt-kinetix-demux && wasm-pack build --target web --out-dir ../web-demo/pkg -- --features wasm
    cd web-demo && python3 -m http.server 8787 || python -m http.server 8787

# Cross-check `// verify-tables:`-annotated spec tables against pinned upstream
# C source (requires network access to fetch the pinned commit; see
# docs/adding-a-codec.md "Extracting spec tables").
verify-tables:
    cargo run -p tpt-kinetix-kg -- verify-tables out-kinetix-h264/src/cabac_tables.rs

# The full local pre-commit gate: format check, lint, build, test, spec tables.
check: fmt-check check-publish-safe clippy build test verify-tables
    @echo "All local checks passed."

# One-shot contributor bootstrap: install the tools CI expects.
setup:
    rustup component add rustfmt clippy
    cargo install cargo-nextest --locked || true
    cargo install cargo-deny --locked || true
    cargo install cargo-llvm-cov --locked || true
    @echo "For fuzzing: rustup toolchain install nightly && cargo install cargo-fuzz --locked"

# Print each decoder's capabilities (machine-readable status).
conformance:
    cargo run -p tpt-kinetix-test-utils --example codec_status

# Assert every decoder is pixel-exact. Currently a non-passing check
# (h264/av1 CABAC paths are not yet pixel-exact); becomes the real gate
# once those decoders reach pixel_exact.
conformance-strict:
    cargo run -p tpt-kinetix-test-utils --example codec_status -- --strict

# Generate the ad-hoc test-src corpus, then decode/diff every file in it.
corpus-check:
    cargo run -p out-kinetix-h264 --example gen_corpus
    cargo run -p out-kinetix-h264 --example corpus_check

# AV1 differential-trace harness: decode a corpus entry (default `mandelbrot`,
# or `--all`) with both dav1d and Av1Decoder, diff pixels, and report the
# first divergence with its nearest symbol-trace context (see
# tpt-kinetix-test-utils/examples/av1_symbol_trace_diff.rs and todo-av1.md
# Phase G.0). Requires ffmpeg (with libdav1d) or a standalone dav1d on PATH.
av1-trace-diff *ARGS="mandelbrot":
    cargo run -p tpt-kinetix-test-utils --example av1_symbol_trace_diff -- {{ARGS}}

# Block-interior-only diff: compare NOFILTER-Kinetix vs FILTERED-dav1d at only
# pixels that deblock/CDEF cannot reach (≥4 from any 8×8 boundary on luma).
# Isolates reconstruction bugs from the filter confound. Requires ffmpeg
# (with libdav1d) or a standalone dav1d on PATH.
av1-interior-diff *ARGS="testsrc":
    cargo run -p tpt-kinetix-test-utils --example av1_interior_diff -- {{ARGS}}

# Re-generate the independent Python oracle's default CDF tables / constants
# from the Rust crate (tools/av1_oracle/cdf_tables_gen.py). Run after any change
# to entropy_cdf.rs / coeff_tables.rs.
av1-oracle-regen:
    cargo run -q -p tpt-kinetix-av1 --example dump_oracle_tables > tools/av1_oracle/cdf_tables_gen.py

# Validate the independent Python AV1 coeff oracle against the Rust crate's own
# golden vectors (tpt-kinetix-av1/src/{entropy,coeff}.rs unit tests).
av1-oracle-validate:
    {{ if os() == "windows" { "python" } else { "python3" } }} tools/av1_oracle/validate.py

# Capture a single transform block's raw tile bytes + TxBlockCtx + Kinetix's
# own symbol slice into av1_capture.json, then re-decode it independently with
# the Python oracle and diff symbol-by-symbol (Phase G.0 item 1: the independent
# coeff oracle bridge). BLOCK is "plane:px_x:px_y" of the target transform block.
# The capture feeds the differential harness, which decodes the corpus entry
# named by ENTRY (default mandelbrot).
# NOTE: the oracle re-seeds neighbour level/dc context from the capture but uses
# fresh (base_q-seeded) CDF tables, so the diff already isolates context- and
# table-value bugs; a residual divergence whose only cause is mid-tile CDF
# adaptation is not yet separated out (capturing adapted CDFs is a future step).
av1-capture BLOCK ENTRY="mandelbrot":
    set -e
    KINETIX_AV1_CAPTURE={{BLOCK}} cargo run -q -p tpt-kinetix-test-utils --example av1_symbol_trace_diff -- {{ENTRY}}
    {{ if os() == "windows" { "python" } else { "python3" } }} tools/av1_oracle/diff_block.py av1_capture.json

# The full "Part 1 oracle": capture a corpus entry's whole tile (base CDFs +
# every symbol + block markers + frame params via KINETIX_AV1_CAPTURE_TILE),
# then re-decode the entire tile syntax (read_lr -> partition -> mode_info ->
# coeffs) independently in Python and diff every symbol against Kinetix's. A
# clean "TRACE MATCHES" means the entropy path is correct and any pixel
# divergence is in reconstruction (2026-08-27: all 5 corpus entries match).
av1-oracle-tile ENTRY="testsrc":
    set -e
    KINETIX_AV1_CAPTURE_TILE=1 cargo run -q -p tpt-kinetix-test-utils --example av1_symbol_trace_diff -- {{ENTRY}}
    {{ if os() == "windows" { "python" } else { "python3" } }} tools/av1_oracle/intra_decode.py av1_tile_trace.json

# Fetch the curated ITU-T H.264.1 conformance bitstream subset (~1 GB, git-ignored)
# into out-kinetix-h264/tests/fixtures/itu/. The `itu_conformance` test then
# decodes each clip and compares byte-exact against the standard's reference YUV.
# Re-running skips clips already present. CLIPS="A B" or GROUP=frext narrows it.
fetch-h264-conformance:
    bash tools/fetch-h264-conformance.sh

# Every crate that ships a Criterion bench (Phase 0 of todo-perf.md).
BENCH_CRATES := "out-kinetix-h264 tpt-kinetix-av1 tpt-kinetix-vp9 tpt-kinetix-bitstream tpt-kinetix-lean tpt-kinetix-lossless tpt-kinetix-realtime tpt-kinetix-screen tpt-kinetix-vision tpt-kinetix-face tpt-kinetix-volumetric tpt-kinetix-demux tpt-kinetix-mux tpt-kinetix-pipeline"

# Run every Criterion bench in the workspace.
bench *FLAGS:
    cargo bench -p out-kinetix-h264 -p tpt-kinetix-av1 -p tpt-kinetix-vp9 -p tpt-kinetix-bitstream -p tpt-kinetix-lean -p tpt-kinetix-lossless -p tpt-kinetix-realtime -p tpt-kinetix-screen -p tpt-kinetix-vision -p tpt-kinetix-face -p tpt-kinetix-volumetric -p tpt-kinetix-demux -p tpt-kinetix-mux -p tpt-kinetix-pipeline {{FLAGS}}

# Run the benches and print a consolidated timing report.
bench-report:
    cargo run -p tpt-kinetix-test-utils --example bench_report -- --release

# Record machine + toolchain details and commit a baseline JSON snapshot of
# every bench target, plus regenerate docs/PERFORMANCE.md from it.
# Use `just bench-baseline <label>` to label the snapshot (default: today's date).
bench-baseline LABEL="":
    cargo run --release -p tpt-kinetix-test-utils --example bench_baseline -- --label "{{LABEL}}"

# Compare the current bench run against a committed baseline snapshot and fail
# on a throughput regression beyond THRESHOLD percent (default 5).
# Usage: just bench-compare [BASELINE_JSON] [THRESHOLD]
bench-compare BASELINE="docs/perf/baseline-2026-10-03.json" THRESHOLD="5":
    cargo run --release -p tpt-kinetix-test-utils --example bench_compare -- --baseline "{{BASELINE}}" --threshold {{THRESHOLD}}

# Kinetix vs ffmpeg comparison (todo-perf.md Phase 1): verifies Kinetix's
# decoded planes byte-exact against the reference decoder BEFORE timing, then
# compares decode speed (ffmpeg -threads 1 and default), AV1 encode (vs libaom),
# the original codecs vs their closest standard references, and the CLI
# end-to-end transcode. Writes docs/perf/ffmpeg-compare-<label>.json and the
# marked section of docs/PERFORMANCE.md. Optional flags: --decode --encode-av1
# --originals --e2e (single sections, for iteration), --quick, --label <l>.
# Requires ffmpeg with libdav1d/libvpx/libaom/libx264 on PATH.
bench-ffmpeg *FLAGS:
    cargo run --release -p tpt-kinetix-test-utils --example ffmpeg_compare -- {{FLAGS}}

# I/O-layer evidence vs ffprobe (todo-io.md M6): probe startup + peak RSS, and a
# mutated-MP4 hostile-input corpus (crash/hang counts). Flags: --startup --hostile
# --seed <mp4> --variants <n> --runs <n>. Needs ffprobe; seed from `just bench-ffmpeg --e2e`.
bench-io *FLAGS:
    cargo run --release -p tpt-kinetix-test-utils --example io_compare -- {{FLAGS}}

# Fetch the FFmpeg FATE AV1 samples (small) into fixtures/av1-fate/.
fetch-av1-fate:
    bash tools/fetch-av1-fate.sh

# Run every conformance suite and regenerate docs/CONFORMANCE.md, conformance.json
# and docs/badges/*.json. Needs ffmpeg (libdav1d, libvpx, libaom), plus
# `just fetch-av1-fate` and `just fetch-h264-conformance` for full coverage.
conformance-report:
    KINETIX_AV1_FATE_DIR="${KINETIX_AV1_FATE_DIR:-fixtures/av1-fate}" cargo run --release -p tpt-kinetix-test-utils --example conformance_report

# WASM packager from Node over async range reads; output must equal the native CLI byte for byte.
wasm-package-test:
    bash tools/wasm-package-test.sh

# The Worker handler (examples/edge-worker) in Node vs the native packager, byte for byte.
edge-worker-test:
    bash tools/edge-worker-test.sh

# hls.js and dash.js playing the just-in-time output in headless Chrome.
browser-package-test:
    bash tools/browser-package-test.sh

# Publish AV1/VP9 + Opus live and play it with hls.js in headless Chrome mid-stream.
live-browser-test:
    bash tools/live-browser-test.sh

# Real-browser publish: headless Chrome -> MediaRecorder -> WebSocket ingest -> live HLS.
browser-publish-test:
    bash tools/browser-publish-test.sh

# True glass-to-glass latency of live HLS (parts on/off) in headless Chrome + hls.js.
latency-test:
    bash tools/latency-test.sh
