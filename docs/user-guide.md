# User guide: remux, package, serve, live

This guide covers the four commands that move media around. Each section gives the
common job first, then the flags that change it. `tpt-kinetix <command> --help` has
the full list. For ffmpeg equivalents, see [migrating from ffmpeg](migrating-from-ffmpeg.md).

Kinetix copies and packages media without decoding it. It takes royalty-free
codecs (AV1, VP9 video; Opus audio) on every path that needs to be fully supported.
Other codecs can pass through the containers, but no decoding or encoding of them is
offered (see [PATENTS.md](../PATENTS.md)).

## remux: change the container

```sh
tpt-kinetix remux in.webm out.mp4          # WebM or MKV to MP4
tpt-kinetix remux in.mp4 out.webm          # MP4 to WebM
tpt-kinetix remux in.mp4 out.mp4 --faststart          # index first, plays while downloading
tpt-kinetix remux in.mp4 out.mp4 --fragmented         # fMP4: init segment plus fragments
tpt-kinetix remux in.mp4 - --fragmented > out.mp4     # fMP4 to stdout, nothing seeks
tpt-kinetix remux in.webm out.ivf          # one AV1 or VP9 video track as IVF
tpt-kinetix remux https://host/in.mp4 out.mp4          # remote input, range requests
```

- The output format follows the extension: `.webm` and `.mkv` write WebM, `.ivf`
  writes one video track as IVF, anything else writes MP4.
- `--fragment-ms` sets the fragment length; fragments end at the next video key frame.
- Text metadata (title, artist, album and similar tags) and chapters carry across. MP4 to MP4 copies the
  raw `udta` box, so cover art comes along. Cover art is dropped for WebM output, and editing metadata is
  not supported.

### sanitize: re-mux untrusted uploads

```sh
tpt-kinetix sanitize upload.webm clean.webm
```

Keeps only AV1, VP9 and Opus tracks. It refuses an input over the packet, count or
byte limits and removes any partial output. Use it on files from users before you
store or play them.

## package: VOD HLS and DASH

```sh
tpt-kinetix package in.mp4 out/                   # HLS and DASH, fMP4 segments
tpt-kinetix package in.mp4 out/ --segment-seconds 4
```

The output directory holds `master.m3u8`, one `track-N.m3u8` per track,
`init-N.mp4` and `seg-N-n.m4s` files, plus `manifest.mpd` for DASH. Audio tracks
become HLS renditions in one group. WebVTT subtitle tracks become `subs.m3u8` and
`sub-N.vtt` renditions. Trick-play I-frame playlists are included.

### Encryption (CENC / ClearKey)

```sh
tpt-kinetix package in.mp4 out/ \
  --cenc-key-id 00112233445566778899aabbccddeeff \
  --cenc-key    0f0e0d0c0b0a09080706050403020100
# or the cbcs scheme (AES-CBC, every block), or cbcs-pattern (AES-CBC with the
# 1:9 pattern, which HLS and most players expect for video):
tpt-kinetix package in.mp4 out/ --cenc-key-id ... --cenc-key ... --cenc-scheme cbcs
```

- Both key values are 32 hex digits. The key ID and key must be given together.
- Encrypted output is for VP9, AV1 and Opus. H.264 and H.265 need subsample
  encryption, which is not implemented, so they are refused.
- The DASH manifest gets the ClearKey `ContentProtection` elements. The HLS media
  playlists get `EXT-X-KEY` for `cenc`; `key.json` is the ClearKey license. `cbcs` is
  not signalled in HLS. `cbcs-pattern` is signalled as `SAMPLE-AES` with the same license,
and plays in hls.js and dash.js (checked in headless Chrome).

## serve: play an MP4 over HTTP without writing files

```sh
tpt-kinetix serve in.mp4 --port 8080
# http://127.0.0.1:8080/master.m3u8   and   /manifest.mpd
```

`serve` packages the file on demand and answers from the source. It listens on
localhost unless you pass `--public`. `--cenc-key-id/--cenc-key` protect the segments
the same way `package` does.

## live: ingest and play live streams

```sh
tpt-kinetix live --port 8080
# publish (any WebM source, stream copy):
ffmpeg -re -i clip.mp4 -c copy -f webm -method POST -chunked_post 1 \
  http://127.0.0.1:8080/ingest/cam
# play, while the publish is still running:
#   http://127.0.0.1:8080/cam/master.m3u8    (HLS, low-latency parts by default)
#   http://127.0.0.1:8080/cam/manifest.mpd   (DASH)
```

A publisher sends WebM with AV1 or VP9 video and Opus audio. Each stream key
(`cam` above) is independent.

Useful flags:

| Flag | What it does |
|---|---|
| `--segment-seconds`, `--window` | Segment length (cut at a key frame) and playlist length. |
| `--part-seconds` | Low-latency HLS part length; `0` gives plain HLS. |
| `--publish-token` | Require this token from HTTP publishers (`Authorization: Bearer` or `?token=`). |
| `--max-streams`, `--max-bitrate-kbps`, `--idle-timeout` | Limits on the whole server. |
| `--key-limit KEY:idle_timeout=…,max_duration=…,max_bitrate_kbps=…,max_bytes=…` | Limits for one stream key. |
| `--reject-concurrent` | Refuse a second publisher on a live key (the default replaces it). |
| `--record-dir`, `--record-depth-secs`, `--keep-generations`, `--record-max-mb` | Record every publish; the recording plays at `/<key>/dvr/master.m3u8`. |
| `--cenc-key-id`, `--cenc-key`, `--cenc-scheme` | Protect the live fragments. Players get the key from `/<key>/key.json`, which accepts GET and POST. |
| `--rtmp-port` | Also accept Enhanced RTMP publishers (AV1/VP9 + Opus) at `rtmp://host:port/live/<key>`. |
| `--tls-cert`, `--tls-key` | HTTPS and `wss://` (needs a build with the `tls` feature). |
| `--whip-…` | WebRTC publishers (needs the `whip` feature). |

Subtitles in a WebM publish become `subs-{k}.m3u8` renditions, and a DASH text set
per track. Live RTMP and WHIP publishes carry no subtitles.

### Limits to know

- Enhanced RTMP ingest takes AV1 and VP9 video and Opus audio. Other tracks in an RTMP
  publish are ignored, with a warning in the log.
- Encrypted live output is checked against ffmpeg, but not yet in a browser player.
- Playlists are a sliding window. For a complete recording use `--record-dir`.

## Where to look next

- [Migrating from ffmpeg / ffprobe](migrating-from-ffmpeg.md): command mapping and
  behaviour differences.
- [Adding a codec](adding-a-codec.md): for contributors.
- [PATENTS.md](../PATENTS.md): which crates carry which codecs.
