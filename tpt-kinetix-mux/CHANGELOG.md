# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/tpt-solutions/tpt-kinetix/compare/v0.1.2...v0.2.0) - 2026-11-07

### Other

- use fn@ links for rustdoc function references in mux and ops
- MP4 chapters go into WebM output
- MP4 text metadata goes into WebM output as Matroska tags
- cbcs-pattern CENC, with a subsample map so the pattern applies
- IVF output, and MP4 metadata carried to MP4 output
- ClearKey pssh in protected init segments
- WebVTT cue forms, Matroska text and MP4 vttc
- WebVTT subtitle tracks (demux, mux, remux) and cue end times
- core, demux, mux: track language from mdhd (read and write)
- cbcs scheme (AES-CBC, whole samples) alongside cenc
- CENC (cenc scheme) for fragmented MP4
- subtitle tracks in core, MP4 demux, MP4 mux and remux
- Ogg writer for one Opus track; cli remux: .ogg and .opus output
- IVF writer; cli remux: .ivf output for one AV1 or VP9 track
- checked box and offset arithmetic in faststart; fuzz mkv_stream, moof, faststart

## [0.1.2](https://github.com/tpt-solutions/tpt-kinetix/releases/tag/v0.1.2) - 2026-11-04

### Changed

- README status refreshed to match the current code.

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
