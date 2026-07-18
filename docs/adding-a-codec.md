# Adding a New Codec via the `tpt-kinetix-kg` Pipeline

This document describes the repeatable process for integrating a new audio or video codec
into TPT Kinetix using the `tpt-kinetix-kg` knowledge-graph tooling. Following this process
consistently keeps each codec crate coherent, fuzz-hardened, and parallelism-aware from
the start.

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
libavcodec/aac.c            # AAC audio decoder
libavcodec/hevcdec.c        # HEVC/H.265 video decoder
```

Some codecs span multiple files (e.g. HEVC uses `hevcdec.c`, `hevc_cabac.c`,
`hevc_filter.c`, etc.). Ingest each file separately and merge the graphs later, or
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
extracted. It is the input to all subsequent pipeline steps. Keep `codec.json` under
version control in `tpt-kinetix-kg/graphs/` so graph evolution can be tracked alongside
code changes.

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

- `src/generated/mod.rs` — top-level module re-exporting the decoder struct
- `src/generated/types.rs` — enums and structs mirroring the C types (macroblock
  types, prediction modes, coefficient arrays)
- `src/generated/parser.rs` — stub functions for every C function identified as a
  parsing step, with `todo!()` bodies and inline comments referencing the source line
- `src/generated/dispatch.rs` — `match` expressions scaffolded from the SwitchCase nodes
- `src/generated/parallel.rs` — `rayon::par_iter` injection points identified in Step 5

Commit this generated output before making hand edits. That way `git diff` clearly
separates machine-generated code from hand-written additions.

---

## Step 7 — Hand-Complete the Scaffold

The scaffold compiles but panics on every `todo!()`. Hand-completion is the largest
effort in the process. Work through these sub-steps in order:

1. **Parser tables**: fill in VLC/Huffman tables, quantiser matrices, and scan orders.
   Cross-reference the codec spec directly — do not copy-paste from FFmpeg to avoid
   licence contamination; implement from the spec.

2. **Entropy decoding**: implement CAVLC, CABAC, or Huffman decoding as appropriate.
   H.264 and HEVC use CABAC; H.264 also has CAVLC; VP8/VP9/AV1 use boolean arithmetic
   coding. Entropy decoding is almost never parallelisable and must be done serially
   before the parallel reconstruction stages.

3. **Inverse transform**: implement the codec's integer DCT or MDCT. Verify
   numerically against reference output before wiring into the full pipeline.

4. **Prediction modes**: intra and inter prediction. Inter prediction requires
   correctly implementing the decoded picture buffer (DPB) and reference frame
   management.

5. **Loop filters**: deblocking and any codec-specific post-filters (SAO in HEVC,
   CDEF in AV1). These often have subtle ordering constraints.

---

## Step 8 — Write a Pixel-Diff Validation Harness

Before declaring correctness, compare decoded output frame-by-frame against FFmpeg:

```bash
# Decode a test clip with FFmpeg to raw YUV
ffmpeg -i test_clip.mp4 -f rawvideo -pix_fmt yuv420p reference.yuv

# Decode the same clip with kinetix and write raw YUV
cargo run -p tpt-kinetix-cli -- decode --input test_clip.mp4 --output candidate.yuv

# Diff the two files
cmp reference.yuv candidate.yuv
```

For automated testing, integrate the comparison into `tpt-kinetix-test-utils`:

```rust
use tpt_kinetix_test_utils::pixel_diff::assert_frames_match;

assert_frames_match("tests/fixtures/test_clip.mp4", tolerance_psnr_db: 60.0);
```

A PSNR tolerance of ≥ 60 dB indicates pixel-exact decode. Values below 40 dB suggest
a logic error in prediction or transform. Test across multiple profiles and
resolutions; H.264 baseline and high profiles exercise different code paths.

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

Run the fuzzer for at least 24 hours before the first release, and add all
crash-inducing inputs as regression fixtures in `fuzz/corpus/`.

---

## Step 10 — Write Conformance Tests

Add conformance tests using `tpt-kinetix-test-utils` that cover:

- ITU-T or IETF conformance test vectors (if available for the codec)
- Boundary conditions: zero-length frames, maximum resolution, all-intra streams
- Profile/level combinations relevant to the target use case

```rust
#[cfg(test)]
mod conformance {
    use tpt_kinetix_test_utils::conformance::run_vector;

    #[test]
    fn itu_t_h264_bp_cavlc_420_baseline() {
        run_vector("tests/vectors/CAVLC_A_Sony_E.264");
    }
}
```

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
| **Simple** | VP8, MPEG-1 audio, PCM | 0.5 | 2–3 | 3–4 weeks |
| **Medium** | VP9, AAC, MPEG-2 video | 1 | 4–6 | 5–7 weeks |
| **Complex** | H.264, AV1, MPEG-4 ASP | 1–2 | 8–12 | 10–14 weeks |
| **Very complex** | HEVC/H.265, VVC/H.266 | 2–3 | 14–20 | 16–24 weeks |

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
