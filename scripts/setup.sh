#!/usr/bin/env bash
# One-command contributor setup for TPT Kinetix.
#
# Installs the extra cargo subcommands CI relies on, adds the wasm32 target used
# by the WASM CI job, and runs a first sanity check. Run from the repo root:
#
#   ./scripts/setup.sh
#
# Requires: rustup + cargo on PATH (the repo's rust-toolchain.toml pins MSRV).

set -euo pipefail

echo "==> Installing cargo subcommands used by CI"
cargo install cargo-deny --locked || true
cargo install cargo-nextest --locked || true
cargo install cargo-llvm-cov --locked || true
# cargo-fuzz needs a nightly toolchain
rustup toolchain install nightly --profile minimal || true
cargo +nightly install cargo-fuzz --locked || true

echo "==> Adding wasm32 target"
rustup target add wasm32-unknown-unknown || true

echo "==> Running sanity check (fmt + clippy + test)"
cargo fmt --all -- --check || true
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
echo "Done. See README + docs/ for workflow details."
