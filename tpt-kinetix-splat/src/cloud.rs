//! The in-memory Gaussian cloud: the format-neutral model that every parser
//! produces and every writer consumes.

use crate::error::SplatError;

/// Highest spherical-harmonics degree the model accepts (the 3DGS PLY range).
pub const MAX_SH_DEGREE: u8 = 3;

/// Number of SH coefficients per colour channel for a given degree, `(d+1)^2`.
#[must_use]
pub const fn sh_coeffs_for_degree(degree: u8) -> usize {
    let d = degree as usize + 1;
    d * d
}

/// The axis convention a cloud's positions and orientations are expressed in.
///
/// The tag is informational: no conversion happens inside the model. It stops
/// clouds from different formats being mixed silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CoordinateFrame {
    /// Right, down, front. The 3DGS PLY convention, and the model's default.
    #[default]
    Rdf,
    /// Right, up, back. SPZ's native frame.
    Rub,
}

/// A cloud of 3D Gaussian splats.
///
/// Values are kept exactly as the source format encodes them: rotations are
/// unnormalised quaternions `[w, x, y, z]`, scales are log-scale, and opacity
/// is a logit. Activations (`exp`, `sigmoid`, quaternion normalisation) are the
/// caller's job, as in 3DGS.
///
/// The spherical-harmonics block is laid out `[point][channel][coeff]`, with
/// `channel` in `{R, G, B}` and `coeff 0` the DC term. Each point therefore
/// holds `3 * sh_coeffs_for_degree(degree)` values.
#[derive(Debug, Clone, PartialEq)]
pub struct GaussianCloud {
    positions: Vec<[f32; 3]>,
    rotations: Vec<[f32; 4]>,
    scales: Vec<[f32; 3]>,
    opacities: Vec<f32>,
    sh_degree: u8,
    sh: Vec<f32>,
    frame: CoordinateFrame,
}

impl GaussianCloud {
    /// Builds a cloud, checking that every column has one entry per point and
    /// that `sh` holds exactly `len * 3 * (degree + 1)^2` values.
    ///
    /// # Errors
    ///
    /// [`SplatError::Invalid`] if the columns disagree in length, or if
    /// `sh_degree` exceeds [`MAX_SH_DEGREE`].
    pub fn new(
        positions: Vec<[f32; 3]>,
        rotations: Vec<[f32; 4]>,
        scales: Vec<[f32; 3]>,
        opacities: Vec<f32>,
        sh_degree: u8,
        sh: Vec<f32>,
    ) -> Result<Self, SplatError> {
        if sh_degree > MAX_SH_DEGREE {
            return Err(SplatError::Invalid("sh degree above 3"));
        }
        let n = positions.len();
        if rotations.len() != n || scales.len() != n || opacities.len() != n {
            return Err(SplatError::Invalid("column length mismatch"));
        }
        let expected = n
            .checked_mul(3 * sh_coeffs_for_degree(sh_degree))
            .ok_or(SplatError::Invalid("sh size overflow"))?;
        if sh.len() != expected {
            return Err(SplatError::Invalid(
                "sh length does not match point count and degree",
            ));
        }
        Ok(Self {
            positions,
            rotations,
            scales,
            opacities,
            sh_degree,
            sh,
            frame: CoordinateFrame::default(),
        })
    }

    /// Sets the frame tag. Readers call this to state their format's frame.
    #[must_use]
    pub fn with_frame(mut self, frame: CoordinateFrame) -> Self {
        self.frame = frame;
        self
    }

    /// The axis convention the values are expressed in.
    #[must_use]
    pub fn frame(&self) -> CoordinateFrame {
        self.frame
    }

    /// Number of splats in the cloud.
    #[must_use]
    pub fn len(&self) -> usize {
        self.positions.len()
    }

    /// `true` if the cloud has no splats.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.positions.is_empty()
    }

    /// Spherical-harmonics degree (0..=3).
    #[must_use]
    pub fn sh_degree(&self) -> u8 {
        self.sh_degree
    }

    /// Centre `(x, y, z)` of each splat.
    #[must_use]
    pub fn positions(&self) -> &[[f32; 3]] {
        &self.positions
    }

    /// Unnormalised rotation quaternion `[w, x, y, z]` of each splat.
    #[must_use]
    pub fn rotations(&self) -> &[[f32; 4]] {
        &self.rotations
    }

    /// Log-scale `(x, y, z)` of each splat.
    #[must_use]
    pub fn scales(&self) -> &[[f32; 3]] {
        &self.scales
    }

    /// Opacity logit of each splat.
    #[must_use]
    pub fn opacities(&self) -> &[f32] {
        &self.opacities
    }

    /// Spherical-harmonics block, laid out `[point][channel][coeff]`.
    #[must_use]
    pub fn sh(&self) -> &[f32] {
        &self.sh
    }
}
