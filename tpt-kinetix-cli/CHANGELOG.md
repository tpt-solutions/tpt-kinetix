# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1](https://github.com/tpt-solutions/tpt-kinetix/releases/tag/v0.1.1) - 2026-10-07

### Added

- *(vp9)* wire the VP9 decoder into the pipeline and CLI
- DecoderCapabilities introspection + MP4 muxer crate

### Fixed

- fix two pre-existing bugs that made `transcode` unusable on VP9 input

### Other

- bump all crates to 0.1.1 and fix crates.io publishing
- HDR forwarding, per-key limits, WebSocket ping keep-alive, HTTPS/wss
- add WHIP ingest, RTMP/WebSocket live hardening, and package/mux updates
- WebSocket ingest, browser publish page, latency tests, live server example
- ingest policy and recording, RTMP live hardening, VP9 predict/loop-filter fixes
- fix five decode bugs that corrupted ordinary libvpx encodes
- demux from a ReadAt source instead of slurping the file
- IVF (bare AV1/VP9) demuxer and probe support
- probe --json (M5), ffprobe-shaped output for every container
- carry DiscardPadding (the encoder's trailing trim) end to end
- just-in-time HLS/DASH from WebM/Matroska input
- WebmWriter — WebM/Matroska writer for AV1, VP9 and Opus
- MkvReader — indexed Matroska/WebM over ReadAt (M1/M2 for MKV)
- low-latency HLS (EXT-X-PART, blocking reload, preload hints)
- Enhanced RTMP ingest (AV1/VP9 + Opus) into live fMP4 HLS
- AV1/VP9 + Opus WebM ingest over HTTP with live fMP4 HLS
- just-in-time HLS (fMP4) and DASH from an MP4 index; async index loading
- fragmented input (moof/traf/trun) and a fragmented-MP4 writer
- multi-track passthrough MP4 writer, faststart, and tpt-kinetix remux (M3)
- codec-agnostic StreamInfo and MP4 codec-config extraction (M2, MP4)
- HTTP range-request source; probe remote MP4s in 2-3 requests
- streaming MP4 reader over a positional ReadAt source (M1)
- Run cargo fmt to fix import ordering after out-kinetix-h264 rename
- Rename tpt-kinetix-h264 to out-kinetix-h264 and unpublish it; AV1 decoder fixes
- add MPEG-TS demuxer, closing the royalty-free codec roadmap
- remove tpt-kinetix-aac crate; AAC support now lives in a separate repo
- add PATENTS.md and gate H.264/AAC behind default-on Cargo features
- AAC PNS noise-scalefactor fix, HLS segment roll-over, and volumetric TMC13 geometry cross-check
- remove reference C/oracle files; vision: overhaul reconstruct + deblock/prediction/headers; h264: interlaced + cabac + ref_pic updates; av1: reconstruct/partition; screen/lean/lossless/cli updates
- reconstruct/deblock at coded dimensions, crop to visible on output; av1: add inverse-transform/filter-intra/palette tests; cli: implement transcode and stream commands
- release v0.1.0
- Catalog good-first-issue candidates and fix CLI doc link
- Add tpt-kinetix-aac: ADTS + AudioSpecificConfig parsing, decoder shell
- rename kinetix-* crates to tpt-kinetix-*, add probe subcommand and CI jobs

## [0.1.0](https://github.com/tpt-solutions/tpt-kinetix/releases/tag/v0.1.0) - 2026-07-19

### Added

- DecoderCapabilities introspection + MP4 muxer crate

### Other

- Catalog good-first-issue candidates and fix CLI doc link
- Add tpt-kinetix-aac: ADTS + AudioSpecificConfig parsing, decoder shell
- rename kinetix-* crates to tpt-kinetix-*, add probe subcommand and CI jobs
