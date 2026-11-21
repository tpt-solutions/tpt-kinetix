//! Time-slab chunking for 4D splat sequences.
//!
//! The timeline is cut into equal slabs. A splat belongs to a slab when its
//! temporal weight, `exp(-d² / 2σ_t²)`, is above `eps` at the nearest point of
//! the slab, where `d` is the distance from the splat's time to the slab. That
//! is the same weight `tpt-math-splat::gaussian::condition_on_time` returns, so
//! a splat is kept in a slab exactly when it would contribute to that slab.
//!
//! A splat near a slab boundary is stored in both slabs. Each chunk can then
//! be decoded and rendered alone, which is what makes seeking possible.

use crate::cloud4d::GaussianCloud4D;
use crate::error::SplatError;

/// Most slabs a single sequence may have. Bounds the index size.
pub const MAX_SLABS: usize = 1 << 20;

/// One time slab and the splats that contribute to it.
#[derive(Debug, Clone, PartialEq)]
pub struct TemporalChunk {
    /// Start of the slab, in the same time units as the splat means.
    pub t_start: f32,
    /// End of the slab (exclusive for indexing, inclusive for weights).
    pub t_end: f32,
    /// Indices into the source cloud, in ascending order.
    pub members: Vec<usize>,
}

/// Splits `cloud` into time-slab chunks.
///
/// `slab` is the slab duration and `eps` the weight threshold, which must lie
/// in `(0, 1)`. The result covers every time at which some splat's weight
/// exceeds `eps`. A splat with no such time is in no chunk.
///
/// # Errors
///
/// * [`SplatError::Invalid`] for a non-positive or non-finite `slab`, an `eps`
///   outside `(0, 1)`, a degenerate splat, or more than [`MAX_SLABS`] slabs.
pub fn temporal_chunks(
    cloud: &GaussianCloud4D,
    slab: f32,
    eps: f32,
) -> Result<Vec<TemporalChunk>, SplatError> {
    if !slab.is_finite() || slab <= 0.0 {
        return Err(SplatError::Invalid(
            "slab duration must be positive and finite",
        ));
    }
    if !(eps > 0.0 && eps < 1.0) {
        return Err(SplatError::Invalid("eps must lie in (0, 1)"));
    }
    if cloud.is_empty() {
        return Ok(Vec::new());
    }

    // Per splat: its time, and the radius where its weight falls to eps.
    // Radius r satisfies exp(-r² / 2σ²) = eps, so r = σ √(-2 ln eps).
    let k = (-2.0 * eps.ln()).sqrt();
    let mut support = Vec::with_capacity(cloud.len());
    for i in 0..cloud.len() {
        let t = cloud.means()[i][3];
        let r = cloud.temporal_variance(i)?.sqrt() * k;
        support.push((t, r));
    }
    let t0 = support
        .iter()
        .map(|&(t, r)| t - r)
        .fold(f32::INFINITY, f32::min);
    let t1 = support
        .iter()
        .map(|&(t, r)| t + r)
        .fold(f32::NEG_INFINITY, f32::max);
    let slabs = (((t1 - t0) / slab).ceil() as usize).max(1);
    if slabs > MAX_SLABS {
        return Err(SplatError::Invalid(
            "too many time slabs for this slab duration",
        ));
    }

    // Slab bounds, computed exactly as the chunks report them.
    let bounds: Vec<(f32, f32)> = (0..slabs)
        .map(|s| {
            let a = t0 + s as f32 * slab;
            (a, a + slab)
        })
        .collect();
    // A splat belongs to slab [a, b] exactly when its distance to the slab is
    // below r, i.e. a < t + r and b > t − r. Each splat's support covers a
    // contiguous range of slabs, so push it only into those, and verify the
    // exact predicate. The estimated range may be off by one at a boundary.
    let mut members: Vec<Vec<usize>> = vec![Vec::new(); slabs];
    for (i, &(t, r)) in support.iter().enumerate() {
        let lo = t - r;
        let hi = t + r;
        let first = (((lo - t0) / slab).floor() as isize - 1).clamp(0, slabs as isize - 1) as usize;
        let last = (((hi - t0) / slab).floor() as isize + 1).clamp(0, slabs as isize - 1) as usize;
        for (s, &(a, b)) in bounds.iter().enumerate().take(last + 1).skip(first) {
            let d = if t < a {
                a - t
            } else if t > b {
                t - b
            } else {
                0.0
            };
            if d < r {
                members[s].push(i);
            }
        }
    }
    let chunks = bounds
        .into_iter()
        .zip(members)
        .map(|((a, b), members)| TemporalChunk {
            t_start: a,
            t_end: b,
            members,
        })
        .collect();
    Ok(chunks)
}

/// Materialises one chunk as its own cloud, in Hilbert order.
///
/// # Errors
///
/// Propagates [`SplatError`] from [`GaussianCloud4D::select`].
pub fn chunk_cloud(
    cloud: &GaussianCloud4D,
    chunk: &TemporalChunk,
) -> Result<GaussianCloud4D, SplatError> {
    let picked = cloud.select(&chunk.members)?;
    let order = picked.hilbert_permutation();
    picked.select(&order)
}
