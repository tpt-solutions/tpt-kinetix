//! Deterministic 3DMM rasterizer (DECISION 2 synthesizer, implementation-order
//! step 4).
//!
//! Produces a [`VideoFrame`] from [`FaceParams`] + a [`BasisAsset`] with **no
//! neural network**: displace the base mesh by the identity + expression bases,
//! place it via pose, shade by a Lambert/SH-style illumination model, and
//! rasterize with a z-buffer to an RGB24 framebuffer. Deterministic and
//! asset-bounded (DECISION 6).
//!
//! This is the v1 mandatory synthesizer. It is intentionally CG-looking, not
//! photoreal — a neural-texture refinement is the v2 opt-in layer (DECISION 2).

use tpt_kinetix_core::error::KinetixError;
use tpt_kinetix_core::frame::VideoFrame;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_core::PixelFormat;

use crate::basis::BasisAsset;
use crate::{FaceParams, FaceSynthesizer};

type Vec3 = [f32; 3];

fn dot(a: Vec3, b: Vec3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn normalize(v: Vec3) -> Vec3 {
    let l = dot(v, v).sqrt().max(1e-8);
    [v[0] / l, v[1] / l, v[2] / l]
}

fn clamp01(x: f32) -> f32 {
    x.clamp(0.0, 1.0)
}

/// Pad a pose vector to the 6 canonical params `[rx, ry, rz, tx, ty, tz]`.
fn pad_pose(pose: &[f32]) -> Vec3_6 {
    let mut p = [0.0f32; 6];
    for (i, &v) in pose.iter().take(6).enumerate() {
        p[i] = v;
    }
    p
}
type Vec3_6 = [f32; 6];

/// Pad an illumination vector, filling SH-style slots with sane Lambert defaults.
fn pad_illum(illum: &[f32]) -> Vec3_9 {
    let mut out = [0.0f32; 9];
    // Defaults: light straight on, moderate ambient + diffuse.
    out[2] = 1.0;
    out[3] = 0.35;
    out[4] = 0.35;
    out[5] = 0.35;
    out[6] = 0.65;
    out[7] = 0.65;
    out[8] = 0.65;
    for (i, &v) in illum.iter().take(9).enumerate() {
        out[i] = v;
    }
    out
}
type Vec3_9 = [f32; 9];

fn rotate(v: Vec3, p: Vec3_6) -> Vec3 {
    let (rx, ry, rz) = (p[0], p[1], p[2]);
    let v = rot_x(v, rx);
    let v = rot_y(v, ry);
    rot_z(v, rz)
}

fn rot_x(v: Vec3, a: f32) -> Vec3 {
    let (s, c) = a.sin_cos();
    [v[0], v[1] * c - v[2] * s, v[1] * s + v[2] * c]
}
fn rot_y(v: Vec3, a: f32) -> Vec3 {
    let (s, c) = a.sin_cos();
    [v[0] * c + v[2] * s, v[1], -v[0] * s + v[2] * c]
}
fn rot_z(v: Vec3, a: f32) -> Vec3 {
    let (s, c) = a.sin_cos();
    [v[0] * c - v[1] * s, v[0] * s + v[1] * c, v[2]]
}

/// Signed area * 2 of triangle (a, b, c) in screen space.
fn edge(a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> f32 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

/// Barycentric weights `(w_a, w_b, w_c)` of point `p` for triangle (a, b, c).
fn bary(a: [f32; 2], b: [f32; 2], c: [f32; 2], p: [f32; 2]) -> (f32, f32, f32) {
    let area = edge(a, b, c);
    if area.abs() < 1e-9 {
        return (0.0, 0.0, 0.0);
    }
    let wa = edge(b, c, p) / area;
    let wb = edge(c, a, p) / area;
    let wc = edge(a, b, p) / area;
    (wa, wb, wc)
}

/// The v1 mandatory synthesizer: deterministic 3DMM rasterization.
pub struct DeterministicRasterizer;

impl FaceSynthesizer for DeterministicRasterizer {
    fn synthesize(
        &self,
        params: &FaceParams,
        basis: &BasisAsset,
        width: u32,
        height: u32,
    ) -> Result<VideoFrame, KinetixError> {
        let width = width.max(1) as usize;
        let height = height.max(1) as usize;
        let n = basis.base_vertices.chunks(3).count();

        // 1. Displace the base mesh by identity + expression bases.
        let mut verts = vec![0.0f32; 3 * n];
        for (vi, (out, base)) in verts
            .chunks_mut(3)
            .zip(basis.base_vertices.chunks(3))
            .enumerate()
        {
            let mut p = [base[0], base[1], base[2]];
            for (k, &c) in params
                .identity
                .iter()
                .enumerate()
                .take(basis.identity_bases.len())
            {
                let b = &basis.identity_bases[k];
                p[0] += c * b[3 * vi];
                p[1] += c * b[3 * vi + 1];
                p[2] += c * b[3 * vi + 2];
            }
            for (k, &c) in params
                .expression
                .iter()
                .enumerate()
                .take(basis.expression_bases.len())
            {
                let b = &basis.expression_bases[k];
                p[0] += c * b[3 * vi];
                p[1] += c * b[3 * vi + 1];
                p[2] += c * b[3 * vi + 2];
            }
            out[0] = p[0];
            out[1] = p[1];
            out[2] = p[2];
        }

        // 2. Pose transform (rotation + translation) of vertices and normals.
        let pose = pad_pose(&params.pose);
        let mut posed = vec![0.0f32; 3 * n];
        let mut posed_nrm = vec![0.0f32; 3 * n];
        for (vi, (vchunk, pchunk)) in verts.chunks(3).zip(posed.chunks_mut(3)).enumerate() {
            let bn = &basis.base_normals[3 * vi..3 * vi + 3];
            let rp = rotate([vchunk[0], vchunk[1], vchunk[2]], pose);
            let rn = rotate([bn[0], bn[1], bn[2]], pose);
            pchunk[0] = rp[0] + pose[3];
            pchunk[1] = rp[1] + pose[4];
            pchunk[2] = rp[2] + pose[5];
            posed_nrm[3 * vi] = rn[0];
            posed_nrm[3 * vi + 1] = rn[1];
            posed_nrm[3 * vi + 2] = rn[2];
        }

        // 3. Orthographic projection to screen space + per-vertex depth.
        let scale = (width.min(height) as f32) * 0.42;
        let cx = width as f32 / 2.0;
        let cy = height as f32 / 2.0;
        let mut screen = vec![[0.0f32; 2]; n];
        let mut depth = vec![0.0f32; n];
        for (vi, p) in posed.chunks(3).enumerate() {
            screen[vi] = [cx + p[0] * scale, cy - p[1] * scale];
            depth[vi] = p[2];
        }

        // 4. Lambert/SH-style per-vertex shading.
        let illum = pad_illum(&params.illumination);
        let ldir = if dot(
            [illum[0], illum[1], illum[2]],
            [illum[0], illum[1], illum[2]],
        ) > 1e-8
        {
            normalize([illum[0], illum[1], illum[2]])
        } else {
            [0.0, 0.0, 1.0]
        };
        let ambient = [clamp01(illum[3]), clamp01(illum[4]), clamp01(illum[5])];
        let diffuse = [clamp01(illum[6]), clamp01(illum[7]), clamp01(illum[8])];
        let mut colors = vec![[0.0f32; 3]; n];
        for (vi, (nrm, alb)) in posed_nrm.chunks(3).zip(basis.albedo.chunks(3)).enumerate() {
            let ndotl = (0.0f32).max(dot([nrm[0], nrm[1], nrm[2]], ldir));
            colors[vi] = [
                alb[0] * (ambient[0] + diffuse[0] * ndotl),
                alb[1] * (ambient[1] + diffuse[1] * ndotl),
                alb[2] * (ambient[2] + diffuse[2] * ndotl),
            ];
        }

        // 5. Z-buffer rasterization into an RGB24 framebuffer.
        let mut data = vec![0u8; 3 * width * height];
        for px in data.chunks_mut(3) {
            px[0] = 60;
            px[1] = 60;
            px[2] = 60;
        }
        let mut zbuf = vec![f32::INFINITY; width * height];
        for tri in basis.indices.chunks(3) {
            let ia = tri[0] as usize;
            let ib = tri[1] as usize;
            let ic = tri[2] as usize;
            if ia >= n || ib >= n || ic >= n {
                continue;
            }
            let a = screen[ia];
            let b = screen[ib];
            let c = screen[ic];
            let (za, zb, zc) = (depth[ia], depth[ib], depth[ic]);
            let (ca, cb, cc) = (colors[ia], colors[ib], colors[ic]);
            if edge(a, b, c).abs() < 1e-6 {
                continue;
            }
            let minx = a[0].min(b[0]).min(c[0]).floor().max(0.0) as usize;
            let maxx = (a[0].max(b[0]).max(c[0]).ceil())
                .min(width as f32 - 1.0)
                .max(0.0) as usize;
            let miny = a[1].min(b[1]).max(c[1]).floor().max(0.0) as usize;
            let maxy = (a[1].max(b[1]).max(c[1]).ceil())
                .min(height as f32 - 1.0)
                .max(0.0) as usize;
            for py in miny..=maxy {
                for px in minx..=maxx {
                    let p = [px as f32 + 0.5, py as f32 + 0.5];
                    let (w0, w1, w2) = bary(a, b, c, p);
                    if w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0 {
                        let wsum = w0 + w1 + w2;
                        let z = (w0 * za + w1 * zb + w2 * zc) / wsum;
                        let idx = py * width + px;
                        if z < zbuf[idx] {
                            zbuf[idx] = z;
                            let o = 3 * idx;
                            data[o] = (clamp01(w0 * ca[0] + w1 * cb[0] + w2 * cc[0]) * 255.0) as u8;
                            data[o + 1] =
                                (clamp01(w0 * ca[1] + w1 * cb[1] + w2 * cc[1]) * 255.0) as u8;
                            data[o + 2] =
                                (clamp01(w0 * ca[2] + w1 * cb[2] + w2 * cc[2]) * 255.0) as u8;
                        }
                    }
                }
            }
        }

        Ok(VideoFrame {
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            data,
            width: width as u32,
            height: height as u32,
            pixel_format: PixelFormat::Rgb24,
            is_key_frame: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::basis::builtin_basis;
    use crate::FaceParams;

    fn params() -> FaceParams {
        FaceParams {
            identity: vec![0.1, -0.05, 0.0, 0.0],
            expression: vec![0.0, 0.2, 0.0, 0.0],
            pose: vec![0.0, 0.3, 0.0, 0.0, 0.0, 0.0],
            illumination: vec![0.0, 0.0, 1.0, 0.4, 0.4, 0.4, 0.7, 0.7, 0.7],
            appearance: vec![],
        }
    }

    #[test]
    fn synthesizes_a_frame_of_correct_size() {
        let basis = builtin_basis();
        let frame = DeterministicRasterizer
            .synthesize(&params(), &basis, 64, 48)
            .expect("synthesize");
        assert_eq!(frame.width, 64);
        assert_eq!(frame.height, 48);
        assert_eq!(frame.pixel_format, PixelFormat::Rgb24);
        assert_eq!(frame.data.len(), 3 * 64 * 48);
    }

    #[test]
    fn synthesis_is_deterministic() {
        let basis = builtin_basis();
        let a = DeterministicRasterizer
            .synthesize(&params(), &basis, 64, 48)
            .expect("synthesize");
        let b = DeterministicRasterizer
            .synthesize(&params(), &basis, 64, 48)
            .expect("synthesize");
        assert_eq!(a.data, b.data);
    }

    #[test]
    fn synthesis_produces_non_uniform_output() {
        let basis = builtin_basis();
        let frame = DeterministicRasterizer
            .synthesize(&params(), &basis, 64, 48)
            .expect("synthesize");
        let first = frame.data[0];
        assert!(frame.data.iter().any(|&b| b != first), "frame is flat");
    }

    #[test]
    fn pose_changes_output() {
        let basis = builtin_basis();
        let mut p = params();
        let base = DeterministicRasterizer
            .synthesize(&p, &basis, 64, 48)
            .expect("synthesize");
        p.pose[1] = 1.2;
        let rotated = DeterministicRasterizer
            .synthesize(&p, &basis, 64, 48)
            .expect("synthesize");
        assert_ne!(base.data, rotated.data, "pose should change the render");
    }
}
