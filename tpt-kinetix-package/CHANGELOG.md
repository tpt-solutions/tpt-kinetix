# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
