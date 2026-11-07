# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

> This crate is **never published** (`publish = false`). Its changes are also listed in the
> workspace [CHANGELOG.md](../CHANGELOG.md).

## [Unreleased]

### Added

- C ABI: `tk_probe_json`, `tk_faststart`, `tk_remux` and `tk_package` (HLS and DASH, with optional
  CENC), plus `tk_version`, `tk_last_error` and `tk_free_string`. Header `include/tpt_kinetix.h`.
- `tools/ffi-smoke-test.sh` compiles `tests/c/smoke.c` against the library and runs it on ffmpeg-made
  media, and checks that the header matches the Rust source.
