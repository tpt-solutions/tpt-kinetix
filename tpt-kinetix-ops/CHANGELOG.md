# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

> This crate is the shared implementation behind the `tpt-kinetix` CLI and the C, Python and
> Node.js bindings. Its changes are also listed in the workspace [CHANGELOG.md](../CHANGELOG.md).

## [Unreleased]

## [0.2.1](https://github.com/tpt-solutions/tpt-kinetix/compare/v0.2.0...v0.2.1) - 2026-11-07

### Other

- updated the following local packages: tpt-kinetix-core, tpt-kinetix-demux, tpt-kinetix-mux, tpt-kinetix-package

### Changed

- Now publishable (`publish = true`). The `tpt-kinetix-cli` crate depends on it, so it must be on crates.io
  before the CLI can be published. Added to the publish order (wave 4).
- Version set to 0.2.0 with the rest of the workspace (2026-11-08).

### Added

- Library form of the container operations: probe JSON, remux, faststart, package and a run summary.
  The CLI and the language bindings call these functions instead of carrying their own copies.
