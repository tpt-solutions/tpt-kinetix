# Codec performance / optimisation tracker

Measure first, optimise second, never lose bit-exactness. Covers every codec crate: AV1, VP9,
H.264 (unpublished, last), and the original codecs (lean, lossless, realtime, screen, vision, face,
volumetric) plus the shared `tpt-kinetix-bitstream`.

Status legend: `[ ]` todo, `[~]` in progress, `[x]` done.

## Current state (2026-10-02)

- `just bench` covers only `out-kinetix-h264`, `tpt-kinetix-av1`, `tpt-kinetix-pipeline`.
- `just bench-report` (`tpt-kinetix-test-utils` example `bench_report`) prints a timing table.
- No benches for vp9, bitstream, lean, lossless, realtime, screen, vision, face, volumetric, demux, mux.
- No committed baseline; no systematic comparison against ffmpeg.

## Phase 0 — Baseline benchmarks

Criterion benches, mirroring `out-kinetix-h264/benches/decode_throughput.rs`. Report frames/s and
MB/s (points/s for volumetric) at 320x240, 720p, 1080p, plus peak memory. Decode and, where present, encode.

| Done | Crate | Decode bench | Encode bench |
|:---:|---|:---:|:---:|
| [x] | tpt-kinetix-av1 | existing | existing (`av1_encode`) |
| [ ] | tpt-kinetix-vp9 | | n/a |
| [x] | out-kinetix-h264 | existing | n/a |
| [ ] | tpt-kinetix-bitstream (BitReader, rANS) | | |
| [ ] | tpt-kinetix-lean | | |
| [ ] | tpt-kinetix-lossless | | |
| [ ] | tpt-kinetix-realtime | | |
| [ ] | tpt-kinetix-screen | | |
| [ ] | tpt-kinetix-vision | | |
| [ ] | tpt-kinetix-face | | |
| [ ] | tpt-kinetix-volumetric | | |
| [ ] | tpt-kinetix-demux / mux | | |
| [x] | tpt-kinetix-pipeline | existing (`transcode_throughput`) | |

- [ ] Extend `just bench` to cover all crates above
- [ ] Extend `bench_report` to one consolidated table across all codecs
- [ ] Commit baseline `docs/perf/baseline-<date>.json` and `docs/PERFORMANCE.md` table
- [ ] Record machine details with results (CPU, cores, rustc, `--release`/lto, target-cpu)
- [ ] Add `just bench-compare` (fails on regression beyond threshold; threshold TBD)

## Phase 1 — Comparison vs current ffmpeg

- [ ] Pin and record versions: ffmpeg, libdav1d, libvpx, libaom
- [ ] `just bench-ffmpeg`: per shared corpus file, ffmpeg decode (`-f null -`, `-threads 1` and default) vs Kinetix; output verified identical BEFORE timing
- [ ] AV1/VP9 encode vs libaom / rav1e / libvpx through ffmpeg
- [ ] Pipeline end-to-end transcode vs ffmpeg CLI (wire in the todo.md Phase 7 harness)
- [ ] Original codecs, compared to the closest standard reference (speed AND ratio/quality):
  - [ ] lossless vs FFV1 / PNG / JPEG-LS
  - [ ] screen vs x264 / libaom screen-content mode
  - [ ] lean, realtime vs x264 `ultrafast`/`zerolatency`, libaom realtime
  - [ ] vision vs AV1/x264 at equal detector accuracy
  - [ ] face vs AV1/x264 at matched quality on talking-head clips
  - [ ] volumetric vs Draco / MPEG G-PCC TMC13
- [ ] Publish `docs/PERFORMANCE.md` with ratio column, date, tool versions

## Phase 2 — Profile and rank hot spots

Tools: `cargo flamegraph` / `perf`, or `samply`/VTune on Windows. Record ranked list per codec below.

- [ ] AV1 (candidates: inverse transforms, MC/subpel, CDEF, loop filter, loop restoration, symbol decode, intra pred)
- [ ] VP9
- [ ] bitstream / rANS
- [ ] lean, realtime, lossless, screen, vision, face, volumetric

## Phase 3 — Optimise (hot spots first)

Rules: safe Rust; scalar reference path stays as fallback and test oracle; SIMD via `std::simd`/`wide`
or runtime-dispatched `std::arch` with scalar fallback; no `unsafe` without justification; parallelism
via existing `rayon` (tile / superblock-row / frame level); check release profile (`lto`,
`codegen-units = 1`) and allocation reuse (frame arenas).

Order:
- [ ] AV1
- [ ] VP9
- [ ] bitstream / rANS (shared by all original codecs, best leverage)
- [ ] lean, realtime
- [ ] lossless, screen
- [ ] vision, face, volumetric
- [ ] out-kinetix-h264 (optional, unpublished, last)

## Phase 4 — Guardrails (every optimisation change)

- [ ] `just check` passes
- [ ] `just conformance` unchanged (`pixel_exact` stays true); FATE 204/204; `tests/libaom_crosscheck.rs` passes
- [ ] Original codecs: existing roundtrip / bit-exact tests pass
- [ ] SIMD-vs-scalar equivalence proptests for each vectorised kernel
- [ ] Relevant fuzz target run >= 60s after touching any parser
- [ ] CI nightly/manual bench job uploads the report

## Open questions

- Target hardware for headline numbers: dev machine only, or also RPi-class ARM (lean/face budgets)?
- Regression threshold for `bench-compare`.
