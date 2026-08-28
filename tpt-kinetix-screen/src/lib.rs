//! `tpt-kinetix-screen` — an original screen/UI-capture codec.
//!
//! Unlike general-purpose codecs ([`tpt-kinetix-h264`] /
//! [`tpt-kinetix-av1`], tuned for natural-image statistics), Screen is built
//! for **synthetic screen content**: sharp edges, large flat regions, and
//! repeated glyph/UI elements. Its bitstream classifies every coding block into
//! one of three modes (see `docs/screen-codec-design.md`):
//!
//! - `FLAT` — a solid color (or simple gradient). Cheapest for backgrounds and
//!   window chrome; runs of same-color flat blocks are coalesced by run-length.
//! - `GLYPH` — a reference into a cross-frame glyph/palette dictionary plus
//!   fg/bg colors. Exploits the fact that the same glyphs/icons recur every
//!   frame.
//! - `NATURAL` — a transform/entropy-coded block (integer Walsh–Hadamard +
//!   intra prediction) for the occasional embedded photo or video region.
//!
//! The classifier and dictionary are Screen-specific; the entropy backend is
//! the shared [`tpt-kinetix-bitstream`] primitive.
//!
//! # Status
//!
//! Mode classification, flat-fill run-length, glyph dictionary + palette, and
//! the NATURAL transform fallback are all **implemented** and run end-to-end
//! ([`reconstruct`], [`decoder`]). Screen is an **original** codec with no
//! external reference oracle, so [`decoder::ScreenDecoder::capabilities`]
//! reports `pixel_exact: false` accordingly — see the [`decoder`] module docs
//! for the honesty contract every Kinetix decoder follows.
//!
//! # References
//!
//! - Design doc: `docs/screen-codec-design.md`
//! - Adding a codec: `docs/adding-a-codec.md`
//!
//! [`tpt-kinetix-h264`]: https://docs.rs/tpt-kinetix-h264
//! [`tpt-kinetix-av1`]: https://docs.rs/tpt-kinetix-av1
//! [`tpt-kinetix-bitstream`]: https://docs.rs/tpt-kinetix-bitstream

pub mod classify;
pub mod decoder;
pub mod dictionary;
pub mod flat;
pub mod glyph;
pub mod headers;
pub mod natural;
pub mod reconstruct;

pub use decoder::ScreenDecoder;
pub use headers::{ChromaFormat, FrameHeader, FrameType, SequenceHeader};
