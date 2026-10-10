# tpt-kinetix-cli

The `tpt-kinetix` command-line tool, tying the other crates together.

```sh
cargo run -p tpt-kinetix-cli -- <command> --help
```

| Command | What it does |
| --- | --- |
| `probe <file\|url> [--json]` | Print a container's tracks (MP4, MPEG-TS, Matroska/WebM, IVF) with per-track decoder capabilities; `--json` is ffprobe-shaped. `http(s)` URLs are probed with range requests. |
| `remux <in> <out>` | Copy an MP4's streams without decoding (`-c copy`); `--faststart` moves `moov` to the front, `--fragmented` writes fMP4/CMAF (`-` for stdout). |
| `package <in> <outdir>` | Package an MP4 into HLS (fMP4) and DASH. |
| `serve <in>` | Serve an MP4 as HLS and DASH, packaged just in time. |
| `live` | Live server: ingest AV1/VP9 + Opus by HTTP POST, WebSocket, WHIP or Enhanced RTMP; plays as low-latency HLS and dynamic DASH. |
| `transcode -i <in> -o <out>` | VP9 MP4 to AV1 through the pipeline (VP9 is the only supported input). |
| `stream` | Older RTMP ingest to MPEG-TS HLS server. |
| `vision` | Decode a Vision (video-for-machines) bitstream; `--demo` runs a self-contained round trip. |

Cargo features: `codec-vp9` (default, VP9 decode for `transcode`), `whip` (WHIP/WebRTC ingest
in `live`), `tls` (HTTPS / `wss://` / RTMPS in `live`).

See the [workspace README](../README.md) for the full project overview, architecture diagram, and quickstart guide.
