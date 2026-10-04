# todo-io.md — memory-safe media I/O: demux, package, stream

**Direction (decided 2026-10-03).** Kinetix is not going to out-run ffmpeg/dav1d at
codec decode: AV1 decode is at 0.03-0.17x of libdav1d single-threaded and the realistic
ceiling for pure Rust without hand assembly is roughly a third to a half of that. Rust
does not buy speed over C; it buys memory safety and ergonomic concurrency. So the
product is the **I/O layer**, where those are the point and where speed is I/O-bound
rather than assembly-bound: containers, packaging, ingest, streaming. Codec work
(AV1/VP9 decode, the original codecs) stays as-is and is not the priority.

## Open tasks (single source of truth — keep this list current)

Priority rule (user, 2026-10-04): **royalty-free first — AV1, VP9, Opus.** H.264/AAC are compatibility paths only.
`[x]` done, `[ ]` open. Details and evidence for finished work are in the "progress" sections below.

### A. Live and ingest

- [x] WebM-over-HTTP ingest (AV1/VP9 + Opus) -> live fMP4 HLS (`tpt-kinetix live`, `LiveServer`)
- [x] **Enhanced RTMP ingest** for AV1 / VP9 + Opus (`tpt-kinetix live --rtmp-port`, `rtmp_live.rs`): `ExVideoTagHeader`
      (`av01`/`vp09`, av1C/vpcC sequence start, CodedFrames/CodedFramesX) and `ExAudioTagHeader` (`Opus`); per-connection
      sessions; frames decode identically to the WebM source (hand-written client, AV1 and VP9) and a stock ffmpeg
      publishes VP9/AV1 end to end. Also fixed two RTMP chunk-stream bugs found on the way (SetChunkSize applied late
      within a read; type-1/2 header delta applied twice when a payload arrived in pieces).
      Open follow-ups: Enhanced `Multitrack` and `Metadata` (HDR colour info) packets, the E-RTMP v2 capability exchange
      (`videoFourCcInfoMap`), extended timestamps on continuation chunks, RTMPS (TLS), and a real OBS pass.
- [ ] **Low-latency HLS**: DONE 2026-10-04 — `EXT-X-PART` partial segments, `EXT-X-SERVER-CONTROL`, blocking
      playlist reload (`_HLS_msn`/`_HLS_part`), `EXT-X-SKIP` delta playlists, `EXT-X-PRELOAD-HINT`
      (`LiveOptions::part_seconds`, default 1/3 s, 0 disables; `live --part-seconds`). Parts of a
      segment concatenate to exactly that segment's samples. Target glass-to-glass is now ~1 part
      rather than ~3 segments (6 s). NOT measured against a real player yet — hls.js latency numbers
      still to be recorded.
- [ ] **Dynamic DASH MPD** for live (`type="dynamic"`, `availabilityStartTime`, sliding `SegmentTimeline`/`$Number$`,
      `minimumUpdatePeriod`) + low-latency DASH (CMAF chunks).
- [ ] **WHIP (WebRTC-HTTP ingest)** — browsers publish VP9/AV1 + Opus natively; needs ICE/DTLS/SRTP (large; evaluate a
      memory-safe Rust WebRTC stack vs. scope).
- [ ] Browser publish via `MediaRecorder` + streaming `fetch` (needs HTTP/2 or WebSocket ingest) and a demo page.
- [ ] Ingest hardening: auth tokens on `/ingest/<key>`, per-key limits (bitrate, duration, max publishers), idle
      timeout, backpressure, bounded memory under slow viewers, structured metrics (`/metrics`).
- [ ] Reconnect/restart handling: publisher drop + resume with `EXT-X-DISCONTINUITY`, config change mid-stream.
- [ ] Recording / DVR: persist segments, VOD playlist after the publish ends, resume window.
- [ ] Multi-rendition ladders (needs transcoding; passthrough only today — separate decision).
- [x] Opus pre-skip carried end to end: the `dOps` pre-skip becomes `OpusHead` on ingest and `CodecDelay`
      + `SeekPreRoll` on WebM output (measured against ffmpeg: without it the stream starts 7 ms late).
- [ ] **`DiscardPadding` (end trim) — partially done, one open question.** 2026-10-04: `MkvStream` now parses
      `DiscardPadding` (0x75A2) and `WebmWriter` re-emits it in a `BlockGroup`, so the trim round-trips
      (verified: our reader sees the identical value, and the bytes match ffmpeg's). NOTE the value is in
      *nanoseconds* per the Matroska TimestampScale, not samples — that cost two 1000x bugs. But ffmpeg
      still decodes the final frame at its full length (803 samples) where it decodes the source as 312,
      even though **`ffmpeg -c copy` itself preserves 312** — so something else differs and is NOT yet
      isolated. Measured state: 100/100 video frames and 200/201 audio frames decode identically after
      a WebM->WebM remux; only the final trimmed frame differs.
- [ ] HEVC/H.264/AAC live paths (secondary): FLV legacy RTMP -> the same `LivePackager` (the old TS HLS server stays).

### B. Packaging (VOD) and edge

- [x] Just-in-time HLS (fMP4) + static DASH from an MP4 index (`tpt-kinetix-package`, `package`, `serve`)
- [x] WASM build + Node byte-identity test; Worker handler example + Node test; hls.js/dash.js Chrome tests
- [ ] **DASH validation with an independent conformance checker** (DASH-IF validator / Shaka packager `--dump`) and a
      Safari/hardware-player pass (note: Apple HLS does not play VP9/AV1 fMP4 everywhere — document the matrix).
- [x] Packager from **WebM/Matroska VOD input**: `MkvIndex` exposes Matroska frames as `SampleRef`s, the
      `Packager` works over a `SampleIndex` trait (implemented by `Mp4Index` and `MkvIndex`), and
      `package`/`serve` sniff the EBML magic. Verified: a real VP9 + Opus WebM packages into HLS that
      ffmpeg decodes frame-for-frame identically to the source.
- [ ] Worker: segment cache layer (Cache API), `Range` on segment responses, real Cloudflare/R2 deployment test,
      CPU/memory limits under load, Deno/Fastly smoke tests.
- [ ] Live variant at the edge (Durable Object / stateful worker holding the `LivePackager`).
- [ ] Encryption: CENC/CBCS + ClearKey/Widevine key hooks (AV1/VP9/Opus).
- [ ] Subtitles/WebVTT passthrough, multi-audio-language tracks and `EXT-X-MEDIA` metadata, trick-play.
- [ ] `sidx` index boxes and `styp` for fragmented output (DASH-IF compatibility).

### C. Containers (demux / mux)

- [x] MP4 (progressive + fragmented) streaming demux over `ReadAt` / `AsyncReadAt`; HTTP range source
- [x] Multi-track MP4 writer, faststart, fragmented MP4 writer, `remux`
- [x] **WebM/Matroska muxer** (`WebmWriter`, `tpt-kinetix-mux/src/webm.rs`): `Write`-based, live (unknown-size
      clusters/segment, no seeking) and finite (patched sizes + `Cues` + `Duration`); AV1/VP9/Opus
      passthrough with `av1C` carried verbatim, `OpusHead` synthesised from `dOps`, pre-skip as
      `CodecDelay`/`SeekPreRoll`, `DiscardPadding` re-emitted in a `BlockGroup`; `remux` writes `.webm`.
      Round-trips ffmpeg-made files: video frame-exact, audio frame-exact but for the final trimmed
      frame (see the `DiscardPadding` note in section A).
- [x] Replaced the whole-buffer MKV demuxer with `MkvReader` (index over `ReadAt` + `StreamInfo`, seek to key
      frame, `probe` sniffs the EBML magic). M2 for MKV. The index pass reads the file once (Matroska has no
      seekable `moov`), but keeps only the frame index in memory and reads each frame by offset.
- [ ] Seek via `Cues` in `MkvReader` (the parser skips `Cues` today; `MkvStream` exposes no cue positions).
- [ ] MPEG-TS: streaming demux (`TsDemuxer` still takes a `Vec`) and `StreamInfo` (M1/M2 for TS).
- [ ] Ogg/Opus (`.opus`) demux/mux; IVF (AV1/VP9) demux/mux (trivial, useful for tests).
- [ ] Metadata/chapters/`udta`/cover art passthrough in `remux`; non-seekable progressive output; MP4 `elst` multi-edit.
- [ ] `av1C`/`vpcC` synthesis when a container omits them (AV1 sequence-header OBU parse; VP9 from key frame is done).
- [ ] Remove remaining `std::fs::read` callers (`pipeline/stage.rs`, CLI `transcode`).

### D. Evidence, quality, tooling

- [ ] **M6 `just bench-io`**: startup, remote-probe round trips, RSS per stream and streams/core vs ffmpeg
      (RTMP/WebM -> HLS density), hostile-input corpus (ffmpeg crash/hang count vs Kinetix).
- [ ] Run the fuzz targets (`fuzz_mp4_reader`, add `fuzz_mkv_stream`, `fuzz_moof`) in CI — local nightly sanitizer broken;
      commit crash regressions to `fuzz/corpus/`.
- [x] CI jobs for the new end-to-end tests: `wasm-package-test`, `edge-worker-test`, `browser-package-test`,
      `live-browser-test` (added to `.github/workflows/ci.yml`; they run the existing `just` recipes /
      `tools/*.sh` scripts). Chrome comes from `tools/fetch-chrome.sh` (Chrome-for-Testing, cached;
      falls back to a system browser), since GitHub runners have none by default. NOT yet observed
      green on a real runner — the scripts were only run locally on Windows.
- [ ] Load test the live server (N concurrent publishers/viewers; CPU, RSS, latency) and record it.
- [ ] Real network latency measurements for remote probe / JIT segments (everything so far is localhost).
- [ ] Docs: user guide for `remux` / `package` / `serve` / `live`, API docs for `ReadAt`/`AsyncReadAt`/`Packager`/
      `LivePackager`, a PATENTS.md note on royalty-free vs passthrough of encumbered codecs.
- [ ] `Packet` has no duration field (it is built in ~200 places): decide on a migration or keep the out-of-band
      `read_packet_timed` / `write_packet_with_duration`.

### E. Explicitly not planned

Beating ffmpeg/dav1d at decode speed; new codecs; transcoding as a headline feature (see Direction).

## What the I/O layer is today (measured/read 2026-10-03)

| Piece | State | Why it is not useful yet |
|:---|:---|:---|
| `tpt-kinetix-demux` (2.7k lines) | MP4 works, MPEG-TS works, MKV/WebM basic | **Reads the whole file into memory** (`std::fs::read` in the CLI and every example); no `Read + Seek`/ranged source |
| `tpt-kinetix-mux` (0.6k lines) | progressive MP4, **one H.264 track** | builds the file in a `Vec<u8>` (`finish() -> Vec<u8>`), no audio, no multi-track, no fMP4/CMAF |
| `tpt-kinetix-stream` (2.4k lines) | RTMP ingest, HLS (MPEG-TS segments) | TS-only HLS; no fMP4/CMAF/DASH; no audio |
| CLI | `probe` works; `transcode` partial; `stream` stub | no `remux`, no `package`, no JSON probe |

Audio is the biggest functional hole: the AAC crate was removed (it lives in tpt-cadence),
so every container path currently carries video only. A packager that cannot carry audio
is not usable. The fix is **codec-agnostic passthrough** (packets with a codec id +
extradata), not decoders.

## Where ffmpeg is actually weak (and Kinetix can win)

Measured here (Windows, 1080p/120 s/85 MB H.264 MP4, avg of 20 runs):

| Operation | ffmpeg | Kinetix today |
|:---|---:|---:|
| fixed process cost: `ffmpeg -i clip.mp4 -f null -t 0.001 -` | 114 ms | n/a (library: 0 ms) |
| `ffprobe -show_streams -show_format` | 152 ms | 61 ms (reads all 85 MB; will not scale) |
| `ffprobe -show_entries stream=...` (minimal) | 44 ms | |

Known/qualitative, to be measured before claiming:
1. **Per-request process spawn.** ffmpeg/ffprobe are CLIs; a service that probes or
   repackages per request pays ~50-150 ms of process + DLL start and a fork/exec each time.
   An in-process library pays nothing.
2. **Remote probing.** ffmpeg's probe downloads `probesize` (5 MB default) and applies
   `analyzeduration`. An MP4 `moov` can be fetched with two HTTP range requests (tail + head).
   Target: probe an S3/HTTP object in ~2 round trips and a few hundred KB.
3. **Concurrency density.** RTMP->HLS in ffmpeg is one process per stream (tens of MB RSS
   each). One async Rust process can carry thousands of streams. Measure RSS/stream and
   streams/core against ffmpeg.
4. **Robustness on hostile input.** ffmpeg demuxers are a steady CVE source. Here every
   parser already has `*_never_panics` proptests + fuzz targets; extend to the whole path.
5. **Throughput is NOT a differentiator**: `-c copy` remux is disk/network-bound for both.
   Do not promise speed; promise latency, memory, density, safety, embeddability (WASM).

## Milestones

- [~] **M1 — Streaming demux.** *(MP4 done 2026-10-04: `ReadAt` + `Mp4Reader` (progressive and fragmented), HTTP-range source, CLI `probe`/`remux` never load the file; TS and MKV still take a whole buffer.)* `Demuxer` over `Read + Seek` (and an async/ranged source
  trait); MP4 reads `moov` without loading `mdat`; samples are read on demand. Replace every
  `fs::read` in CLI/examples. Test with a >RAM synthetic sparse file and an HTTP-range mock.
- [~] **M2 — Codec-agnostic tracks.** *(MP4 done 2026-10-04: `core::StreamInfo`, `Mp4Reader::streams()`; MKV and TS still to do.)* Packets carry `codec id + extradata` for H.264, AV1,
  VP9, **AAC/Opus/MP3 as opaque passthrough** (no decoder). Demux MP4/MKV/TS multi-track.
- [x] **M3 — Streaming muxer.** *(done 2026-10-04: progressive multi-track MP4, faststart, fragmented MP4/CMAF writer, `remux`; see "M3 progress" below. Non-MP4 outputs and metadata copy remain.)* `Write`-based MP4 muxer: multi-track, faststart, edit lists,
  and **fragmented MP4 / CMAF**. Remux (`tpt-kinetix remux in out`) round-trips vs
  `ffprobe`/`ffmpeg -c copy` byte-for-semantics.
- [~] **M4 — Packaging.** *(VOD just-in-time HLS-fMP4 + DASH done; live HLS from WebM-over-HTTP (AV1/VP9+Opus) done 2026-10-04; low-latency parts, dynamic DASH, Enhanced RTMP/WHIP ingest still open.)* HLS with fMP4 segments + DASH (on top of M3), live sliding window,
  low-latency parts; RTMP/SRT ingest -> package, audio included.
- [x] **M5 — Probe service.** 2026-10-04: `probe --json` emits ffprobe-shaped JSON (a `streams`
  array plus a `format` object; `index`, `codec_name`, `codec_type`, `width`/`height`,
  `channels`, `sample_rate`, `time_base`, `duration`, `nb_frames`) for MP4, MPEG-TS and
  Matroska/WebM, local and `http(s)://`. Values cross-checked against `ffprobe -of json` on
  real files. Fields a demuxer cannot know (pix_fmt, colour, disposition) are omitted rather
  than guessed. Remote range-request probing and the WASM build are unchanged.
- [ ] **M6 — Evidence.** `just bench-io`: startup, remote-probe round trips, RSS/stream and
  streams/core vs ffmpeg, hostile-input corpus (ffmpeg crash/hang count vs Kinetix).

## Out of scope

Beating ffmpeg/dav1d at codec decode speed; new codec work; transcoding as a headline
feature. (Existing decoders stay and keep their conformance tests.)

### M1 progress (2026-10-04)

* `tpt-kinetix-demux`: `source::ReadAt` (positional reads; impls for memory, `File`, any `Read+Seek`, plus a
  `CountingSource` for I/O assertions) and `mp4::Mp4Reader` — hops top-level box headers, reads only `moov`,
  builds the sample index once (O(samples)), serves each packet with one positional read.
* Fixed on the way: the old demuxer was O(n^2) per file (stsc/stss rescans per sample), ignored `ctts`
  (so `pts == dts` on B-frame streams), read tracks sequentially instead of by decode time, and
  `parse_stsd` allocated from an attacker-controlled `entry_count`.
* Hostile-input limits: moov <= 256 MiB, <= 16 Mi samples/track, bounded top-level scan, sample bytes
  allocated only after an in-file bounds check. Proptest + new `fuzz_mp4_reader` target.
* Measured (1080p, 85 MB / 2.2 GB MP4, this machine): `tpt-kinetix probe` 27 ms / 32 ms (was 61 ms and
  would have read all 2.2 GB); `ffprobe -show_streams` 90 ms / 59 ms. Open of the 21 MB test file reads < 4 KiB.
* Remaining for M1: streaming TS (`TsDemuxer` still takes a `Vec`), MKV, an HTTP-range `ReadAt`
  (probe an S3 object in ~2 requests), fragmented MP4 (`moof`) input, `Mp4Demuxer` callers in
  pipeline/CLI transcode that still `fs::read` (`stage.rs`, `main.rs`).

### Remote probing (2026-10-04) — first measured win vs ffprobe

`tpt-kinetix probe http(s)://…` runs `Mp4Reader` over `http::HttpRangeSource` (cargo feature `http`, `ureq`
transport, `RangeFetch` trait for custom stacks such as signed S3). 64 KiB read-ahead blocks; the first
request doubles as the length probe (`Content-Range`); reads >= a block go as one exact range; a server that
ignores `Range` is an error, never a silent full download. Local range server, same machine:

| File | Tool | Requests | Bytes transferred | Time |
|:---|:---|---:|---:|---:|
| 85 MB, moov at end | `tpt-kinetix probe` | 2 | **127 KB** | 71 ms |
| 85 MB, moov at end | `ffprobe -show_streams -show_format` | 3 | 2.14 MB | 114 ms |
| 2.2 GB (93,600 samples) | `tpt-kinetix probe` | 3 | 1.14 MB (≈ the moov itself) | 108-480 ms* |
| 2.2 GB (93,600 samples) | `ffprobe -show_streams -show_format` | 3 | 3.11 MB | 1446 ms |

\* cold-cache variance of the test server; ffprobe's full probe also spends time analysing the stream.
Over a real network the byte count and request count (each a round trip) are what matter: ~17x fewer bytes
for a moov-at-end file. Caveats: localhost only (no latency), ffprobe was not tuned (`-probesize` /
`-analyzeduration` can lower its cost at the price of accuracy), one container type.
Tests: `tests/http_range.rs` (real TCP server, request/byte bounds, non-Range server, seek cost).

### M2 progress (2026-10-04) — MP4 side

* `tpt-kinetix-core::StreamInfo` (codec, timescale, duration, video size, audio channels/rate/bits, codec
  config record `extradata`); new `CodecId::{Mp3, Ac3, Eac3}`.
* `mp4::config::parse_sample_entry`: video/audio sample-entry fixed fields (audio v0/v1/v2 layouts) and the
  config child boxes: `avcC`, `hvcC`, `av1C`, `vpcC`, `dOps`, `dfLa`, `dac3`, `dec3`, plus the AAC
  `AudioSpecificConfig` out of `esds` (MPEG-4 descriptor walk; `mp4a` + object type 0x6B/0x69 is MP3).
  Bounds-checked; truncation/hostile input yields fewer fields, never a panic.
* Validated against real encoders (`tests/real_ffmpeg.rs`, skipped without ffmpeg): H.264+AAC, AV1+Opus,
  VP9+Opus, HEVC+AC-3 (6 ch), H.264+MP3, H.264+FLAC, H.264+E-AC-3 — codec, size, channels, rate all equal
  `ffprobe`, and `avcC`/`hvcC`/`av1C` sizes equal ffmpeg's `extradata_size`.
* `probe` prints audio layout and config sizes.
* Still to do for M2: Matroska (`CodecPrivate` → extradata, multi-track, lacing, Cues, EBML-void/unknown sizes),
  MPEG-TS (PMT descriptors -> StreamInfo; AAC ADTS -> AudioSpecificConfig), stream-info from fragmented MP4.

### M3 progress (2026-10-04) — progressive MP4 writer, faststart, `remux`

* `tpt-kinetix-mux`: `Mp4Writer<W: Write + Seek>` (multi-track passthrough; media written in ~0.5 s chunks, sample
  tables buffered, `moov` at end; 64-bit `mdat`, `stco`/`co64` chosen by size; edit lists keep AAC priming and
  B-frame composition delay; late-starting tracks get an empty edit), `faststart()` (one streaming pass, patches
  `stco`/`co64`, works on any MP4), sample-entry writers for H.264/H.265/AV1/VP9/AAC/MP3/Opus/FLAC/AC-3/E-AC-3
  from `StreamInfo`. `Mp4Reader::read_packet_timed()` supplies sample durations (the writer takes a hint for
  each stream's last sample); `Packet` itself is unchanged (it is built in ~200 places).
* CLI: `tpt-kinetix remux <in|url> <out> [--faststart]`.
* Verified on ffmpeg-made files (H.264 w/ B-frames, AV1, H.265, VP9 with AAC/Opus/AC-3/E-AC-3/MP3/FLAC): ffmpeg
  decodes every output with zero errors, and `ffprobe -show_packets` (stream, pts, dts, duration, size, flags) is
  **identical** to the source for all packets; stream and format durations match. Tests: `tests/writer_roundtrip.rs`
  (round-trips, co64, faststart idempotence, bad input, rescaling, proptest, ffmpeg interop).
* Speed (this machine, warm cache, process start included): 85 MB remux ~100 ms vs `ffmpeg -c copy` ~160 ms;
  with faststart ~150 ms vs ~190 ms. 2.2 GB runs are disk-bound (17-48 s for both, high variance): not a
  differentiator, as expected.
* Not yet: fragmented MP4 / CMAF (needed for M4), MKV/TS/WebM output, `udta`/metadata/chapters/subtitles copy,
  non-MP4 inputs for `remux`, `Write`-only (non-seekable) progressive output.

### Fragmented MP4 (2026-10-04)

* **Input:** `Mp4Reader` now indexes `moof`/`traf`/`trun` (with `trex` defaults, `tfhd` base-offset rules incl.
  default-base-is-moof, `tfdt`, signed composition offsets, first-sample flags), reading only the small `moof`
  boxes; durations/sample counts come from the index (`probe` shows real values). Untrusted `trun` counts must be
  backed by bytes; `moof` <= 64 MiB; samples <= 16 Mi/track. Checked against ffmpeg-made fMP4 (with and without
  `default_base_moof`, B-frames, VP9/Opus): `remux`ed to progressive, ffprobe packets are identical to the source's
  (negative-CTS files differ only by ffprobe applying/ignoring the edit list differently for fragmented input).
* **Output:** `mux::FragmentWriter` — init segment (`ftyp iso6/cmfc` + `moov` with empty tables, `mvex/trex`, source
  edit list kept) and `moof`+`mdat` fragments (`tfhd` default-base-is-moof, `tfdt` v1, `trun` v1 with duration/size/
  flags/signed cts), cut wherever the caller flushes (unresolved last-sample durations carry into the next
  fragment). Write-only: no seeking, bounded memory, usable for sockets/pipes. CLI:
  `remux in out --fragmented [--fragment-ms N]` (`out` may be `-` for stdout).
* Tests: 17 in `tests/writer_roundtrip.rs` (boundary-independence, carry-over, sequence numbers, proptest,
  mutation proptest on fragmented files, ffmpeg decode + packet count).
* Next (M4): segmenter that cuts at key frames and names/indexes segments; HLS (fMP4) playlists, DASH MPD, live
  sliding window; RTMP -> fMP4 including audio.

### M4 progress (2026-10-04) — just-in-time packaging, async index, edge story

* **Async index.** `demux::AsyncReadAt` (+ `Blocking` adapter, `block_on`) and `Mp4Index::load`: the same
  loader serves native sync files (`Mp4Reader::open`), HTTP range sources, and any future/`fetch`-backed source
  (WASM Workers/browsers). Proven with a source that really suspends on every read.
* **`tpt-kinetix-package`** (compiles to `wasm32-unknown-unknown`): `Packager` loads only the MP4 index, plans
  key-frame-aligned segments (lead = first video track; other tracks cut at the same instant), and produces on
  demand: HLS master + per-track media playlists (fMP4, `EXT-X-MAP`, separate audio rendition group, RFC 6381
  `CODECS`, `RESOLUTION`, `FRAME-RATE`, peak/average `BANDWIDTH`), a static DASH MPD (`SegmentTimeline`, presentation-
  time based), init segments, and `moof`+`mdat` segments built from ranged reads of just that segment's samples
  (nearby reads coalesced; `max_read_gap`). Codec strings for H.264/H.265/AV1/VP9/AAC/MP3/Opus/FLAC/AC-3/E-AC-3.
* **CLI:** `tpt-kinetix package in out/ [--segment-seconds N]` (static tree) and `tpt-kinetix serve in
  [--port N]` (JIT HTTP, CORS enabled, std-only), input a file or an `http(s)://` URL.
* **Verification (ffmpeg as the oracle):** 12 s H.264(B-frames)+AAC file, 3 s segments; ffmpeg pulling
  `master.m3u8` from `serve` decodes **300/300 video and 563/563 audio frames with frame-MD5s identical to decoding
  the source**; video timestamps identical too (audio shifted by the AAC priming because ffmpeg ignores edit lists
  in fragmented input; the init segment keeps the `elst`). DASH: ffmpeg's own `-f dash` output read through
  ffmpeg's DASH demuxer shows the same +/-1-3 frame quirk (301/560), so the DASH frame counts are a demuxer
  behaviour, not our MPD; DASH is covered structurally (balanced XML, timeline durations sum to track length).
* Tests: 10 in `tests/package.rs` (plan invariants, init+segments reassemble to the source, independent segments
  with own sequence numbers, document structure, read coalescing, suspending source, proptest, ffmpeg HLS identity).
* Next: WASM bindings (wasm-bindgen, JS range callback), live/sliding-window + low-latency HLS, RTMP/SRT ingest
  with audio, DASH validation with a real client (dash.js / Shaka), multi-bitrate ladders need transcoding (out of
  scope; passthrough only).

### WASM / edge (2026-10-04)

* `tpt-kinetix-package` feature `wasm` (wasm-bindgen): `WasmPackager.open(len, read, segmentSeconds)` where
  `read(offset, length)` returns `Promise<Uint8Array>` (e.g. `fetch` with a `Range` header or an R2/S3 range GET);
  `hlsMaster()`, `hlsMedia(i)`, `dashMpd()`, `initSegment(i)`, `await mediaSegment(i, n)`, `trackCount`,
  `segmentCount`, `codec(i)`, `requests`. The module is ~278 KB (no `wasm-opt`).
* `just wasm-package-test` (`tools/wasm-package-test.sh`): builds the WASM package, drives it from Node with
  genuinely asynchronous range reads of a real file, and requires all 14 output files (playlists, MPD, init and media
  segments) to be **byte-identical** to the native `package` output; the index loads in 5 range reads.
* Also compiling for `wasm32-unknown-unknown`: `core`, `demux` (plus its existing `wasm` probe feature), `mux`,
  `package`. Not yet: a Worker example, caching headers/ETag handling, `Range` support on segment responses.

### Real clients and the edge handler (2026-10-04)

* **Real MSE players (`just browser-package-test`):** headless Chrome + **hls.js** and **dash.js** play the
  `serve`d stream (12 s clip, 3 s segments, H.264 B-frames + AAC): both advance past a segment boundary
  (hls.js 5.22 s / 135 frames decoded, dash.js 5.08 s / 129 frames; 0 dropped, 0 player errors). This also covers
  DASH with a real client, which the ffmpeg DASH-demuxer frame-count quirk could not.
* **Worker handler (`examples/edge-worker`, `just edge-worker-test`):** runtime-neutral `handler.mjs` (Workers /
  Deno / Fastly / Node 18+), Workers entry `index.mjs`, `wrangler.toml`. Origin = HTTP range requests or an R2-style
  bucket binding. Segments/inits are `Cache-Control: immutable`, playlists 300 s, weak ETag from the origin,
  `If-None-Match` -> 304, HEAD, 404/405/502 (an origin that ignores `Range` is a 502, never a full download).
  Node test: all output files byte-identical to the native packager for a 645 KiB clip and for the 85 MB / 120 s
  moov-at-end clip; on the latter the whole stream cost **21 origin range requests reading every byte exactly once**
  (83,708 KiB of 83,708 KiB), and a cold segment costs ~7 requests (index + its own ~6 MB).
* **Not done / not verified:** a real Cloudflare deployment (no account here), real R2, Workers CPU/memory limits
  under load, `Range` on segment responses, a segment cache layer inside the Worker, Safari/hardware players.

### Live, royalty-free first (2026-10-04) — AV1 / VP9 / Opus

Direction change from the user: H.264/AAC are secondary; lead with **AV1, VP9, Opus** (see the memory note).
First confirmed the existing VOD packager is already exact for them: AV1+Opus and VP9+Opus `package`d output
decodes in ffmpeg to **frame-MD5-identical** video (300/300) and audio (601/601), and plays in hls.js and dash.js in
Chrome (135 frames, 0 dropped, 0 errors, HLS and DASH).

* **`demux::mkv_stream::MkvStream`** — sans-IO incremental WebM/Matroska parser for live ingest: any chunking,
  unknown-size `Segment`/`Cluster`, `SimpleBlock` and `BlockGroup`, laced blocks rejected, element and buffering
  caps. Emits `StreamInfo` with MP4-ready config records: `av1C` straight from `CodecPrivate`, **`vpcC` built from
  the first VP9 key-frame header** (`rfconfig::vp9_config_from_frame`, level from picture size), **`dOps` from
  `OpusHead`** (`opus_head_to_dops`; pre-skip becomes the edit-list media time). Opus packet durations come from the
  TOC byte (`opus_packet_samples`). Verified against `ffprobe` frame-for-frame on real AV1+Opus and VP9+Opus WebM
  as a file, as unknown-size (live) layout, as ffmpeg's live pipe (VP9), byte-at-a-time and at odd chunk sizes,
  plus a never-panics proptest. (ffmpeg 6.1 cannot write AV1 to a *live* WebM; the unknown-size AV1 case is a
  size-patched copy of the finite file.)
* **`package::live::LivePackager`** — pure, codec-agnostic live fMP4 HLS state machine: lead track = first video,
  cuts at key frames >= target length, sliding window (`EXT-X-MEDIA-SEQUENCE`, retains window+3), `ENDLIST` on
  finish, init/segment/master/media playlists. Video 90 kHz; Opus 48 kHz with TOC-derived durations and synthesised
  gapless timestamps (re-anchor at 100 ms drift); audio that arrives before the first video key frame is buffered
  and pruned by timestamp (a bug found by test: it used to be dropped by arrival order).
* **`stream::LiveServer`** (tokio, `tpt-kinetix live --port N`): `POST|PUT /ingest/<key>` takes a WebM (chunked,
  length-delimited or until EOF, `Expect: 100-continue`), `GET /<key>/master.m3u8|track-N.m3u8|init-N.mp4|seg-N-M.m4s`.
  CORS enabled; one publisher per key.
* **Verified with real tools:** ffmpeg publishing AV1+Opus and VP9+Opus over HTTP POST -> the served HLS decodes
  to **250/250 identical video frames** and identical audio (all but the final frame, whose Matroska
  `DiscardPadding` trim a passthrough fMP4 does not carry); a real-time (`-re`) publish shows the playlist
  *live* mid-stream (no `ENDLIST`, window <= 3 slides) and complete afterwards; and **hls.js in headless Chrome
  plays the stream while it is still being published** (VP9 and AV1: 130 frames, 0 dropped, no errors;
  `just live-browser-test`).
* **Not done:** low-latency HLS (`EXT-X-PART`, blocking reload) so latency is ~3 segments; a *dynamic* DASH MPD;
  Enhanced RTMP ingest (FourCC `av01`/`vp09`; ffmpeg 6.1 sends no Opus over RTMP) and WHIP/WebRTC; multiple renditions;
  DVR/recording; auth on `/ingest`; HTTP/2 (browser `fetch` upload streaming needs it); a Worker/edge live variant.
