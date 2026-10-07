# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1](https://github.com/tpt-solutions/tpt-kinetix/compare/v0.1.0...v0.1.1) - 2026-10-07

### Other

- bump all crates to 0.1.1 and fix crates.io publishing
- carry Enhanced RTMP colorInfo into colr/mdcv/clli
- multi-track passthrough MP4 writer, faststart, and tpt-kinetix remux (M3)
- codec-agnostic StreamInfo and MP4 codec-config extraction (M2, MP4)
- AV1 decoder and coefficient/inter/transform updates; codec crate conformance reporting updates
- 10/12-bit 4:2:2 and 4:4:4 output formats; fix profile-2 10-bit depth detection and 12-bit SGR overflow
- film grain, superres, monochrome support; thread real subsampling through the tile decoder
- high-bit-depth qlookup and reconstruct updates; h264: deblock and interlaced fixes, add ITU localize debug test; update todos
- Merge branch 'master' of https://github.com/tpt-solutions/tpt-kinetix
- Add new codec crates (face, lossless, realtime, screen, volumetric) and bitstream foundation

## [0.1.0](https://github.com/tpt-solutions/tpt-kinetix/releases/tag/v0.1.0) - 2026-07-19

### Added

- DecoderCapabilities introspection + MP4 muxer crate

### Other

- Add tpt-kinetix-aac: ADTS + AudioSpecificConfig parsing, decoder shell
- rename kinetix-* crates to tpt-kinetix-*, add probe subcommand and CI jobs
