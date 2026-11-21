//! Render-PSNR checks for 4D splat sequences.
//!
//! A small test-only CPU renderer (not a product viewer) draws a 4D cloud at a
//! fixed time. The check renders the source cloud and the decoded `SPLS` slab
//! for that time, then compares the images. Splats are conditioned on time with
//! `tpt-math-splat`'s `condition_on_time`, projected with EWA, and alpha
//! blended front to back.
//!
//! The reference is the source cloud drawn in full. Slab chunking drops only
//! splats whose temporal weight is at or below `eps` at every time in the slab,
//! so the slab render should match the full render closely.

use tpt_kinetix_splat::{read_sequence, write_sequence, GaussianCloud4D, Quantization};
use tpt_math_splat::gaussian::{
    condition_on_time, cov4_from_rot_scale, perspective_jacobian, project_cov_ewa,
};
use tpt_math_splat::sh::eval_color;
use tpt_math_splat::sym::Sym3;

const W: usize = 64;
const H: usize = 64;
const FOCAL: f32 = 40.0;
/// Camera sits at this point and looks down +z.
const EYE: [f32; 3] = [0.0, 0.0, -20.0];
/// Screen-space anti-aliasing term, as in 3DGS.
const LOW_PASS: f32 = 0.3;

/// Deterministic pseudo-random numbers in `[0, 1)` (a 64-bit LCG).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 40) as f32) / (1u64 << 24) as f32
    }
    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.next()
    }
}

/// A scene of `n` random 4D splats in a box around the origin, spread over
/// `[0, duration]` in time. Rotations are random unit quaternions, so the time
/// axis mixes with space.
fn scene(n: usize, seed: u64, duration: f32) -> GaussianCloud4D {
    let mut rng = Lcg(seed);
    let unit = |rng: &mut Lcg| {
        let q = [
            rng.range(-1.0, 1.0),
            rng.range(-1.0, 1.0),
            rng.range(-1.0, 1.0),
            rng.range(-1.0, 1.0),
        ];
        let norm = q.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-3);
        q.map(|x| x / norm)
    };
    let mut means = Vec::with_capacity(n);
    let mut log_scales = Vec::with_capacity(n);
    let mut rot_l = Vec::with_capacity(n);
    let mut rot_r = Vec::with_capacity(n);
    let mut opacities = Vec::with_capacity(n);
    let mut sh = Vec::with_capacity(n * 3);
    for _ in 0..n {
        means.push([
            rng.range(-4.0, 4.0),
            rng.range(-4.0, 4.0),
            rng.range(-2.0, 2.0),
            rng.range(0.0, duration),
        ]);
        log_scales.push([
            rng.range(-1.2, -0.2),
            rng.range(-1.2, -0.2),
            rng.range(-1.2, -0.2),
            rng.range(-0.5, 0.8),
        ]);
        rot_l.push(unit(&mut rng));
        rot_r.push(unit(&mut rng));
        opacities.push(rng.range(0.0, 3.0));
        // Degree 0: one DC coefficient per channel, layout [channel][coeff].
        for _ in 0..3 {
            sh.push(rng.range(-1.0, 1.0));
        }
    }
    GaussianCloud4D::new(means, log_scales, rot_l, rot_r, opacities, 0, sh).unwrap()
}

/// Draws the splats of `cloud` as seen at time `t`, returning RGB in `[0, 1]`.
fn render(cloud: &GaussianCloud4D, t: f32) -> Vec<[f32; 3]> {
    struct Splat {
        depth: f32,
        center: [f32; 2],
        conic: [f32; 3],
        radius: f32,
        opacity: f32,
        color: [f32; 3],
    }
    let identity = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    let mut splats = Vec::new();
    let k = 1; // degree 0 only in this scene
    for i in 0..cloud.len() {
        let s = cloud.log_scales()[i].map(f32::exp);
        let Some(cov4) = cov4_from_rot_scale(cloud.rot_left()[i], cloud.rot_right()[i], s) else {
            continue;
        };
        let Some(cond) = condition_on_time(cloud.means()[i], &cov4, t) else {
            continue;
        };
        let cam = [
            cond.mean[0] - EYE[0],
            cond.mean[1] - EYE[1],
            cond.mean[2] - EYE[2],
        ];
        let Some(j) = perspective_jacobian(cam, FOCAL, FOCAL) else {
            continue;
        };
        let cov2 = project_cov_ewa(
            &Sym3::new(
                cond.cov.get(0, 0),
                cond.cov.get(0, 1),
                cond.cov.get(0, 2),
                cond.cov.get(1, 1),
                cond.cov.get(1, 2),
                cond.cov.get(2, 2),
            ),
            &identity,
            &j,
            LOW_PASS,
        );
        let Some(inv) = cov2.inverse() else {
            continue;
        };
        let (l1, l2) = cov2.eigenvalues();
        let radius = 3.0 * l1.max(l2).max(0.0).sqrt();
        let opacity = cloud.opacities()[i];
        let opacity = 1.0 / (1.0 + (-opacity).exp()) * cond.weight;
        let sh = cloud.sh();
        let coeffs: Vec<[f32; 3]> = (0..k)
            .map(|c| {
                [
                    sh[i * 3 * k + c],
                    sh[i * 3 * k + k + c],
                    sh[i * 3 * k + 2 * k + c],
                ]
            })
            .collect();
        let dir = [cam[0], cam[1], cam[2]];
        let len = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2])
            .sqrt()
            .max(1e-6);
        let color =
            eval_color(0, &coeffs, [dir[0] / len, dir[1] / len, dir[2] / len]).unwrap_or([0.5; 3]);
        let u = FOCAL * cam[0] / cam[2] + W as f32 / 2.0;
        let v = FOCAL * cam[1] / cam[2] + H as f32 / 2.0;
        splats.push(Splat {
            depth: cam[2],
            center: [u, v],
            conic: [inv.xx, inv.xy, inv.yy],
            radius,
            opacity,
            color,
        });
    }
    splats.sort_by(|a, b| a.depth.total_cmp(&b.depth));

    let mut img = vec![[0.0f32; 3]; W * H];
    let mut transmittance = vec![1.0f32; W * H];
    for s in &splats {
        let x0 = (s.center[0] - s.radius).floor().max(0.0) as usize;
        let x1 = ((s.center[0] + s.radius).ceil().max(0.0) as usize).min(W - 1);
        let y0 = (s.center[1] - s.radius).floor().max(0.0) as usize;
        let y1 = ((s.center[1] + s.radius).ceil().max(0.0) as usize).min(H - 1);
        if x0 > x1 || y0 > y1 {
            continue;
        }
        for py in y0..=y1 {
            for px in x0..=x1 {
                let dx = px as f32 + 0.5 - s.center[0];
                let dy = py as f32 + 0.5 - s.center[1];
                let [a, b, c] = s.conic;
                let power = -0.5 * (a * dx * dx + 2.0 * b * dx * dy + c * dy * dy);
                if power > 0.0 {
                    continue;
                }
                let alpha = (s.opacity * power.exp()).min(0.99);
                if alpha < 1.0 / 255.0 {
                    continue;
                }
                let idx = py * W + px;
                let w = transmittance[idx] * alpha;
                for ch in 0..3 {
                    img[idx][ch] += w * s.color[ch];
                }
                transmittance[idx] *= 1.0 - alpha;
            }
        }
    }
    img
}

/// PSNR over RGB values in `[0, 1]`, capped at 100 dB for identical images.
fn psnr(a: &[[f32; 3]], b: &[[f32; 3]]) -> f64 {
    let mse: f64 = a
        .iter()
        .zip(b)
        .flat_map(|(p, q)| (0..3).map(move |c| (f64::from(p[c]) - f64::from(q[c])).powi(2)))
        .sum::<f64>()
        / (a.len() * 3) as f64;
    if mse == 0.0 {
        100.0
    } else {
        (10.0 * (1.0 / mse).log10()).min(100.0)
    }
}

/// Renders the decoded sequence at `t`, using the first slab that contains it.
/// Slabs overlap at boundaries, so using one slab avoids counting a splat twice.
fn render_sequence(bytes: &[u8], t: f32) -> Vec<[f32; 3]> {
    let seq = read_sequence(bytes).unwrap();
    let k = *seq
        .slabs_at(t)
        .first()
        .expect("time is inside the sequence");
    render(&seq.decode_slab(k).unwrap(), t)
}

#[test]
fn lossless_sequence_renders_like_the_source() {
    let cloud = scene(300, 7, 12.0);
    let bytes = write_sequence(&cloud, 2.0, 0.01, Quantization::Lossless).unwrap();
    // Negative control: the test must be able to fail. A render at a
    // different time shows different content, and must score far below the
    // threshold. The image must also not be blank.
    let at_t = render(&cloud, 3.7);
    assert!(at_t.iter().flatten().any(|&v| v > 0.01), "render is blank");
    let other = render(&cloud, 3.7 + 4.0);
    let control = psnr(&at_t, &other);
    eprintln!("negative control (t vs t+4): PSNR {control:.2} dB");
    assert!(
        control < 40.0,
        "control too similar to test the metric: {control:.2} dB"
    );
    for t in [1.0, 3.7, 6.2, 9.9] {
        let want = render(&cloud, t);
        let got = render_sequence(&bytes, t);
        let p = psnr(&want, &got);
        eprintln!("lossless t={t}: PSNR {p:.2} dB");
        assert!(p > 40.0, "t={t}: PSNR {p:.2} dB");
    }
}

#[test]
fn lossy_sequence_renders_close_to_the_source() {
    let cloud = scene(300, 7, 12.0);
    let quant = Quantization::Lossy {
        position_bits: 16,
        attribute_bits: 8,
    };
    let bytes = write_sequence(&cloud, 2.0, 0.01, quant).unwrap();
    for t in [1.0, 3.7, 6.2, 9.9] {
        let want = render(&cloud, t);
        let got = render_sequence(&bytes, t);
        let p = psnr(&want, &got);
        eprintln!("lossy 16/8 t={t}: PSNR {p:.2} dB");
        assert!(p > 30.0, "t={t}: PSNR {p:.2} dB");
    }
}
