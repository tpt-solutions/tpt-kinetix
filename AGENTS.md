# AGENTS.md — TPT Kinetix

Rust Cargo **workspace** for a media processing engine (demux/mux, H.264/AV1/AAC,
RTMP/HLS streaming, a transcoding pipeline). Each codec/format is its own crate
with a public API. Read `README.md` for the architecture diagram and crate map.

## Toolchain & prerequisites

- `rust-toolchain.toml` pins **stable 1.82.0** (also the declared MSRV in `Cargo.toml`
  `rust-version` and `clippy.toml`). CI has a dedicated **msrv** job that fails if the
  workspace stops building on 1.82 — do not raise MSRV casually.
- Extra cargo subcommands CI expects: `cargo-nextest`, `cargo-deny`, `cargo-llvm-cov`.
  Fuzzing also needs `cargo-fuzz` **+ nightly**. Install all of them with
  `just setup` (or `scripts/setup.ps1` / `scripts/setup.sh`).
- Fuzz crates live under each crate's `fuzz/` dir and are **excluded** from the
  workspace (`Cargo.toml` `exclude`). They do not build with normal `cargo` commands.

## Canonical commands (`just`)

A `justfile` is the source of truth for contributor commands. Install `just`
(https://github.com/casey/just); `just` with no args lists recipes.

| Task | What it runs |
|------|--------------|
| `just check` | `fmt-check → clippy → build → test` (local pre-PR gate) |
| `just fmt` / `just fmt-check` | `cargo fmt --all` (+ `--check`) |
| `just clippy` | `cargo clippy --workspace --all-targets -- -D warnings` |
| `just test` | `cargo nextest run --workspace --lib --bins --tests` (falls back to `cargo test` if nextest missing) |
| `just test-doc` | `cargo test --workspace --doc` (nextest cannot run doctests) |
| `just deny` | `cargo deny check` (licenses/advisories/duplicates) |
| `just doc` | `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` |
| `just fuzz <crate> <target> <seconds>` | run one fuzz target for N seconds |
| `just conformance` | prints every decoder's `DecoderCapabilities` (`--strict` asserts pixel-exact) |
| `just wasm-demo` | builds the wasm web-demo (`web-demo/`) and serves it |

CI mirrors these as separate jobs: build / test (nextest) / clippy / fmt / doc /
wasm / msrv / deny / fuzz-check (compile only) / conformance.

## Testing gotchas

- **nextest vs doctests:** CI runs `cargo nextest run --workspace --lib --bins --tests`,
  which does **not** run doctests. Run `just test-doc` (or `cargo test --workspace --doc`)
  to cover `///` doc examples.
- **Single crate:** `cargo test -p tpt-kinetix-demux` (or `-p <crate>`).
- **Conformance tests are ffmpeg-gated.** In CI they only run where `ffmpeg` is installed
  (the `conformance` job installs it on Ubuntu). Locally use `just conformance`.
  The `--strict` (pixel-exact) conformance assertion is **currently non-blocking**
  (`continue-on-error: true` in CI) because AV1, AAC, Lean, and Vision decoders are
  **not pixel-exact yet** (H.264 reports `pixel_exact: true`).
- **ffmpeg-dependent `dbg_*`/conformance tests must skip, not panic.** `tpt-kinetix-h264/tests/dbg_*.rs`
  and `examples/dbg_*.rs` (and the `conformance_*`/`phase_c_*` test files) shell out to `ffmpeg`
  to synthesize reference clips/YUV. Gate them on a `ffmpeg_available()` helper (either the shared
  `tpt_kinetix_test_utils::reference::ffmpeg_available`, or a local
  `Command::new("ffmpeg").arg("-version").output().map(|o| o.status.success()).unwrap_or(false)`)
  and `return` early (or have the generator return `Option` and use `else { eprintln!(...); return; }`)
  so they **skip** when `ffmpeg` is absent from `PATH`. Never `.unwrap()` a
  `Command::new("ffmpeg").output()` result — when `ffmpeg` is missing this panics with
  `Os { kind: NotFound, … "No such file or directory" }` and fails the whole test run.
- **Decoders are incomplete by design.** H.264 CAVLC I/P/B and CABAC I slices are bit-exact vs
  `ffmpeg` for a supported subset (4:2:0, progressive, 16-px-aligned, no 8x8 transform); CABAC
  P/B, the 8x8 transform/High profile, and interlaced (PAFF/MBAFF) are not yet, so strict mode
  returns `KinetixError::NotPixelExact` for unsupported features. `capabilities().pixel_exact`
  reports `true` for supported H.264 streams. AV1 decode has its CDF-based entropy decoder
  implemented but not yet wired into `decode_tile_group`, so it still emits placeholder frames.
  AAC, Lean, and Vision also report `pixel_exact: false`. `KinetixError::NotPixelExact` under
  strict mode signals the gap either way. CLI `probe` works end-to-end; `transcode`/`stream` are
  still stubs. Don't treat decoder output as correct without checking `capabilities()`.
- **proptest:** `proptest_*.rs` tests under `<crate>/tests/` persist shrunk reproducers in
  committed `.proptest-regressions/` files — keep them.
- Fuzz crashes reproduce in `fuzz/artifacts/<target>/crash-*`; add them to
  `fuzz/corpus/<target>/` as permanent regression cases.

## Lint / format conventions

- `rustfmt.toml` deviates from defaults: `max_width = 100`, `imports_granularity = "Crate"`,
  `group_imports = "StdExternalCrate"`. Use `just fmt` so the project config is honored.
- CI treats **all** clippy warnings and rustdoc warnings as errors (`-D warnings`,
  `RUSTDOCFLAGS="-D warnings"`). A clean `just check` is the bar for a PR.

## Knowledge-graph codegen & new codecs

- `tpt-kinetix-kg` derives Rust scaffolding from reference C source:
  `ingest → graph → analyze → codegen`, or `kg run <c>.c --crate-name … --inject-rayon`.
  See `docs/adding-a-codec.md` and `tpt-kinetix-kg/DEVELOPER.md` before hand-writing a decoder.
- New codec crates start from the `cargo generate` template in `templates/codec-crate`,
  then must be added to `[workspace] members` in `Cargo.toml`.

## Workspace & release structure

- **Shared version:** all published crates use one version number (monorepo style); a breaking
  change to any public API bumps every crate. `release-plz.toml` opens one release PR and
  publishes in dependency order (core → codecs/demux/mux → pipeline → stream → cli).
- `tpt-kinetix-test-utils` is **never published** (`release = false`).
- `tpt-kinetix-aac` is documented in the README but lacks an explicit `publish = true` entry in
  `release-plz.toml`; `tpt-kinetix-lean` is absent from both; `tpt-kinetix-vision` is explicitly
  published in `release-plz.toml` but not shown in the README architecture map. Verify before
  assuming a change to any of them will be released.
- `tpt-kinetix-demux` and `tpt-kinetix-core` build for `wasm32-unknown-unknown`
  (the in-browser `web-demo`).

## Build profile quirks

- `[profile.dev]` uses `opt-level = 1` with `debug = "line-tables-only"` for fast builds.
- `rav1e` and `tree-sitter` are forced to `opt-level = 3` even in dev (they are too slow
  otherwise). Don't "fix" these overrides.

## Branches & CI

- CI runs on pushes to `master`, `claude/**`, `feature/**`, and PRs into `master`.
- `deny.toml` enforces license/advisory policy; adding a dependency must pass `cargo deny check`.

## Other

- `todo.md` (repo root) and `history/`, `scratch_cabac/`, `memory/`, `out.yuv`,
  `p_frame_test_output.txt` are scratch/work artifacts — not documentation or build inputs.
- Repo-local Kilo config: `.kilo/kilo.jsonc` (`snapshot: false`). Project instructions also
  live in `README.md`, `CONTRIBUTING.md`, and `docs/`.
