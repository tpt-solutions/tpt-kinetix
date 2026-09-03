# Codec Backlog

The full ~400-codec FFmpeg surface is explicitly out of scope for the current phases.

**Direction (2026-09-03):** new multi-month codec work targets **royalty-free**
formats only. H.264 decode is already done and stays (ingest coverage for existing
content, not an endorsement of new AVC work). **HEVC/H.265 is dropped** — patent-pool
fragmentation stalled its adoption and a pure-Rust decoder is a 12–16 week effort;
any 4K HEVC ingest need should use platform hardware decode via FFI, not a native
decoder.

**Precondition:** close the H.264 High-profile 8×8 transform and get AV1 decode to
`pixel_exact` (or explicitly park it) before starting the next codec.

Tracked candidates in priority order:

| Codec | Priority | Notes |
|-------|----------|-------|
| VP9 (decode) | Next | Royalty-free; structurally a simpler AV1 (superblocks/tiles/transforms) — de-risks the AV1 reconstruction pipeline. Shares KG tooling. |
| Opus (decode) | After VP9 | Royalty-free audio; ubiquitous (WebRTC/streaming). Native impl to keep the no-third-party-codec stance (do **not** wrap `opus`). Companion to a real MKV/WebM path. |
| MP3 (decode) | Filler | Patents expired 2017; small, well-documented breadth win for `probe`/`transcode`. |
| MPEG-2 Video | Low | Legacy; low priority |
| MPEG-2 Audio | Low | Legacy; low priority |

Not a codec but adjacent and high-leverage: an **MPEG-TS demuxer** in
`tpt-kinetix-demux` unlocks broadcast and HLS *input* (HLS is output-only today).

Done: AAC-LC decode (see `codec-evaluations/aac.md`). Dropped: HEVC/H.265
(see `codec-evaluations/hevc.md` for the technical evaluation, retained for reference).

Add new candidates here as they are prioritized.

## Original Specialist Codec Concepts (not ports)

The table above tracks *ported* implementations of existing standards. This
section tracks candidate *original* bitstream designs — codecs this project
would design itself, in the same category as `tpt-kinetix-lean` (see
`tpt-kinetix-lean/README.md` and `todo.md` Phase 13). None of these are
started; they're recorded here as a backlog while the idea is fresh, per
Phase 14 in `todo.md`.

Each trades general-purpose compression ratio for being much better suited
to one specific use case than AV1/HEVC/VVC, which are optimized for a human
viewer's perceptual quality across arbitrary natural-image content.

| Crate (proposed) | Optimizes for | Why existing codecs underserve it |
|---|---|---|
| `tpt-kinetix-lean` *(in progress, `todo.md` Phase 13)* | Bounded memory/time on constrained hardware | AV1/VVC chase ratio via unbounded recursive search + serial CABAC |
| `tpt-kinetix-vision` | Detector/classifier accuracy per bit, not human perceptual quality | General codecs optimize for a human viewer; ML pipelines don't need perceptual smoothing and can lose the exact high-frequency detail a detector needs. Design notes: chroma is optional (many detection/pose/tracking models run luma-only — drop color outright, not just subsample it); bit depth/quantization should match the consuming model's trained precision, not human-eye 8-bit convention; decoder output target is a feature/embedding tensor for the common case, only reconstructing full pixels on demand for human review of a flagged clip — a materially different decoder shape (bitstream → tensor) than every other entry in this table. Overlaps `tpt-kinetix-lean`'s embedded target (edge cameras). |
| `tpt-kinetix-realtime` | Sub-frame latency + graceful degradation under packet loss | Cloud gaming/video conferencing need built-in loss resilience (partial-frame recovery, no B-frame lookahead), which general codecs support only as a configuration, not a design center. AR/smart-glasses overlay content is an especially demanding target profile for this codec (see note below), alongside `tpt-kinetix-lean`'s power constraints. |
| `tpt-kinetix-lossless` | Bit-exact reversibility, high bit depth (medical/scientific/archival) | "Perceptually close enough" is disqualifying for this use case; existing lossless modes (FFV1, lossless HEVC) aren't simple/embeddable |
| `tpt-kinetix-screen` | Screen/UI capture: sharp edges, flat regions, repeated glyphs | General codecs are tuned for natural-image statistics, not synthetic/screen content |
| `tpt-kinetix-face` | Talking-head/video-conferencing via landmark-driven synthesis instead of pixel coding | Pixel-based codecs can't exploit the fact that the content is a constrained class (a face) the way a generative model can. Design draft: `docs/face-codec-design.md` |
| `tpt-kinetix-volumetric` | Point-cloud / volumetric / AR-VR content | Fundamentally different data shape (3D, not 2D frames) — 2D video codecs don't apply at all |

**Considered and deliberately excluded:** a codec tuned per display density
(phone vs. tablet vs. TV). That's normally an *encode profile* decision
within one codec — multiple resolution renditions selected via adaptive
bitrate streaming (the HLS/DASH pattern `tpt-kinetix-stream` already
implements) — not a different bitstream design per device class. AR/smart
glasses is the one part of that idea that *is* a genuine new constraint
class (extreme power budget, foveated/gaze-contingent rendering, latency
sensitivity for real-world overlay), which is why it's noted as a target
profile of `tpt-kinetix-lean`/`tpt-kinetix-realtime` above rather than
given its own row.

Not part of this codec list, but adjacent: reducing source data volume at
*capture* time (event/neuromorphic cameras, region-of-interest capture,
edge inference sending features instead of pixels) rather than compressing
it after the fact. That's a sensor/pipeline-architecture lever, not a
codec — out of scope here, but relevant context for why `tpt-kinetix-vision`
above targets a tensor output rather than pixels.
