//! `tpt-kinetix-lean` — an original, embedded-first video codec.
//!
//! Unlike [`tpt-kinetix-h264`](https://docs.rs/tpt-kinetix-h264) and
//! [`tpt-kinetix-av1`](https://docs.rs/tpt-kinetix-av1), which are from-scratch
//! *conformant* implementations of existing ITU/AOMedia standards, Lean is an
//! original bitstream format designed by this project. It deliberately does
//! not chase AV1-class compression ratio; it optimizes for the properties
//! that matter on constrained hardware instead:
//!
//! - **Bounded memory.** The sequence header declares the maximum frame
//!   dimensions and reference count up front, so a decoder can size its
//!   working arenas once at stream start and never allocate again on the
//!   per-frame decode path (see [`headers`]).
//! - **Bounded, predictable decode time.** No recursive partition search —
//!   block partitioning is a fixed, shallow scheme, not a recursive
//!   quad/multi-type tree.
//! - **Parallel entropy decode.** Coefficients are coded with an rANS/tANS
//!   family coder (see [`tpt_kinetix_bitstream`]) split across
//!   independently-decodable interleaved sub-streams, instead of CABAC's
//!   bit-serial adaptive arithmetic coding, which cannot be parallelized
//!   across a single slice.
//! - **Integer-only math**, so the pipeline has no floating-point dependency
//!   and can eventually run on MCU-class targets with no FPU.
//!
//! The accepted tradeoff is roughly 10-15% worse compression than AV1 at
//! matched content, in exchange for a decoder that stays small, auditable,
//! and genuinely parallel at the entropy stage.
//!
//! # Status
//!
//! This crate is a **scaffold**: header types and the rANS primitive exist,
//! but block reconstruction (prediction, transform, in-loop filter) is not
//! implemented yet. [`LeanDecoder::capabilities`] reports `pixel_exact:
//! false` accordingly — see the [`decoder`] module docs for the honesty
//! contract every Kinetix decoder follows.
//!
//! # v1 target envelope
//!
//! Not yet load-bearing (revisitable as the format firms up), but the
//! numbers real work should be checked against:
//!
//! - Max resolution: 1920×1080
//! - Max reference frames: 4
//! - Target decode arena ceiling: a few tens of MB at 1080p (bounded by
//!   `max_width * max_height * max_ref_frames`, no per-frame growth)
//! - Target platform class: embedded Linux, Raspberry Pi–class SBC (v1);
//!   `no_std`/MCU is explicit future work once the alloc-free hot path is
//!   proven here.
//!
//! # Transform design
//!
//! Lean uses a **fixed shallow partition** (8×8..64×64 blocks, declared in
//! the sequence header) with a constrained set of sub-partition splits. Each
//! leaf partition gets exactly one integer transform:
//!
//! | Partition size | Transform |
//! |---|---|
//! | 64×64 | 16×16 DCT-II (luma DC) — only for skip/intra_64; no residual coding |
//! | 32×32 | 16×16 DCT-II |
//! | 16×16 | 8×8 DCT-II |
//! | 8×8 | 4×4 DCT-II |
//! | 4×4 (chroma sub-partition) | 4×4 Hadamard (chroma DC) |
//!
//! The transform bank is a strict subset of the H.264/AV1 DCT family —
//! no DST-VII, no AEBI, no multi-transform selection. A single DCT-II
//! core handles all non-4×4 sizes via scaling; the 4×4 case uses the
//! Hadamard for the chroma DC plane, matching H.264 §8.5.11. This keeps
//! the integer ALU footprint under 2 KB of tables.
//!
//! # Intra prediction modes
//!
//! Lean supports **12 directional intra modes** plus DC and planar, for a
//! total of 14 modes (4 bits to signal):
//!
//! | Mode index | Name | Angle (approx.) |
//! |---|---|---|
//! | 0 | Intra_DC | — |
//! | 1 | Intra_Planar | — |
//! | 2 | Intra_Horizontal | 0° |
//! | 3 | Intra_DiagDownLeft | 45° |
//! | 4 | Intra_DiagDownRight | 45° (mirrored) |
//! | 5 | Intra_Vertical | 90° |
//! | 6 | Intra_DiagUpLeft | 135° |
//! | 7 | Intra_DiagUpRight | 135° (mirrored) |
//! | 8 | Intra_HorizontalUp | 22° |
//! | 9 | Intra_HorizontalDown | 22° (mirrored) |
//! | 10 | Intra_VerticalLeft | 67° |
//! | 11 | Intra_VerticalRight | 67° (mirrored) |
//! | 12 | Intra_HorizontalUpDiag | 112° |
//! | 13 | Intra_VerticalDownDiag | 112° (mirrored) |
//!
//! The mode set is intentionally a strict subset of AV1's 56 intra
//! directions. Fewer modes means fewer mode-coded symbols per MB, which
//! matters more on an rANS coder with fixed-frequency tables than the
//! marginal compression gain from denser angular sampling. The most
//! probable mode (MPM) is derived from left/top neighbours exactly as
//! in H.264 §8.3.1.1 — a single MPM bit is sufficient.
//!
//! # Inter prediction / motion vectors
//!
//! Lean supports **unidirectional inter prediction only** (no B-frames,
//! no weighted prediction, no compound modes). Each partition may use
//! one of:
//!
//! - **Skip** (zero MV, predicted from collocated reference — costs 0 bits)
//! - **NEWMV** — one delta-MV per partition, signalled relative to a
//!   median predictor from left/top/collocated, same as H.264 §8.4.1
//! - **NEARESTMV** — reuse the nearest spatial MV predictor (costs 0 bits
//!   for the MV, only a mode-signal bit)
//!
//! Motion vectors are quarter-pixel precision. Sub-pel interpolation uses
//! a 6-tap separable filter for luma (same kernel as H.264 §8.4.2.2) and
//! bilinear for chroma. The reference index is signalled per-frame (not
//! per-MB), so a frame is either entirely inter or entirely intra — the
//! frame header carries `ref_frame_count` and the decoder indexes the DPB
//! directly.
//!
//! The maximum search range is declared in the sequence header
//! (`max_mv_range`, 1 byte: range in 4-pixel units, 0..=255 → 0..1020
//! pixels). This bounds the decode-time working memory for MV storage and
//! interpolation line buffers — the same bounded-memory principle as the
//! rest of the format.
//!
//! # In-loop filter
//!
//! Lean uses a **single-stage deblocking filter**, structurally identical
//! to H.264's but simplified:
//!
//! - Filter strength (`bs`) is derived from whether the boundary is
//!   MB-internal, MB-edge, or reference-frame-edge — no transform-skip
//!   or residual-coefficient check (all partitions are transform-coded).
//! - The `tc0` and `beta` tables are the same 4-entry tables as H.264
//!   Table 8-16/8-17, indexed by `QP_avg` and `bs`.
//! - The filter is applied on every MB boundary in raster order (no
//!   deblocking-aware tile/tile-boundary skip).
//! - The filter runs **after** all MBs in a frame are reconstructed
//!   (same as H.264 — not edge-parallel like AV1's CDEF/loop-restoration).
//!
//! A second-stage filter (similar to AV1's CDEF or loop-restoration) is
//! explicitly **out of scope for v1** — the design trades a small amount
//! of post-filter quality for keeping the decode pipeline to a single
//! filter pass that runs in-place on the reconstructed frame buffer.

pub mod decoder;
pub mod headers;

pub use decoder::LeanDecoder;
pub use headers::{FrameHeader, FrameType, SequenceHeader};
