# Player and browser compatibility

What Kinetix's packaged and live output has been checked against. Each row says how it was
checked. "Not checked" means no test in this repository has run it, so it is not a promise.

## Packaged VOD (HLS fMP4 and DASH)

| Client | Content | Status | How it was checked |
| --- | --- | --- | --- |
| ffmpeg (decode) | HLS and DASH, AV1 / VP9 + Opus | ✅ frame-exact | `tpt-kinetix-cli/tests/*`, framemd5 vs source |
| ffmpeg (decode) | HLS, H.264 + AAC (passthrough) | ✅ 150 frames, no errors | 2026-11-08, `docs/what-works-with-my-files.md` |
| hls.js (headless Chrome) | HLS, VP9 + Opus | ✅ plays | `just browser-package-test` |
| hls.js (headless Chrome) | subtitles (WebVTT renditions) | ✅ cues shown | `tools/browser-subtitle-test.sh` |
| hls.js (headless Chrome) | I-frame trick-play | ❌ not checked | hls.js does not scrub with I-frame playlists; no browser test |
| dash.js (headless Chrome) | DASH, VP9 + Opus | ✅ plays | `just browser-package-test` |
| dash.js + ClearKey EME | DASH, CENC `cenc` and `cbcs` | ✅ plays; without the key, 0 frames | `just browser-cenc-test` |
| hls.js + ClearKey | HLS, `cenc` (`SAMPLE-AES-CTR`) and `cbcs-pattern` (`SAMPLE-AES`) | ✅ plays, 133 frames | `CENC_PROTOCOL=hls tools/browser-cenc-test.sh` |
| Safari / iOS (native HLS) | fMP4 HLS, VP9 or AV1 | ❌ not checked | No Safari in this repository's tests |
| Hardware TV / set-top players | any | ❌ not checked | No device available |
| DASH-IF validator / Shaka `--dump` | DASH MPD | ❌ not checked | Open item in `todo-io.md` |

## Live (low-latency HLS and dynamic DASH)

| Client | Ingest | Status | How it was checked |
| --- | --- | --- | --- |
| hls.js (headless Chrome) | WebSocket browser publish, AV1 or VP9 + Opus | ✅ plays | `just browser-publish-test` |
| hls.js (headless Chrome) | WHIP, AV1 and VP9 | ✅ decodes | `TRANSPORT=whip-av1 tools/browser-publish-test.sh` |
| hls.js (headless Chrome) | LL-HLS with audio | ⚠️ about 2 s, 1-2 stalls on a smooth publisher | `todo-io.md` latency section; start-up stall still open |
| dash.js (headless Chrome) | dynamic DASH, low-latency | ✅ 1.9 s median glass-to-glass | `todo-io.md` latency section |
| hls.js | LL-HLS subtitles | ✅ 7 cues, 3 runs | `tools/live-subtitle-browser-test.sh` |
| Safari / iOS (native) | live HLS | ❌ not checked | No Safari in this repository's tests |
| ffmpeg publisher (Enhanced RTMP, VP9 and AV1) | live HLS, decoded end to end | ✅ | `todo-io.md`, RTMP section |

## Known limits

- **Apple fMP4 HLS and VP9 / AV1.** Support for VP9 and AV1 in native Apple playback varies by OS
  version, and this repository has not tested it. Expect H.264 + AAC to be the safe choice for
  Safari and iOS until a device test says otherwise. A Safari pass is an open item in `todo.md`.
- **MPEG-TS HLS is not produced.** Output is fMP4 only, so players that need TS segments will not
  play it.
- **Ingest codecs.** Live ingest takes AV1 or VP9 with Opus only (see `what-works-with-my-files.md`).
