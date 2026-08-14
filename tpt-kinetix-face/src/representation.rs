//! v1 face representation (DECISION 1 of `docs/face-codec-design.md`).
//!
//! The bitstream carries a *control vector* that drives synthesis. Three
//! parametrizations were evaluated; the v1 decision is:
//!
//! - **Primary:** a parametric **3D Morphable Model (3DMM)** coefficient vector
//!   (identity / expression / pose / illumination / appearance). Carried by
//!   [`FaceParams`]; rendered by the DECISION-2 deterministic rasterizer.
//! - **Companion:** a **sparse-landmark** vector as a lowest-bitrate mode /
//!   avatar-drive signal (additive framing; never the sole v1 target).
//! - **Deferred:** a **learned latent code** to v2 — it would make the decoder
//!   a neural network, contradicting the v1 deterministic, NN-free, embedded
//!   envelope (DECISION 2/6/7).
//!
//! The choice is encoded as [`FaceRepresentation`];
//! [`FaceRepresentation::v1_primary`] identifies the canonical v1
//! representation.

use std::fmt;

/// The parametrization a face bitstream carries.
///
/// This is the load-bearing "representation for v1" decision (DECISION 1). A v1
/// decoder is required to support [`FaceRepresentation::Parametric3Dmm`] and
/// optionally [`FaceRepresentation::SparseLandmarks`];
/// [`FaceRepresentation::LearnedLatent`] is a v2-only representation and is
/// *not* decodable by a v1 decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FaceRepresentation {
    /// 3D Morphable Model coefficient vector — the v1 canonical representation.
    ///
    /// Maps 1:1 onto [`FaceParams`] (identity / expression / pose /
    /// illumination / appearance). Self-contained decode: the decoder renders
    /// from the vector plus a fixed, versioned 3DMM asset — no source frame
    /// needed (DECISION 1, Alternative A).
    Parametric3Dmm,
    /// Sparse facial landmarks (e.g. 68-point IBUG or a conferencing-tuned
    /// subset). A low-bitrate companion / avatar-drive signal; not a
    /// self-sufficient synthesis target on its own (DECISION 1, Alternative B).
    SparseLandmarks,
    /// Learned latent code (face-vid2vid / StyleGAN-style). **Deferred to v2** —
    /// requires shipping a fixed, versioned neural renderer, which conflicts with
    /// the v1 deterministic, NN-free decoder contract (DECISION 2/6/7).
    LearnedLatent,
}

impl FaceRepresentation {
    /// The canonical representation a v1 decoder is guaranteed to support.
    pub fn v1_primary() -> Self {
        FaceRepresentation::Parametric3Dmm
    }

    /// Whether a v1 decoder can synthesize from this representation.
    ///
    /// `LearnedLatent` is excluded: a v1 decoder ships no neural renderer.
    pub fn is_supported_in_v1(self) -> bool {
        matches!(
            self,
            FaceRepresentation::Parametric3Dmm | FaceRepresentation::SparseLandmarks
        )
    }

    /// Stable short tag for diagnostics and capability `notes`.
    pub fn as_str(self) -> &'static str {
        match self {
            FaceRepresentation::Parametric3Dmm => "3dmm",
            FaceRepresentation::SparseLandmarks => "landmarks",
            FaceRepresentation::LearnedLatent => "latent",
        }
    }
}

impl fmt::Display for FaceRepresentation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Default 3DMM parameter-group dimensions for the v1 basis.
///
/// These are the *nominal* counts from DECISION 1 (identity sent once per call;
/// per-frame expression/pose deltas; slowly-varying illumination/appearance).
/// The actual decoded length is carried per-stream in the sequence header; these
/// constants size working buffers and document the intended manifold.
#[derive(Debug, Clone, Copy)]
pub struct V1DimensionSpec {
    /// Identity / shape basis weights (sent once per call).
    pub identity: usize,
    /// Expression basis weights (per-frame delta).
    pub expression: usize,
    /// Pose parameters (3D rotation + translation; per-frame delta).
    pub pose: usize,
    /// Spherical-harmonic illumination coefficients (slowly varying).
    pub illumination: usize,
    /// Appearance / albedo basis weights (slowly varying).
    pub appearance: usize,
}

impl V1DimensionSpec {
    /// Total scalar count across all five groups.
    pub fn total(&self) -> usize {
        self.identity + self.expression + self.pose + self.illumination + self.appearance
    }
}

/// Nominal v1 3DMM group sizes (DECISION 1).
///
/// `identity` carries ~80 PCA weights (FLAME/FaceWarehouse scale); `expression`
/// ~50; `pose` 6 (3 rotation + 3 translation); `illumination` 27 (3-band SH ×
/// RGB); `appearance` ~40 albedo PCA weights. Basis selection is open-question 1
/// in `docs/face-codec-design.md`.
pub const V1_3DMM_DIMS: V1DimensionSpec = V1DimensionSpec {
    identity: 80,
    expression: 50,
    pose: 6,
    illumination: 27,
    appearance: 40,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v1_primary_is_3dmm() {
        assert_eq!(FaceRepresentation::v1_primary(), FaceRepresentation::Parametric3Dmm);
    }

    #[test]
    fn v1_supports_3dmm_and_landmarks_not_latent() {
        assert!(FaceRepresentation::Parametric3Dmm.is_supported_in_v1());
        assert!(FaceRepresentation::SparseLandmarks.is_supported_in_v1());
        assert!(!FaceRepresentation::LearnedLatent.is_supported_in_v1());
    }

    #[test]
    fn v1_dims_total_is_sum_of_groups() {
        assert_eq!(V1_3DMM_DIMS.total(), 80 + 50 + 6 + 27 + 40);
    }

    #[test]
    fn representation_display_is_stable_tag() {
        assert_eq!(FaceRepresentation::Parametric3Dmm.to_string(), "3dmm");
        assert_eq!(FaceRepresentation::SparseLandmarks.to_string(), "landmarks");
        assert_eq!(FaceRepresentation::LearnedLatent.to_string(), "latent");
    }
}
