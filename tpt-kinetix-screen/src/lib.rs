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
//! - `NATURAL` — a transform/entropy-coded block (borrowed from
//!   [`tpt-kinetix-lean`] via [`tpt-kinetix-bitstream`]) for the occasional
//!   embedded photo or video region.
//!
//! The classifier and dictionary are Screen-specific; the entropy backend and
//! the `NATURAL` fallback are shared [`tpt-kinetix-bitstream`] primitives.
//!
//! # Status
//!
//! This crate is a **scaffold**. The byte-aligned sequence/frame headers
//! (`[headers]`) and the shared `BitReader`/rANS primitives exist and
//! round-trip, but block reconstruction (mode classifier, flat-fill run-length,
//! glyph dictionary + palette, and the `NATURAL` transform path) is not
//! implemented yet. [`ScreenDecoder::capabilities`] reports `pixel_exact:
//! false` accordingly — see the [`decoder`] module docs for the honesty
//! contract every Kinetix decoder follows.
//!
//! # References
//!
//! - Design doc: `docs/screen-codec-design.md`
//! - Adding a codec: `docs/adding-a-codec.md`
//!
//! [`tpt-kinetix-h264`]: https://docs.rs/tpt-kinetix-h264
//! [`tpt-kinetix-av1`]: https://docs.rs/tpt-kinetix-av1
//! [`tpt-kinetix-lean`]: https://docs.rs/tpt-kinetix-lean
//! [`tpt-kinetix-bitstream`]: https://docs.rs/tpt-kinetix-bitstream

pub mod decoder;
pub mod headers;

pub use decoder::ScreenDecoder;
pub use headers::{ChromaFormat, FrameHeader, FrameType, SequenceHeader};
