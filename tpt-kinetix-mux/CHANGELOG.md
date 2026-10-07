# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1](https://github.com/tpt-solutions/tpt-kinetix/releases/tag/v0.1.1) - 2026-10-07

### Added

- runnable examples per crate + release-plz automation
- *(stream)* RTMP AMF connect/publish + FLV depacketization, MPEG-TS HLS muxing
- DecoderCapabilities introspection + MP4 muxer crate

### Other

- bump all crates to 0.1.1 and fix crates.io publishing
- carry Enhanced RTMP colorInfo into colr/mdcv/clli
- add WHIP ingest, RTMP/WebSocket live hardening, and package/mux updates
- ingest policy and recording, RTMP live hardening, VP9 predict/loop-filter fixes
- carry DiscardPadding (the encoder's trailing trim) end to end
- WebmWriter — WebM/Matroska writer for AV1, VP9 and Opus
- just-in-time HLS (fMP4) and DASH from an MP4 index; async index loading
- fragmented input (moof/traf/trun) and a fragmented-MP4 writer
- multi-track passthrough MP4 writer, faststart, and tpt-kinetix remux (M3)
- Add per-crate Criterion benches, perf baseline/compare tooling, and publishing docs
- Merge branch 'master' into release-plz-2026-07-18T10-34-58Z
- release v0.1.0

## [0.1.0](https://github.com/tpt-solutions/tpt-kinetix/releases/tag/v0.1.0) - 2026-07-19

### Added

- runnable examples per crate + release-plz automation
- *(stream)* RTMP AMF connect/publish + FLV depacketization, MPEG-TS HLS muxing
- DecoderCapabilities introspection + MP4 muxer crate
