# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.1](https://github.com/tpt-solutions/tpt-kinetix/compare/v0.2.0...v0.2.1) - 2026-11-07

### Other

- updated the following local packages: tpt-kinetix-core, tpt-kinetix-demux, tpt-kinetix-mux

## [0.2.0](https://github.com/tpt-solutions/tpt-kinetix/compare/v0.1.2...v0.2.0) - 2026-11-07

### Other

- double EXT-X-TARGETDURATION when parts are on
- DevTools network log for the browser runner, and the stall analysis
- LL-HLS subtitle parts
- signal cbcs-pattern as SAMPLE-AES, checked in hls.js and dash.js
- cbcs-pattern CENC, with a subsample map so the pattern applies
- CENC encryption and DASH text sets for live packaging
- npm package for the WASM packager, bindings in CI
- HLS ClearKey plays in headless Chrome
- HLS key signaling for CENC (ClearKey), not yet playing in hls.js
- WebVTT subtitle renditions from WebM ingest
- WebVTT in WebM checked against ffmpeg
- subtitle and trick-play renditions in the WASM packager
- trick-play I-frame renditions for every video track
- LANGUAGE on HLS renditions and lang on DASH sets
- every WebVTT track becomes its own subtitle rendition
- --cenc-scheme cenc|cbcs for package and serve
- tests and CI: WebVTT and CENC end to end, in ffmpeg and in browsers
- WebVTT subtitle group (HLS and DASH) and CENC protection
- subtitle tracks in core, MP4 demux, MP4 mux and remux

## [0.1.2](https://github.com/tpt-solutions/tpt-kinetix/releases/tag/v0.1.2) - 2026-11-04

### Changed

- README status refreshed to match the current code.
- Browser test page (`tests/browser/page.html`) tweaked.

## [0.1.1](https://github.com/tpt-solutions/tpt-kinetix/releases/tag/v0.1.1) - 2026-10-07

### Other

- bump all crates to 0.1.1 and fix crates.io publishing
- fix hls.js stalls with a separate audio rendition in low-latency HLS
- add WHIP ingest, RTMP/WebSocket live hardening, and package/mux updates
- WebSocket ingest, browser publish page, latency tests, live server example
- ingest policy and recording, RTMP live hardening, VP9 predict/loop-filter fixes
- parse the Matroska Cues index and expose it from MkvReader
- dynamic DASH MPD for the live presentation
- just-in-time HLS/DASH from WebM/Matroska input
- low-latency HLS (EXT-X-PART, blocking reload, preload hints)
- AV1/VP9 + Opus WebM ingest over HTTP with live fMP4 HLS
- edge Worker example and real-client (hls.js/dash.js) playback tests
- WebAssembly bindings and a Node byte-identity test
- just-in-time HLS (fMP4) and DASH from an MP4 index; async index loading
