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
//!   family coder ([`rans`]) split across independently-decodable
//!   interleaved sub-streams, instead of CABAC's bit-serial adaptive
//!   arithmetic coding, which cannot be parallelized across a single slice.
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

pub mod bitreader;
pub mod decoder;
pub mod headers;
pub mod rans;

pub use decoder::LeanDecoder;
pub use headers::{FrameHeader, FrameType, SequenceHeader};
