# Live ingest density, 2026-11-07: RSS and CPU per stream

`tools/live-load-test.py --viewers 0` against the release build of
`tpt-kinetix live` (default options, low-latency parts on). Each publisher pushes
one 30-second 1280x720 VP9 WebM at 3 Mbit/s by stream copy, paced with
`ffmpeg -re`, for a 15-second window. No viewers, so this is ingest and packaging
only. Machine: Windows 11 (10.0.26200), one host running server and publishers.

| publishers | server CPU (cores) | RSS median per publisher | RSS min–max (server, MiB) |
|---:|---:|---:|---:|
| 1 | 0.000 | 4.35 MiB | 11.2–20.8 |
| 4 | 0.004 | 5.13 MiB | 20.6–53.8 |
| 8 | 0.029 | 4.84 MiB | 29.8–98.9 |
| 16 | 0.027 | 5.22 MiB | 52.5–203.9 |

Raw summaries: `docs/perf/live-streams-2026-11-07-p{1,4,8,16}.json`.

## How to read this

- **Memory per stream is about 5 MiB** of steady-state working set, and it stays flat
  from 1 to 16 publishers. The window holds six 2-second segments and their parts.
- **The maximum RSS is much higher than the median** (204 MiB at 16 publishers).
  The spikes are short, most likely segment assembly at each cut. The median is the
  figure to plan with.
- **CPU per stream is below about 0.004 cores** for this copy-mode ingest, but the
  measurement cannot resolve less than about 0.01 cores (the counter's noise floor,
  visible in the 1-publisher run). Streams per core therefore cannot be pinned down
  better than "hundreds". The honest claim is that copy ingest is cheap, not a
  capacity figure.
- **Copy mode only.** Nothing is transcoded, and no viewers are attached. Viewers
  add the HTTP cost measured in `live-load-2026-11-07.md`.

## Method notes

- Server CPU on Windows is the `\Process(tpt-kinetix)\% Processor Time` counter,
  sampled once a second with `typeperf` over the window and averaged.
  `TotalProcessorTime` was tried first and moves in 15.6 ms steps, which is too
  coarse for this load.
- Per-publisher RSS is the window's median RSS minus the idle server's RSS,
  divided by the number of publishers.
