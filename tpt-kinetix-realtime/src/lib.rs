//! `tpt-kinetix-realtime` — an original low-latency, loss-resilient video
//! codec.
//!
//! Unlike [`tpt-kinetix-h264`](https://docs.rs/tpt-kinetix-h264) /
//! [`tpt-kinetix-av1`](https://docs.rs/tpt-kinetix-av1) (conformant
//! implementations of existing standards) or [`tpt-kinetix-lean`] (an
//! embedded-first original codec), Realtime's design center is **sub-frame
//! latency** and **graceful degradation under packet loss**, not maximum
//! compression ratio.
//!
//! The format is **profile-agnostic** (see `docs/realtime-codec-design.md`).
//! Cloud gaming, video conferencing, and AR/smart-glasses overlay are not
//! separate codecs — they are three preset parameter sets over one shared
//! core: no B-frame lookahead (every frame is decodable the instant its
//! reference is available; GOP = rolling intra-refresh + on-demand IDR, single
//! backward reference); partial-frame loss recovery (hybrid FEC + intra-block
//! refresh + decoder concealment, so a dropped packet bounds its damage to its
//! slice and self-heals instead of stalling the frame); sub-frame latency (the
//! frame is partitioned into an independently packetizable slice grid and the
//! decoder emits slices top-to-bottom as they arrive); and an enforced latency
//! budget (the encoder carries a `deadline_ms` per frame and the decoder
//! exposes `max_decode_ms` so callers can reject streams that would miss their
//! deadline).
//!
//! The AR profile is the *hardest* preset (fine slice grid, 20-30% FEC
//! overhead, <10 MB decode arena, gaze-contingent foveation) and leans on
//! [`tpt-kinetix-lean`]'s power-conscious primitives rather than inventing a
//! fourth codec.
//!
//! # Status
//!
//! The profile-aware sequence/frame headers, the shared `BitReader`/rANS
//! primitives, slice-grid framing, intra-refresh masking, FEC, and decoder
//! concealment are implemented and round-trip. Block reconstruction — intra
//! (14 modes) + unidirectional-P inter prediction ([`prediction`]), the DCT-II
//! / Hadamard transform bank and dequant ([`transform`]), and the single-stage
//! in-loop deblock ([`deblock`]) — now runs end-to-end ([`reconstruct`],
//! [`decoder`]). Realtime is an **original** codec with no external reference
//! oracle, so its output is not pixel-exact against any standard decoder;
//! [`RealtimeDecoder::capabilities`] keeps `pixel_exact: false` accordingly —
//! see the [`decoder`] module docs for the honesty contract every Kinetix
//! decoder follows.
//!
//! [`tpt-kinetix-lean`]: https://docs.rs/tpt-kinetix-lean

pub mod decoder;
pub mod conceal;
pub mod deblock;
pub mod fec;
pub mod headers;
pub mod prediction;
pub mod rate;
pub mod reconstruct;
pub mod refresh;
pub mod slice;
pub mod transform;

pub use decoder::RealtimeDecoder;
pub use deblock::{deblock_chroma, deblock_luma, DeblockBlock};
pub use fec::{Fec, DEFAULT_SYMBOL_SIZE};
pub use conceal::conceal;
pub use headers::{FrameHeader, FrameType, ProfilePreset, SequenceHeader};
pub use prediction::{predict_intra_block, predict_inter_luma, IntraMode, MotionVector};
pub use rate::{adapt_to_deadline, max_decode_ms_estimate, EncodeDeadline, RateControlAction};
pub use reconstruct::{decode_frame_payload, encode_frame_slices, FrameBuffer};
pub use refresh::IntraRefreshScheduler;
pub use slice::SliceGrid;
pub use transform::{dequant, inverse_2d, quant, transform_2d, quant_step};
