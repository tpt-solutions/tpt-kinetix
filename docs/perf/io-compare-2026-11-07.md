# I/O comparison, 2026-11-07: `tpt-kinetix probe` against `ffprobe`

Produced by `cargo run --release -p tpt-kinetix-test-utils --example io_compare`
(`just bench-io`), on the release build of this tree. Machine: Windows 11
(10.0.26200). Seed: a 4 s 1280x720 VP9 MP4 (480 KiB), made with ffmpeg 2023 and
kept in `target/perf-corpus/`, which the harness reads.

## Startup and memory (`--startup`, best of 15)

| tool | wall time | peak RSS |
|---|---:|---:|
| `tpt-kinetix probe` | 14.4 ms | 7.9 MiB |
| `ffprobe -show_streams -show_format` | 35.8 ms | 19.2 MiB |

## Hostile input (`--hostile`, mutated copies of the seed, 10 s hang limit)

| tool | variants | clean | rejected | crash | hang |
|---|---:|---:|---:|---:|---:|
| `tpt-kinetix probe` | 1000 | 799 | 201 | 0 | 0 |
| `ffprobe -show_streams -show_format` | 1000 | 799 | 201 | 0 | 0 |

A rejection is a non-zero exit with a message, which is the right outcome for a
malformed file. Neither tool crashed or hung on any variant.

## Limits of this evidence

- One seed file, probe only. The hostile run does not cover `remux`, `package` or
  `sanitize`, which parse the same containers more deeply.
- ffprobe is run with its defaults, not tuned (`-probesize`, `-analyzeduration`).
- The startup and hostile runs use local disk. The remote section adds its delay on localhost (see below).
- Peak RSS is measured on Windows only.

## Remote probe (`--remote`)

An MP4 with `moov` at the end, served over HTTP by the harness with an added
round-trip delay. Each tool makes three requests (the box headers, the index, and
the tail), and the table shows the time and the bytes moved.

| added RTT | tpt-kinetix probe | ffprobe | bytes moved (tpt / ffprobe) |
|---:|---:|---:|---:|
| 0 ms | 18 ms | 59 ms | 199 KiB / 28.2 MiB |
| 20 ms | 100 ms | 168 ms | 199 KiB / 29.9 MiB |
| 50 ms | 221 ms | 349 ms | 199 KiB / 24.8 MiB |
| 100 ms | 422 ms | 648 ms | 199 KiB / 36.0 MiB |

The delay is simulated on localhost, so this measures round trips and bytes, not a
real network. The file is 18.0 MiB, and the probe reads about 1% of it.
