# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

> This crate is the Python package `tpt-kinetix` (PyPI). Its changes are also listed in the
> workspace [CHANGELOG.md](../CHANGELOG.md).

## [Unreleased]

### Changed

- Version set to 0.2.0 (`pyproject.toml`) with the rest of the workspace (2026-11-08).
- Release workflow `.github/workflows/pypi.yml`: manual dispatch, wheels for Linux, macOS and Windows plus an sdist, publish through PyPI trusted publishing in the `pypi` environment.
- The PyPI name `tpt-kinetix` is approved. Not published yet: the first publish needs a trusted publisher configured on PyPI.
- Wheel built and the 7 binding tests run from a clean venv on Windows (`tools/py-binding-test.sh`).

### Added

- Python bindings (maturin): `probe`, `faststart`, `remux` (with `fragment_ms`), `package` (with
  `segment_seconds`, CENC `key_id`, `key` and `scheme`), `version` and `KinetixError`.
- `tools/py-binding-test.sh` builds a wheel, installs it into a fresh virtualenv and runs the tests
  against ffmpeg-made media.
