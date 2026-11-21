//! 4D Gaussian splat model: splats with a position in space and time.
//!
//! Each splat is a 4D Gaussian over `(x, y, z, t)`. Its covariance is built from
//! two unit quaternions (an isoclinic rotation) and four scales, as in
//! `tpt-math-splat`. Scales are stored in log form, like the 3D model.
//!
//! The model is for time-varying scenes. A static 3D cloud is not converted
//! into it; that would need a choice of temporal extent that the input does
//! not supply.

use tpt_math_splat::gaussian::cov4_from_rot_scale;
use tpt_math_splat::sfc::hilbert_key;

use crate::cloud::{sh_coeffs_for_degree, MAX_SH_DEGREE};
use crate::error::SplatError;

/// Bits per axis for Hilbert ordering in 4D: 4 × 16 = 64 bits, the full key.
const HILBERT_BITS: u32 = 16;

/// A set of 4D Gaussian splats in structure-of-arrays layout.
#[derive(Debug, Clone, PartialEq)]
pub struct GaussianCloud4D {
    means: Vec<[f32; 4]>,
    log_scales: Vec<[f32; 4]>,
    rot_left: Vec<[f32; 4]>,
    rot_right: Vec<[f32; 4]>,
    opacities: Vec<f32>,
    sh_degree: u8,
    sh: Vec<f32>,
}

impl GaussianCloud4D {
    /// Builds a 4D cloud, checking every column's length.
    ///
    /// `sh` is laid out `[point][channel][coeff]`, as in `GaussianCloud`.
    ///
    /// # Errors
    ///
    /// [`SplatError::Invalid`] if columns disagree in length or the SH degree
    /// is above 3.
    pub fn new(
        means: Vec<[f32; 4]>,
        log_scales: Vec<[f32; 4]>,
        rot_left: Vec<[f32; 4]>,
        rot_right: Vec<[f32; 4]>,
        opacities: Vec<f32>,
        sh_degree: u8,
        sh: Vec<f32>,
    ) -> Result<Self, SplatError> {
        if sh_degree > MAX_SH_DEGREE {
            return Err(SplatError::Invalid("sh degree above 3"));
        }
        let n = means.len();
        if log_scales.len() != n
            || rot_left.len() != n
            || rot_right.len() != n
            || opacities.len() != n
        {
            return Err(SplatError::Invalid("4D column length mismatch"));
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
            means,
            log_scales,
            rot_left,
            rot_right,
            opacities,
            sh_degree,
            sh,
        })
    }

    /// Number of splats.
    #[must_use]
    pub fn len(&self) -> usize {
        self.means.len()
    }

    /// `true` if the cloud has no splats.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.means.is_empty()
    }

    /// Spherical-harmonics degree (0..=3).
    #[must_use]
    pub fn sh_degree(&self) -> u8 {
        self.sh_degree
    }

    /// Centre `(x, y, z, t)` of each splat.
    #[must_use]
    pub fn means(&self) -> &[[f32; 4]] {
        &self.means
    }

    /// Log scales `(s_x, s_y, s_z, s_t)`; the linear scale is `exp` of each.
    #[must_use]
    pub fn log_scales(&self) -> &[[f32; 4]] {
        &self.log_scales
    }

    /// Left quaternion of the isoclinic rotation, `[w, x, y, z]`.
    #[must_use]
    pub fn rot_left(&self) -> &[[f32; 4]] {
        &self.rot_left
    }

    /// Right quaternion of the isoclinic rotation, `[w, x, y, z]`.
    #[must_use]
    pub fn rot_right(&self) -> &[[f32; 4]] {
        &self.rot_right
    }

    /// Opacity logit of each splat.
    #[must_use]
    pub fn opacities(&self) -> &[f32] {
        &self.opacities
    }

    /// Spherical-harmonics block, `[point][channel][coeff]`.
    #[must_use]
    pub fn sh(&self) -> &[f32] {
        &self.sh
    }

    /// Temporal variance `Σ_tt` of splat `i`: the squared temporal extent of
    /// its 4D covariance.
    ///
    /// # Errors
    ///
    /// [`SplatError::Invalid`] if the quaternions are degenerate or the
    /// resulting variance is not positive and finite.
    pub fn temporal_variance(&self, i: usize) -> Result<f32, SplatError> {
        let s = self.log_scales[i].map(f32::exp);
        let cov = cov4_from_rot_scale(self.rot_left[i], self.rot_right[i], s)
            .ok_or(SplatError::Invalid("degenerate 4D rotation"))?;
        let var = cov.get(3, 3);
        if !var.is_finite() || var <= 0.0 {
            return Err(SplatError::Invalid("non-positive temporal variance"));
        }
        Ok(var)
    }

    /// Copies the splats at `indices`, in that order. Indices may repeat, so a
    /// splat can appear in several selections.
    ///
    /// # Errors
    ///
    /// [`SplatError::Invalid`] if an index is out of range.
    pub fn select(&self, indices: &[usize]) -> Result<Self, SplatError> {
        let n = self.len();
        if indices.iter().any(|&i| i >= n) {
            return Err(SplatError::Invalid("selection index out of range"));
        }
        let k3 = 3 * sh_coeffs_for_degree(self.sh_degree);
        let mut sh = Vec::with_capacity(indices.len() * k3);
        for &i in indices {
            sh.extend_from_slice(&self.sh[i * k3..(i + 1) * k3]);
        }
        Ok(Self {
            means: indices.iter().map(|&i| self.means[i]).collect(),
            log_scales: indices.iter().map(|&i| self.log_scales[i]).collect(),
            rot_left: indices.iter().map(|&i| self.rot_left[i]).collect(),
            rot_right: indices.iter().map(|&i| self.rot_right[i]).collect(),
            opacities: indices.iter().map(|&i| self.opacities[i]).collect(),
            sh_degree: self.sh_degree,
            sh,
        })
    }

    /// Permutation of splat indices along a 4D Hilbert curve over the means.
    ///
    /// Ordering by this curve puts splats that are close in space and time next
    /// to each other, which helps the entropy coder.
    #[must_use]
    pub fn hilbert_permutation(&self) -> Vec<usize> {
        if self.is_empty() {
            return Vec::new();
        }
        let mut min = [f32::INFINITY; 4];
        let mut max = [f32::NEG_INFINITY; 4];
        for m in &self.means {
            for a in 0..4 {
                if m[a].is_finite() {
                    min[a] = min[a].min(m[a]);
                    max[a] = max[a].max(m[a]);
                }
            }
        }
        for a in 0..4 {
            if min[a] > max[a] {
                min[a] = 0.0;
                max[a] = 0.0;
            }
        }
        let mut keyed: Vec<(u64, usize)> = self
            .means
            .iter()
            .enumerate()
            .map(|(i, &m)| (hilbert_key(m, min, max, HILBERT_BITS).unwrap_or(0), i))
            .collect();
        keyed.sort_unstable();
        keyed.into_iter().map(|(_, i)| i).collect()
    }
}
