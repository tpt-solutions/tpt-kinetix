# Migrating from ffmpeg / ffprobe

This page maps the common ffmpeg and ffprobe jobs onto `tpt-kinetix`. Kinetix
is an I/O layer: it copies, packages and serves media, and it decodes only
royalty-free codecs. Anything that needs a filter graph or an encoder for a
patent-encumbered codec has no equivalent here.

Every command below is shown with the flags the CLI takes today. Run
`tpt-kinetix <command> --help` for the full list.

## Command mapping

| You want | ffmpeg / ffprobe | tpt-kinetix |
|---|---|---|
| List streams | `ffprobe -show_streams in.mp4` | `tpt-kinetix probe in.mp4` |
| Stream info as JSON | `ffprobe -of json -show_streams in.mp4` | `tpt-kinetix probe --json in.mp4` |
| Probe a remote MP4 | `ffprobe https://host/in.mp4` | `tpt-kinetix probe https://host/in.mp4` (range requests) |
| Copy to MP4 | `ffmpeg -i in.webm -c copy out.mp4` | `tpt-kinetix remux in.webm out.mp4` |
| Copy, index first | `ffmpeg -i in.mp4 -c copy -movflags +faststart out.mp4` | `tpt-kinetix remux in.mp4 out.mp4 --faststart` |
| Copy to fragmented MP4 | `ffmpeg -i in.mp4 -c copy -movflags frag_keyframe+empty_moov out.mp4` | `tpt-kinetix remux in.mp4 out.mp4 --fragmented` |
| Fragmented MP4 to stdout | `ffmpeg -i in.mp4 -c copy -movflags frag_keyframe+empty_moov -f mp4 -` | `tpt-kinetix remux in.mp4 - --fragmented` |
| Copy to WebM | `ffmpeg -i in.mp4 -c copy out.webm` | `tpt-kinetix remux in.mp4 out.webm` |
| Copy one video track to IVF | `ffmpeg -i in.webm -map 0:v:0 -c copy out.ivf` | `tpt-kinetix remux in.webm out.ivf` |
| HLS and DASH, fMP4 segments | `ffmpeg -i in.mp4 -c copy -f hls out/index.m3u8` | `tpt-kinetix package in.mp4 out/` (HLS and DASH) |
| Serve an MP4 as HLS | (no direct equivalent: write segments, then serve them) | `tpt-kinetix serve in.mp4 --port 8080` |
| Live ingest to HLS | `ffmpeg -re -i in -c copy -f webm -method POST http://host:8080/ingest/cam` | `tpt-kinetix live --port 8080`, then publish with the same `ffmpeg` command and play `/cam/master.m3u8` |
| Transcode VP9 to AV1 | `ffmpeg -i in.mp4 -c:v libaom-av1 out.mkv` | `tpt-kinetix transcode -i in.mp4 -o out.ivf` (VP9 input; see `transcode --help`) |

## Behaviour that differs

- **Copy only.** `remux` never re-encodes. ffmpeg filters (`-vf`, `-af`,
  `-filter_complex`) have no equivalent. Stream selection is automatic: `remux`
  keeps every video and audio track its output container can carry and says
  which ones it skipped.
- **Container from the output name.** `.webm`/`.mkv` writes WebM, `.ivf`
  writes one AV1 or VP9 track, and anything else writes MP4. The input is
  detected from its first bytes, not its name.
- **`--faststart` and `--fragmented` are MP4-only.** They are refused for WebM
  and IVF output, and writing a progressive MP4 to stdout is refused: a
  progressive MP4 must seek back, so use `--fragmented` with `-`.
- **Packaging is fMP4, not MPEG-TS.** `package` writes CMAF-style fMP4 segments
  for HLS and DASH, not `.ts` segments. Players that need MPEG-TS HLS will not
  play its output.
- **`probe --json`** prints JSON shaped like `ffprobe -of json`. The field set
  is a subset; compare the fields you use before relying on it.

## Codecs

Kinetix decodes VP9, AV1 and Opus. It copies other codecs through `remux`
without decoding them, as ffmpeg's `-c copy` does, but it cannot decode them.
H.264 and AAC are not published from this repository (see `PATENTS.md`), so
there is no encoder or decoder for them here.

## Not available yet

These ffmpeg features are on the roadmap (`todo.md`) and do not work yet:

- thumbnails and keyframe extraction (`ffmpeg -ss ... -frames:v 1`);
- WebVTT subtitles, subtitle groups and multiple audio renditions;
- CENC or CBCS encryption;
- streaming MPEG-TS demux (the TS demuxer reads the whole input);
- Ogg and Opus muxing;
- metadata editing (chapters, cover art).
