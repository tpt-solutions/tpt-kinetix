//! `tpt-kinetix-splat` — 3D and 4D Gaussian splat I/O for the TPT Kinetix
//! media processing engine.
//!
//! * [`GaussianCloud`] — the format-neutral in-memory model.
//! * [`read_ply`] / [`write_ply`] — 3D Gaussian Splatting PLY interop.
//!
//! Still to come: SPZ interop, the 4D sequence model, and the `SPLT` codec
//! (see `todo.md`).
//!
//! The splat math lives in [`tpt_math_splat`] (`no_std`, f64). This crate uses
//! it for derived quantities only and keeps `f32` at the I/O boundary.

#![forbid(unsafe_code)]

mod block;
pub mod chunk;
pub mod cloud;
pub mod cloud4d;
pub mod error;
pub mod gltf;
pub mod ply;
pub mod sequence;
pub mod splt;
pub mod spz;
pub mod spz_write;

pub use chunk::{chunk_cloud, temporal_chunks, TemporalChunk};
pub use cloud::{sh_coeffs_for_degree, CoordinateFrame, GaussianCloud, MAX_SH_DEGREE};
pub use cloud4d::GaussianCloud4D;
pub use error::SplatError;
pub use gltf::{to_rub, write_glb};
pub use ply::{read_ply, write_ply};
pub use sequence::{
    read_sequence, sequence_config, sequence_from_config, write_sequence, Sequence, SlabEntry,
    CONFIG_LEN,
};
pub use splt::{read_splt, write_splt, Quantization};
pub use spz::read_spz;
pub use spz_write::write_spz;

/// Crate version string, mirroring the workspace version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Re-export of the splat math crate so callers can use one dependency.
pub use tpt_math_splat as math;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn math_crate_is_linked() {
        let id = [1.0_f64, 0.0, 0.0, 0.0];
        let cov = math::gaussian::cov4_from_rot_scale(id, id, [1.0, 1.0, 1.0, 0.5]);
        assert!(cov.is_some());
    }

    #[test]
    fn version_is_set() {
        assert!(!VERSION.is_empty());
    }
}
