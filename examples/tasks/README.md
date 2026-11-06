# Task examples

Each script does one job you would otherwise do with ffmpeg, using the
`tpt-kinetix` CLI. Each one makes its own sample media with ffmpeg, checks the
result, and cleans up. CI runs all of them (`task-examples` in
`.github/workflows/ci.yml`).

| Script | Task | Command it shows |
|---|---|---|
| [01-probe.sh](01-probe.sh) | What is in this file? | `probe`, `probe --json` |
| [02-remote-probe.sh](02-remote-probe.sh) | Inspect an MP4 over HTTP without downloading it | `probe http://...` (range requests) |
| [03-remux.sh](03-remux.sh) | Change the container without re-encoding | `remux` to MP4, fragmented MP4 and IVF; `doctor` |
| [04-package.sh](04-package.sh) | Make HLS and DASH for a player | `package` |
| [05-sanitize-upload.sh](05-sanitize-upload.sh) | Accept user uploads safely | `sanitize`, including a refused over-limit upload |
| [06-live-ingest.sh](06-live-ingest.sh) | Play a live stream as HLS while it arrives | `live` with an ffmpeg publisher |

The edge worker task (`examples/edge-worker`) is not repeated here: its own
`edge-worker-test` CI job already runs it against the native packager output.

## Running them

```bash
cargo build --release -p tpt-kinetix-cli
bash examples/tasks/run-all.sh          # all of them
bash examples/tasks/05-sanitize-upload.sh   # one of them
```

Needs `ffmpeg` and `ffprobe` on `PATH` (with `libvpx-vp9` and `libopus`), and
Python 3 for example 02. Set `TPT=/path/to/tpt-kinetix` to use another binary.

See [docs/migrating-from-ffmpeg.md](../../docs/migrating-from-ffmpeg.md) for the
ffmpeg command each task replaces.
