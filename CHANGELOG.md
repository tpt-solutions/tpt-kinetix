# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- H.264 is no longer published. The decoder crate was renamed `tpt-kinetix-h264` ->
  `out-kinetix-h264`, set `publish = false`, and removed from `tpt-kinetix-pipeline` /
  `tpt-kinetix-cli`. It is patent-encumbered (encode and decode) and is kept in-repo for
  local/reference work only. See `PATENTS.md`.
- AV1 decoder is pixel-exact (FFmpeg FATE 204/204 vs libdav1d, plus a libaom crosscheck),
  including 4:2:2/4:4:4, 10/12-bit and film grain. See `docs/CONFORMANCE.md`.
- H.264 decoder: temporal direct, PAFF, MBAFF and High-profile 8x8 transform are bit-exact; the
  curated ITU-T H.264.1 set is 34/34 hard-checked bit-exact.

### Removed

- AAC (now in the separate `tpt-cadence` project); root-level debug scratch files. The FFmpeg
  reference sources moved to `out-kinetix-h264/oracle/`.

### Added

- `just check-publish-safe` / `tools/check_no_encumbered.py` and a CI job that fail if a published
  crate can reach an `out-*` (patent-encumbered) crate.
- `docs/CONFORMANCE.md` / `docs/conformance.json` generated conformance report.
- Phase 0: Full Cargo workspace bootstrap with 8 crates
- Phase 1: Knowledge-graph tooling (tree-sitter-c ingestion, graph, codegen)
- Phase 2: nom-based MP4/ISO-BMFF demuxer
- Phase 3: H.264 decoder (NAL, SPS/PPS, CAVLC, macroblock, rayon parallel rows)
- Phase 4: AV1 OBU parser + rav1e encoder integration
- Phase 5: Concurrent processing pipeline (crossbeam stages, backpressure)
- Phase 6: RTMP ingest server + HLS packaging engine
- VP9 decoder (`tpt-kinetix-vp9`): profile-0 8-bit 4:2:0 decode end to end
  (superframes, tiles, intra/inter prediction, loop filter, frame-context
  adaptation); byte-exact vs `ffmpeg -c:v vp9` on the 13-clip conformance
  corpus (`pixel_exact: true`); wired into the pipeline (`codec-vp9` feature,
  `Vp9DecodeStage`) and the CLI (`probe`, `transcode --vcodec av1`)
