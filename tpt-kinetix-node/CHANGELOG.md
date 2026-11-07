# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

> This package is **private** (`"private": true` in `package.json`) until the maintainer approves
> an npm name. Its changes are also listed in the workspace [CHANGELOG.md](../CHANGELOG.md).

## [Unreleased]

### Added

- Node.js bindings (napi-rs): `probe`, `faststart`, `remux` (with `fragmentMs`), `package` (returns a
  promise of track, segment, file and byte counts; CENC `keyId`, `key` and `scheme`) and `version`.
  Typed with `index.d.ts`.
- `tools/node-binding-test.sh` packs the npm tarball and tests the installed copy.
