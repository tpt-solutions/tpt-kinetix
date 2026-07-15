# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Phase 0: Full Cargo workspace bootstrap with 8 crates
- Phase 1: Knowledge-graph tooling (tree-sitter-c ingestion, graph, codegen)
- Phase 2: nom-based MP4/ISO-BMFF demuxer
- Phase 3: H.264 decoder (NAL, SPS/PPS, CAVLC, macroblock, rayon parallel rows)
- Phase 4: AV1 OBU parser + rav1e encoder integration
- Phase 5: Concurrent processing pipeline (crossbeam stages, backpressure)
- Phase 6: RTMP ingest server + HLS packaging engine
