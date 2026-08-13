# `tpt-kinetix-realtime` — Design Draft

> **Status:** Draft with decision points flagged. Nothing is implemented yet.
> Every `DECISION:` block lists the alternatives and a recommendation.
> Resolve all of them before scaffolding begins.
>
> **Profile decision (resolved, 2026-08-13):** `tpt-kinetix-realtime` v1 is
> **profile-agnostic**. The bitstream is designed around the shared realtime
> core — low-latency GOP (no B-frame lookahead) + partial-frame loss
> recovery — and cloud gaming, video conferencing, and AR/smart-glasses
> overlay are expressed as **decode profiles / config knobs**, not separate
> codecs. AR is the *hardest* profile (the stress test) and leans on
> `tpt-kinetix-lean`'s power-conscious primitives rather than being invented
> independently here. See `docs/codec-backlog.md` and the `todo.md` Phase 14
> entry for the rationale.

## Goal

A video codec whose design center is **sub-frame / sub-GOP latency** and
**graceful degradation under packet loss**, rather than maximum compression
ratio. The two properties that general-purpose codecs (AV1/HEVC/VVC) treat as
a *configuration* are this codec's *design center*:

1. **No B-frame lookahead.** Every frame is decodable the instant its
   reference is available. B-frames force the decoder to wait for future
   frames, which is fatal for cloud gaming / conferencing / AR where a frame
   must be on-screen within one RTT + one decode.
2. **Partial-frame loss recovery.** A dropped packet must not stall the whole
   frame or propagate forever. The bitstream is partitioned so loss is
   spatially bounded and self-healing.

### Why not just use AV1/HEVC with low-latency settings?

- AV1's low-delay mode still assumes a long encode search and a single
  dependency per frame; its "low overhead" framing (`-low_latency` /
  `low_overhead` OBU) reduces header cost but does nothing for intra-refresh
  cadence or per-region FEC.
- B-frame removal is a *tuning* in those codecs, not a *guarantee* of bounded
  decode work — their partition search and in-loop filter stack (CDEF + loop
  restoration) have unbounded-ish decode cost that a hard latency deadline
  can't easily cap.
- None of them expose foveated / gaze-contingent resolution as a first-class
  bitstream concept, which is the AR profile's core constraint.

---

## DECISION 1: Partial-frame loss-recovery mechanism

This is the defining feature. Three building blocks, any subset of which can
be active:

| Mechanism | What it does | Cost |
|---|---|---|
| **FEC (forward error correction)** | Add redundant repair packets (e.g. Reed-Solomon / RaptorQ fountain codes) over a group of source packets. Lost packets are reconstructed at the receiver without a round-trip. | Pure bandwidth overhead (e.g. 10-30%). No extra decode latency if the FEC group is one frame or smaller. |
| **Intra-block refresh (rolling intra)** | Instead of periodic full-IDR keyframes, a rotating subset of blocks per frame is coded intra. After N frames every block has been refreshed, bounding error propagation to <N frames without a keyframe bit spike. | Small steady-state bitrate tax (more intra blocks); no latency spike. |
| **Decoder concealment** | When a block/packet is genuinely unrecoverable, the decoder hides the gap via spatial (neighboring block copy / edge extension) or temporal (previous frame motion copy) interpolation. | No bandwidth cost; quality loss only on the concealed region. |

| Alternative | Tradeoff |
|---|---|
| **A) FEC-only** | Cleanest recovery (lost packets fully reconstructed, no quality loss), but fixed bandwidth tax even when the network is perfect. FEC group size trades recovery burst-size vs. overhead. |
| **B) Concealment-only** | Zero steady-state overhead; relies on intra-refresh to bound propagation and on the decoder to hide the rest. Quality dips on loss but never stalls. Simplest to implement. |
| **C) Hybrid: FEC + intra-refresh + concealment** | FEC covers the common small-loss case with no quality loss; intra-refresh bounds multi-loss propagation; concealment is the last-resort safety net so the decoder never has to drop a whole frame. Most robust, most knobs. |
| **D) FEC + concealment, no intra-refresh** | Avoids the intra-refresh bitrate tax but then a full-IDR is needed periodically for error reset, reintroducing a latency/bitrate spike that conflicts with the low-latency goal. |

**Recommendation:** **(C) hybrid**, with the *strength* of each layer
profile-parameterized (see DECISION 6). Rationale: the whole point of the
codec is graceful degradation, and a single mechanism can't cover both
"occasional single-packet loss" (FEC wins) and "network outage for several
frames" (intra-refresh + concealment wins) without the other. The latency
goal rules out (D)'s periodic full-IDR.

Concretely for v1:
- **FEC group = one frame** (or one slice-group), so recovery never waits
  for the next frame. RaptorQ (systematic fountain code) so the source
  packets are sent verbatim and repair packets are pure overhead — a receiver
  that loses nothing pays zero FEC decode cost.
- **Intra-refresh period** default ~ `frame_height / intra_block_rows_per_frame`
  so a full refresh completes in roughly 0.5-1 s at 30-60 fps, well within a
  conferencing RTT budget.
- **Concealment** is always-on as the terminal fallback (decoder must never
  stall on a missing packet).

---

## DECISION 2: GOP structure under the no-B-frame constraint

With B-frames forbidden, the only degrees of freedom are keyframe placement
and intra-refresh cadence.

| Alternative | Tradeoff |
|---|---|
| **A) Periodic full-IDR (IPPPI… with IDR every K frames)** | Simplest. But the IDR is a large bit spike that, on a bandwidth-constrained link, *itself* increases latency (the IDR takes longer to transmit + the receiver buffers it). Conflicts with the low-latency goal at small K. |
| **B) Rolling intra-refresh, no periodic full-IDR** | No bitrate/latency spike ever. Error propagation bounded to the refresh period. But a brand-new decoder joining a session (or a post-outage reset) has no clean entry point until a full refresh cycle completes — solved by an on-demand forced IDR. |
| **C) Rolling intra-refresh + on-demand forced IDR** | Default to (B); emit a full IDR only when explicitly requested (new joiner, major scene change, decoder resync request). Best of both. |

**Recommendation:** **(C).** The bitstream carries a per-frame
`refresh_mask` (which block rows are intra this frame) so the decoder always
knows the propagation state, plus an `idr_requested`/`force_idr` flag in the
control plane. This is the same no-B-frame inter design `tpt-kinetix-lean`
already uses (unidirectional P, no weighted pred), so realtime inherits
lean's inter-prediction shape and adds the refresh masking on top.

GOP shape: `IDR, P, P, …, P(intra-refresh cycling), …, (on-demand IDR)`.
Max reference frames = 1 for v1 (single backward reference) to keep the DPB
tiny and the decode deadline trivially bounded; this also matches lean's
unidirectional-P simplicity.

---

## DECISION 3: Sub-frame partition / slice structure

To make loss *spatially* bounded and to let the decoder start work before the
whole frame arrives, a frame is partitioned into **independently
packetizable slices**.

| Alternative | Tradeoff |
|---|---|
| **A) One slice per frame, FEC over the frame** | Simplest packetization. But one lost packet corrupts the whole frame's region unless concealment covers it; no sub-frame parallelism. |
| **B) Fixed grid of slices (e.g. 4x4 or 8x8 slice grid)** | Each slice is an independent entropy + (optionally) reference island. A lost packet affects only its slice; FEC can be per-slice-group; decode can start per-slice as packets arrive. More header overhead. |
| **C) Tile + slice (AV1-style)** | Maximum flexibility but maximum complexity; overkill for v1. |

**Recommendation:** **(B) fixed slice grid**, with each slice
self-contained (own rANS stream, own reference clamp to slice boundary or to
the single previous frame). Slice count is profile-tunable: cloud gaming can
afford a coarse grid (fewer slices, more parallelism headroom at encode), AR
wants a fine grid (smaller loss blast radius, finer foveation). Slice grid
size is declared in the sequence header so the decoder allocates once.

This also delivers the "sub-frame latency" property: the decoder can emit
rendered slices top-to-bottom as they arrive and decode, rather than waiting
for the full frame.

---

## DECISION 4: Per-frame latency budget and how it's enforced

Latency here = encode time + one network RTT + decode time + display. The
codec can only bound the encode and decode halves; the network RTT is the
environment. The design enforces the codec-controllable halves:

| Mechanism | Side | How |
|---|---|---|
| **Encode deadline** | encoder | Rate control caps the search/work per frame to a `deadline_ms` budget. If the budget is exceeded, the encoder falls back to a faster mode (skip partition search, raise QP) rather than overrunning. Profiles set the default deadline. |
| **Decode bounded work** | decoder | No B-frames (DECISION 2), single reference (DECISION 2), fixed slice grid (DECISION 3), and a capped in-loop filter stack mean per-frame decode work is `O(pixels)` with a known constant. The decoder exposes a `max_decode_ms` capability so callers can reject streams that would miss their deadline. |
| **Slice-level pipelining** | both | Decoder starts emitting slices as they arrive (DECISION 3), so "time to first rendered pixel" < full-frame decode time. |

| Alternative for the decode work cap | Tradeoff |
|---|---|
| **A) Fixed in-loop filter set (single deblock, no CDEF/LR)** | Matches lean; bounded, predictable decode. Slightly lower quality than AV1's stack but the latency guarantee is worth it for realtime. |
| **B) Adaptive filter stack** | Better quality but variable decode cost violates the deadline contract. |

**Recommendation:** **(A) fixed, lean-style in-loop filter** (single
deblock, no CDEF/loop-restoration for v1). The latency contract is a
first-class feature; variable-cost filtering undermines it.

The `deadline_ms` and the resulting QP are carried in the frame header so the
decoder knows the encoder stayed within budget (and can detect a misbehaving
encoder).

---

## DECISION 5: How loss resilience is measured for design validation

General codecs validate with PSNR/SSIM at a bitrate. Realtime needs the
**packet-loss × quality** curve — the realtime analogue of vision's
mAP-vs-bitrate (Phase 15, DECISION 6).

| Alternative | Tradeoff |
|---|---|
| **A) PSNR/SSIM vs. packet-loss-rate curve** | Standard, reproducible. Encode a test clip, inject packet loss at 0/1/5/10/20/30%, decode with the realtime decoder, plot quality vs. loss. Covers the FEC + concealment recovery. |
| **B) Gaze-weighted quality vs. loss (AR-specific)** | Weights PSNR by a simulated gaze map so foveated loss matters more than peripheral loss. More realistic for AR but needs a gaze model. |
| **C) Freeze/drop-rate vs. loss** | Counts how often a frame misses its display deadline (stall) rather than its pixel quality. Captures the latency half of the goal, which PSNR misses entirely. |

**Recommendation:** **(A) + (C) for v1, (B) added for the AR profile.**
The validation harness must report *both* a quality curve (A) and a
stall/freeze rate (C), because a codec that keeps pixel quality but misses
deadlines has failed the realtime goal. The harness lives in
`tpt-kinetix-test-utils` behind a `realtime-bench` feature (it needs a loss
injector + the realtime decoder; no model weights, unlike vision).

Harness shape:
1. Encode reference clip at 3+ bitrate/FEC-overhead points.
2. For each loss rate in {0,1,5,10,20,30}% (with a reproducible loss RNG seed), drop packets, decode.
3. Emit `PSNR-vs-loss` and `stall-rate-vs-loss` curves as test artifacts.

---

## DECISION 6: Memory / perf / latency budget for v1 (profile-parameterized)

The profile-agnostic decision means one budget *envelope* with three preset
parameter sets, not three separate budgets.

| Constraint | Cloud gaming (default) | Video conferencing | AR / smart-glasses |
|---|---|---|---|
| Target client | GPU-backed console/PC | phone / laptop (SW decode) | ultra-low-power wearable |
| Max resolution | 3840x2160 | 1920x1080 | 1280x720 (foveated: full-res only at gaze) |
| Slice grid | coarse (e.g. 4x4) | medium (8x8) | fine (16x16) — small blast radius |
| Intra-refresh period | ~1 s | ~0.5 s | ~0.5 s |
| FEC overhead | 10% | 20% | 20-30% (harshest link) |
| `deadline_ms` (encode) | 8 ms (120 fps headroom) | 16 ms (60 fps) | 16 ms (but tiny frame budget) |
| Max DPB | 1 ref frame | 1 ref frame | 1 ref frame |
| Foveation | off | off | **on** (gaze-contingent resolution falloff) |
| Decode arena ceiling | ~50 MB (4K luma+ref) | ~20 MB (1080p) | <10 MB (power/thermal) |

**Foveation (AR only):** the bitstream supports a `gaze_map` (low-res center
of interest) so peripheral slices are coded at reduced resolution / higher QP.
This is the one genuinely AR-specific bitstream feature; everything else is
shared. The decoder without a gaze feed decodes the full-res fallback.

| Alternative for the v1 hardware envelope | Tradeoff |
|---|---|
| **A) Single envelope, three presets (recommended)** | One codec, one test surface, profiles are just header field sets. Matches the profile-agnostic decision. |
| **B) Hardcode one profile, ignore the others for v1** | Less work but reneges on the profile-agnostic decision and forces a v2 rewrite for the other two. |

**Recommendation:** **(A).** All three profiles are header-parameterized
from day one so the core never has to be forked.

---

## DECISION 7: Relationship to `tpt-kinetix-lean`

Realtime and lean now share *three* overlapping consumers of the same
primitives (lean, vision, realtime). Realtime specifically reuses:

- No-B-frame unidirectional-P inter prediction (DECISION 2) — identical to lean.
- Fixed shallow partition + DCT transform bank — identical to lean/vision.
- Single-stage deblock in-loop filter (DECISION 4) — identical to lean.
- `BitReader` + rANS (`RansStreamSet`) — identical to lean/vision.

| Alternative | Tradeoff |
|---|---|
| **A) Shared `tpt-kinetix-bitstream` crate (extract now)** | With three consumers, the duplication argument is now stronger than when vision was designed. Extract `bitreader.rs` + `rans.rs` + partition/transform into a shared crate; all three depend on it. One source of truth. Requires refactoring lean + vision. |
| **B) Copy primitives into realtime (start independent)** | Same as vision's original DECISION 8 — no refactoring, two/three copies to maintain. |
| **C) Realtime depends on lean directly** | Realtime re-exports lean's primitives. Odd dependency direction (a realtime codec depending on an embedded codec), but avoids extraction. |

**Recommendation:** **(A) extract `tpt-kinetix-bitstream` now**, because
the duplication is no longer hypothetical (lean + vision already copy it, and
realtime would be the third copy). The interface is stable enough across the
three to freeze: `BitReader`, `RansEncoder`/`RansDecoder`/`RansStreamSet`,
`SymbolModel` trait, fixed partition/transform bank, single deblock. Realtime
adds *on top*: slice-grid framing, intra-refresh masking, FEC packet framing,
optional foveation, and the latency-deadline header fields. These are
realtime-specific and stay in `tpt-kinetix-realtime`.

---

## Bitstream structure (sketch)

```
[Sequence Header]            // magic "RTIM", version, max dims, slice grid,
                             // profile preset id, foveation flag, FEC params
[Frame Header]               // frame_type (IDR/P), refresh_mask, deadline_ms,
                             // base_qp, gaze_map ref (AR), payload_len
[Slice Group 0 .. N]         // each: independent rANS stream (partition/mode
                             // + coefficients), self-contained
[FEC repair packets]         // RaptorQ repair for the frame's slice group
[Control plane]              // force_idr / decoder-resync requests (out of band)
```

The rANS stream framing inside each slice reuses `RansStreamSet` from
`tpt-kinetix-bitstream`. A decoder can parse + emit each slice as it arrives
(DECISION 3 sub-frame latency) and apply concealment to any slice it can't
recover via FEC (DECISION 1).

---

## Open research questions (not resolved in this design)

1. **RaptorQ vs. Reed-Solomon for the FEC layer.** RaptorQ (fountain) has
   near-optimal overhead and no fixed block size, but the decode is heavier;
   RS is simpler and well-understood but needs fixed group sizes. The hybrid
   design (DECISION 1) works with either — pick during implementation.
2. **Optimal intra-refresh period vs. FEC overhead tradeoff.** More FEC means
   fewer forced intra blocks needed for propagation bounds, and vice versa.
   The validation harness (DECISION 5) should sweep this.
3. **Foveation gaze-map representation.** A low-res center-of-interest raster
   vs. a parametric (center + falloff) model. AR-profile-only; defer to AR
   profile work.
4. **Single-reference vs. two-reference.** One reference keeps the DPB and
   decode deadline trivially bounded (DECISION 2/4). Two references (past +
   a long-term) could improve quality for static-background conferencing but
   complicates the latency contract. Stay at one for v1.

---

## Implementation order (post-design resolution)

1. Extract `tpt-kinetix-bitstream` from lean (DECISION 7) — `BitReader`,
   rANS, partition/transform, deblock.
2. Scaffold `tpt-kinetix-realtime` crate from `templates/codec-crate/`.
3. Implement sequence/frame header parsing (incl. profile preset + slice grid).
4. Port lean's intra + unidirectional-P reconstruction (DECISION 2).
5. Add slice-grid framing + per-slice independent rANS (DECISION 3).
6. Add intra-refresh masking (`refresh_mask`) + on-demand IDR (DECISION 2).
7. Add FEC packet framing (RaptorQ or RS) + decoder concealment (DECISION 1).
8. Add `deadline_ms` rate-control hook + `max_decode_ms` capability (DECISION 4).
9. Build the packet-loss-vs-quality + stall-rate harness (DECISION 5).
10. (AR profile) Add foveation / gaze-map support (DECISION 6).
