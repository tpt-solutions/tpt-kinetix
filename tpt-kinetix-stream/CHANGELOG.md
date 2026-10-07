# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1](https://github.com/tpt-solutions/tpt-kinetix/releases/tag/v0.1.1) - 2026-10-07

### Added

- runnable examples per crate + release-plz automation
- *(stream)* RTMP AMF connect/publish + FLV depacketization, MPEG-TS HLS muxing

### Other

- bump all crates to 0.1.1 and fix crates.io publishing
- update the live server and stream crate READMEs
- make the FLV no-panic tests scalable (KINETIX_FUZZ_ITERS)
- Enhanced RTMP v2 capability exchange (capsEx, FourCC info maps)
- Enhanced RTMP audio multitrack
- test RTMPS handshake and connect over TLS
- HDR forwarding, per-key limits, WebSocket ping keep-alive, HTTPS/wss
- carry Enhanced RTMP colorInfo into colr/mdcv/clli
- fix hls.js stalls with a separate audio rendition in low-latency HLS
- add WHIP ingest, RTMP/WebSocket live hardening, and package/mux updates
- WebSocket ingest, browser publish page, latency tests, live server example
- ingest policy and recording, RTMP live hardening, VP9 predict/loop-filter fixes
- parse the Matroska Cues index and expose it from MkvReader
- dynamic DASH MPD for the live presentation
- low-latency HLS (EXT-X-PART, blocking reload, preload hints)
- Enhanced RTMP ingest (AV1/VP9 + Opus) into live fMP4 HLS
- AV1/VP9 + Opus WebM ingest over HTTP with live fMP4 HLS
- add MPEG-TS demuxer, closing the royalty-free codec roadmap
- remove tpt-kinetix-aac crate; AAC support now lives in a separate repo
- add PATENTS.md and gate H.264/AAC behind default-on Cargo features
- AAC PNS noise-scalefactor fix, HLS segment roll-over, and volumetric TMC13 geometry cross-check
- fix section_cb decoding and ics_info parsing; harden h264 CABAC and transform
- Advance AAC syntax/decoder, AV1 reconstruction, and H.264 CAVLC slice paths
- Merge branch 'master' of https://github.com/tpt-solutions/tpt-kinetix
- Merge branch 'master' into release-plz-2026-07-18T10-34-58Z
- Fix H.264 inter CBP table, coeff_token FLC codes, and dec_ref_pic_marking gating
- Clean up av1 clippy lints and fix rtmp AMF strict-array OOM
- Add README/wasm browser demo, AV1 frame scaffold, and Phase 11 adoption polish
- Add tpt-kinetix-aac: ADTS + AudioSpecificConfig parsing, decoder shell
- fuzz targets (mkv/rtmp/hls), CI wiring, dependabot, justfile, templates, README status
- rename kinetix-* crates to tpt-kinetix-*, add probe subcommand and CI jobs

## [0.1.0](https://github.com/tpt-solutions/tpt-kinetix/releases/tag/v0.1.0) - 2026-07-19

### Added

- runnable examples per crate + release-plz automation
- *(stream)* RTMP AMF connect/publish + FLV depacketization, MPEG-TS HLS muxing

### Other

- Add tpt-kinetix-aac: ADTS + AudioSpecificConfig parsing, decoder shell
- fuzz targets (mkv/rtmp/hls), CI wiring, dependabot, justfile, templates, README status
- rename kinetix-* crates to tpt-kinetix-*, add probe subcommand and CI jobs
