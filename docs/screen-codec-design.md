# `tpt-kinetix-screen` — Design Draft

> **Status:** Draft with decision points flagged. Nothing is implemented yet.
> Every `DECISION:` block below lists the alternatives and a recommendation.
> Resolve all of them before scaffolding begins.
>
> This doc is the home for the `tpt-kinetix-screen` design-phase checklist in
> `todo.md` (Phase 14). Each checklist item maps to a `DECISION:` block here.

## Goal

A video codec designed for **synthetic screen content** — sharp edges, large
flat regions, and repeated glyph/UI elements — rather than natural imagery.
The bitstream classifies each block into one of:

1. **Flat-fill / run-length** — a solid color or a simple gradient (terminal
   backgrounds, window chrome, empty space).
2. **Glyph / edge palette** — a small indexed palette encoding a repeated UI
   element (text glyphs, icons, button shapes), with cross-frame dictionary
   reuse for repeated elements.
3. **Natural-image fallback** — a transform/entropy-coded block for the
   occasional embedded photo or video region.

The differentiator vs. AV1/HEVC/VVC is that modes 1–2 exploit structure
general codecs throw away, so screen content compresses far better per byte
than a perceptual-quality-tuned codec achieves.

### Why not just use AV1/HEVC?

General-purpose codecs are tuned for natural-image statistics (smooth
gradients, photographic noise, low-frequency-dominant energy). Screen content
is the opposite:

- Huge flat areas where a single color index beats a transform block.
- Hard, high-contrast edges that trigger ringing/blocking artifacts under
  perceptual quantization — and cost many bits to represent as coefficients.
- Repeated glyphs/icons across frames that a natural-image codec re-encodes
  from scratch every time.
- H.264 Screen Content Coding (SCC) extensions and VP9 lossless exist, but
  they are add-ons to natural-image codecs; an from-scratch design centers the
  mode classifier on screen statistics instead of treating them as a special
  case.

---

## DECISION 1: Target content class for v1

The whole mode classifier and the validation corpus depend on "what kind of
screen content does v1 optimize for?"

| Alternative | Tradeoff |
|---|---|
| **A) Desktop capture** | Canonical screen-content scenario: OS windows, text/UI chrome, browsers, presentations, code editors, with cursor and occasional embedded image/video. Richest demand for all three modes; cleanest baseline comparison (H.264 SCC / VP9 lossless benchmarked on desktop screen-content test sets). Broadest and best-understood use case. |
| **B) Mobile UI** | Same synthetic statistics as desktop but at higher DPI, smaller text, rounded affordances, possible notch/status-bar regions. Does **not** need a different bitstream — a resolution/block-size profile covers it. Treating it as a separate v1 design center fragments effort without changing the core design. |
| **C) Mixed screen+video content** | Screen that contains embedded natural-video regions (video call with shared screen, game streaming with overlay, screen-share with a playing movie). The superset case, but it demands the natural-image fallback (mode 3) be genuinely competitive with AV1 — i.e. a v2 maturity concern, not a v1 design center. |

**Recommendation:** **(A) Desktop capture** is the v1 content class. It is the
most common, best-defined screen-content scenario and the one where modes 1–2
deliver the clearest win, so it is the right target to *prove the codec*.

Concretely:

- **v1 design + validation target:** desktop capture (the baseline comparison
  in DECISION 4 is run on desktop screen-content clips — e.g. a SVT-style
  corpus, terminal/editor/browser captures).
- **Mobile UI (B):** deferred to a *profile*, not a new design center. It is
  the same bitstream at higher DPI / smaller text; folded into the validation
  corpus as a higher-resolution variant once the desktop path is solid. No
  separate v1 decision needed.
- **Mixed screen+video (C):** deferred to a *stretch/validation corpus* that
  exercises mode 3 (natural-image fallback). For v1 the fallback only needs to
  be "good enough to not regress" on embedded video regions — it can reuse the
  generic Lean-style transform/entropy path (see DECISION 8) rather than being
  a first-class optimized mode. Promoting C to a design center is a v2 step
  once mode 3 is itself competitive.

### Resolution rule for future content classes

Any content class that is "a higher-DPI / different-resolution variant of the
same synthetic statistics" is a **profile**, not a new design center (covers
mobile UI and AR/glasses overlays). Only content that changes which modes are
first-class (e.g. a screen that is *mostly* natural video) is a candidate for
a new design center — and that is explicitly out of scope for v1.

---

## DECISION 2: Per-block mode classification (flat-fill / glyph-palette / natural fallback)

*Resolved.* How each block is classified, how the mode is signalled, and how
the three mode decoders share one partition/entropy framework.

### The classification problem

Every coding block (CB) is one of three modes (a fourth, IBC copy, is deferred
— see "Open questions"):

| Mode | What it encodes | Cheapest for |
|---|---|---|
| `FLAT` | One color (+ optional simple gradient). | Solid backgrounds, window chrome, empty space. |
| `GLYPH` | A reference into the glyph/palette dictionary + fg/bg colors. | Text, icons, button shapes, any repeated UI element. |
| `NATURAL` | Transform coefficients (Lean-style DCT + rANS). | Embedded photo / video region, complex gradients. |

The classifier's job is to assign each block a mode at encode time and signal
that mode cheaply at decode time. The design must keep the mode map itself
small (a screen is mostly `FLAT` + `GLYPH`, so the map should compress to a
handful of bits per block).

### Alternatives

| Alternative | Partition | Mode signalling | Tradeoff |
|---|---|---|---|
| **A) Fixed base-block grid + per-block mode tag** | Uniform grid (e.g. 16x16 CBs; `NATURAL` subdivides to 8x8 transform blocks like Lean). | Per-CB mode tag, context-coded (MPM single bit + escape, neighbor-mode context). `FLAT` runs coalesced via run-length in the block stream. | Simplest, most predictable decode. No recursive partition search → matches Lean's bounded-cost philosophy. Slightly less RD-optimal on abrupt flat↔detail boundaries than a quadtree, but screen content has large homogeneous regions so the loss is small. **Recommended.** |
| **B) Quadtree partition (variable CB size)** | HEVC-style recursive split to 8x8. | Mode per leaf; split flags entropy-coded. | Best RD for mixed content, but unbounded split search at encode and a variable-depth decode loop — conflicts with the bounded/deadline-predictable goal shared with Lean. Overkill for v1. |
| **C) Frame-level mode map + per-mode payloads** | Any of the above, but the mode map is a separate plane encoded with its own spatial model (context from left/up neighbors), independent of the coefficient streams. | Mode map stream first, then payloads grouped by mode. | Cleanest separation (a `FLAT`-only or `GLYPH`-only decoder can skip coefficient streams), but adds a whole-plane buffering step. Best used as the *signalling strategy* inside (A), not a replacement for it. |

**Recommendation:** **(A) fixed base-block grid**, with the mode map signalled
using the **(C) frame-level spatial-model strategy** (a dedicated rANS sub-stream
for the mode map, neighbor-context coded) and **`FLAT` runs coalesced by
run-length**. `NATURAL` blocks reuse Lean's fixed shallow transform partition
(8x8–64x64) internally. No quadtree for v1.

### Proposed classification / bitstream sketch

```
[Sequence Header]            (DECISION 5/6: arena, grid size, dict cap)
[Frame Header]               (FLAT-run cap, dict version, mode-map model)
[Mode-Map rANS stream]       per-CB mode, neighbor-context (MPM bit + escape)
[FLAT rANS stream]           color (palette-indexed if in dict) + run length
[GLYPH rANS stream]          dict_index + fg/bg color (+ new-glyph bitmap if dict miss)
[NATURAL rANS stream set]    Lean-style partition/mode + transform coefficients
```

- **Mode map:** each CB carries `mode ∈ {FLAT, GLYPH, NATURAL}`. Coded with a
  Most-Probable-Mode single bit (context = modes of left & up CBs) plus a 2-bit
  escape for the non-MPM modes. Run-length is applied *after* classification*:
  a run of consecutive `FLAT` CBs of the same color is encoded as one
  `(color, run_len)` pair in the FLAT stream rather than `run_len` identical
  mode-map entries — so the mode map stays short and the FLAT stream carries the
  coalesced runs.
- **FLAT mode:** payload = color (optionally a palette index when the color is
  already in the dictionary) + optional 1-D/2-D gradient params (two endpoint
  colors) for simple title-bar / progress-bar gradients. Default = solid.
- **GLYPH mode:** payload = `dict_index` into the cross-frame glyph dictionary
  (DECISION 3) + foreground/background color. A `dict miss` emits the new glyph
  bitmap (small, e.g. ≤32x32) into the stream and inserts it into the decoder's
  dictionary. An optional per-glyph sub-position/flip lets one glyph tile a
  repeated row of icons cheaply.
- **NATURAL mode:** identical internal structure to Lean / Vision — fixed
  partition (8x8–64x64), 14 intra modes, DCT-II bank, rANS coefficients. This is
  the generic fallback; for v1 it only needs to be "not worse than Lean" on the
  embedded natural regions.

### How the three modes share one partition/entropy framework

- **Partition layer is shared.** The CB grid, frame/super-block sizing, and
  `max_*` arena allocation follow Lean's conventions exactly; only the per-CB
  *mode payload* differs.
- **Entropy layer is shared.** All four sub-streams use the same `RansStreamSet`
  framing as Lean/Vision. A `FLAT`-only or `GLYPH`-only decoder reads the mode
  map + the relevant payload stream(s) and stops — same dual-path idea as
  Vision's tensor/pixel split.
- **NATURAL mode reuses Lean wholesale.** Its transform, intra prediction, and
  rANS coefficient coding are the Lean implementation (DECISION 6 covers the
  primitive-sharing question). The screen codec adds value only in modes 1–2 and
  in the classifier — the natural fallback is deliberately "borrowed," not
  re-invented.
- **Decoder contract.** `decode_frame()` returns a `VideoFrame`; a
  `decode_mode_map()` helper exposes just the classifier (useful for analysis /
  the efficiency benchmark in DECISION 4 without running full reconstruction).

### Open questions (not a v1 blocker)

1. **IBC copy mode (intra block copy).** Screen content has large *repeated
   regions* (a static toolbar, a copied terminal block) that neither FLAT (not
   one color) nor GLYPH (not one element) covers well. HEVC/H.264 SCC use IBC.
   For v1 we defer IBC and let such regions fall to `NATURAL`; promote to a 4th
   mode in v2 if the benchmark (DECISION 4) shows repeated-region clips regress.
2. **Base grid size.** 16x16 is the default; smaller (8x8) helps dense text but
   grows the mode map. Make it a sequence-header field so it is tunable per
   content class without a format change.
3. **Gradient expressiveness in FLAT.** Solid-only is simplest; 2-endpoint
   linear gradient covers most UI chrome. More complex gradients (radial,
   multi-stop) are out of scope for v1.

## DECISION 3: Cross-frame glyph/palette dictionary reuse

*Resolved.* How repeated UI elements (glyphs, icons, button shapes) and
frequently-used colors are reused across frames instead of re-encoded every
frame. This is the core win of mode 2 over a natural-image codec.

### The reuse problem

A desktop screen changes slowly: the same glyphs, icons, and theme colors
recur in every frame (a terminal shows the same font glyphs; a toolbar shows
the same icons; a theme uses a fixed palette). Without cross-frame reuse,
`GLYPH` mode would still pay the bitmap cost on every frame. The dictionary
stores each distinct element **once**; later frames reference it by slot.

### Dictionary model

Two cross-frame tables, both encoder-authoritative (explicit, deterministic):

| Table | Entry | Cap | Referenced by |
|---|---|---|---|
| **Glyph dictionary** `D` | A small coverage bitmap (mask) of size ≤ `glyph_max_dim` (e.g. 32×32), 1-bit or few-bit. | `dict_cap` (DECISION 5) | `GLYPH` mode payloads |
| **Color palette** `P` | A `Pixel` (RGBA / the sequence's color format). | `palette_cap` (DECISION 5) | `FLAT` color + `GLYPH` fg/bg |

A glyph is stored as a **color-independent mask**: the reference supplies
`fg`/`bg` palette indices at use time, so one glyph entry serves every
color variant (same icon in hover/disabled states). `bg = transparent` means
"leave underlying block content" — enables overlays without a full repaint.

### Alternatives

| Alternative | Tradeoff |
|---|---|
| **A) Single global cross-frame dict + palette, encoder-assigned slots, inline-at-first-use, key-frame reset** | Slot assignment is authoritative → decoder never guesses → fully deterministic and easy to validate. Inline def keeps the bitmap next to its first reference (less buffering). Key-frame reset gives clean seek/recovery. **Recommended.** |
| **B) Per-frame local dict (no cross-frame reuse)** | Simplest, but throws away the entire win — every frame re-pays glyph bitmaps. Reject. |
| **C) Implicit LRU (both sides maintain LRU, no slot signalling)** | Most compact (no slot field, no defs), but any miss-handling divergence desyncs encoder/decoder state — hard to validate and a packet-loss hazard. Reject for v1. |
| **D) Two-tier (global theme palette + per-scene glyph dict reset on scene cut)** | More structure (theme rarely changes; scene glyphs reset on cut), but scene-cut detection is its own problem. Defer to v2; (A) subsumes it via explicit overwrite. |

**Recommendation:** **(A).** Encoder assigns every slot; a `GLYPH` miss emits a
`GlyphDef{slot, bitmap}` and inserts/overwrites at that slot. Inter frames
accumulate; a **key frame** (or a `dict_reset` flag in the frame header) clears
both tables and rebuilds from that frame's defs.

### Bitstream sketch (refines DECISION 2's GLYPH stream)

```
[GLYPH rANS stream]            (per GLYPH coding block)
  hit/miss flag
  if miss:
    GlyphDef { slot: u8; w: u8; h: u8; mask: run-length/packed bits }
    (optionally) PaletteDef { pidx: u8; color: Pixel }   # if fg/bg color is new
  slot: u8
  fg: PaletteRef ; bg: PaletteRef           # PaletteRef = palette index or literal
  [optional] sub_pos / flip flags           # tile a repeated row of icons cheaply

[Palette updates]            # interleaved where first referenced, like glyph defs
  PaletteDef { pidx: u8; color: Pixel }      # overwrite = explicit eviction
```

- **Determinism safeguard:** slot/palette-index assignment is encoder-authoritative
  and overwrites are explicit, so the decoder's tables are always a pure function
  of the bitstream — no LRU heuristics, no divergence path. This is the property
  that makes the codec validatable against a reference decoder.
- **Eviction:** no separate `free` command — overwriting a slot (or a key-frame
  reset) is the only invalidation. Encoder picks the victim (e.g. least-recently
  used by its own accounting) and emits `GlyphDef{slot=victim, …}`. `dict_cap` /
  `palette_cap` bound memory (DECISION 5).
- **Glyph bitmap coding:** variable `w×h` up to `glyph_max_dim`; the mask is
  coded with a short run-length (long runs of 0/1 in a glyph) or a tiny rANS
  model. Keep it simple for v1 — RLE over the packed mask is sufficient and
  trivially correct.
- **Seek / mid-stream join / packet loss:** a decoder that starts on an inter
  frame calls `decode_mode_map()` to find `GLYPH` blocks, but its dict is empty →
  it must wait for the next key frame (or a `dict_reset` frame) before producing
  correct output. The frame header's `dict_version` (monotonic) lets a decoder
  detect "I am behind" and drop to `NATURAL`/hold until resync. (v1: resync on
  next key frame; robust recovery is a v2 concern shared with
  `tpt-kinetix-realtime`.)

### How it shares the framework

- The dictionary is **screen-specific state** owned by the screen decoder, not a
  Lean/Vision primitive — but its entries are referenced through the same
  `RansStreamSet` GLYPH sub-stream from DECISION 2.
- `FLAT` color + `GLYPH` fg/bg both route through the **same** palette table, so
  `palette_cap` covers the whole frame's color working set in one allocation.

### Open questions (not a v1 blocker)

1. **Glyph similarity / delta.** Two font weights or anti-aliased variants of one
   glyph currently cost two slots. A "glyph delta" (store diff vs. an existing
   slot) could help dense text, but adds decoder complexity — defer to v2.
2. **Palette reset granularity.** Key-frame-full-reset is simplest; a partial
   palette (theme palette pinned for the whole sequence, scene palette reset) is
   the (D) two-tier idea — evaluate after the benchmark (DECISION 4) shows palette
   churn behavior on real desktop captures.

## DECISION 4: Compression-efficiency measurement for design validation

*Resolved.* How we prove the codec is actually better than the baselines, and
what metric(s) we freeze the format against.

### The measurement problem

"Better than AV1 on screen content" is meaningless without (a) a fixed baseline,
(b) a fixed quality metric, and (c) a corpus that exercises all three modes. The
metric matters especially: screen content is edge/structure-sensitive, and a
perceptual metric tuned for natural images (VMAF/SSIM) can *reward* smoothing
that destroys a 1-px glyph stroke — exactly the artifact this codec exists to
avoid. So we need a metric that is faithful to small high-contrast structure.

### Alternatives

| Alternative | Baseline | Metric | Tradeoff |
|---|---|---|---|
| **A) Ratio-vs-PSNR against H.264 SCC + VP9 lossless, on a desktop screen corpus** | H.264 SCC extensions (`-x264encopts`/`x264` with SCC, or `ffmpeg` + `libx264` SCC build) and `libvpx-vp9` `-lossless 1`. | PSNR (dB) vs bits/frame; plot RD curve. | PSNR is cheap, reproducible, and monotonic — but it over-penalizes the *structure-preserving* differences this codec makes and is blind to readability. Good primary, insufficient alone. **Use as the backbone, pair with (C).** |
| **B) Ratio-vs-VMAF against AV1 (libaom) + H.264 SCC** | `libaom-av1` (constrained quality) + the SCC baselines. | VMAF vs bits/frame. | VMAF correlates with human *viewing* quality but, per above, can prefer smoothed glyphs — it would understate this codec's advantage on text. Useful as a secondary "don't regress on human viewing" check, not the primary. |
| **C) Screen-content structure-preservation metric (glyph/edge fidelity)** | N/A (codec-vs-self + codec-vs-baseline on the same clips). | E.g. edge-map F1 / SSIM on a Canny edge map, or OCR character-error-rate (CER) on text clips, or a small-structure PSNR (mask to high-gradient regions). | Directly measures the thing this codec protects. CER needs an OCR engine; edge-F1 is cheap. **Pair with (A) as the differentiator metric.** |

**Recommendation:** **Primary = (A) PSNR-vs-bits RD curve vs H.264 SCC + VP9
lossless**; **differentiator = (C) edge-preservation metric** (cheap Canny
edge-map F1, with OCR CER as an optional text-clip add-on); **guard = (B) VMAF**
as a secondary "no human-viewing regression" check. Three metrics, one RD
harness.

### Corpus (DECISION 1 target = desktop capture)

A fixed, version-pinned corpus covering all three modes:

| Clip class | Dominant mode | Example |
|---|---|---|
| Solid/UI chrome | `FLAT` | Empty desktop, file manager, settings panel |
| Text-heavy | `GLYPH` | Terminal session, code editor, browser text page |
| Mixed UI + image | `GLYPH` + `NATURAL` | Browser with photos, slide deck with screenshot |
| Embedded video region | `NATURAL` (fallback) | Video call with shared screen, movie in a window |

Synthetic clips are preferred for reproducibility (no licensing), generated from
real UI themes/fonts; 1–2 real captures included as sanity checks. Mobile-UI and
mixed-screen+video clips enter here as **stretch/validation** corpora (DECISION 1),
not as the v1 design target.

### Harness shape

A `tpt-kinetix-test-utils` integration test, gated behind a `screen-bench`
feature (it needs `ffmpeg`/`x264` SCC + `libvpx`/`aom` on the runner — same
ffmpeg-gating pattern as the existing H.264/AV1 conformance harness in
`tpt-kinetix-test-utils::reference`):

1. Encode each corpus clip at 5+ quality levels with `tpt-kinetix-screen`.
2. Encode the same clips at matched points with H.264 SCC, VP9 lossless, AV1.
3. Decode all; compute PSNR, edge-F1, VMAF per clip.
4. Emit an RD curve (bits vs metric) saved as a test artifact + a summary table.
5. **Gate:** `tpt-kinetix-screen` must beat H.264 SCC and VP9 lossless on
   **bits-at-equal-edge-F1** for the text-heavy and UI-chrome clips (the modes
   it is designed for). It is *allowed* to tie-or-trail on the embedded-video
   clip (that is the `NATURAL` fallback, not its design center).

### Open questions (not a v1 blocker)

1. **Which SCC baseline is available in CI?** A full H.264 SCC build is not in
   every `ffmpeg`; if unavailable, VP9 lossless + AV1 are the enforceable
   baselines and SCC is reported best-effort. Confirm runner toolchain before
   freezing the gate.
2. **Edge-F1 threshold.** The exact "beats baseline" edge-F1 delta needs tuning
   once real numbers exist; start as "no worse than baseline at equal bits" and
   tighten after the first benchmark run.

## DECISION 5: Memory/perf budget for v1

*Resolved.* The allocation + timing envelope the v1 format must stay inside.
Values are conservative starting points, frozen by the benchmark (DECISION 4)
and tunable via sequence-header fields — they are design targets, not hard
spec floors, but the decoder MUST allocate within them.

### Constraints

| Constraint | v1 value | Rationale |
|---|---|---|
| Target resolution | 1920×1080 (and down) | Matches Lean/Vision v1; covers the DECISION 1 desktop-capture target. 4K noted as a v2 profile. |
| Base coding-block grid | 16×16 (sequence-header field) | From DECISION 2; 1080p → 120×68 CBs. |
| Max frame rate | 60 fps decode | Desktop capture / screen-share envelope. |
| Glyph dict cap `dict_cap` | 256 entries | A desktop screen has far fewer than 256 distinct glyphs/icons on screen at once; ample headroom. Bounds dict arena. |
| Glyph max dims `glyph_max_dim` | 32×32 | Covers icon + largest font glyph; larger UI elements fall to `NATURAL`. Bounds per-entry bitmap. |
| Palette cap `palette_cap` | 64 entries | A theme's working color set is small (tens of colors). Bounds palette arena. |
| Max reference frames | 1 (previous) | Screen content is near-I-frame (most blocks `FLAT`/`GLYPH`); no long-term reference chain needed. Simplest DPB. |
| Decode arena ceiling | ~12 MB at 1080p | See breakdown below. Deliberately under Lean's 20 MB because screen decode skips the heavy transform/deblock on `FLAT`/`GLYPH` blocks. |
| Target decode time | < 8 ms/frame at 1080p, RPi 5 class | Matches Lean's embedded envelope; `FLAT`/`GLYPH` decode is cheaper than Lean's full transform path, so headroom exists. |
| `no_std` / MCU | Future work, not v1 | Same as Lean — prove the alloc-bounded hot path on embedded Linux first. |

### Arena breakdown (1080p, ~12 MB ceiling)

```
CB mode map (120*68 * ~2 bits)        ~ 2 KB     (negligible — context-coded)
FLAT stream workspace                   ~ 1 MB    (run buffer + color cache)
GLYPH dict (256 * 32*32 * 1 bit mask)  ~ 32 KB   (masks only; color via palette)
Palette (64 * RGBA)                    < 1 KB
NATURAL ref frame (1 * 1080p luma)     ~ 2 MB    (YUV 4:2:0 ≈ 3 MB; luma-only cheaper)
NATURAL coefficient/workspace buffer    ~ 4 MB    (peak during transform decode)
Per-frame output buffer (1080p YUV)     ~ 3 MB
-----------------------------------------------
total working set                       ~ 12 MB
```

The dominant cost is the `NATURAL` fallback's reference + workspace; the
screen-specific structures (mode map, glyph dict, palette) are tiny. This is the
intended shape: spend the budget on the generic path, keep the specialist state
small.

### Relationship to the benchmark gate

DECISION 4's `screen-bench` harness reports bits/frame at the corpus's native
1080p (and a downscaled 720p run for the embedded envelope). If a design change
blows the ~12 MB arena or the 8 ms budget, the harness flags it before the
format is frozen. `dict_cap` / `palette_cap` are the knobs that bound the
specialist state if a real desktop capture proves larger than estimated.

### Open questions (not a v1 blocker)

1. **4K profile.** Doubles the ref/workspace buffers (~24 MB). Defer to v2; the
   sequence-header `max_*` fields already allow it without a format change.
2. **Multi-reference for `NATURAL`.** If embedded-video regions turn out to
   benefit from 2 refs (DECISION 1 stretch corpus), raise max ref frames — costs
   ~3 MB per extra ref. Evaluate after the benchmark.

## DECISION 6: Relationship to existing kinetix bitstream primitives

*Resolved.* Whether to reuse or re-implement the low-level bitstream machinery,
and where the `NATURAL` fallback comes from.

### Current shared state

`tpt-kinetix-bitstream` already exists (extracted per `tpt-kinetix-realtime`
DECISION 7) and is the single source of truth for the entropy + bit-level
primitives. It currently exports:

- `BitReader` — bit-level reader (byte-aligned headers, payload bitstreams).
- `RansEncoder` / `RansDecoder` / `RansStreamSet` — byte-renormalizing rANS,
  split into independently-decodable sub-streams (exactly what DECISION 2's
  four-sub-stream framing needs).
- `SymbolModel` / `StaticModel` / `SymbolInfo` — probability-model extension
  point for context-coded symbols (the mode-map MPM coding, the FLAT/glyph
  payloads).

`lean`, `vision`, and `realtime` all depend on it. The transform / partition /
intra / deblock path is still in `lean` but is *slated to be promoted into
`tpt-kinetix-bitstream`* by the same realtime DECISION 7 (it notes "lean +
vision + realtime are 3 copies of `BitReader`/rANS/partition/transform/deblock").

### Alternatives

| Alternative | Tradeoff |
|---|---|
| **A) Screen depends on `tpt-kinetix-bitstream` for entropy+bitreader; `NATURAL` mode reuses the transform/partition/intra/deblock path from `tpt-kinetix-bitstream` (after promotion) — screen implements only the screen-specific modes** | No duplication of entropy/transform; screen is a thin layer over shared primitives + its own classifier/dict. Cleanest, matches the workspace direction. **Recommended.** |
| **B) Screen depends on `tpt-kinetix-lean` directly for the whole `NATURAL` path** | Lean is the generic natural-image codec, so a codec→codec dependency is "acceptable" (lean is the generic base), but it couples screen to lean's header/sequence layout and pushes the codec→codec dependency the vision DECISION 8 note called "odd." Acceptable as an *interim* until the bitstream promotion lands. |
| **C) Screen copies `bitreader.rs` + `rans.rs` (start independent)** | The vision DECISION 8 "start independent, extract later" stance — but `tpt-kinetix-bitstream` already exists, so copying would reintroduce the exact duplication the extraction removed. Reject. |
| **D) Screen diverges the entropy coder (custom model for FLAT/glyph)** | The flat-fill/glyph modes are highly structured; a bespoke coder *could* beat rANS. But it adds a whole entropy implementation + fuzz surface for marginal gain; the screen-specific win is in the *classifier + dictionary*, not the entropy backend. Reject for v1 — use `RansStreamSet` + `SymbolModel`, revisit only if the benchmark (DECISION 4) shows entropy is the bottleneck. |

**Recommendation:** **(A).** `tpt-kinetix-screen` depends on
`tpt-kinetix-bitstream` for `BitReader` + the `Rans*` family (zero new entropy
code). The screen-specific machinery — mode classifier, glyph dictionary,
palette, FLAT run-length, glyph-bitmap RLE, and the four-sub-stream framing —
is implemented **only in screen**. The `NATURAL` fallback consumes the
transform/partition/intra/deblock path from `tpt-kinetix-bitstream` once the
realtime DECISION 7 promotion lands; as an interim, screen delegates its
`NATURAL` blocks to `lean`'s public transform API (option B) and swaps to
`bitstream` when the promotion completes. Screen must **not** copy or fork the
entropy/transform primitives.

### What screen owns vs. what it borrows

| Primitive | Owner | Screen's use |
|---|---|---|
| `BitReader` | `tpt-kinetix-bitstream` | All headers + payload bitstreams |
| `RansEncoder`/`RansDecoder`/`RansStreamSet` | `tpt-kinetix-bitstream` | The 4 sub-streams (mode map, FLAT, GLYPH, NATURAL) |
| `SymbolModel` | `tpt-kinetix-bitstream` | Mode-map MPM coding; FLAT/glyph payload models |
| Transform / partition / intra / deblock | `tpt-kinetix-bitstream` (promoted) / interim: `lean` | `NATURAL` fallback only |
| **Mode classifier + run-length** | `tpt-kinetix-screen` | DECISION 2 — screen-specific |
| **Glyph dictionary + palette** | `tpt-kinetix-screen` | DECISION 3 — screen-specific |
| **GLYPH bitmap RLE** | `tpt-kinetix-screen` | DECISION 3 — screen-specific |

### Open questions (not a v1 blocker)

1. **Promotion timeline.** Screen's `NATURAL` interim (delegating to `lean`)
   should be a thin shim so the swap to `tpt-kinetix-bitstream` is a one-file
   change. Confirm the realtime DECISION 7 promotion is still the plan before
   scaffolding, so screen isn't left maintaining a `lean` dependency it should
   drop.
2. **`SymbolModel` for the mode map.** The MPM single-bit + escape needs a small
   adaptive context model; verify `bitstream`'s `SymbolModel` trait fits (it
   should — lean/vision/realtime already use it) before inventing a screen-local
   one.

---

## Implementation order (post-design resolution)

1. Resolve DECISIONs 1–6 (this doc).
2. Scaffold `tpt-kinetix-screen` crate from `templates/codec-crate/`.
3. Sequence/frame header parsing (byte-aligned, `max_*` arena sizing like Lean
   / Vision).
4. Flat-fill / run-length mode (mode 1) encode + decode — the simplest mode,
   proves the classifier framing.
5. Glyph / edge palette mode (mode 2) + cross-frame dictionary (DECISION 3).
6. Natural-image fallback (mode 3), reusing Lean-style transform/entropy.
7. Build the efficiency benchmark harness (DECISION 4) on a desktop
   screen-content corpus.
8. Tune the mode classifier / palette dictionaries against the baseline.
