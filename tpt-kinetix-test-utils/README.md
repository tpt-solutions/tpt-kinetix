# tpt-kinetix-test-utils

Shared testing helpers for the [TPT Kinetix](https://github.com/tpt-solutions/tpt-kinetix)
workspace.

> **Internal crate — never published.** This crate has `publish = false` and
> `release = false` in `release-plz.toml`, so it never appears on crates.io. The
> `keywords`/`categories`/`readme` fields are kept for consistency with the rest
> of the workspace.

Every crate in this workspace needs the same handful of testing capabilities:
diffing decoded frames, generating synthetic inputs, and comparing against an
external reference decoder. This crate implements them once so they are not
copy-pasted into 19 different `dev-dependencies` blocks and drift apart.

## Modules

- **`pixel_diff`** — PSNR and tolerance helpers for comparing decoded frames.
- **`audio_diff`** — tolerance / max-difference helpers for comparing decoded
  audio PCM frames.
- **`reference`** — drive external reference decoders (`ffmpeg`, `dav1d`) and
  diff their output against Kinetix decoders.
- **`synthetic`** — generate synthetic frames and minimal bitstreams.
- **`corpus`** — reusable malformed-input corpora for fuzz-regression tests.
- **`tmc13`** — drive the MPEG-I G-PCC `tmc3` reference decoder as the
  conformance oracle for `tpt-kinetix-volumetric` (DECISION 8).
- **`trace` / `trace_dump`** — decoder trace capture and dumping helpers.
- **`realtime_bench`** — loss-injector + realtime-decoder validation harness
  for the `tpt-kinetix-realtime` DECISION 5 gate (no model weights). Gated
  behind the `realtime-bench` feature.

## Graceful skipping

> **Important:** the `reference` and `tmc13` helpers shell out to external
> binaries (`ffmpeg`, `dav1d`, `tmc3`). These are **not** guaranteed to be on
> `PATH` — locally, or on a CI runner without them installed.

Never `.unwrap()` a `Command::new("ffmpeg").output()` result. When the binary
is missing, `Command::output()` panics with
`Os { kind: NotFound, … "No such file or directory" }`, which fails the whole
test run rather than skipping one test.

Use the shared `tpt_kinetix_test_utils::reference::ffmpeg_available` helper (or
a local equivalent) and return early:

```rust
if !ffmpeg_available() {
    eprintln!("skipping: ffmpeg not on PATH");
    return;
}
```

If you add a helper that spawns an external process, give it a matching
`*_available()` predicate and use it from every caller.

## Usage

Add it as a dev-dependency:

```toml
[dev-dependencies]
tpt-kinetix-test-utils = { path = "../tpt-kinetix-test-utils", version = "0.1.0" }
```

Optional features:

- `realtime-bench` — enables `tpt-kinetix-realtime` and the `realtime_bench`
  module.

## License

MIT OR Apache-2.0
