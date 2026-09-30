//! Decoder capability introspection.
//!
//! Some Kinetix decoders are original formats with no reference decoder, and
//! others only cover a subset of their standard (for example, H.264 strict mode
//! rejects multi-slice pictures and non-4:2:0 chroma). To avoid silently
//! returning unverified pixel data as if it were correct, every decoder exposes
//! a [`DecoderCapabilities`] value so that callers, the CLI, and tests can detect
//! an unverified decode path *programmatically* rather than by reading source
//! comments.
//!
//! # Examples
//!
//! ```rust
//! use tpt_kinetix_core::capabilities::DecoderCapabilities;
//!
//! let caps = DecoderCapabilities {
//!     codec: "H.264",
//!     pixel_exact: false,
//!     deterministic: false,
//!     supports_cabac: false,
//!     supports_cavlc: true,
//!     supports_intra_prediction: false,
//!     supports_inter_prediction: false,
//!     supports_deblocking: false,
//!     notes: "bitstream + scaffold reconstruction only",
//! };
//!
//! // Callers can refuse to trust non-pixel-exact output.
//! assert!(!caps.pixel_exact);
//! ```

use std::fmt;

/// Describes what a decoder can (and cannot yet) do.
///
/// The most important field is [`DecoderCapabilities::pixel_exact`]: when it is
/// `false`, the decoder's output frames are **not** guaranteed to match a
/// reference decoder and must not be treated as correct pixel data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecoderCapabilities {
    /// Human-readable codec label, e.g. `"H.264"` or `"AV1"`.
    pub codec: &'static str,

    /// `true` only when decoded frames are bit-exact with a reference decoder.
    ///
    /// While `false`, output frames are placeholders/approximations and callers
    /// should surface a warning or refuse to use them for anything that needs
    /// correct pixels.
    pub pixel_exact: bool,

    /// `true` when decoding is fully deterministic and its exact output (and, for
    /// original codecs, the exact encoded bytes) is pinned by committed golden
    /// vectors, so any change is caught in CI.
    ///
    /// This is weaker than [`pixel_exact`](Self::pixel_exact): it says the output is
    /// stable, not that it matches an independent reference decoder. Original
    /// codecs with no reference oracle set this instead of `pixel_exact`.
    pub deterministic: bool,

    /// Whether CABAC entropy decoding is implemented.
    pub supports_cabac: bool,

    /// Whether CAVLC entropy decoding is implemented.
    pub supports_cavlc: bool,

    /// Whether intra prediction is implemented.
    pub supports_intra_prediction: bool,

    /// Whether inter prediction (motion compensation) is implemented.
    pub supports_inter_prediction: bool,

    /// Whether the in-loop deblocking filter is implemented.
    pub supports_deblocking: bool,

    /// Free-form notes describing the current state of the decode path.
    pub notes: &'static str,
}

impl DecoderCapabilities {
    /// Returns `true` when the decoder is not yet pixel-exact and its output
    /// should be treated as untrusted placeholder data.
    #[must_use]
    pub fn is_incomplete(&self) -> bool {
        !self.pixel_exact
    }
}

impl fmt::Display for DecoderCapabilities {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} decoder: pixel_exact={} deterministic={} (cabac={}, cavlc={}, intra={}, inter={}, deblock={}) — {}",
            self.codec,
            self.pixel_exact,
            self.deterministic,
            self.supports_cabac,
            self.supports_cavlc,
            self.supports_intra_prediction,
            self.supports_inter_prediction,
            self.supports_deblocking,
            self.notes,
        )
    }
}
