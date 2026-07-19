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
    cd tpt-kinetix-stream && cargo fuzz build fuzz_rtmp_chunk && cargo fuzz build fuzz_rtmp_amf && cargo fuzz build fuzz_rtmp_flv && cargo fuzz build fuzz_hls_playlist

# Run a single fuzz target for N seconds: `just fuzz tpt-kinetix-demux fuzz_mp4_box 60`
fuzz crate target seconds="60":
    cd {{crate}} && cargo fuzz run {{target}} -- -max_total_time={{seconds}}

# Build the browser wasm demo and serve it locally (requires wasm-pack; see web-demo/README.md).
wasm-demo:
    cd tpt-kinetix-demux && wasm-pack build --target web --out-dir ../web-demo/pkg -- --features wasm
    cd web-demo && python3 -m http.server 8787 || python -m http.server 8787

# The full local pre-commit gate: format check, lint, build, test.
check: fmt-check clippy build test
    @echo "All local checks passed."

# One-shot contributor bootstrap: install the tools CI expects.
setup:
    rustup component add rustfmt clippy
    cargo install cargo-nextest --locked || true
    cargo install cargo-deny --locked || true
    cargo install cargo-llvm-cov --locked || true
    @echo "For fuzzing: rustup toolchain install nightly && cargo install cargo-fuzz --locked"
