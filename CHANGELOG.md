# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/tpt-solutions/tpt-kinetix/releases/tag/v0.2.0) - 2026-11-07

### Added

- Subtitles: WebVTT tracks in `tpt-kinetix-core` (cue forms and end times), with MP4 (`wvtt`/`vttc`)
  and WebM/Matroska demux, mux and remux. `package` and `serve` make every WebVTT track its own HLS and
  DASH subtitle rendition (`subs-N.m3u8`, DASH text sets). Live ingest from WebM publishes WebVTT
  renditions, with LL-HLS subtitle parts.
- Track language (read and written from `mdhd`; `LANGUAGE` on HLS renditions, `lang` on DASH sets) and
  `disposition` default/forced flags. Both appear in `probe` text output and `--json`.
- CENC encryption (ClearKey) for fragmented MP4 in `tpt-kinetix-mux`, with the `cenc` and `cbcs` schemes,
  a subsample map, ClearKey `pssh` in protected init segments and HLS key signalling (`key.json`). Set
  with `--cenc-key-id`, `--cenc-key` and `--cenc-scheme` on `package`, `serve` and `live`, and as
  keyword arguments in the bindings. VP9, AV1 and Opus samples only. ClearKey HLS plays in headless Chrome.
- Trick-play: I-frame playlists (`iframes-N.m3u8`) for every video track. `serve` builds each rendition once.
- Remux output: `.webm` from MP4 input, `.ivf` (one AV1 or VP9 track, new `IvfWriter`) and `.ogg`/`.opus`
  (one Opus track, new `OggWriter`). MP4 text metadata and chapters carry into WebM output as Matroska
  tags and chapters.
- CLI: `sanitize` re-muxes an untrusted WebM or MP4 keeping only AV1, VP9 and Opus, and refuses files over
  size limits; `doctor` says whether a file will play in a browser and why not; `thumbnails` writes PNG
  keyframe stills, with an optional contact sheet (`sprite.png`).
- `probe` reports pixel format and colour fields for AV1 (from the sequence header) and VP9, and the
  duration of IVF tracks. IVF keyframes come from the bitstream.
- WHIP ingest publishes AV1 over WebRTC. The codec is negotiated from the one the transport names.
- WASM packager: subtitle and trick-play renditions. The `tpt-kinetix-package` npm package is `private`
  until the npm name is approved.
- Edge worker (`examples/edge-worker`): an edge cache for immutable init and segment responses, `Range`
  on segments, and trick-play and subtitle routes.
- Starters and docs: `templates/cli-media-tool` and `templates/live-ingest-server` (cargo-generate),
  six task examples (`examples/tasks`), a user guide, an ffmpeg migration guide, `SECURITY.md` and
  `CODE_OF_CONDUCT.md`.
- Distribution: prebuilt CLI binaries with SHA-256 sums, `cargo binstall` metadata, a container image
  (version tags only), and a WASM probe playground on GitHub Pages.
- `tpt-kinetix-ops`: the probe JSON, remux and package operations as a library, shared by the
  CLI and the language bindings. The CLI's remux and package summaries still print by default.
- C ABI (`tpt-kinetix-ffi`): `tk_remux` and `tk_package` (HLS + DASH to a directory, with optional
  CENC), alongside `tk_probe_json` and `tk_faststart`. Header `include/tpt_kinetix.h`, README, and
  `tools/ffi-smoke-test.sh`, which compiles and runs a C program against the library.
- Python bindings (`tpt-kinetix-py`, maturin): `probe`, `faststart`, `remux`, `package`, `version`,
  `KinetixError`. Tested by `tools/py-binding-test.sh` against an installed wheel.
- Node.js bindings (`tpt-kinetix-node`, napi-rs): the same operations as promises, with `index.d.ts`.
  Tested by `tools/node-binding-test.sh` against a packed npm tarball.
- WebAssembly packager npm package: `tools/wasm-package-test.sh` now packs it and tests the installed
  copy. The package is `private` until the maintainer approves the npm name.

### Changed

- `tpt-kinetix-cli`'s `package` runs on `tpt-kinetix-ops`, the same code the bindings call.
- `tpt-kinetix-stream` streams MPEG-TS from a byte source instead of building the whole buffer first.
- CENC moved to `aes` 0.9 and `ctr` 0.10; WHIP moved to `str0m` 0.25.
- CI: fuzz smoke test on PRs (nightly covers every target), fuzzing of the Matroska and MP4 metadata
  parsers, weekly `cargo-semver-checks`, workspace doctests, and tests for the C ABI, Python and Node
  bindings.

### Fixed

- Live: `EXT-X-TARGETDURATION` is doubled when LL-HLS parts are on, as the HLS spec requires.
- Ingest: a refused publish keeps its connection open long enough for the status line to be read.
- AV1 IBC: a negative source origin overflowed (found by fuzzing). AV1 and VP9 panics found by the
  fuzz targets are fixed.
- MP4 and WebM inputs without a codec configuration record get one. Colour and `udta` boxes are read.

### Security

- RTMP ingest caps chunk streams and buffered bytes, drains the body of a refused publish, and recovers
  from poisoned locks. AV1 temporal delimiters are stripped on RTMP ingest.
- Faststart uses checked box and offset arithmetic, so a malformed box size or offset is rejected
  instead of overflowing.

## [0.1.2](https://github.com/tpt-solutions/tpt-kinetix/releases/tag/v0.1.2) - 2026-11-04

### Fixed

- AV1 decoder refuses frames above the level 6.3 picture-size limit before allocating planes. A few
  header bytes could name a 32768x32768 or larger picture, which was a multi-gigabyte allocation.
  Found by `fuzz_av1_frame`.

## [0.1.1] - 2026-10-08

### Fixed

- Publishing: added missing `version` requirements on internal dependencies, broke the demux/mux
  dev-dependency cycle, and bumped all crates to 0.1.1 (0.1.0 of `tpt-kinetix-core` on crates.io predates
  the current API).

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
