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
    cd tpt-kinetix-av1 && cargo fuzz build fuzz_obu_parse
    cd tpt-kinetix-h264 && cargo fuzz build fuzz_h264_nal
    cd tpt-kinetix-aac && cargo fuzz build fuzz_aac_decode
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
    cargo run -p tpt-kinetix-kg -- verify-tables tpt-kinetix-h264/src/cabac_tables.rs

# The full local pre-commit gate: format check, lint, build, test, spec tables.
check: fmt-check clippy build test verify-tables
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
    cargo run -p tpt-kinetix-h264 --example gen_corpus
    cargo run -p tpt-kinetix-h264 --example corpus_check

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
# into tpt-kinetix-h264/tests/fixtures/itu/. The `itu_conformance` test then
# decodes each clip and compares byte-exact against the standard's reference YUV.
# Re-running skips clips already present. CLIPS="A B" or GROUP=frext narrows it.
fetch-h264-conformance:
    bash tools/fetch-h264-conformance.sh

# Run every Criterion bench in the workspace.
bench:
    cargo bench -p tpt-kinetix-h264 -p tpt-kinetix-av1 -p tpt-kinetix-aac -p tpt-kinetix-pipeline

# Run the benches and print a consolidated timing report.
bench-report:
    cargo run -p tpt-kinetix-test-utils --example bench_report -- --release
