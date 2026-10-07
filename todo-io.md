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
      Extended timestamps on continuation chunks are handled and tested (`chunk.rs`). RTMP publishers are now held to the
      ingest policy too (2026-10-06): token in the stream key (`name?token=...`, the OBS convention), stream-count /
      concurrency limits, and byte / bitrate / duration cut-offs, with the cut-off publish still finishing its playlists
      (`live_rtmp.rs::rtmp_publish_honours_token_and_limits`). **Enhanced RTMP v2 Multitrack DONE 2026-10-07**, to the
      real spec layout (checked against the veovera spec text; the earlier parser guessed it wrong: `Multitrack` is
      packet type 6 and `ModEx` 7, not 5/6): `AvMultitrackType<<4|VideoPacketType`, shared or per-track FourCC,
      `trackId`, a 24-bit `sizeOfVideoTrack` (absent for OneTrack), and ModEx prefixes skipped. Each track is its own
      `RtmpMediaEvent::Video` (`tag.track_id`) and becomes its own packager track, i.e. an OBS Enhanced Broadcasting
      publish is a multi-rendition ladder (`live_rtmp.rs::multitrack_rtmp_publish_becomes_a_ladder`: two renditions,
      each decoding frame-exactly). Parser tests + 30k random-input no-panic test + fuzz target updated. `Metadata`
      (HDR colour info) is surfaced as an event but not yet forwarded into `colr`/`mdcv`/`clli`. Open follow-ups: the
      E-RTMP v2 capability exchange (`capsEx` / `videoFourCcInfoMap`; another session has started it), RTMPS (TLS;
      in progress elsewhere), audio multitrack, and a real OBS pass. RTMP idle timeout DONE 2026-10-06 (a per-publisher watchdog in `rtmp_live.rs` completes the
      presentation and frees the slot when a publisher goes silent with the socket open; `rtmp_idle_publisher_is_ended`).
- [ ] **Low-latency HLS** built 2026-10-04; **first real-player measurement 2026-10-06** (`just latency-test`,
      `tools/latency-test.sh`: headless Chrome publishes a wall-clock barcode through MediaRecorder + WebSocket, plays the
      served HLS with hls.js 1.7.3 and decodes the barcode from the playing frame, so the number is true
      glass-to-glass: capture, VP9 encode, ingest, packaging, HTTP, decode). Same box, 2 s segments:
      parts off, hls.js default: **~4.2 s** (tight, p95 ≈ p50); parts 0.333 s + `lowLatencyMode`: **~1.6-1.8 s** at best.
      **It found four real server bugs, all fixed:** (1) a blocking reload for a *part of the segment in progress*
      waited for the whole segment (`satisfies` returned false for `msn == next`); (2) `_HLS_msn` was treated as a
      delta request, so every LL playlist carried an unrequested `EXT-X-SKIP` that crashed hls.js ("Previous playlist
      missing segments skipped", ~4 s in) — `_HLS_skip=YES|v2` is now parsed and is the only thing that skips, with
      `MEDIA-SEQUENCE` staying the full playlist's first segment and `CAN-SKIP-UNTIL` = 6 target durations;
      (3) `PART-HOLD-BACK` was 12 *seconds* — now 3 x `PART-TARGET`, and `PART-TARGET` covers the longest published
      part (parts ran 0.36 s against a 0.333 s target); (4) the `EXT-X-PRELOAD-HINT` part URL answered 404 instead of
      blocking until published. Regression tests: `ll_playlist_tags_follow_the_spec`, `ll_hls_parts_are_served_*`.
      **STILL OPEN — hls.js drops audio segments in low-latency mode.** Narrowed a lot (2026-10-07). With a steady
      `ffmpeg -re` publisher, hls.js LL shows 2-5 `bufferStalledError` and sometimes `fragGap` per 30 s and its latency
      drifts to 4-10 s, **but only with an audio rendition**: the identical stream *video-only* is stall-free
      (1053 frames, 0 events, ~1.3 s), and dash.js plays the same video+audio output steadily at 1.9 s. The proxy log
      shows what happens: hls.js fetches every video part but skips whole *audio* segments (e.g. 9, 16, 17 of 24; 109
      audio part requests vs 134 video), and the hole starves the audio buffer. Ruled out: server part availability
      (median 344 ms between parts, max 523 ms), part contents (durations match, contiguous), publisher gaps, hold-back
      distance (`liveSyncDuration` 1.5-3.5 s), the catch-up speed-up, background-tab timer throttling (flags added to
      the harness), the Chrome 6-connections-per-host limit (Resource Timing: no request queued > 65 ms), and playlist
      structure (polled 21 segments: audio and video playlists describe every segment identically) and start-time
      alignment (`tfdt` + sample durations of 5 audio/video segment pairs: identical start and end, 0.0 ms apart).
      hls.js's own fragment tracker marks a fragment a gap when an elementary stream has no buffered data inside its
      time range (`addAsGap`), so the remaining suspect is hls.js's own alternate-audio handling in LL mode (initPTS /
      per-part start times / how it decides an audio fragment is already covered).
      Next experiments: mux audio into the video track (no alternate rendition), compare with a reference LL-HLS server,
      log hls.js `audio-stream-controller` decisions at debug level around a skipped segment. Treat ~4 s (non-LL) as the
      dependable HLS number and ~1.6-1.8 s as the LL floor for video-only. Fixed on the way: a blocking reload for a
      part index beyond a *completed* segment waited the full cap and 404'd (it must answer at once).
      Harness knobs: `EXTERNAL=1 KEY=..` (play an ffmpeg publisher), `SYNC`, `RATE`, `DEBUG_HLS`, `PLAYER=dash`, `SWITCH`.
- [x] **Dynamic DASH MPD** for live, 2026-10-04: `LivePackager::dash_mpd()` emits `type="dynamic"` with
      `availabilityStartTime`, a `minimumUpdatePeriod` (half a segment), `minBufferTime`, and a
      `SegmentTimeline` over the current sliding window with `t` on the first entry — no
      `mediaPresentationDuration`. Served at `GET /<key>/manifest.mpd` as `application/dash+xml`,
      naming the *same* `init-N.mp4` / `seg-N-M.m4s` URLs the HLS playlists do, so a player can
      switch between the two manifests and a segment is byte-identical either way. Tested over
      real HTTP (both AV1 and VP9 publishes): the manifest is well-formed XML and every segment it
      names is fetchable and `moof`/`styp`-headed. **Low-latency DASH 2026-10-07**: with parts on, a segment is now the
      concatenation of its parts (a completed segment = the chunks served so far + its tail, byte for byte), a request
      for the segment in progress is *streamed* with HTTP chunked framing as each CMAF chunk is published
      (`live_webm.rs::ll_dash_segment_streams_while_it_is_published`: several chunks, first one >= 0.5 s before the end,
      bytes identical to the finished segment), and the MPD was rewritten to be a correct live manifest: real
      `availabilityStartTime` (it was `1970-01-01T<time of day>`), per-`S` `t` = the real `tfdt` (it was always 0 for the
      first entry, wrong once the window slides), `availabilityTimeOffset`/`availabilityTimeComplete` on the
      `SegmentTemplate` (they were on the `MPD` root, which is invalid), the in-progress segment listed in the timeline,
      `timeShiftBufferDepth`, `ServiceDescription` latency target and `UTCTiming`. **Real player VERIFIED 2026-10-07: dash.js 4.7.4
      plays it steadily, glass-to-glass 1.90 s median / 2.0 s p95** (965 frames, no errors) with the same barcode
      harness as the HLS numbers. (The long chase before that was a bug in *my test page*: it called `video.play()`
      itself right after `dash.initialize(..., autoplay)`, so dash.js missed the native `play` event, never learnt
      playback had started, never armed its manifest-refresh timer and went silent after two segments. ffmpeg's own
      reference LL-DASH manifest failed identically, which is what exposed it.)
- [x] **WHIP (WebRTC-HTTP ingest)** DONE 2026-10-07 — decision: use the **sans-I/O `str0m`** stack with its pure-Rust
      crypto (`rust-crypto` feature: DTLS/SRTP/ICE, no OpenSSL, no C), behind the optional `whip` cargo feature
      (`cargo build --features whip`; CLI `--features whip`, flags `--whip-candidate-ip`, `--whip-udp-port`), so the
      default build stays dependency-light and the memory-safety story holds. `POST /whip/<key>` (SDP offer,
      `Content-Type: application/sdp`) -> `201` + SDP answer + `Location`; `DELETE <location>` ends the session;
      bearer / `?token=` auth and the stream-count / concurrency limits apply; CORS preflight and `Location`
      exposure for browsers. The media path (VP9 + Opus) is translated into synthetic events for the *same*
      `RtmpLiveSession` the RTMP ingest uses, so limits, idle timeout, recording, reconnect/discontinuity and
      ladders all apply unchanged. VERIFIED with a real browser: headless Chrome with a fake camera publishes over
      WHIP (`TRANSPORT=whip just browser-publish-test`; the `/publish` page has a WHIP transport) and the live HLS
      decodes in ffmpeg (122 frames from the first 3 segments); HTTP-level tests in `tests/whip.rs`. Four things
      only a real browser showed: Windows turns an ICMP "port unreachable" from an unreachable ICE candidate into
      an error on the *next* `recv_from` (ignored now), str0m needs the true local address of each datagram (one UDP
      socket per advertised address, not one on 0.0.0.0), a WebRTC encoder sends a key frame only at the start and
      on request (so the server sends a PLI every segment duration, or no segment is ever cut), and the VP9
      configuration must come from the first key frame. OPEN: AV1 over WebRTC (needs `av1C` built from the sequence
      header), trickle ICE / `PATCH`, A/V alignment from RTCP sender reports (str0m already surfaces the NTP<->RTP
      mapping; today each track is aligned by the arrival time of its first packet), TURN/STUN for publishers
      behind NAT (`--whip-candidate-ip` advertises a public address), an OBS WHIP pass, a session metric.
- [x] **Browser publish** DONE 2026-10-06: chose **WebSocket ingest** over streaming `fetch` (Chrome's upload streaming
      needs HTTP/2 over TLS, which the server does not speak). `GET /ingest/<key>` with `Upgrade: websocket` feeds binary
      messages into the same ingest path as HTTP POST, so tokens (`?token=`, since browsers cannot set WebSocket
      headers), limits, reconnect/discontinuity, recording and metrics all apply; refusals are plain HTTP errors (a
      failed handshake) and an ended publish sends a close frame (1000 ok / 1007 bad data / 1008 policy / 1009 size).
      Hand-written RFC 6455 server side (`ws.rs`, no new dependency; SHA-1 + base64 checked against the RFC's
      vector). The server serves a ready-made page at **`/publish`** (camera or screen -> `MediaRecorder` AV1/VP9 +
      Opus, 250 ms chunks). VERIFIED with a real browser: `just browser-publish-test` runs headless Chrome with a fake
      camera/mic, which publishes AV1 + Opus; the live HLS appears, `/metrics` shows the publisher, and the segments
      decode in ffmpeg (303 frames from the first 3). Rust-level tests in `tests/ws_ingest.rs`. OPEN: `wss://` (needs
      TLS termination in front or `rustls` on the HTTP port), WebSocket ping keep-alive from the server, compressed
      frames (permessage-deflate is not offered), publishing from non-Chromium browsers (Firefox/Safari
      MediaRecorder does not produce AV1/VP9 WebM everywhere).
- [ ] **Ingest hardening** — HTTP/WebSocket/RTMP ingest DONE 2026-10-06 (`IngestPolicy`, `LiveServer::with_policy`, `policy.rs`; CLI
      `live --publish-token/--idle-timeout/--max-streams/--max-bitrate-kbps/--reject-concurrent`): bearer/`?token=`
      auth (global + per-key, constant-time compare) -> 401; idle timeout -> 408; max duration -> 408; max bytes ->
      413; sustained-bitrate cap (after a 2 s grace) -> 429; max live streams -> 503; optional 409 on a second
      publisher for a live key; `GET /metrics` (Prometheus text: active publishers, started/refused/cut-off
      counters, bytes in, playback requests). A cut-off publish still finishes its playlists. Tested over real
      HTTP in `tests/ingest_policy.rs`. STILL OPEN: per-key bitrate/duration overrides, bounded memory under
      slow *viewers* (responses are whole buffers today), auth on `/metrics`.
- [x] **Reconnect handling (HTTP ingest)** DONE 2026-10-06: a publisher that drops and re-POSTs under the same key with
      the same codec configuration *resumes* the presentation — `LivePackager::set_tracks` on a finished stream reopens
      it; segment numbers and the media timeline continue (incoming timestamps are offset to the end of the previous
      publish), the first new segment is preceded by `EXT-X-DISCONTINUITY`, and `EXT-X-DISCONTINUITY-SEQUENCE`
      counts discontinuities that slid out of the window. A *changed* configuration (extradata or dimensions —
      VP9's `vpcC` has no size) starts a fresh presentation, the old one ending with `ENDLIST`. Tests:
      `tpt-kinetix-package/tests/live.rs` (resume, config change) and `tpt-kinetix-stream/tests/live_webm.rs`
      (over real HTTP). OPEN: RTMP ingest still replaces on reconnect (`LiveServer::begin`); DASH has no new
      `Period` (timeline is kept continuous instead); a resumed stream is not marked in `_stats`.
- [x] **Recording / DVR** DONE 2026-10-06 (`Recorder`, `LiveServer::with_recording`, CLI `live --record-dir`): finished
      segments are persisted as they complete (`<dir>/<key>/g<N>/{init-T.mp4,seg-T-N.m4s,track-T.m3u8,master.m3u8}`,
      atomic write-then-rename) and served at `/<key>/dvr/<file>`; the track playlist is `EVENT` while the
      publish runs and `VOD` + `ENDLIST` after, so a viewer can seek back to the start live or later. A
      reconnect with the same config extends the same generation (with its discontinuity); a new presentation
      (config change, RTMP reconnect, server restart) starts generation N+1 so nothing is overwritten. Works for
      HTTP and RTMP ingest. Test: `live_webm.rs::recording_serves_a_vod_after_the_publish_ends` (tiny live
      window, full recording, reconnect, ffmpeg decodes the concatenated recording). Retention DONE: `RecordingLimits` (`--record-depth-secs` rolling DVR
      depth: oldest segments deleted + playlist slides, `--keep-generations` prunes old generations; tested in
      `recording_depth_and_generations_are_bounded`). Byte budget DONE (`RecordingLimits::max_bytes`, `live --record-max-mb`:
      oldest segments across tracks deleted, newest per track always kept; `recording_byte_budget_trims_oldest_segments`).
      OPEN: DASH static MPD for the recording,
      listing generations, serving older generations,
      recording survives restart only as files (no in-memory index rebuilt, so a restarted server serves
      the newest generation's files but does not extend it).
- [x] **Multi-rendition ladders without transcoding** DONE 2026-10-07 (the publisher supplies the renditions; the server
      packages them): the packager accepts several video tracks that share key frame timestamps (non-lead renditions
      cut on the lead's boundaries and join at their first key frame); the HLS master lists one variant per rendition
      (own `RESOLUTION`/`BANDWIDTH`, shared audio group) and the DASH MPD puts them in one `AdaptationSet`. Inputs:
      a WebM with several video tracks (`a_multi_rendition_ladder_is_packaged_per_rendition`: each rendition decodes
      frame-exactly vs ffmpeg) and **Enhanced RTMP v2 Multitrack** (below). Real player: hls.js sees both levels
      (320x180, 640x360), switched 1->0->1 mid-stream, 934 frames, 0 errors. Server-side transcoding to build a ladder
      from a single input is NOT done and is a different decision: it needs decode + scale + encode in real time, and
      the in-tree AV1/VP9 decoders are far slower than real time at useful sizes (see the 2026-10-03 direction note).
- [x] Opus pre-skip carried end to end: the `dOps` pre-skip becomes `OpusHead` on ingest and `CodecDelay`
      + `SeekPreRoll` on WebM output (measured against ffmpeg: without it the stream starts 7 ms late).
- [x] **`DiscardPadding` (end trim)** DONE 2026-10-06: `MkvStream` parses it and `WebmWriter` re-emits it in a
      `BlockGroup`; a WebM->WebM remux now decodes frame-identically to the source *including* the final
      trimmed frame (312 samples). The two causes of the old 803-vs-312 mismatch: `CodecDelay`/`SeekPreRoll`
      were written inside the `Audio` master instead of `TrackEntry` (ffmpeg saw `initial_padding=0` and
      shifted every timestamp +7 ms), and `DiscardPadding` is a *signed* int, so a minimal-width unsigned
      encoding of 13.5e6 ns (`CD FE 60`, top bit set) read back negative. The value is nanoseconds, not samples.
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
- [x] `styp` + `sidx` on VOD media segments DONE 2026-10-07 (`FragmentWriter::with_segment_index`; single-track segments get a v1 one-reference `sidx`, used by `Packager::media_segment`; test in `package.rs`). Still open: `sidx` for live segments (parts stay bare `moof`+`mdat`) and a whole-file `sidx` for `remux --fragmented`; verify with the DASH-IF validator.

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
- [~] Matroska `Cues` **parsing** done 2026-10-04: `MkvStream` emits `MkvEvent::Cue` (`CueTime` /
      `CueTrack` / `CueClusterPosition`) and `MkvReader::cues()` returns them time-sorted with
      *absolute* cluster offsets (Matroska states them relative to the Segment's data, which is
      captured as the Segment's header is consumed). Tested against a real file: every cue points at
      an actual `Cluster` element that holds a key frame at the stated time. Two bugs found and fixed
      while testing — `segment_data_start` was captured before the Segment header was skipped, and
      `CueTrackPositions` itself was ending the cue before its children were read.
      **Using the cues to skip I/O is NOT possible for a local file** — Matroska stores `Cues`
      *after* every cluster, so reading them means reading the whole file anyway. What it buys is a
      validated index a web-demuxer-style client (or a cached one) can seek with. Still open:
      actually serving that index / cue-based seeking over HTTP.
- [ ] MPEG-TS: streaming demux (`TsDemuxer` still takes a `Vec`) and `StreamInfo` (M1/M2 for TS).
- [~] IVF (AV1/VP9) demux done 2026-10-04 (`IvfDemuxer`: fourcc, geometry, frame ranges, seek,
      truncated-tail tolerant, `probe` support incl. `--json`; values match `ffprobe`). IVF mux and
      Ogg/Opus (`.opus`) demux/mux still open.
- [ ] Metadata/chapters/`udta`/cover art passthrough in `remux`; non-seekable progressive output; MP4 `elst` multi-edit.
- [ ] `av1C`/`vpcC` synthesis when a container omits them (AV1 sequence-header OBU parse; VP9 from key frame is done).
- [x] Removed the remaining `std::fs::read` callers, 2026-10-04. `DemuxStage` now holds a
      `Box<dyn ReadAt + Send>` and demuxes through `Mp4Reader` (positional reads) instead of a
      `Vec<u8>`; a `Vec<u8>` still satisfies `ReadAt` so existing callers are unaffected. The CLI
      `transcode` opens the file and hands it straight to the pipeline, so a multi-gigabyte input is
      never resident; the geometry and codec probes each take their own short-lived handle. A test
      pins that a file-backed source yields byte-identical packets to an in-memory one. The
      whole-buffer reads left in `probe` are inherent — `TsDemuxer` and `IvfDemuxer` take a slice.
- [x] **`transcode` no longer panics on real VP9 input.** Two bugs, both pre-existing
      (`a73aa41`), both fixed. (1) `loop_filter.rs`: the chroma edge mask shifted in `u16` where the
      reference shifts in `u32` and truncates — a shift of 16 overflowed and killed the stage thread.
      See `todo-vp9.md`. (2) `write_ivf` wrote `u32` width/height into 16-bit IVF fields, shifting
      every later header field by two bytes; `probe` then reported `160x0` and libdav1d rejected the
      file with "No sequence header available". Both fixed with regression tests.
      `transcode` now completes and emits a valid AV1 IVF whose geometry (160x120), frame count (25)
      and clean decode all match `ffprobe`.
- [x] **transcode output pixels FIXED 2026-10-06.** The earlier raw-YUV dump that said VP9 *decode*
      was garbage was right; the conformance suite only looked narrow because the corpus pinned
      `-cpu-used 4` encodes, which never emit `TX_MODE_SELECT`. Widening the matrix showed every
      ordinary libvpx encode (`-cpu-used` 0-3, realtime) corrupted from the first keyframe, and an
      instrumented-libvpx symbol diff root-caused **five** decoder bugs: the inverted `bs >= BS_8X8`
      tx-size guard, the inter tx-size read after (not before) the mode info, sub-8x8 `fill_mv`
      comparing a tree leaf against mapped mode ids, sub-8x8 chroma MC sized to the block shape
      instead of one full 4x4, and intra-in-inter sub-8x8 reading four y-mode trees where 8x4/4x8
      read two. All fixed; a 15-case encoder-parameter matrix, ordinary 25-frame clips and the VP9
      suite decode byte-exact. Four new corpus cases pin the paths (see todo-vp9.md). The remaining
      `fixtures/div128` "luma loop-filter edge divergence" (300-frame perf clip diverging from
      frame 128 only) was CLOSED 2026-10-06: it was the n>4 D153/D117 intra predictors, not the
      loop filter — all clips now decode byte-exact (see todo-vp9.md and the rewritten div128
      README, which also documents that the isolation had been misled by a corrupted
      instrumented oracle).

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
