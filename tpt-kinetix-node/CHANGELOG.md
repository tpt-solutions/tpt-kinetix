# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

> This package is **private** (`"private": true` in `package.json`) until the maintainer approves
> an npm name. Its changes are also listed in the workspace [CHANGELOG.md](../CHANGELOG.md).

## [Unreleased]

### Changed

- Version set to 0.2.0 (`package.json`) with the rest of the workspace (2026-11-08).
- The npm name is approved, but the package stays `private` until prebuilt addons are built for every platform in CI. Publishing one machine's build would break installs elsewhere.
- Packed tarball installed into a clean project and the 7 binding tests run (`tools/node-binding-test.sh`).

### Added

- Node.js bindings (napi-rs): `probe`, `faststart`, `remux` (with `fragmentMs`), `package` (returns a
  promise of track, segment, file and byte counts; CENC `keyId`, `key` and `scheme`) and `version`.
  Typed with `index.d.ts`.
- `tools/node-binding-test.sh` packs the npm tarball and tests the installed copy.
