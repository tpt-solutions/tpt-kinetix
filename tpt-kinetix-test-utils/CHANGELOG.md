# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

> This crate is **never published** (`publish = false`, `release = false` in
> `release-plz.toml`), so it has no released versions. This changelog tracks
> workspace-internal changes only.

## [Unreleased]

### Added

- Initial `tpt-kinetix-test-utils` crate: shared test helpers for the workspace.
- `pixel_diff` (PSNR / tolerance) and `audio_diff` (tolerance / max-difference)
  frame comparison helpers.
- `reference` module driving external reference decoders (`ffmpeg`, `dav1d`),
  skipping gracefully when the binaries are absent from `PATH`.
- `synthetic` frame and minimal-bitstream generators.
- `corpus` reusable malformed-input corpora for fuzz-regression tests.
- `tmc13` driver for the MPEG-I G-PCC reference decoder, the conformance oracle
  for `tpt-kinetix-volumetric` (DECISION 8).
- `trace` / `trace_dump` decoder trace capture and dumping helpers.
- `realtime_bench` validation harness (loss injector + realtime decoder, no
  model weights) behind the `realtime-bench` feature, for the
  `tpt-kinetix-realtime` DECISION 5 gate.
