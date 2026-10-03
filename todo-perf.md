# Codec performance / optimisation tracker

Measure first, optimise second, never lose bit-exactness. Covers every codec crate: AV1, VP9,
H.264 (unpublished, last), and the original codecs (lean, lossless, realtime, screen, vision, face,
volumetric) plus the shared `tpt-kinetix-bitstream`.

Status legend: `[ ]` todo, `[~]` in progress, `[x]` done.

- **`ffmpeg_compare --quick` silently rewrites the perf corpus.** Running it
  with `--quick` regenerates the 320x240 clips as **120-frame** files instead of
  the 300-frame ones the Phase 0/2 baselines were recorded against (it updates
  `target/perf-corpus/manifest.json` to match, so the next run sees no
  mismatch). Any 320x240 timing taken after a `--quick` run is therefore not
  comparable with the rest of this file. If you run `--quick` for iteration,
  either restore the 300-frame clips afterwards or avoid quoting the numbers.
  Caused on 2026-10-02 while checking VP9's byte-exactness; the clips were
  regenerated and the manifest corrected, and the restored files matched the
  originals byte for byte.
- `just` cannot run on this machine (no `sh` on PATH), so the four `just check`
  gates were run directly as `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace --lib --bins --tests` and `cargo +1.82.0 check`.

## Phase 0 — Baseline benchmarks

## Current state (2026-10-02)

- `just bench` covers **every** codec crate: h264, av1, vp9, bitstream, lean, lossless, realtime,
  screen, vision, face, volumetric, demux, mux, pipeline.
- `just bench-report` (`tpt-kinetix-test-utils` example `bench_report`) prints one consolidated
  timing table across all codecs.
- `just bench-baseline <label>` records machine + toolchain metadata and writes
  `docs/perf/baseline-<label>.json` + regenerates `docs/PERFORMANCE.md`.
- `just bench-compare <baseline> <threshold%>` diffs a fresh run against a committed baseline and
  exits non-zero on a throughput regression beyond the threshold.
- A baseline snapshot is committed at `docs/perf/baseline-2026-10-03.json`
  (re-recorded 2026-10-03; supersedes `baseline-2026-10-02.json`, which is **not
  comparable** to it — see the "Notes on this snapshot" section of
  `docs/PERFORMANCE.md` and the Phase 3 closing section below).
- `just bench-ffmpeg` (`tpt-kinetix-test-utils` example `ffmpeg_compare`) compares Kinetix against
  ffmpeg (decode, AV1 encode, original codecs, CLI e2e) and writes
  `docs/perf/ffmpeg-compare-<label>.json` plus the marked section of `docs/PERFORMANCE.md`
  (Phase 1; committed at `docs/perf/ffmpeg-compare-2026-10-02.json`).

## Phase 0 — Baseline benchmarks

Criterion benches, mirroring `out-kinetix-h264/benches/decode_throughput.rs`. Report frames/s and
MB/s (points/s for volumetric) at 320x240, 720p, 1080p, plus peak memory. Decode and, where present, encode.

| Done | Crate | Decode bench | Encode bench |
|:---:|---|:---:|:---:|
| [x] | tpt-kinetix-av1 | existing | existing (`av1_encode`) |
| [x] | tpt-kinetix-vp9 | `vp9_decode` (ffmpeg/libvpx clip; **skips** without ffmpeg) | n/a |
| [x] | out-kinetix-h264 | existing | n/a |
| [x] | tpt-kinetix-bitstream (BitReader, rANS) | `bitstream_ops` (`MiB/s`, 1 MiB payloads) | same target |
| [x] | tpt-kinetix-lean | `lean_codec` | same target |
| [x] | tpt-kinetix-lossless | `lossless_codec` (10/16-bit, samples/s) | same target |
| [x] | tpt-kinetix-realtime | `realtime_codec` (per-**slice** throughput) | same target |
| [x] | tpt-kinetix-screen | `screen_codec` (UI-like source) | same target |
| [x] | tpt-kinetix-vision | `vision_codec` (`decode_pixels` + `decode_tensor`) | same target |
| [x] | tpt-kinetix-face | `face_codec` (decode includes 3DMM synthesis) | same target |
| [x] | tpt-kinetix-volumetric | `volumetric_codec` (points/s; lift + RAHT) | same target |
| [x] | tpt-kinetix-demux / mux | `container_demux` (mp4/mkv/ts) | `mp4_mux` (samples/s) |
| [x] | tpt-kinetix-pipeline | existing (`transcode_throughput`) | |

- [x] Extend `just bench` to cover all crates above
- [x] Extend `bench_report` to one consolidated table across all codecs
- [x] Commit baseline `docs/perf/baseline-<date>.json` and `docs/PERFORMANCE.md` table
- [x] Record machine details with results (CPU, cores, rustc, `--release`/lto, target-cpu)
- [x] Add `just bench-compare` (fails on regression beyond threshold; default 5%)

Notes / follow-ups:

- [x] ~~The scraper Criterion-output parser is duplicated in `bench_report`,
  `bench_baseline` and `bench_compare`~~ — collapsed into the shared
  `tpt-kinetix-test-utils::bench_parse` module (2026-10-02); the copies had
  already drifted (two of them silently dropped duration-only benches such as
  `av1_encode`).
- Peak memory is **not** yet measured for the Kinetix side; the benches report
  throughput only. (`ffmpeg -benchmark` reports `maxrss`, which
  `bench-ffmpeg` records for the reference side.) Add an allocator-counting
  harness (or a `dhat`/`jemalloc` pass) if a memory ceiling becomes a gate.
- `bench-compare` compares only benches present in **both** snapshots and with matching unit
  dimensions; anything else is reported as skipped rather than silently passing.
- The `tpt-kinetix-realtime` **decode** baseline figure is stale: the bench's
  packet builder shipped a broken frame header (empty intra-refresh mask +
  `payload_len: 0`), so the decode case errored immediately and timed the error
  path. Fixed 2026-10-02 — re-record the baseline before trusting that row.

## Phase 1 — Comparison vs current ffmpeg

- [x] Pin and record versions: ffmpeg, libdav1d, libvpx, libaom
  (`ffmpeg_compare` records the ffmpeg build line, libavcodec version and the
  libdav1d/libvpx/libaom/libx264/FFV1/PNG/JPEG-LS presence flags; gyan-style
  builds do not expose per-library versions through the CLI)
- [x] `just bench-ffmpeg`: per shared corpus file, ffmpeg decode (`-f null -`,
  `-threads 1` and default) vs Kinetix; output verified identical BEFORE timing
  (byte-exact plane compare; ratio column reads `n/a (unverified)` otherwise;
  corpus cached in `target/perf-corpus/` and regenerated when the generator
  arguments change; AV1 FATE fixtures included when `fixtures/av1-fate` exists)
- [x] AV1/VP9 encode vs libaom / rav1e / libvpx through ffmpeg
  (AV1: Kinetix `Av1Encoder` vs `libaom-av1 -cpu-used 8 -crf 30` with
  time + size + Y-PSNR on identical raw frames; librav1e was already in the
  Criterion `av1_encode` bench. VP9 encode: n/a — Kinetix has no VP9 encoder)
- [x] Pipeline end-to-end transcode vs ffmpeg CLI (the `transcode_throughput`
  bench covers pipeline-in-process vs ffmpeg libaom; `bench-ffmpeg --e2e` times
  the real `tpt-kinetix transcode` CLI (VP9 MP4 → AV1) against the equivalent
  ffmpeg invocation)
- [x] Original codecs, compared to the closest standard reference (speed AND
  ratio/quality) — automated rows in `bench-ffmpeg --originals`:
  - [x] lossless vs FFV1 / PNG (10-bit 4:2:0, the codec's supported depth;
    JPEG-LS skipped — ffmpeg's encoder is 8-bit-only so it cannot take the
    10-bit source; x264 `-qp 0` skipped for the same reason)
  - [x] screen vs x264 / libaom realtime mode
    (this ffmpeg build exposes no `-tune-content`; noted in the row settings)
  - [x] lean, realtime vs x264 `ultrafast`/`zerolatency`, libaom realtime
  - [ ] vision vs AV1/x264 at equal detector accuracy (skipped: needs a
    detector-accuracy ground-truth study; not automatable in this harness yet)
  - [ ] face vs AV1/x264 at matched quality on talking-head clips (skipped:
    needs matched-quality clips)
  - [ ] volumetric vs Draco / MPEG G-PCC TMC13 (blocked: the codec is not yet
    byte-compatible with `tmc3`, so there is nothing to compare against — see
    `tpt-kinetix-test-utils::tmc13`)
- [x] Publish `docs/PERFORMANCE.md` with ratio column, date, tool versions
  (the `<!-- ffmpeg-compare -->` section; `just bench-baseline` preserves it)

### Phase 1 findings (2026-10-02 run)

Correctness gaps the verify-before-timing gate surfaced (each UNVERIFIED row in
`docs/PERFORMANCE.md` is one of these):

- **VP9 — OPEN**: clips encoded with libvpx `-deadline realtime` fail
  byte-exact vs ffmpeg (identical frame count and size, differing pixels), and
  longer `testsrc` clips diverge even at `-deadline good -cpu-used 4` (first
  difference at frame 128 of 300 at 320x240). Localized 2026-10-02: frame 128
  is a **key frame**, decodes standalone (fresh decoder reproduces it), luma
  only, bottom-right 86x32 px, pre-loop-filter buffers already wrong — a
  **4x4 coefficient-decode desync** in a mixed-block-size neighbourhood.
  Repro + reference pinned at
  `tpt-kinetix-vp9/tests/fixtures/div128/`.
  **Oracle rebuilt and bug isolated 2026-10-03**: an instrumented libvpx
  v1.13.1 (provenance-only, `/tmp/libvpx` + `vpxsym.c` harness; rebuild steps
  in the div128 README) now proves on the solo keyframe that the coefficient
  token stream (9109 PTE/PTV lines), every dequantized value, and all 867 luma
  intra predictions are **bit-identical** to libvpx. With both loop filters
  disabled the frames still differ from pixel (240, 208) — and the first
  overall difference is a single sample, (231, 2), one sample left of the
  vertical block edge x=232 between a **skip** 8x8 DC block and a
  4x4-partitioned intra neighbour: our loop filter modifies it (−1) where
  libvpx leaves it untouched. The remaining bug is a per-edge level/skip-rule
  difference in `tpt-kinetix-vp9/src/loop_filter.rs`; the full isolation
  method and oracle rebuild steps are in the div128 README.
  Sharpened 2026-10-03: with BOTH loop filters disabled the pre-LF frames are
  identical through the x=232 edge region — the first post-LF divergence is a
  single sample, (231, 2) on row 0, where OUR filter modified the flat-145
  skip block by -1 and libvpx left it. The edge separates a **skip** 8x8 DC
  block (mi 0,28) from a 4x4-partitioned intra block (mi 0,29, D153/TM
  sub-modes): our LF filters that edge, libvpx does not. VP9 decoder
  allocation-scratch migration (XfmScratch, removing 4 vec allocs + a
  `.to_vec()` per transform block) was also tried and REJECTED the same day:
  tiles 560 us vs a 538-609 us noise band, matching the AV1 precedent. The perf corpus
  stays pinned to clips that verify; VP9 rows read `UNVERIFIED` until then.
- **realtime — FIXED 2026-10-02**: two stacked bugs. (1) The frame-header
  writer/parser pair was asymmetric (writer emitted an empty intra-refresh
  mask where the parser always reads `refresh_mask_len()` bytes, and
  `payload_len` was left 0) — fixed in the bench + harness. (2)
  `slice_index_for` was not the inverse of `chunk_range` for block totals that
  do not divide evenly by the slice count (320x240 = 1200 blocks / 64 slices),
  so decode desynced at chunk boundaries ("chroma block index out of range");
  fixed with the exact ceil-form inverse. Roundtrip is now **bit-exact at
  every swept geometry**; regression tests in `reconstruct.rs`.
- **screen — FIXED 2026-10-02 (luma)**: the rANS stream counts were coded as
  one byte-wide symbol, wrapping at 256 — a 320x240 frame has 300 coding
  blocks (mode count -> 44) and a full 16x16 natural block has 256
  coefficients (count -> 0), so most of the frame decoded as flat black.
  Counts are now 4 symbols (u32 LE); luma round-trips **bit-exact**, chroma is
  still uncoded in v1 (decodes as 0). The Phase 0 screen decode throughput was
  measured on this corrupt path.
- **lossless — FIXED 2026-10-02**: the decoder used the frame-level
  width/height for every plane, over-reading the half-size chroma residual
  streams (rANS exhausted). The frame header now carries per-plane dims;
  3-plane 10/12/16-bit roundtrips are **bit-exact** at all swept sizes.

The original codecs also compress poorly today (e.g. lean ≈ 1.6× *larger* than
raw at `base_qp 0` while x264 ultrafast lands ~200× smaller at higher PSNR) —
that is the Phase 3 work list in numbers.

## Phase 2 — Profile and rank hot spots

Tools: `samply` is installed but needs Administrator (ETW) and `pprof` does not
compile on this toolchain, so Phase 2 shipped **env-gated phase timers**
instead (admin-free, zero-cost when off, kept as diagnostics):
`TPT_VP9_PHASE=1` (tpt-kinetix-vp9 `decoder.rs`) and `KINETIX_AV1_PHASE=1`
(tpt-kinetix-av1 `dbg_env.rs`), driven by the
`tpt-kinetix-test-utils` `profile_decode` example. A sampling profiler, when
available, should refine these numbers to function level.

Per-frame phase split, `testsrc` content clip, release profile (2026-10-02):

- [x] AV1 320x240: **deblock 4.0 ms (62%)**, tiles (entropy + reconstruction)
  2.4 ms (36%); CDEF/LR/superres/film-grain ~0 (not enabled on this clip).
  At 720p: **deblock 45 ms (70%)**, tiles 20 ms (30%).
  (Candidates ranked: deblocking loop filter >> entropy+reconstruction > rest.)
- [x] VP9 320x240: **loop filter 0.54 ms (49%)**, tile decode 0.51 ms (46%),
  compressed header 0.04 ms (3%). At 1080p: **loop filter 10.3 ms (63%)**,
  tiles 6.1 ms (37%).
- [x] bitstream / rANS: from the Phase 0 Criterion table — `BitReader::read_bit`
  101 MiB/s is the outlier (a tight bit reader does GB/s; per-bit call overhead
  dominates), and rANS `decode_noise` 41 MiB/s vs `decode_static` 374 MiB/s
  (model lookup on near-uniform data). Rank: read_bit fast path > rANS decode
  inner loop.
- [x] lean / realtime / lossless / screen / vision / face / volumetric: ranked
  by the Phase 0 baseline itself — the encoders are the bottleneck
  (lean ~0.49 Melem/s, realtime ~173 ms/frame-equivalent, vision ~0.44
  Melem/s encode vs multi-Melem/s decodes), so Phase 3 for the originals is an
  *encoder* story first. Decodes: screen 1.1-1.2 Gelem/s (fine), realtime
  ~230 Melem/s (fine), lean ~11.5 Melem/s and vision pixels ~11-12 Melem/s
  (the slow original decodes).

## Phase 3 — Optimise (hot spots first)

Rules: safe Rust; scalar reference path stays as fallback and test oracle; SIMD via `std::simd`/`wide`
or runtime-dispatched `std::arch` with scalar fallback; no `unsafe` without justification; parallelism
via existing `rayon` (tile / superblock-row / frame level); check release profile (`lto`,
`codegen-units = 1`) and allocation reuse (frame arenas).

Order:
- [~] AV1 — deblock done 2026-10-02 (-16% @320x240, -15% @720p). Tiles/entropy and
  reconstruction audited 2026-10-02: the per-leaf residual `vec![0i32; ..]` was
  tried as a per-tile reusable scratch and **rejected — 1.6%, inside the ±5%
  noise floor** (see below). Unlike the original codecs, allocation is *not*
  AV1's lever: the entropy decode it sits next to dominates. The reconstruct
  path's own allocations and a SIMD add-residual kernel were then measured on a
  purpose-built decode bench — **NEUTRAL**, see "AV1 reconstruct — decode bench,
  allocations and SIMD" below. Entropy decode itself is still the open AV1 item.
- [x] VP9 — **done 2026-10-02**, see below
- [x] bitstream / rANS (shared by all original codecs, best leverage) — **done
  2026-10-02**, see below
- [x] lean, realtime — **done 2026-10-02**; lean encode +28% / decode +18%,
  realtime 1080p encode ~+47% / decode ~+36%. Both were the same two causes
  (Hadamard matrix rebuilt per transform call; ~5 allocations per block plus
  ~6 per intra-mode trial in the encoder) and both also had the unvalidated
  4-bit block-size field.
- [x] lossless, screen — **done 2026-10-02**, by two different routes.
  screen: 1080p encode ~+32%, decode ~+32% (see below). lossless: no local
  hot spot left — its decode went 11.9 -> 35.0 Melem/s from the shared bitstream
  rANS inverse table, and its own code has no transform and no per-block
  allocations left to remove
- [~] vision, face, volumetric — vision done 2026-10-02 (1080p encode +19%,
  decode_pixels +19%; no new validation needed, its parser already bounded the
  block size). face and volumetric audited the same day: **no hot spot found**,
  so this row is done-but-for-the-record rather than work still to do
- [x] out-kinetix-h264 — **done 2026-10-03**, see below

### face, volumetric — audited 2026-10-02, no hot spot found

The last of the originals. Checked for every pattern that paid off elsewhere —
`env::var` in a hot loop, per-call matrix rebuilds, per-block `Vec` allocation,
per-symbol table lookups — and found **none of them**:

- `tpt-kinetix-face`: no transform, no unguarded `env::var`, and the `vec!`s in
  `params.rs`/`synthesizer.rs`/`basis.rs` are per-frame, not per-block.
- `tpt-kinetix-volumetric`: no transform; `raht_forward`/`raht_inverse` are
  already allocation-free and in-place (`chunks_exact_mut`); its own entropy
  coder uses `FixedBinaryModel`, whose `find` is a single comparison (a binary
  model needs no inverse table), and `decode_bits` reserves capacity.

Their current numbers also agree with Phase 2's own assessment, which flagged
only lean and vision as slow decodes: face encode 10-22 Gelem/s, volumetric
encode ~18 Melem/s / decode ~82 Melem/s. **This row is closed as audited, not
optimised** — recording that so it is not re-investigated from scratch. Further
gains here would need SIMD or a different algorithm, not the code-shape work
that paid off in the other seven crates.

Useful side-finding from the same measurement run: face and volumetric were
untouched this session yet still printed **-2% to -7%** against Criterion's
stored baseline. That puts this machine's **between-session noise floor at
roughly ±5%**, which is why every change in this phase was A/B'd same-session
(and the VP9 `decode_static` / `encode_*` "controls" were worth running at
all). It also retroactively explains the -5..-10% that Criterion reported
against the committed baseline in the very first session of this phase.

### Phase 3 closing — ffmpeg comparison, and two correctness bugs found DONE 2026-10-03

`just bench-ffmpeg 2026-10-03` (full, ~32 min) refreshed the Kinetix-vs-ffmpeg
comparison that `docs/PERFORMANCE.md` had been carrying since 2026-10-02 — i.e.
since *before* the `8291e0b` bug fixes, so those ratios described the broken
decoders too. New file `docs/perf/ffmpeg-compare-2026-10-03.json`.

Note the tool did the right thing under pressure: for every VP9 clip it reports
`verified: false` and leaves `ratio_kinetix_over_ffmpeg_1thread` **empty** rather
than publishing a speed number for output that does not match the reference.
Verify-before-time is doing its job.

**Where Kinetix stands against ffmpeg (verified rows only):**

| Clip | Kinetix | ffmpeg `-threads 1` | ffmpeg default |
|:---|---:|---:|---:|
| av1/fate (median) | 22–33 MPix/s | 159–425 MPix/s | 258–1080 MPix/s |
| vp9 320x240 | 159.6 | 914.3 | 1184.6 |
| vp9 1280x720 | 240.8 | 2032.9 | 5386.0 |

Kinetix is roughly **6–9x slower than single-threaded libdav1d/libvpx** on these
clips. That is the honest headline: Phase 3 made a real dent in a large gap, it
did not close it. Against the original codecs' nearest standard references the
gap is far wider still (e.g. lossless vs FFV1: 3.36 s enc / 3.52 s dec against
0.220 s / 0.105 s) — those codecs are not competing with libvpx, they are a
research target.

**Bug 1 — screen decode corrupts partial edge blocks (pre-existing).**
`ffmpeg_compare` reports `screen: bit-exact=false` while *expecting*
`bit-exact=true`. `tpt-kinetix-screen` had **no integration tests at all**, so
`5358e2c` changed 367 lines with no correctness net. Added
`tpt-kinetix-screen/tests/roundtrip.rs`, which reproduces it and localises it
exactly:

| Geometry | aligned to the 16px block? | result |
|:---|:---|:---|
| 320x240, 1280x720 | both axes | **bit-exact** |
| 1920x1080 | height 67.5 blocks | fails at `(0, 1072)` |
| 67x53 | width 4.19 blocks | fails at `(64, 0)` |

Both failures are the first *partial* block, reconstructed as zeros. Confirmed
**pre-existing**: `git checkout 5358e2c^ -- tpt-kinetix-screen/src` reproduces
identical mismatches at identical positions. Every benchmark and comparison clip
in the repo uses block-aligned dimensions, which is why it went unnoticed.

The failing cases are `#[ignore]`d (with the diagnosis in the `ignore` reason) so
CI stays green and the bug stays documented and ready to flip when fixed; the
aligned cases run as real regression guards.

Note also a **reporting** bug found alongside it: `ffmpeg_compare.rs:2452`
renders a failed screen bit-exact as `"lossy"` rather than `MISMATCH`, so a
correctness failure displays as an expected quality setting. Worth fixing —
`screen` is declared `bit_exact: true` (an expectation of losslessness), so a
`false` there is a failure, not a mode.

**Bug 2 — VP9 luma decode is not byte-exact vs libvpx (pre-existing, not ours).**
All three VP9 clips mismatch: 320x240 at frame 128, 1280x720 at frame 38, and
1920x1080 **from frame 0**. `git diff origin/master..HEAD -- tpt-kinetix-vp9`
was empty when first found, so this is not from Phase 3. Investigated with a new
`vp9_dump` example (`tpt-kinetix-test-utils/examples/vp9_dump.rs`) that decodes
an IVF to raw `yuv420p` and reports the first difference, so the loop-filter
switch could be used as an isolator:

| Build | Differing samples (of 3,110,400) | PSNR |
|:---|---:|---:|
| loop filter on | **866** (0.028%) | 46.59 dB |
| `TPT_VP9_NO_LF=1` | 26,304 (0.846%) | — |

So the loop filter is **not** the cause — disabling it makes the output ten
times worse, so deblocking is doing its job. Frame 0 has no motion compensation
either. What is left is intra prediction or the inverse transform.

Block-level mapping of the 1080p frame 0: the 866 samples fall in only **20 of
the 8,100** 16x16 luma blocks, clustered around py 832–960, with deltas up to
±170. That is a localised block-decoding defect, not a whole-plane error.

The decisive clue came from adding conformance tests at realistic sizes:
`conformance_vp9.rs` asserted byte-exactness only on **synthetic sources at
≤256x144**. Real-content `testsrc` fails at 320x240, 640x360 and 1920x1080 —
and **chroma is bit-exact in every failing case** (`u_bad=0`, `v_bad=0`, PSNR
U/V = 99 dB) while only luma is wrong. A 640x360 control was expected to pass
and instead failed hardest of all (36,470 samples, PSNR Y = **35.47 dB**), which
rules out "large frames break it": it is about content, not size.

Because chroma is exact, all the shared machinery is demonstrably correct — bool
decoder, mode parsing, segmentation map, loop filter. The defect is in a
**luma-only** path: 4x4 WHT-vs-DCT transform selection, luma dequantisation, or
luma intra prediction edge handling.

Changes made rather than only documenting:
- `vp9_dump` example: decode-to-raw plus a `--info` first-difference reporter, so
  this is diagnosable without a 32-minute benchmark run.
- Three conformance tests at 320x240 / 640x360 / 1920x1080 on real content with
  the perf corpus's exact encoder settings. The three failing ones are
  `#[ignore]`d with the full analysis in the ignore reason, so CI stays green
  and `cargo test -p tpt-kinetix-vp9` will catch the fix.
- `DecoderCapabilities::notes` for VP9 **scoped down**: it previously said
  "byte-exact vs ffmpeg/libvpx on the conformance corpus" with no hint that the
  corpus is synthetic and ≤256x144. `AGENTS.md`'s flat "VP9 reports
  `pixel_exact: true`" inherits the same overstatement and should be read
  against this note.

Not attempted: locating the exact faulty transform/prediction branch. That is a
focused correctness task with its own debugging budget, not a Phase 3
performance item, and the three `#[ignore]`d tests now pin the failure so it
cannot regress unnoticed.

**Corpus side effect.** The full run regenerated the 320x240 clips
(`raw_320x240.yuv` 120 -> 300 frames, `h264_320x240.h264`, `vp9mp4_320x240.mp4`),
normalising a previously mixed quick/full state. `target/perf-corpus/` is
gitignored so nothing is committed, but note the committed manifest had been in
a half-`--quick` state — worth being aware of when comparing against older runs.

### Phase 3 closing — baseline re-recorded, and why the old one is void DONE 2026-10-03

Phase 3's last item was re-recording `docs/perf/baseline-2026-10-02.json`. Doing
so surfaced the most important methodological result of the phase, so it gets
its own section.

**The re-record.** `just bench-baseline 2026-10-03` over all 14 bench crates
(83 benchmarks, ~20 min). New file `docs/perf/baseline-2026-10-03.json`,
regenerated `docs/PERFORMANCE.md`, `just bench-compare` default repointed.
Nothing else touched: `target/perf-corpus/manifest.json` is unchanged, which
matters because `ffmpeg_compare --quick` has silently rewritten that corpus
before.

**The old baseline is void, not merely stale.** Diffing the two files suggested
absurd results — `screen_320x240/decode` 1.1071 Gelem/s -> 8.0094 Melem/s
(**-99.3%**), `realtime_320x240/decode` -100.0%. A 134x "regression" on code
whose tests pass and which measured 32% *faster* in the A/B that motivated the
change. Chasing it:

- `screen_codec.rs` is untouched since `1a8623c`, so the bench definition did
  not change — the difference is in the library underneath.
- `1a8623c` committed the baseline at **06:49** on 2026-10-02.
- `8291e0b` ("fix codec bugs it surfaced") landed at **15:19** the same day and
  changed `src/` in **av1, bitstream, lean, lossless, realtime, screen, vp9**
  — and was never followed by a re-record.

So the 10-02 numbers for those crates measure a decoder that was bailing out
early, not decoding. This was already suspected during the phase (the
`screen_*_decode` rows were flagged as the Phase 1 flat-black luma fix rather
than a regression); what is new is the **scope** and the mechanism.

**The trap: the same confound inflates the positive deltas too.** This is the
part worth remembering. `bitstream_rans/decode_noise` reads as **+361%** against
the old baseline, and `vp9_decode_320x240` as **+242%**. The same-session A/Bs
for those crates measured 181 -> 195 MiB/s and ~2.58x respectively. Anyone
quoting the cross-file diff would have overstated this phase by an order of
magnitude while every number looked real.

**Quantified with a control group.** The five crates this phase never touched
(face, volumetric, demux, mux, pipeline) show a mean of **-3.3%** and a range of
**-24.6%..+29%** across 31 benchmarks with *zero* code change. The optimised
crates show a median of **+19.4%**. So the phase did produce a real improvement
— but the per-benchmark deltas carry roughly ±25-30% of cross-session error on
top of it, which is the concrete argument for having A/B'd same-session
throughout. It also supersedes the "±5% noise floor" figure recorded above from
face/volumetric alone: that number was a same-session estimate and the
cross-session spread is far wider. (The AV1 residual-scratch rejection above
still stands — it was a *same-session* A/B at 1.6%, and same-session A/B is the
tighter of the two methods.)

**Durability.** `docs/PERFORMANCE.md` is fully regenerated from the bench run,
so a hand-written caveat would have been erased by the next
`just bench-baseline` — precisely when someone is about to trust a bad delta.
`bench_baseline` now carries a `<!-- perf-notes:start/end -->` section over
verbatim, mirroring the existing `ffmpeg-compare` markers, with four tests
covering preservation, ordering relative to the ffmpeg table, and rejection of
an unterminated pair. The caveat is in that section.

**What is trustworthy:** the per-change figures in this file and in the commit
messages, all measured same-session against alternating builds. The new
baseline is the reference point going forward.

### screen — Hadamard caching + natural-path allocations DONE 2026-10-02

The same matrix-rebuild bug as the lean family, in `screen/src/natural.rs`
(the natural-image fallback mode). Cached behind a `OnceLock`, and the natural
path's ~8 per-block allocations moved into one per-frame `NaturalScratch`.

| screen @ 1920x1080 | Matrix cache | + allocation removal | Total |
|:---|---:|---:|---:|
| `encode` | +29.7% | +1.6% | **~+32%** |
| `decode` | +30.6% | +1.1% | **~+32%** |

**Unlike lean/realtime/vision, the matrix rebuild was very nearly the whole
story here.** On a UI-like source most blocks classify as FLAT or GLYPH, and
those paths touch neither the transform nor the per-block buffers — so only
NATURAL blocks benefited from the second step, which is why it is worth only
~1.5%. Both changes are kept (the allocation win grows on sources with more
natural content), but it is worth recording so nobody re-derives the same
lesson on a different codec and over-credits the allocation half.

`lossless` was audited in the same pass and needs nothing: it has no transform
and no per-block allocations in its hot path. Its gain already came from the
shared bitstream rANS inverse table (11.9 -> 35.0 Melem/s decode).

### lean — Hadamard caching + allocation removal DONE 2026-10-02

Phase 2 said lean is an *encoder* story (encode ~0.49 Melem/s vs multi-Melem/s
decode). Two causes, both found by reading the hot path rather than by
speculating about arithmetic:

1. **`hadamard_2d_raw` rebuilt the transform matrix on every call.**
   `hadamard_matrix(n)` allocated a nested `Vec<Vec<i32>>` (`1 + n` heap
   allocations) and rebuilt the matrix in `O(n² log n)` — and the function is
   called once per block per frame in *both* directions, inside an `O(n^4)`
   loop that then double-indexed `h[j][l]`. The matrices are pure functions of
   `n` and `n` only ever takes a few power-of-two values, so they are now built
   once per size behind a `OnceLock` (thread-safe for concurrent decode).
   Entries and accumulation order are unchanged, so the transform stays
   bit-exact.
2. **~5 allocations per block in reconstruction, ~6 more per intra mode trial
   in encoding.** The encoder trials all 14 modes per block, so that is ~84
   allocations per block. Both directions now use a single per-frame scratch.

| Case (320x240) | Before | After | Delta |
|:---|---:|---:|---:|
| `lean_320x240/encode` | 477 Kelem/s | 610 Kelem/s | **+28%** |
| `lean_320x240/decode` | 11.7 Melem/s | 13.8 Melem/s | **+18%** |

- **A robustness bug was fixed on the way.** `min_block_size_log2` and
  `max_block_size_log2` are 4-bit fields with only `min <= max` validated, and
  `block_sizes` derives the block size from `min_block_size_log2` — so a stream
  declaring a 32k×32k minimum block made the decoder ask for a multi-gigabyte
  allocation from a few bytes of input. The parser now rejects
  `max_block_size_log2 > 6` (v1 defines 8x8..64x64), which is also what makes
  the fixed-size scratch sound rather than a buffer overflow waiting to happen.
- Bit-exactness: all 30 lean tests pass, including the `qp == 0` lossless
  round-trip suite the crate's guarantee rests on; workspace clippy `-D
  warnings` and `cargo fmt --check` clean; full workspace test suite green.
- **The same shape was present in `tpt-kinetix-realtime`, and is DONE** — it
  was a mechanical port: realtime 1080p encode **~+47%** and decode **~+36%**
  cumulative (transform caching alone accounted for +36.9% / +26.7%, the
  allocation removal for a further +7.2% / +7.1%). It had the identical
  unvalidated 4-bit `block_size_log2` field, now bounded the same way.
  **`tpt-kinetix-vision` port — DONE**: 1080p encode **+19.0%**,
  `decode_pixels` **+18.7%**. Its parser already bounded `block_size_log2` to
  3..=6, so unlike lean and realtime it needed no new validation — only the
  named `MAX_BLOCK_SIZE` constant the scratch asserts against. `decode_tensor`
  printed -5.8% in the same run, but that path is a pure block parser (it calls
  neither the transform nor the scratch), so it is machine noise, not a
  regression. **face and volumetric are the remaining originals.**

### VP9 — debug-switch environment lookups DONE 2026-10-02

Phase 2 ranked VP9's *loop filter* first (49% of decode at 320x240, 63% at
1080p), so the expectation was a filter-arithmetic problem. It was not. The
real cost was that the crate's debug switches were read with a bare
`std::env::var_os` — which locks the environment, scans it and allocates an
`OsString` — and two of the 25 read sites were in the innermost loops:

- `booldec::read_bool` read `TPT_VP9_TRACE` **once per bool decoded** (millions
  of times per frame: every coefficient token, every mode).
- `loop_filter::loop_filter_edge` read `TPT_VP9_DBG56` **once per deblocking
  edge of every superblock**, and evaluated it *before* the `off == 56` test
  that actually gates the debug output — the one int comparison it should have
  done first.

Fix: a `tpt-kinetix-vp9::dbg_env` module, mirroring the one AV1 already had
(`tpt-kinetix-av1` got this guard earlier; VP9 never did). It scans the
environment once per `decode` and, while no `TPT_VP9_*` switch is set, answers
from a single relaxed atomic load. All 25 read sites now route through it, and
the `off == 56` comparison is evaluated first.

| Clip | Before | After | Speedup |
|:---|---:|---:|---:|
| 320x240, 18000 frames | 20.64 s | 8.00 s | **2.58x** |
| 1920x1080, 2400 frames | 38.29 s | 19.51 s | **1.96x** |

A/B by reverting only the seven touched VP9 source files; identical frame
counts and sink values in both arms.

- Bit-exactness held: the libvpx row of the `ffmpeg_compare` harness reports
  `verified=true` (byte-identical planes vs ffmpeg), 13 `conformance_vp9` tests
  pass, every VP9 suite passes, workspace clippy `-D warnings` and
  `cargo fmt --check` clean, `cargo +1.82.0` MSRV check clean.
- A new unit test (`dbg_env::tests::refresh_tracks_set_and_unset_keys`) asserts
  the fast path never makes a *set* switch invisible, so the `TPT_VP9_*`
  debugging tools cannot silently stop printing.
- Lesson worth carrying to the remaining codecs: **grep for `env::var` inside
  per-block / per-edge / per-token loops before optimising any arithmetic.**
  These switches are invisible in the source's intent — they look like debug
  scaffolding. Two follow-up audits were run the same day and are worth
  recording so nobody repeats them:
  - **The original codecs are clean.** `lean`, `lossless`, `realtime`,
    `screen`, `vision`, `face` and `volumetric` contain *no* `env::var` at
    all, so they do not have this bug. (This contradicts the guess made when
    the note was first written, which is why it is spelled out here.)
  - **AV1 had two remaining leaks**, both now fixed: `decoder.rs`'s
    `KINETIX_AV1_NO_GRAIN` grain check used a bare `std::env::var_os` on the
    per-frame path, and `dbg_env::phase_enabled()` — the gate for the Phase 2
    timers themselves — did a real environment lookup on every call. Both now
    go through the existing `dbg_env` fast path, and `dbg_env` gained an
    `is_set` helper mirroring VP9's. This makes the phase instrumentation
    itself much cheaper; it does not change any decoded output.
  - After both audits, no crate is left with a bare `env::var` in a decode
    path.

### VP9 loop-filter scalar rewrite — REJECTED 2026-10-03

The Phase 2 ranking made the LF kernel the top VP9 target, so its inner loop
(`loop_filter_edge`) was rewritten three ways, each keeping the arithmetic
identical: (1) per-line window slices with usize addressing and the repeated
wide-tap loads hoisted, (2) the same plus a `Tap` trait abstraction,
(3) const-generic vertical/horizontal specializations with compile-time
window offsets. **All three measured 2.1–2.4x SLOWER than the existing
kernel** (438 us vs 1070-1205 us per frame at 320x240, clean A/B via
`git stash`). The existing i64-addressing code is already compiler-optimal
scalar — the casts fold into base+constant addressing and the bounds checks
are cheap. Lesson recorded: this kernel only moves via real SIMD
(`std::arch` with the scalar kept as oracle + equivalence proptests, per the
Phase 3 rules) or via per-superblock-row parallelism (which needs the
dav1d-style delayed-horizontal-pass restructuring to stay bit-exact). Both
are real projects, not drive-by wins.

### AV1 tiles — per-leaf residual scratch REJECTED 2026-10-02

The one AV1 pattern that looked exactly like the win that gave lean/realtime/
vision 18-47%. Both `intra_block.rs` and `inter_block.rs` allocated
`vec![0i32; leaf_tx_w * leaf_tx_h]` — up to 16 KiB — **per leaf transform
block**, and in the intra path it was allocated *before* the skip test, so a
skipped leaf paid a full malloc + memset for a buffer that stayed zero until
the add.

Implemented it as a per-tile `TileDecodeState::residual_scratch` (64×64, cleared
per use, so bit-exactness is trivially preserved), then A/B'd same-session on
the cached 1280x720 corpus via `profile_decode`, 3 iterations x 180 frames,
alternating stash/unstash builds:

| Build | samples | median |
|:---|---:|---:|
| with scratch | 16.99 / 17.14 / 17.08 s | 17.08 s |
| control | 17.36 / 17.11 / 17.38 s | 17.36 s |

**1.6% — inside the ±5% noise floor, so not a win. Reverted.** Had this been
measured against the stored Criterion baseline instead of same-session, it would
have looked like a plausible small win and shipped for nothing.

The lesson is the one that separates the two halves of this phase: for the
*original* codecs the hot loops were transform-bound, so removing per-block
allocation paid 18-47%. In AV1 the leaf sits next to a symbol decoder reading
CDF-coded coefficients, and that decode is an order of magnitude more expensive
than the malloc it would save. **Allocation was never AV1's lever** — deblock
was, and that one was two mallocs per *filtered row*, a far higher-frequency
allocation than one per leaf.

A second finding from the same attempt: the **inter** path cannot take the
scratch as-is. It calls whole-`self` methods (`read_coeffs`, the context
helpers) while the residual is live, so a borrow of `self.residual_scratch`
cannot outlive them — 7 borrow errors. The intra path has no such
interleaving. Reverting inter and keeping intra is what made it compile, and
given the measurement it was not worth further plumbing.

Not pursued for the same reason: CDEF, loop restoration, superres and film
grain are untested for perf, but none has the two-mallocs-per-row shape that
made deblock worth 15-16%. They need a sampler to rank first.

### AV1 — deblocking loop filter DONE 2026-10-02

Phase 2 ranked the deblocking filter as the #1 hot spot (62% of decode at
320x240, 70% at 720p). The cause was allocation, not arithmetic: the filter
reads at most 7 taps before and 6 after an edge, but both passes built a
`Vec<i32>` for the line *and* got a second `Vec<i32>` back from the kernel, per
filtered row. That is two mallocs per row of every luma and chroma edge.

`filter_line_1d` is now split into an allocating wrapper (kept, `#[cfg(test)]`,
as the reference oracle the existing filter unit tests check against) and
`filter_line_1d_into`, which writes into a caller-owned buffer. Both deblocking
passes use a single reusable 16-sample stack buffer per edge.

| Clip | Before | After | Delta |
|:---|---:|---:|---:|
| 320x240, 12000 frames | 36.23 s | 30.42 s | **-16.0%** |
| 1280x720, 3600 frames | 130.59 s | 110.71 s | **-15.2%** |

A/B by stashing only `loop_filter.rs`, phase timers off, cached `testsrc`
corpus, identical frame counts.

- Bit-exactness held at every gate: FATE **204/204 bit-exact vs libdav1d**,
  `libaom_crosscheck` (2 tests, incl. `libaom_streams_match_libdav1d`) ok,
  `phase_c_conformance` luma diff 0/12288, 173 lib tests + all integration
  tests pass, workspace clippy `-D warnings` and `cargo fmt --check` clean.
- Caution for the next session: the `KINETIX_AV1_PHASE=1` timers are
  themselves expensive here — the same 320x240 workload took 74 s *with* the
  timers on vs 30 s with them off, because the wrappers sit at per-edge
  granularity. They are fine for ranking phases, useless for absolute timings.
  Do not quote a timer-on number as a decode benchmark.
- Still open on AV1: the tile/entropy+reconstruction phase is the next-largest
  (36% of a 320x240 frame, 30% at 720p), plus CDEF / loop restoration /
  superres / film grain, which are ~0 on this corpus only because the clip does
  not enable them — they are untested for performance, not proven fast.

### bitstream / rANS — DONE 2026-10-02

Both ranked hot spots fixed, in `tpt-kinetix-bitstream`, with the scalar path
kept as the reference oracle:

1. **`BitReader::read_bits` refills a 64-bit window** instead of calling
   `read_bit` per bit. The per-bit *call* overhead, not the data movement, was
   the cost — Phase 2 called the reader at 101 MiB/s where a tight bit reader
   does GB/s. The bit-at-a-time path is still used for the tail (fewer than the
   window's bytes remain), so exhaustion and partial-consumption semantics are
   byte-for-byte unchanged.
2. **`SkewedModel` gained a flat inverse table** (4 KiB of `u8` for the 4096
   slots) so `SymbolModel::find` is one indexed load instead of a binary search
   over 257 cumulative frequencies — the reason `decode_noise` ran an order of
   magnitude behind the uniform-model decode.

Measured, same machine, same session, A/B by stashing only these two files:

| Case | Before | After | Delta |
|:---|---:|---:|---:|
| `bitstream_bitreader/read_bits_16` | 193.2 MiB/s | 307.6 MiB/s | **+59%** |
| `bitstream_bitreader/read_u32_be` | 203.5 MiB/s | 619.9 MiB/s | **+205%** |
| `bitstream_bitreader/read_bit` | 99.0 MiB/s | 99.0 MiB/s | unchanged |
| `bitstream_rans/decode_noise` | 41.5 MiB/s | 181.0 MiB/s | **+338%** |
| `bitstream_rans/decode_static` | 351.6 MiB/s | 351.4 MiB/s | unchanged (control) |
| `lossless_1080px_10bit/decode` | 11.88 Melem/s | 35.04 Melem/s | **+195%** |
| `lossless_1080px_10bit/encode` | 37.59 Melem/s | 37.83 Melem/s | +0.6% |
| `screen_320x240/decode` | 7.00 Melem/s | 6.92 Melem/s | −1.2% |

Notes:

- `decode_static` / `encode_*` are deliberate **controls**: they do not touch
  either changed path, and they came out identical A/B, which is what proves
  the −5…−10% that Criterion reports against the *committed* baseline is
  machine drift between sessions rather than a regression. The committed
  `docs/perf/baseline-2026-10-02.json` predates this change and several
  correctness fixes, so it must be re-recorded before it is trusted again —
  **done 2026-10-03**, see "Phase 3 closing" below.
- `screen_320x240/decode` is 1.2% slower: the screen header reads are narrow
  and the window path adds a span computation the old per-bit loop did not pay.
  That is well inside noise for a 10-sample run and is dwarfed by the +195% on
  the rANS decode path, so it is accepted, not chased.
- Do **not** trust the `screen_*_decode` rows in the committed
  `docs/PERFORMANCE.md` — they were recorded on the corrupt flat-black luma path
  fixed in Phase 1, and now read ~100x lower simply because the decoder is now
  doing the work it always claimed to do.
- The `-99%` Criterion prints on those screen rows are that correctness fix,
  not this optimisation.
- The same confound is **wider than screen/realtime decode**, and this is the
  trap worth recording: it inflates the *positive* deltas too. Commit `8291e0b`
  changed `src/` in av1, bitstream, lean, lossless, realtime, screen **and vp9**,
  so every cross-file comparison against the 10-02 baseline is contaminated in
  both directions. `bitstream_rans/decode_noise` reads as +361% against it; the
  same-session A/B for that crate measured 181 -> 195 MiB/s. **Never quote a
  number from that diff.**
- Bit-exactness guards: `read_bits` is checked against an independent
  bit-at-a-time reference for every width 1..=32 at every start offset, and the
  inverse table is checked against the old `partition_point` definition for all
  4096 slots at five skew values. All seven downstream original-codec crates
  still pass their round-trip suites.
- Still open on this item: `read_bit` itself is unchanged at ~99 MiB/s (it is a
  genuinely per-bit API, so the ceiling is the loop, not the primitive), and
  `StaticModel` still computes its slot arithmetically rather than by table.

### bitstream follow-up — whole-struct windowed reader (same day, on top)

The "still open" note above is closed. A follow-up rewrite makes a persistent
64-bit window plus a single absolute bit position the `BitReader`'s whole
state: the window refills once per 64 bits (or at end-of-buffer), every
positioning API (`bit_position` / `byte_align` / `remaining_bytes` / ...)
derives from the one counter, and the per-bit path has no bounds check at all.
Same public API, same exhaustion and partial-consumption contracts, pinned by
the existing reference-equality tests plus new mixed-read coherence tests.

Measured on top of the committed state (same machine, Criterion, `--quiet`):

| Case | committed (2026-10-02) | windowed reader | Delta |
|:---|---:|---:|---:|
| `bitstream_bitreader/read_bit` | 99.0 MiB/s | 286.8 MiB/s | **+190%** |
| `bitstream_bitreader/read_bits_16` | 307.6 MiB/s | 861.9 MiB/s | **+180%** |
| `bitstream_bitreader/read_u32_be` | 619.9 MiB/s | 1420.4 MiB/s | **+129%** |

One real bug was caught by the reference tests during the rewrite and fixed:
the refill must cap the loaded-bit count at 64 *before* subtracting the
consumed head of the current byte, or a mid-byte refill reports shifted-out
bits as valid and feeds zeros to the decoder mid-stream.

### lean / realtime / vision — scratch-slice test fix (2026-10-03)

`master` shipped with 4 failing `tpt-kinetix-lean` tests: the scratch-reuse
refactor passed whole-frame scratch buffers where
`hadamard_2d_raw` / `inverse_2d_with_scratch` debug-assert exact `n*n`
slices, so any 4x4 chroma block (scratch 64, block 16) tripped the assert.
The same unsliced pattern existed in `tpt-kinetix-realtime` and
`tpt-kinetix-vision` (latent - their tests size chroma differently). All call
sites now hand down `&mut buf[..n * n]` slices; the strict asserts stay.

### out-kinetix-h264 — debug-switch environment lookups DONE 2026-10-03

Same disease VP9 had, one crate over: 148 `std::env::var` / `var_os` call
sites, several of them per macroblock — `KINETIX_BINTRACE` read 4+ times per
MB in both the CAVLC and CABAC paths, `KINETIX_FFLAG` per MB, and
`KINETIX_SKIP_DEBLOCK` per MB in all three deblocking entry points. At 1080p
that is tens of thousands of environment locks + scans + `OsString`
allocations per frame on the hottest paths.

Fix: an `out_kinetix_h264::dbg_env` module mirroring the AV1/VP9 pattern — an
`ANY_SET` atomic re-scanned once per `H264Decoder::decode` call, so with no
`KINETIX_*` variable set every lookup is one relaxed atomic load, and with one
set the reads are exactly `std::env::var` (tools that toggle variables between
decode calls behave as before). All 148 sites now route through it; every env
key in the crate is `KINETIX_`-prefixed, which the fast path's scan assumes.

Measured against `baseline-2026-10-03.json` (committed before this change),
full `bench-compare` run, nothing else executing:

| Case | Before | After | Delta |
|:---|---:|---:|---:|
| `h264_decode_slice_1080p/parallel` | 127.75 Melem/s | 291.92 Melem/s | **+128%** |
| `h264_decode_slice_1080p/serial` | 117.25 Melem/s | 229.73 Melem/s | **+96%** |

Bit-exactness holds: all 391 crate tests pass, including the CAVLC/CABAC
conformance suites and the env-toggling dbg tests (they set variables *before*
`decode`, which re-scans).

### bench-compare drift floor (2026-10-03, methodology note)

A full `bench-compare` against the 2026-10-03 baseline with the bitstream +
h264 changes in tree reported 31 "regressions" — 24 of them in crates with
**zero code delta** in this working tree (face, vision, screen, volumetric),
at the same −5…−15% the previous session already proved is machine drift via
control rows. The one touched crate that showed rows down (realtime, from the
scratch-slice fix) was A/B'd interleaved 2×2 and the gap flips direction
between rounds: ±3% noise, no real cost. Lesson recorded for the next person:
never run two bench suites concurrently on one machine (the first attempt
showed 57 regressions purely from self-contention), and never trust a
cross-session diff below ±10%.

## Phase 3b — Closing the gap to ffmpeg (opened 2026-10-03)

Goal: from ~6-9x slower than single-threaded libdav1d/libvpx to ~2-3x. Beating ffmpeg is not a goal.
Code-shape wins (env::var, matrix caching, allocations) are exhausted; the remaining levers are build
codegen, SIMD and threading. Rules: same-session A/B only, bit-exact gates after every step, scalar
path kept as oracle. Commit or stash unrelated working-tree edits before A/B work.

- [x] 1. Build profile: root `Cargo.toml` had **no `[profile.release]`** (codegen-units=16, no LTO).
  `lto = "fat"` + `codegen-units = 1` now set for `release` and `bench`. **-4 to -5%, every
  round, every case** — see "release profile — LTO" below. `target-cpu=x86-64-v3` still
  deliberately not done (would need to be an opt-in documented build, never the default).
- [~] 2. Function-level profiler: `samply` needs an elevated shell here, so the ranking came from
  the built-in `KINETIX_AV1_PHASE=1` timers. Coarse: **tiles 53-90%, deblock 6-34%, loop
  restoration 2-13%**. Sub-split inside tiles (four new timers): coefficients 10-12%, inverse
  transform **1-2%**, intra prediction **<1%**, **~86% unattributed block syntax**. See
  "Tile-phase sub-split" below — the ~86% is the whole ballgame. Still missing: attribution
  *within* that 86% (instrument the symbol decoder, not its callers), and any MC split.
- [~] 3. SIMD kernels (runtime-dispatched `std::arch`, AVX2 with SSE4.1 fallback, scalar oracle,
  SIMD-vs-scalar proptests per kernel), in order:
  - [ ] VP9 loop filter (`tpt-kinetix-vp9/src/loop_filter.rs`, 49-63% of VP9) — still the best
    candidate in the whole file, but see the VP9 correctness prerequisite below
  - [x] ~~AV1 inverse transforms~~ — **struck 2026-10-03: measured at 1-2% of a frame**
  - [ ] AV1/VP9 motion-compensation filters — unmeasured (the AV1 inter path is unattributed)
  - [ ] AV1 CDEF + loop restoration — ~0 on the current corpus only because it does not enable
    them; still unmeasured for performance
  - [x] ~~Intra predictors~~ — **struck 2026-10-03: measured at <1% of a frame**

  **Caveat earned the hard way**: the first kernel attempted (AV1 `add_residual_row`) measured
  neutral in two profiles and six cases, because LLVM already auto-vectorises that pattern.
  Do not assume hand-SIMD wins here — measure each candidate against its own scalar form
  *first*, and prefer item 5 where the hot code is branchy rather than arithmetic.
- [ ] 4. Parallelism on single-tile streams:
  - [ ] Superblock-row post-filters (needs dav1d-style delayed horizontal pass to stay bit-exact)
  - [ ] Frame-level overlap of loop filter (frame N) with entropy decode (N+1)
  - [ ] VP9 tile-column threading
- [ ] 5. Entropy decode: symbol decoder refill, branchless CDF adaptation, coef-context lookups.
- [ ] 6. Allocation/memory: only if the profiler shows it matters (AV1 evidence says no).
- [ ] Prerequisite for trustworthy VP9 speed numbers: fix the VP9 loop-filter skip-edge correctness
  bug (rows read `UNVERIFIED` until then).
- [ ] Headline metric: refresh `just bench-ffmpeg` ratio vs `-threads 1` ffmpeg after each major step.

Out of scope: H.264, the original codecs, anything that changes decoded output.

### AV1 reconstruct — decode bench, allocations and SIMD NEUTRAL 2026-10-03

The AV1 crate had **no decode bench at all** (only `av1_encode`), which is why
two previous attempts at AV1 reconstruct work could only be judged by eyeballing
wall clock. Added `tpt-kinetix-av1/benches/av1_decode.rs` with two corpora:
`av1_decode/fate_corpus` (the six `fixtures/av1-fate` streams, 204 frames — the
same corpus the bit-exactness gate uses) and `av1_decode/320x240` / `1280x720`
(ffmpeg `testsrc` + libaom, 60 / 20 frames).

With that in place, two changes to the intra reconstruct path were measured
against `dd59364` (the parent of the session's auto-commit `071dae4`),
interleaved 2x2 in one session, `sample-size 25`:

| Case | OLD (dd59364) | NEW | Delta |
|:---|---:|---:|---:|
| `fate_corpus` | 1.695 s / 1.740 s | 1.673 s / 1.764 s | ±2% (noise) |
| `320x240` | 157.4 ms / 156.6 ms | 157.5 ms / 156.3 ms | ±0.5% (noise) |
| `1280x720` | 642 ms / 633 ms | 643 ms / 636 ms | ±1% (noise) |

**Verdict: neutral — no win, no regression.** What was in the change:

1. `src/simd.rs` (new): runtime-dispatched `add_residual_row`
   (`pred + residual -> clamp -> u16`, the §7.11.2.1 write-back), AVX2 /
   SSE4.1 / scalar, with a scalar oracle, an exact i32-overflow guard so the
   vector paths are bit-identical to the oracle for *every* input, and
   SIMD-vs-scalar equivalence proptests.
2. `inverse_dct_permute` / `adst_input_permute` / `adst_output_permute`: the
   `t.to_vec()` copies replaced by a stack array / cycle chase. That was one
   heap allocation per 1-D transform, i.e. `w + h` per transform block.
3. `inverse_transform`: the two per-block `vec![i64]>` replaced by a stack
   `t` and a per-thread reusable `residual` scratch (grows once, never shrinks).

The reason the SIMD kernel buys nothing is worth recording: **the scalar
version already auto-vectorises.** `clamp` after a plain wrapping add is a
pattern LLVM turns into exactly the same `pmaxsd`/`pminsd`/`packus` sequence
the hand-written AVX2 path emits. The same trap as the VP9 loop-filter scalar rewrite above, one level
deeper: on this compiler and ISA, "scalar" is not a baseline.

All three changes are kept: they are bit-exact (FATE **204/204 vs dav1d**,
`libaom_crosscheck` 2 passed, `phase_c_conformance` luma diff 0/12288, 179 lib
tests), they remove real work from the allocator, and the SIMD module is the
crate's only vectorised kernel with a testable oracle — but they are **not**
claimed as a speedup. Recorded here so the next session does not re-measure
them as if they were still untried.

**Re-measured under fat LTO (same day, later): still neutral.** Because the
kernel's dispatch is a runtime env switch that does *not* trip `dbg_env`, this
A/B needs no rebuild at all — the same binary was run alternately with and
without `TPT_AV1_NO_SIMD`, 4 paired rounds:

| Case | mean simd-vs-scalar | per-round spread |
|:---|---:|:---|
| `fate_corpus` | **-0.3%** | +2.1 / -1.0 / -1.9 / -0.6 |
| `320x240` | **+1.2%** | +6.5 / +2.0 / -7.0 / +3.3 |
| `1280x720` | **+0.8%** | +3.9 / +2.0 / -6.6 / +4.1 |

Means within ±1.2%, round-to-round spread ±7% and sign-flipping. Two profiles,
six corpora-cases, one verdict: **the hand-written AVX2 buys nothing on this
ISA, with or without LTO.** If someone wants to delete it, `src/simd.rs` plus
the 15-line fast path in `reconstruct_block.rs` are the whole removal, and the
scalar fast path should stay — the allocation removals are the part of this
work with independent justification.

**Methodology trap found the hard way — do not A/B AV1 through a `KINETIX_*`
variable.** The first version of the kernel's override switch was
`KINETIX_AV1_NO_SIMD`. `dbg_env`'s `ANY_SET` scanner treats *every*
`KINETIX_*` variable that is not `*_DIR` / `KINETIX_BENCH_ITERS` as "a debug
switch is on", which converts ~200 per-block `std::env::var` lookups (OS lookup
+ `String` allocation each) back on. The A/B read **4.43 s vs 1.69 s** — a
"2.7x SIMD win" in which both arms ran the *identical* kernel. Renaming the
switch to `TPT_AV1_NO_SIMD` (no `KINETIX_` prefix) collapsed the two arms to
within 1%, which is the real answer. Any future perf knob in this crate must
avoid the `KINETIX_` prefix, or be excluded from the scanner the way
`*_DIR` is.

### release profile — LTO + codegen-units=1 DONE 2026-10-03

The workspace shipped with **no `[profile.release]` at all**, i.e. Cargo's
defaults: `opt-level = 3`, `codegen-units = 16`, no LTO. Set
`lto = "fat"` + `codegen-units = 1` on both `release` and `bench` (stated
explicitly on `bench` so a later edit to `release` cannot silently move the
published numbers). `panic` stays `unwind` — every crate here is a library.

**-4 to -5%, and unlike everything else in this file it survived every round of
every case.** Medians of 3 paired rounds, both binaries pre-built and then
alternated run-by-run in one window (see the method note below — this is the
only configuration that produced a trustworthy result, because rebuilds of
1.3-2.4 min each were themselves shifting the machine):

| Case | default profile | fat LTO | Delta |
|:---|---:|---:|---:|
| `av1_decode/fate_corpus` | 1.833 s | 1.752 s | **-4.4%** |
| `av1_decode/320x240` | 165.5 ms | 159.0 ms | **-4.0%** |
| `av1_decode/1280x720` | 707.4 ms | 671.1 ms | **-5.1%** |
| `vp9_decode_1280x720` | 37.79 ms | 35.23 ms | **-6.8%** |
| `vp9_decode_1920x1080` | 79.73 ms | 75.70 ms | **-5.1%** |

Paired, every LTO run beat the default run it was interleaved with, on all
three AV1 corpora — 9/9 pairs. The VP9 rows are from an earlier same-session
pair (also LTO-favourable, but 1 pair only, so treat them as indicative).

Gates under the new profile: FATE **204/204 bit-exact vs dav1d**,
`libaom_crosscheck` 2 passed (93 s), `phase_c_conformance` luma diff 0/12288,
AV1 lib tests 179 passed **in release**, VP9 lib+integration tests pass in
release, full `cargo build --release --workspace` clean (1.7 min incremental),
and `wasm32-unknown-unknown` still builds for `tpt-kinetix-core` /
`tpt-kinetix-demux` (the web-demo target). LTO is a codegen change, so the
release-mode test runs above are the ones that matter — `cargo test` alone uses
the dev profile and would not have exercised this at all.

### Tile-phase sub-split (item 2 completed) — and it kills two SIMD targets 2026-10-03

Added four tile sub-phase timers (`coeff_ns`, `itx_ns`, `pred_ns`, `mc_ns`) to
`dbg_env::Av1PhaseTimers`, wrapped around `read_coeffs`, dequant +
`inverse_transform`, and the intra-prediction dispatch in
`reconstruct_block.rs`. Free when `KINETIX_AV1_PHASE` is unset (same guarded
predicate as the existing six phases); a new second report line keeps the
existing phase line's format intact for anything scraping it.

Same corpus, same run (`av1_fate_score`, 204 frames):

| Phase | frames 1-100 | frames 101-200 |
|:---|---:|---:|
| tiles (total) | 7617 us | 17718 us |
| — coefficients (`read_coeffs`) | 764 us (10.0%) | 2156 us (12.2%) |
| — dequant + inverse transform | 141 us (1.9%) | 212 us (1.2%) |
| — intra prediction | 46 us (0.6%) | 70 us (0.4%) |
| **— everything else in the tile phase** | **~87%** | **~86%** |

**This is the most actionable measurement in the file.** Only ~13% of the tile
phase is coefficient reading, inverse transform and prediction combined. The
inverse transform — the thing `inverse_dct`/`inverse_adst` do, and the obvious
"vectorise the transforms" target — is **1-2% of a frame**. Intra predictors are
**under 1%**. Neither can ever repay a SIMD kernel, let alone both.

The remaining ~86% is the *non-coefficient* symbol decoding: partition/mode
syntax, intra mode + angle delta + filter params, palette/CFL syntax, tx-size
derivation, MV prediction, and dequant (untimed here, but it sits between the
two timers and is nowhere near 80% of a frame). So:

- **Item 3's AV1 rows "inverse transforms" and "intra predictors" are dead
  ends — struck, with a measurement rather than a guess.**
- **Item 5 (entropy decode) is the only high-value target left in the AV1
  decoder**, and specifically the *block syntax* reads rather than the
  coefficient reads everyone assumed. That also explains, retroactively, why
  every AV1 reconstruct-side attempt in this file has been a wash: the
  reconstruct side is not where the time is.

Caveats, stated rather than buried: three `Instant::now()` pairs per transform
block inflate the tile total (`tiles` moved 9374 -> 7617 us in block 1 and
16526 -> 17718 us in block 2 across runs, i.e. the overhead is real and
sign-inconsistent), and it is *additive*, so if anything the ~13% is an
**over**estimate and the ~86% an underestimate. Also `mc` reads 0: the inter
path's `motion_compensate*` calls sit inside nested arms of a 227 KB function
and were deliberately left uninstrumented rather than risk that file, so the
inter share of the ~86% is unattributed. The FATE corpus is intra-heavy.

**Concrete next step**: instrument the symbol decoder itself (reads + CDF
updates) rather than its callers, which attributes the ~86% to specific syntax
elements and directly informs item 5's three sub-items (refill, branchless CDF
adaptation, context lookups).


`samply` needs an elevated shell on this box, so the ranking came from the
`KINETIX_AV1_PHASE=1` timers instead (`av1_fate_score`, 204 frames, two 100-frame
report blocks):

| Phase | frames 1-100 | frames 101-200 |
|:---|---:|---:|
| tiles (entropy + reconstruct) | 9374 us (53%) | 16526 us (90%) |
| deblock | 6032 us (34%) | 1505 us (9%) |
| loop restoration | 2341 us (13%) | 275 us (2%) |
| film grain | 74 us (0.4%) | 0 |
| CDEF / superres | 0 | 0 |

(The timers sit inside a `KINETIX_`-prefixed run, so `dbg_env` is on its slow
path and absolute numbers are inflated — most of that overhead lands in the
per-block tile phase, so treat the *tile* share as an upper bound and the
deblock/restoration shares as lower bounds. The ordering is what matters.)

Two things follow, and both change the plan:

1. **The tile phase is 53-90% of decode.** Everything else combined is under
   half, and deblock — the one AV1 win this project has (-16%) — is already
   spent. Loop restoration is the only untouched post-filter showing real time
   (13% on the first block), while CDEF/superres/film-grain are ~0 *only because
   this corpus does not enable them*: still unmeasured for performance, exactly
   as the deblock entry warned.
2. **Item 5 (entropy decode) should be attempted before more of item 3 (SIMD).**
   The tile phase lumps symbol decode, coefficient read, inverse transform and
   motion compensation, and every AV1 measurement so far says the entropy
   decode dominates whatever it sits next to (that is what sank the two rejected
   allocation attempts). SIMD is the right tool for the transform/MC/post-filter
   slice; the entropy slice wants refill, branchless CDF adaptation and cheaper
   context lookups instead. Ranking those four needs finer timers than the
   current six phases — **that is the concrete next step**, and it is cheap:
   four more `av1_timed` wrappers around the existing call sites, gated on the
   same env var so they cost nothing when off.


Cost: a cold release build of the 19-crate workspace goes from roughly a minute
to a few minutes. Worth it for a decoder, and the dev profile is untouched, so
edit-build-test loops are unaffected.

**Method note — the measurement design matters more than usual here.** The
first three attempts (rebuild-and-measure per arm) produced a *reversing*
signal: one round said LTO was 6% faster on `fate_corpus` and 1.4% faster at
720p, the next said 4% faster and 0% — because a 2-minute rebuild between arms
warms/cools the box more than the effect being measured. Only after building
both bench binaries once and alternating them run-by-run did the result become
monotonic. **For any future change whose expected effect is under ~5%, build
both binaries first, then alternate runs; never rebuild between arms.**

This also retroactively explains item 3's disappointing first entry: the SIMD
kernel was measured under the *default* profile, where the compiler had the
least cross-crate information. Re-measuring it under fat LTO is still open.

## Phase 4 — Guardrails (every optimisation change)

- [x] `just check` passes — `cargo fmt --all --check` clean, `cargo clippy --workspace --all-targets
  -- -D warnings` clean, `cargo test --workspace --lib --bins --tests` exit 0
- [x] `just conformance` unchanged (`pixel_exact` stays true); FATE 204/204; `tests/libaom_crosscheck.rs` passes
  — re-run **under fat LTO** (a codegen change, so the release-profile runs are the ones that count):
  FATE 204/204, libaom_crosscheck 2 passed, `phase_c_conformance` luma diff 0/12288
- [x] Original codecs: existing roundtrip / bit-exact tests pass — VP9 lib+integration tests pass in
  release; workspace suite green
- [x] SIMD-vs-scalar equivalence proptests for each vectorised kernel — `simd::tests::prop_add_residual_row_matches_scalar` (AV1 `add_residual_row`), plus edge/extremes/clamp unit tests
- [ ] Relevant fuzz target run >= 60s after touching any parser — attempted
  2026-10-03: `cargo +nightly fuzz run` cannot link on this box (the installed
  nightly's sysroot is missing `librustc-nightly_rt.asan.a`; `--sanitizer none`
  still links it). CI's scheduled `fuzz.yml` workflow is the coverage path;
  local runs need a nightly reinstall first
- [ ] CI nightly/manual bench job uploads the report

## Open questions

- Target hardware for headline numbers: dev machine only, or also RPi-class ARM (lean/face budgets)?
- Regression threshold for `bench-compare`.

## 2026-10-03 — real profile: entropy decode is NOT the bottleneck

Built `tpt-kinetix-av1/examples/prof_sample.rs` (in-process sampler: suspends the
decode thread every 0.5 ms, resolves RIPs through inlined frames; also a counting
allocator that attributes allocation sites). Run with
`cargo run --profile profiling -p tpt-kinetix-av1 --example prof_sample -- "" 2`
(`PROF_ALLOC=1` for allocation sites). Needs no elevation, unlike `samply`.

FATE corpus, main thread (only the calling thread is sampled, rayon workers are not):

| Share | Where |
|---:|:---|
| 17-18% | blocked in rayon join (`ZwWaitForAlertByThreadId`) — idle, not work |
| ~8% | `warp::warp_affine_8x8` |
| ~5% | `inter::filter_rows_h` (+ `motion_compensate`/`gather_patch` ~5%) |
| ~10% | heap alloc/free (`RtlAllocateHeap`, `RtlFreeHeap`, `NtFree/AllocateVirtualMemory`) |
| ~5% | `sgrproj_filter_plane`, ~4% `filter_line_1d_into` |
| **~3%** | `entropy::read_symbol` |

Only ~4.5M `read_symbol` calls decode in ~1.6 s (~350 ns/symbol of total wall
time), so the earlier "86% unattributed block syntax" is inter reconstruction and
allocation, not the arithmetic decoder. **Item 5's refill/CDF micro-opts were
implemented and are bit-exact (FATE 204/204) but measure neutral** (1.567 vs 1.572 s).
Kept because they are correct and cheap (bit-by-bit refill -> 32-bit window,
`leading_zeros` floor_log2, split CDF update, trace flag one atomic load).

Allocation: 3.43M allocations / 2.68 GB per corpus pass. Pooling the MC scratch
(`inter.rs` `take_buf`/`put_buf`) -> 2.57M / 1.92 GB, ~4-5% faster in an alternating
A/B (noisy: base 1.68/1.73/1.73 s vs 1.68/1.67/1.55 s). Remaining top sites:
`inter_predict_plane` tmp `Vec<Px>`, `apply_obmc`, per-block `residual` Vecs in
`add_inter_residual`, `read_coeffs` `quant` Vec, `inter_mv_stack` scratch,
`split_into_subblocks`, `block_borders`.

Next: pool those remaining per-block Vecs; then warp_affine_8x8 and filter_rows_h
(the actual arithmetic hot spots, where SIMD may now be justified); investigate the
17% rayon wait (post-filter load imbalance).

### Follow-up (same day): pooling + warp patch, measured

Alternating A/B of prebuilt bench binaries (`fate_corpus`, 4 pairs each):

| Step | Mean | vs previous |
|:---|---:|---:|
| base (entropy micro-opts only) | 1.656 s | |
| + MC scratch pool, per-block residual/pred buffer pool (`pool.rs`; 3.43M -> 1.98M allocs, 2.68 -> 1.62 GB) | 1.569 s | -5.3% |
| + warp 15x15 patch gathered once (`gather_warp_patch`) instead of 960 clamped taps/8x8 | 1.525 s | -3.4% |

Cumulative ~-8%. FATE 204/204 and the av1 test suite stay green. Remaining
allocation sites are small per-block Vecs (`inter_mv_stack` stack/out, `block_borders`,
`split_into_subblocks`, `read_coeffs` quant, obmc jobs). Next: `filter_rows_h`
(row-wise 8-tap, SIMD candidate), `sgrproj`, and the 17% rayon wait.

**Rejected 2026-10-03:** tap-major rewrite of `inter::filter_rows_h` (8 contiguous multiply-add passes per row, intended to vectorise) — bit-exact but consistently ~1.5% *slower* (1.540 vs 1.511 s, 4/4 alternating pairs): most MC blocks are narrow, so eight passes over a short row lose to the fused per-pixel form. Reverted. A real win here needs explicit SIMD on a transposed/padded layout, not a loop reshuffle.

**Allocation round 2 (2026-10-03):** OBMC job list -> fixed array; intra `BlockBorders`, `CoeffBlock.quant`, `dequantize_coeffs_qm` output, intra `residual`/`pred` and the OBMC prediction now come from `pool::Pooled`. Bit-exact (FATE 204/204). Measured only ~-0.8% (1.536 vs 1.549 s, 4 alternating pairs, within noise): the allocator is no longer a lever — the remaining ~2M allocations are small and cheap. Stop pooling; next gains must come from arithmetic (explicit SIMD for `filter_rows_h`/`motion_compensate`, ~9%, then `sgrproj`/`deblock`, ~3% each) or the 17% rayon wait.
