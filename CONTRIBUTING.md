# Contributing to TPT Kinetix

Thank you for your interest in contributing! This document explains how to get
the project building, how to run the various test layers, and what the PR
checklist looks like before a merge.

---

## Development environment setup

### Prerequisites

| Tool | Minimum version | Install |
|------|-----------------|---------|
| Rust toolchain | 1.82 (stable) | `rustup toolchain install stable` |
| Rust nightly | any recent | `rustup toolchain install nightly` |
| cargo-fuzz | latest | `cargo install cargo-fuzz --locked` |
| cargo-llvm-cov | latest | `cargo install cargo-llvm-cov --locked` |
| cargo-deny | latest | `cargo install cargo-deny --locked` |

Clone and verify the build:

```sh
git clone https://github.com/tpt-solutions/tpt-kinetix
cd tpt-kinetix
cargo build --workspace
```

---

## Running the test suite

```sh
cargo test --workspace
```

This runs all unit tests, integration tests, proptest property tests, and the
cross-codec conformance suite in `tpt-kinetix-test-utils/tests/`.

To run tests for a specific crate only:

```sh
cargo test -p tpt-kinetix-demux
```

---

## Linting and formatting

```sh
# Clippy — treat all warnings as errors (matches CI)
cargo clippy --workspace -- -D warnings

# Formatting check
cargo fmt --check

# Auto-fix formatting
cargo fmt
```

---

## Running a fuzz target

cargo-fuzz requires the nightly toolchain.  Each crate that has fuzz targets
keeps them under its own `fuzz/` directory.

```sh
# Run the MP4 box fuzzer for 60 seconds
cd tpt-kinetix-demux
cargo fuzz run fuzz_mp4_box -- -max_total_time=60

# Run the AV1 OBU fuzzer for 60 seconds
cd tpt-kinetix-av1
cargo fuzz run fuzz_obu_parse -- -max_total_time=60
```

To just check that all fuzz targets compile (no fuzzing):

```sh
cd tpt-kinetix-demux && cargo fuzz build
cd tpt-kinetix-av1   && cargo fuzz build
```

If cargo-fuzz finds a crash it writes a reproducer to
`fuzz/artifacts/<target>/crash-*`.  Add that file to the crate's
`fuzz/corpus/<target>/` directory so it becomes a permanent regression case.

---

## Running coverage

Requires the `llvm-tools-preview` rustup component and `cargo-llvm-cov`:

```sh
# HTML report in target/llvm-cov/html/
cargo llvm-cov --workspace --open

# LCOV report (as used by CI)
cargo llvm-cov --workspace --lcov --output-path lcov.info
```

---

## Adding a new codec using the KG pipeline

`tpt-kinetix-kg` is the knowledge-graph tool that turns C codec source into
parallel Rust scaffolding.  The full workflow:

1. **Ingest** the C source and inspect statistics:

   ```sh
   tpt-kinetix-kg ingest path/to/codec.c
   ```

2. **Build the graph JSON** (commit this alongside the source):

   ```sh
   tpt-kinetix-kg graph path/to/codec.c -o mycodec.kg.json
   ```

3. **Analyse** the dependency graph to find independent parallel sets:

   ```sh
   tpt-kinetix-kg analyze mycodec.kg.json
   ```

4. **Generate** the Rust scaffolding with rayon injection:

   ```sh
   tpt-kinetix-kg codegen mycodec.kg.json \
     --crate-name tpt-kinetix-mycodec \
     --inject-rayon \
     --output-dir tpt-kinetix-mycodec/
   ```

5. **Run end-to-end** (steps 1–4 in one command):

   ```sh
   tpt-kinetix-kg run path/to/codec.c \
     --crate-name tpt-kinetix-mycodec \
     --inject-rayon \
     --output-dir tpt-kinetix-mycodec/
   ```

After code generation, add the new crate to `Cargo.toml`'s `[workspace]
members` list and wire in any `tpt-kinetix-core` types as needed.  See
`tpt-kinetix-kg/DEVELOPER.md` for the full reference.

---

## Testing strategy

The project uses four complementary layers of testing:

### Unit tests

Inline `#[cfg(test)]` modules inside each source file.  These cover individual
functions at the boundary of a single module.  Run with `cargo test`.

### Integration tests

Files under `<crate>/tests/`.  Each file is a separate integration test binary
that exercises the public API of the crate end-to-end.  Examples:
`tpt-kinetix-demux/tests/mp4_parse.rs`, `tpt-kinetix-av1/tests/encode_smoke.rs`.

### Proptest (property-based tests)

Files named `proptest_*.rs` under `<crate>/tests/`.  Each test feeds thousands
of randomly generated inputs through a public entry point and asserts that it
*never panics*.  Proptest automatically shrinks failing inputs to a minimal
reproducer and persists them in `.proptest-regressions/` files that are
committed to the repository.

Current proptest suites:

| File | Tested invariant |
|------|-----------------|
| `tpt-kinetix-demux/tests/proptest_mp4.rs` | `parse_mp4` and `parse_box_header` never panic |
| `tpt-kinetix-h264/tests/proptest_nal.rs` | `parse_nal_units_from_annexb` and `remove_emulation_prevention_bytes` never panic |
| `tpt-kinetix-av1/tests/proptest_obu.rs` | `parse_obu_sequence` never panics |

### Fuzz testing

`cargo-fuzz` targets in each codec/demux crate exercise the parsers with
coverage-guided mutation.  Targets are compiled with sanitizers enabled (ASan,
UBSan) so memory safety bugs surface immediately.  Run a target for at least 60
seconds before submitting a parser change.

### Conformance tests

`tpt-kinetix-test-utils/tests/conformance.rs` uses the shared helpers in
`tpt-kinetix-test-utils` to run cross-boundary assertions — for example, checking
that a synthetic frame is pixel-identical to itself, that different synthetic
frames actually differ, and that all corpus edge cases pass through the demuxer
without panicking.

---

## Pull request checklist

Before opening a PR, verify the following locally:

- [ ] `cargo build --workspace` succeeds with no warnings
- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --workspace -- -D warnings` is clean
- [ ] `cargo fmt --check` reports no formatting issues
- [ ] New public API is documented with `///` doc comments
- [ ] New parsing code has a corresponding proptest `*_never_panics` test
- [ ] If a fuzz crash was found, the reproducer is added to the corpus
- [ ] `cargo deny check` passes (no new unlicensed or advisory-flagged deps)
- [ ] Coverage does not regress significantly (`cargo llvm-cov --workspace`)
