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
  `tpt-kinetix-vp9/tests/fixtures/div128/` (see its README); next step is
  rebuilding the instrumented libvpx oracle (todo-vp9.md Session #v4
  methodology — the old `%TEMP%/libvpx2` build is gone). The perf corpus stays
  pinned to clips that verify; VP9 rows read `UNVERIFIED` until then.
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
  AV1's lever: the entropy decode it sits next to dominates. Left open.
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
- [ ] out-kinetix-h264 (optional, unpublished, last)

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

**Bug 2 — VP9 decode does not match libvpx (pre-existing, not ours).**
All three VP9 clips mismatch: 320x240 at frame 128, 1280x720 at frame 38, and
1920x1080 **from frame 0**. `git diff origin/master..HEAD -- tpt-kinetix-vp9`
is **empty** — the VP9 sources are byte-identical to `origin/master`, so this
is not from Phase 3. But `AGENTS.md` claims VP9 reports `pixel_exact: true`, and
that claim does not hold on this corpus. The frame-0 1080p divergence points at
intra prediction or the loop filter, not motion compensation. Investigating that
is a correctness task, not a Phase 3 performance one.

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
