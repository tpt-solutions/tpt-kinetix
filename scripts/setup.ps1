# One-command contributor setup for TPT Kinetix (PowerShell).
#
# Installs the extra cargo subcommands CI relies on, adds the wasm32 target used
# by the WASM CI job, and runs a first sanity check. Run from the repo root:
#
#   pwsh ./scripts/setup.ps1
#
# Requires: rustup + cargo on PATH (the repo's rust-toolchain.toml pins MSRV).

$ErrorActionPreference = 'Stop'

Write-Host '==> Installing cargo subcommands used by CI'
cargo install cargo-deny --locked
cargo install cargo-nextest --locked
cargo install cargo-llvm-cov --locked
# cargo-fuzz needs a nightly toolchain
rustup toolchain install nightly --profile minimal
cargo +nightly install cargo-fuzz --locked

Write-Host '==> Adding wasm32 target'
rustup target add wasm32-unknown-unknown

Write-Host '==> Running sanity check (fmt + clippy + test)'
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
Write-Host 'Done. See README + docs/ for workflow details.'
