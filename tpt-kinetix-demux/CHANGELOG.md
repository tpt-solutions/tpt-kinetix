# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1](https://github.com/tpt-solutions/tpt-kinetix/releases/tag/v0.1.1) - 2026-10-07

### Added

- runnable examples per crate + release-plz automation

### Fixed

- fix two pre-existing bugs that made `transcode` unusable on VP9 input

### Other

- bump all crates to 0.1.1 and fix crates.io publishing
- parse the AV1 sequence header's colour config for av1C
- parse the Matroska Cues index and expose it from MkvReader
- IVF (bare AV1/VP9) demuxer and probe support
- carry DiscardPadding (the encoder's trailing trim) end to end
- just-in-time HLS/DASH from WebM/Matroska input
- MkvReader — indexed Matroska/WebM over ReadAt (M1/M2 for MKV)
- Enhanced RTMP ingest (AV1/VP9 + Opus) into live fMP4 HLS
- AV1/VP9 + Opus WebM ingest over HTTP with live fMP4 HLS
- just-in-time HLS (fMP4) and DASH from an MP4 index; async index loading
- fragmented input (moof/traf/trun) and a fragmented-MP4 writer
- multi-track passthrough MP4 writer, faststart, and tpt-kinetix remux (M3)
- codec-agnostic StreamInfo and MP4 codec-config extraction (M2, MP4)
- HTTP range-request source; probe remote MP4s in 2-3 requests
- streaming MP4 reader over a positional ReadAt source (M1)
- Add per-crate Criterion benches, perf baseline/compare tooling, and publishing docs
- Tidy repo root: move H.264 oracle sources and tools, drop debug artifacts
- add MPEG-TS demuxer, closing the royalty-free codec roadmap
- Advance AAC, AV1, and H.264 decode paths, plus realtime and face codec scaffolding
- Advance H.264/AV1 decode paths and add volumetric codec scaffolding
- Merge branch 'master' of https://github.com/tpt-solutions/tpt-kinetix
- Merge branch 'master' into release-plz-2026-07-18T10-34-58Z
- Wire H.264 CAVLC I-slice decode into decoder.rs, scaffold tpt-kinetix-vision crate
- Fix MKV timestamp overflow panic and H.264 OOM from unbounded SPS dimensions
- Fix cargo fmt violations flagged by CI
- Fix shift-overflow panic in MKV EBML vint size parsing
- Add README/wasm browser demo, AV1 frame scaffold, and Phase 11 adoption polish
- Add tpt-kinetix-aac: ADTS + AudioSpecificConfig parsing, decoder shell
- fuzz targets (mkv/rtmp/hls), CI wiring, dependabot, justfile, templates, README status
- rename kinetix-* crates to tpt-kinetix-*, add probe subcommand and CI jobs

## [0.1.0](https://github.com/tpt-solutions/tpt-kinetix/releases/tag/v0.1.0) - 2026-07-19

### Added

- runnable examples per crate + release-plz automation

### Other

- Add tpt-kinetix-aac: ADTS + AudioSpecificConfig parsing, decoder shell
- fuzz targets (mkv/rtmp/hls), CI wiring, dependabot, justfile, templates, README status
- rename kinetix-* crates to tpt-kinetix-*, add probe subcommand and CI jobs
