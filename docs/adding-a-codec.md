# Adding a New Codec via the `tpt-kinetix-kg` Pipeline

This document describes the repeatable process for integrating a new video codec
(audio codecs live in the separate `tpt-cadence` repo and do not belong here) into TPT Kinetix using the `tpt-kinetix-kg` knowledge-graph tooling. Following this process
consistently keeps each codec crate coherent, fuzz-hardened, and parallelism-aware from
the start.

> This is the "hand-completion from real C source" path. If you just need an empty crate
> skeleton (Cargo.toml, lib.rs, fuzz target) — e.g. because you're implementing a codec from
> a written spec rather than porting a C decoder — start with the `cargo-generate` template
> described in [`CONTRIBUTING.md`](../CONTRIBUTING.md#adding-a-new-codec-crate) instead. Most
> new codecs use the template first, then optionally layer this KG workflow on top.

---

## Overview

The `tpt-kinetix-kg` pipeline converts C source code (typically from FFmpeg's `libavcodec/`
directory) into a structured knowledge graph and then generates a Rust scaffold. That
scaffold is the starting point for a hand-completed, production-quality decoder or encoder
crate. The full flow looks like this:

```
FFmpeg C source
    │
    ▼
tpt-kinetix-kg ingest    ← parse C AST, emit graph statistics
    │
    ▼
tpt-kinetix-kg graph     ← export full knowledge graph as JSON
    │
    ▼
tpt-kinetix-kg analyze   ← identify independent decode units / parallelism points
    │
    ▼
tpt-kinetix-kg codegen   ← emit Rust scaffold crate
    │
    ▼
Hand-completion      ← parser tables, entropy decoding, transforms, prediction
    │
    ▼
Validation harness   ← pixel-diff against `ffmpeg -f rawvideo`
    │
    ▼
Fuzz + conformance   ← cargo-fuzz + tpt-kinetix-test-utils
```

---

## Step 1 — Obtain the C Source

Download or locate the codec's C implementation. The canonical source is FFmpeg's
`libavcodec/` directory. For example:

```
libavcodec/vp8.c            # VP8 video decoder
libavcodec/vp9.c            # VP9 video decoder
```

Some codecs span multiple files (e.g. H.264 uses `h264dec.c`, `h264_cabac.c`,
`h264_cavlc.c`, `h264_loopfilter.c`, etc.). Ingest each file separately and merge the graphs later, or
ingest the primary decoder file first and add supporting files incrementally.

Prefer a pinned FFmpeg release tag rather than `HEAD` so the graph is reproducible
across different developer machines.

---

## Step 2 — Ingest and Get Graph Statistics

```bash
tpt-kinetix-kg ingest path/to/codec_decoder.c
```

This command parses the C source, builds the internal graph, and prints a summary:

```
Nodes:  2 841
Edges:  9 203
  Functions:   147
  SwitchCases: 312
  LoopBodies:   89
  DataDeps:  8 655
Parse time: 340 ms
```

Review the node/edge counts before proceeding. A very low function count may indicate
the ingestion missed `#include`-d helper files; a very high switch-case count is normal
for entropy-coded bitstream parsers.

---

## Step 3 — Export the Full Graph

```bash
tpt-kinetix-kg graph path/to/codec_decoder.c -o codec.json
```

The output is a JSON document containing every node and edge the ingestion pass
extracted. It is the input to all subsequent pipeline steps. Consider keeping `codec.json`
under version control (e.g. a `graphs/` directory of your own choosing — the repo does not
ship one) so graph evolution can be tracked alongside code changes.

---

## Step 4 — Inspect the Graph

Open `codec.json` in any JSON viewer (or pipe through `jq`). Look for three node types
that are particularly informative:

### Function nodes — decode entry points

```json
{ "type": "Function", "name": "ff_vp8_decode_frame", "file": "vp8.c", "line": 2103 }
```

These are the primary decode entry points. The top-level decode function is usually
called `ff_<codec>_decode_frame` or `<codec>_decode_frame`. Identify it early; the
generated Rust scaffold will produce a corresponding `decode_frame` method.

### SwitchCase nodes — bitstream state machines

```json
{ "type": "SwitchCase", "parent": "decode_mb_mode", "cases": 12, "file": "vp8.c" }
```

Large switch statements with many cases correspond to the codec's state machine:
macroblock type dispatch, prediction mode dispatch, entropy code tables. Each will
become a Rust `match` expression in the scaffold.

### LoopBody nodes — slice/tile loops that may be parallelisable

```json
{ "type": "LoopBody", "parent": "decode_slice_row", "iter_var": "mb_x", "file": "vp8.c" }
```

Loop bodies over spatial units (macroblocks, CTUs, tiles, slices) are the primary
parallelism candidates. Note which iteration variables are used — if the loop body
reads only from previously decoded rows (no write-after-read hazards across
iterations), `rayon` can safely parallelise it.

---

## Step 5 — Identify Independent Sets

```bash
tpt-kinetix-kg analyze codec.json
```

This command runs a dependency-analysis pass over the graph and emits a report of
independent decode units — groups of operations that share no data dependencies and
can therefore execute concurrently:

```
Independent sets found: 3
  Set A: slice-row decode (mb_y axis) — no inter-row dependency in intra frames
  Set B: AC coefficient inverse-transform per macroblock — embarrassingly parallel
  Set C: deblocking filter horizontal pass — independent after vertical pass
Recommended rayon injection points: 2 (Set A, Set B)
```

Treat the analysis output as a guide, not gospel. The dependency analysis operates
on the C graph and may miss implicit dependencies communicated through shared mutable
state (global arrays, thread-local caches). Always review the injected `par_iter`
calls during hand-completion.

---

## Step 6 — Generate the Rust Scaffold

```bash
tpt-kinetix-kg codegen codec.json \
    --crate-name tpt-kinetix-{codec} \
    --inject-rayon \
    --output-dir src/generated/
```

The codegen step emits:

- `src/generated/functions.rs` — stub functions for the C functions found in the graph
- `src/generated/mb_states.rs` — enums for macroblock/state nodes (only emitted if the
  graph contains any `MacroblockState` nodes)
- `src/generated/parallel_sets.rs` — `rayon` injection points from the Step 5 independent
  sets (only emitted with `--inject-rayon` and when independent sets exist)
- `src/generated/mod.rs` — top-level module re-exporting the above

The scaffold is much thinner than a full decoder skeleton: it does not mirror C structs or
generate `match` dispatch from SwitchCase nodes.

Commit this generated output before making hand edits. That way `git diff` clearly
separates machine-generated code from hand-written additions.

---

## Step 7 — Hand-Complete the Scaffold

The scaffold is a starting point only; expect stubbed bodies. Hand-completion is the largest
effort in the process. Work through these sub-steps in order:

1. **Parser tables**: fill in VLC/Huffman tables, quantiser matrices, and scan orders.
   Cross-reference the codec spec directly — do not copy-paste from FFmpeg to avoid
   licence contamination; implement from the spec.

2. **Entropy decoding**: implement CAVLC, CABAC, or Huffman decoding as appropriate.
   H.264 uses CABAC or CAVLC; VP8/VP9/AV1 use boolean/multi-symbol arithmetic
   coding. Entropy decoding is almost never parallelisable and must be done serially
   before the parallel reconstruction stages.

3. **Inverse transform**: implement the codec's integer DCT or MDCT. Verify
   numerically against reference output before wiring into the full pipeline.

4. **Prediction modes**: intra and inter prediction. Inter prediction requires
   correctly implementing the decoded picture buffer (DPB) and reference frame
   management.

5. **Loop filters**: deblocking and any codec-specific post-filters (CDEF and loop
   restoration in AV1). These often have subtle ordering constraints.

---

## Step 8 — Write a Pixel-Diff Validation Harness

Before declaring correctness, compare decoded output frame-by-frame against a reference
decoder (FFmpeg, or a codec's own reference such as dav1d/JM):

```bash
# Decode a test clip with FFmpeg to raw YUV
ffmpeg -i test_clip.mp4 -f rawvideo -pix_fmt yuv420p reference.yuv
```

The CLI has no raw-YUV `decode` subcommand (`tpt-kinetix` currently offers `probe`,
`transcode`, `stream` and `vision`), so drive your decoder from an integration test in
`tpt-kinetix-test-utils` instead. That crate provides:

- `reference` — helpers that shell out to reference decoders
  (`decode_h264_with_ffmpeg`, `decode_av1_with_dav1d`, `ffmpeg_available`, ...)
- `pixel_diff` — `psnr_yuv420p`, `within_tolerance`, `luma_diff_count`
- `corpus` / `synthetic` — generated test streams

Examples live in `tpt-kinetix-test-utils/tests/conformance.rs`. Prefer an exact match
(zero differing samples) over a PSNR threshold: a decoder is either bit-exact or it isn't,
and PSNR figures hide small drift that cascades through inter prediction.

---

## Step 9 — Wire in a `cargo-fuzz` Target

Every bitstream parser must have a fuzz target before the codec is considered
production-ready:

```rust
// fuzz/fuzz_targets/fuzz_<codec>_parser.rs
#![no_main]
use libfuzzer_sys::fuzz_target;
use tpt_kinetix_{codec}::parse_bitstream;

fuzz_target!(|data: &[u8]| {
    let _ = parse_bitstream(data);
});
```

Fuzz targets live in a per-crate `fuzz/` directory that is excluded from the workspace
(see `exclude` in the root `Cargo.toml`; add yours there). Run the fuzzer for at least
60 s per target after touching a parser (`just fuzz <crate> <target> 60`) and longer before
a release, and commit crash-inducing inputs into `fuzz/corpus/<target>/` as regression
cases.

---

## Step 9.5 — Extracting Spec-Mandated Numeric Tables

Codecs are full of numeric tables the spec mandates exactly (CABAC context-init tables,
VLC tables, scan orders, quantization matrices, CDF tables, ...). These are usually
transcribed once by hand from a reference decoder like FFmpeg — and a single wrong digit
is easy to miss and hard to catch: H.264's `TRANS_IDX_LPS[28]` was wrong for a long time
and was only found by building a whole separate C harness and diffing bit-exact decode
output. Don't repeat that: use `tpt-kinetix-kg`'s table-extraction tooling instead of
retyping tables from a PDF or C file by hand.

1. Find the array in FFmpeg's C source (`libavcodec/<codec>*.c`) and note its exact name
   and the commit hash you're reading it at.
2. Sanity-check the extractor against it:
   ```sh
   cargo run -p tpt-kinetix-kg -- fetch-source \
     --commit <commit-hash> --file libavcodec/foo.c
   cargo run -p tpt-kinetix-kg -- extract-tables \
     .cache/ffmpeg/<commit-hash>/libavcodec/foo.c --symbol some_table_name
   ```
   This prints the array flattened to a plain integer list in source order — paste it
   into your new Rust `const`, reshaping to whatever `[T; N]` / tuple layout you need
   (the tool doesn't infer Rust types, only extracts the raw numbers).
3. Above the resulting `const`, add a `// verify-tables:` marker so it stays checked
   against upstream forever after, instead of being a one-shot transcription:
   ```rust
   // verify-tables: rust=SOME_TABLE symbol=some_table_name commit=<commit-hash> file=libavcodec/foo.c
   pub const SOME_TABLE: [i8; 64] = [ ... ];
   ```
   If your Rust const only covers part of a larger combined C array (see
   `CABAC_CTX_INIT_PB0/1/2` in `out-kinetix-h264/src/cabac_tables.rs`, each a slice of
   FFmpeg's single `cabac_context_init_PB[3][1024][2]`), add `range=start:end` — the
   half-open index range into the *flattened* C array your const corresponds to.
4. `cargo run -p tpt-kinetix-kg -- verify-tables path/to/your_file.rs` re-fetches the
   pinned commit and re-diffs every marked const, failing loudly with the exact
   mismatched indices if anything drifted — this is the check that would have caught
   `TRANS_IDX_LPS[28]` immediately instead of requiring a bespoke harness. It's wired
   into `just check` via `just verify-tables`.

Note this needs network access to fetch the pinned FFmpeg commit — nothing from FFmpeg
(LGPL/GPL) is ever committed into this repo (Apache/MIT); fetched files land in the
gitignored `tpt-kinetix-kg/.cache/` and are re-fetched on demand. See
`tpt-kinetix-kg/src/fetch_source.rs` and `tpt-kinetix-kg/src/table_extract.rs`.

Not every table is a simple named C array — some (like H.264's `RANGE_TAB_LPS`/
`TRANS_IDX_LPS`/`TRANS_IDX_MPS` in `out-kinetix-h264/src/entropy.rs`) are packed by
FFmpeg into one combined byte blob (`ff_h264_cabac_tables`) with hand-computed offsets;
those aren't yet wired to `verify-tables` and still rely on the doc-comment provenance
trail instead. Extending the extractor to unpack a unified byte blob is a reasonable
follow-up if you hit one of these for a new codec.

---

## Step 10 — Write Conformance Tests

Add conformance tests using `tpt-kinetix-test-utils` that cover:

- ITU-T or IETF conformance test vectors (if available for the codec) — e.g.
  `just fetch-h264-conformance` downloads the free ITU-T H.264 suite
- Boundary conditions: zero-length frames, maximum resolution, all-intra streams, odd
  (non-multiple-of-block) dimensions
- Profile/level combinations relevant to the target use case
- Assert `capabilities().pixel_exact` matches what the tests actually prove

Add these as integration tests under `<crate>/tests/` (or in
`tpt-kinetix-test-utils/tests/conformance.rs` for cross-crate comparisons); `just
conformance` prints each decoder's `DecoderCapabilities`.

---

## Decision Table: tree-sitter-c vs. libclang

| Criterion | tree-sitter-c | libclang |
|-----------|---------------|----------|
| **Build complexity** | Low — pure Rust dependency, no system LLVM required | High — requires a matching LLVM/Clang installation |
| **Parse accuracy** | Syntactic only; cannot resolve macros or typedefs | Full semantic analysis; resolves all macros, typedefs, includes |
| **Speed** | Very fast (incremental parsing) | Slower; performs full compilation |
| **Graph quality** | Good for control flow; poor for data types | Excellent for data-flow and type information |
| **Maintenance** | Simpler CI setup | CI must pin an LLVM version |
| **Recommendation** | Use for initial codec exploration and control-flow graphs | Use when data-type accuracy matters (e.g. struct layout, bitfield sizes) |

For the majority of codec ingestion tasks, `tree-sitter-c` is sufficient and is the
default backend. Switch to libclang if the analysis pass reports unresolved type
references that affect the quality of the independence analysis.

---

## Typical Effort Estimates by Codec Complexity

| Complexity Class | Examples | KG scaffold (days) | Hand-completion (weeks) | Total estimate |
|-----------------|----------|--------------------|------------------------|----------------|
| **Simple** | VP8, PCM | 0.5 | 2–3 | 3–4 weeks |
| **Medium** | VP9, MPEG-2 video | 1 | 4–6 | 5–7 weeks |
| **Complex** | H.264, AV1, MPEG-4 ASP | 1–2 | 8–12 | 10–14 weeks |
| **Very complex** | VVC/H.266 (HEVC was evaluated and dropped — see `codec-evaluations/hevc.md`) | 2–3 | 14–20 | 16–24 weeks |

These estimates assume one experienced Rust developer who is familiar with the codec
spec but is implementing it in Rust for the first time. Reuse of entropy-decoding
and transform modules from prior codec crates can reduce the hand-completion effort
by 20–40 % for subsequent codecs in the same family.

---

## Tips for Identifying Parallelism Opportunities

1. **Look for loop bodies over spatial units** (`mb_x`, `mb_y`, `tile_idx`,
   `slice_idx`). If the body reads only from the current row's left neighbour and the
   row above (causal neighbourhood), a wavefront-parallel scheme is feasible.

2. **Separate entropy decoding from reconstruction**. Entropy decoding in CABAC is
   inherently serial. The reconstruction pass (prediction + transform + loop filter)
   is usually independent per macroblock row once entropy decoding is complete.

3. **Check for explicit `ff_thread_*` calls in the FFmpeg source**. These are
   FFmpeg's own threading annotations and are strong hints that the upstream
   developers already verified the independence.

4. **Use the `analyze` output's "Set B" candidates cautiously**. Coefficient-level
   parallelism (e.g. per-macroblock inverse DCT) adds Rayon overhead that may not
   be worthwhile at typical resolutions; profile before committing.

---

## Common Pitfalls

### Endianness

FFmpeg's bitstream readers are big-endian by convention for most video codecs (NAL
units, VP8, AV1 OBUs). Audio formats are often little-endian (PCM, AC-3). Always
check the spec's byte-order section before implementing the bit reader and write
explicit unit tests covering multi-byte reads that cross byte boundaries.

### CABAC vs. CAVLC

H.264 supports both entropy-coding modes; CABAC is used in Main and High profiles,
CAVLC in Baseline. The context initialisation tables differ per slice type (I, P, B)
and must be reset correctly at slice boundaries. A common bug is reusing the context
state across slices in multi-slice frames, which produces incorrect decoded output
only on streams with more than one slice per frame.

### Reference Frames and the DPB

Incorrect decoded picture buffer (DPB) management is the most common source of
pixel-corruption bugs in inter-coded video. Key rules:

- Reference frames must be output in display order, not decode order. B-frames
  can cause decode order and display order to diverge by up to 16 frames in
  H.264 High profile.
- DPB overflow handling: when the DPB is full, frames must be bumped in POC order,
  not FIFO order.
- Long-term reference frames in H.264 are managed via `memory_management_control_operation`
  (MMCO) commands in the slice header; these are easy to miss and cause subtle
  reference-corruption artefacts on streams that use scene cuts with IDR frames
  followed by long-term reference assignments.

Always test DPB management with streams that use explicit frame reordering
(non-zero `num_reorder_frames` in VUI parameters) before declaring inter-prediction
correct.
