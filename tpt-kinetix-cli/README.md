# tpt-kinetix-cli

The `tpt-kinetix` command-line tool, tying the other crates together.

```sh
cargo run -p tpt-kinetix-cli -- <command> --help
```

| Command | What it does |
| --- | --- |
| `probe <file\|url> [--json]` | Print a container's tracks (MP4, MPEG-TS, Matroska/WebM, IVF) with per-track decoder capabilities, language and disposition; `--json` is ffprobe-shaped. `http(s)` URLs are probed with range requests. |
| `remux <in> <out>` | Copy the streams without decoding (`-c copy`). The output extension picks the container: `.mp4` (`--faststart` moves `moov` to the front, `--fragmented` writes fMP4/CMAF, `-` for stdout), `.webm`/`.mkv`, `.ivf` (one AV1 or VP9 track) or `.ogg`/`.opus` (one Opus track). MP4 and WebM inputs are accepted. |
| `sanitize <in> <out>` | Re-mux an untrusted WebM or MP4, keeping only AV1, VP9 and Opus. It refuses files over the size limits (`--max-packet-bytes`, `--max-packets`, `--max-total-mb`). No decoding happens. |
| `doctor <file>` | Say whether a file will play in a browser, and why not. No decoding happens. |
| `thumbnails <in> <outdir>` | Write PNG stills from the keyframes of an AV1 or VP9 video (8-bit 4:2:0). `--count` (default 10) sets how many; `--sprite-columns` also writes `sprite.png`, a contact sheet, with `--sprite-width` per tile. |
| `package <in> <outdir>` | Package an MP4 into HLS (fMP4) and DASH: playlists, init and media segments, I-frame playlists, and a WebVTT subtitle rendition per subtitle track. Optional CENC (`--cenc-key-id`, `--cenc-key`, `--cenc-scheme cenc\|cbcs`). |
| `serve <in>` | Serve an MP4 as HLS and DASH, packaged just in time, with the same CENC options. |
| `live` | Live server: ingest AV1/VP9 + Opus by HTTP POST, WebSocket, WHIP or Enhanced RTMP; plays as low-latency HLS and dynamic DASH, with optional CENC, WebVTT subtitles from WebM ingest, ingest limits and DVR recording (`--record-dir`). |
| `transcode -i <in> -o <out>` | VP9 MP4 to AV1 through the pipeline (VP9 is the only supported input). |
| `stream` | Older RTMP ingest to MPEG-TS HLS server. |
| `vision` | Decode a Vision (video-for-machines) bitstream; `--demo` runs a self-contained round trip. |

Cargo features: `codec-vp9` (default, VP9 decode for `transcode`), `whip` (WHIP/WebRTC ingest
in `live`), `tls` (HTTPS / `wss://` / RTMPS in `live`).

Worked examples for each task, with the ffmpeg command it replaces, are in
[docs/user-guide.md](../docs/user-guide.md), [docs/migrating-from-ffmpeg.md](../docs/migrating-from-ffmpeg.md)
and [examples/tasks](../examples/tasks/README.md).

Changes are listed in [CHANGELOG.md](CHANGELOG.md).

See the [workspace README](../README.md) for the full project overview, architecture diagram, and quickstart guide.
