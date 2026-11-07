# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

> This crate is **never published** to PyPI (`publish = false`). Its changes are also listed in
> the workspace [CHANGELOG.md](../CHANGELOG.md).

## [Unreleased]

### Added

- Python bindings (maturin): `probe`, `faststart`, `remux` (with `fragment_ms`), `package` (with
  `segment_seconds`, CENC `key_id`, `key` and `scheme`), `version` and `KinetixError`.
- `tools/py-binding-test.sh` builds a wheel, installs it into a fresh virtualenv and runs the tests
  against ffmpeg-made media.
