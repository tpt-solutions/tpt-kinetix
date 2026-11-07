# What works with my files

Most media in the wild is H.264 video with AAC audio in an MP4. Kinetix does not decode
or encode either codec (see [PATENTS.md](../PATENTS.md)), but it can still copy them
through containers and package them. This page says what works for each kind of input,
checked on real files.

## Quick answer

| Your file | `probe` | `remux` to MP4 | `remux` to WebM | `package` (HLS/DASH) | `sanitize` | `live` ingest |
| --- | --- | --- | --- | --- | --- | --- |
| H.264 + AAC MP4 | ✅ | ✅ | ❌ (no H.264 in WebM) | ✅ passthrough | drops both | ❌ |
| AV1 or VP9 + Opus MP4 | ✅ | ✅ | ✅ | ✅ | keeps all | ✅ (WebM, WHIP, RTMP) |
| AV1 or VP9 + Opus WebM/MKV | ✅ | ✅ | ✅ | ✅ | keeps all | ✅ |
| IVF (AV1 or VP9) | ✅ | ✅ | ✅ | not supported (`package` takes MP4 and WebM) | not supported | ❌ |
| MPEG-TS (H.264 + AAC) | ✅ | ❌ (TS input not remuxed yet) | ❌ | not supported | not supported | ❌ |

"Passthrough" means the packets are copied byte for byte: no decode, no re-encode. Kinetix
never changes H.264 or AAC.

## What was checked

Checked on 2026-11-08 with a 6-second, 320x180 H.264 + AAC MP4 made by ffmpeg:

- `probe` reports both tracks with the right codec, resolution, sample rate and duration.
- `remux in.mp4 out.mp4 --faststart` writes 150 video frames. ffmpeg decodes it with no errors.
- `package in.mp4 out/` writes HLS and DASH with `avc1` codec strings. ffmpeg decodes
  `master.m3u8` with no errors and reads 150 video frames.
- `remux in.mp4 out.webm` stops with `no AV1, VP9 or Opus track in the input`. That error is
  correct but does not say why, so it is tracked as a message fix in `todo.md`.
- `doctor` reports H.264 and AAC as playable in every major browser, which is true for
  browsers. Kinetix still cannot make or change that content.

## Where it stops

- **Transcoding.** `transcode` takes only VP9 input and writes AV1. There is no H.264 or
  AAC decode, so there is no way to make an AV1 copy of an H.264 file here.
- **Live ingest.** Live ingest accepts AV1 or VP9 with Opus only. H.264 over RTMP or WHIP is
  refused.
- **Encryption.** CENC refuses H.264 because whole-sample encryption would break its NAL
  length prefixes. AV1, VP9 and Opus are the supported encrypted paths.
- **Sanitize.** `sanitize` keeps only AV1, VP9 and Opus, so it removes H.264 and AAC on purpose.
- **MPEG-TS input.** TS input is probed and read, but there is no remux of TS into MP4 or
  WebM for H.264 and AAC, and no TS seeking.

If you need to decode or encode H.264 or AAC, use a codec library you have licensed
separately, then feed the result to Kinetix as AV1 or VP9 with Opus.
