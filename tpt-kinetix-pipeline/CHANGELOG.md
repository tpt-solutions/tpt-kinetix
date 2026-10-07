# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1](https://github.com/tpt-solutions/tpt-kinetix/releases/tag/v0.1.1) - 2026-10-07

### Added

- *(vp9)* wire the VP9 decoder into the pipeline and CLI
- runnable examples per crate + release-plz automation

### Other

- bump all crates to 0.1.1 and fix crates.io publishing
- demux from a ReadAt source instead of slurping the file
- Rename tpt-kinetix-h264 to out-kinetix-h264 and unpublish it; AV1 decoder fixes
- add PATENTS.md and gate H.264/AAC behind default-on Cargo features
- opt-in display-order (POC) reorder buffer
- Merge branch 'master' into release-plz-2026-07-18T10-34-58Z
- Fix H.264 inter CBP table, coeff_token FLC codes, and dec_ref_pic_marking gating
- Add README/wasm browser demo, AV1 frame scaffold, and Phase 11 adoption polish
- rename kinetix-* crates to tpt-kinetix-*, add probe subcommand and CI jobs

## [0.1.0](https://github.com/tpt-solutions/tpt-kinetix/releases/tag/v0.1.0) - 2026-07-19

### Added

- runnable examples per crate + release-plz automation

### Other

- rename kinetix-* crates to tpt-kinetix-*, add probe subcommand and CI jobs
