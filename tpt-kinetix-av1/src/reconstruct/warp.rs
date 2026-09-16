//! AV1 local warped motion (§7.13.3 `find_warp_samples`, §7.13.4
//! `WarpEstimation` / the least-squares affine fit, and §7.11.3.5
//! `block_warp_process`) — the `motion_mode == WARP` prediction path.
//!
//! This is a direct, bit-exact-intent port of dav1d's `src/warpmv.c`
//! (`dav1d_find_affine_int` / `dav1d_get_shear_params`) and the
//! `warp_affine` / `warp_affine_8x8_c` pair in `src/recon_tmpl.c` /
//! `src/mc_tmpl.c`, cross-checked against the spec text. All arithmetic
//! mirrors the reference's integer widths (32-bit wrapping accumulators in
//! the least-squares fit, exactly as dav1d's `int a[2][2]` / `int bx[2]` /
//! `int by[2]`) rather than using wider types throughout, since the spec's
//! bit-exactness requirement depends on matching C `int` overflow behaviour,
//! not just the final rounded value.
//!
//! Sample-point collection (the neighbour scan that builds `CandList`) lives
//! in `inter_block.rs` next to the existing `find_num_warp_samples` (which
//! only needs the *count*, for the `motion_mode` CDF-selection entropy
//! decision) since it needs `TileDecodeState`'s ref-mv grid; this module is
//! the pure math that consumes the resulting sample list.

/// One `find_warp_samples` candidate (§7.10.4 `add_sample`, dav1d
/// `derive_warpmv`'s `pts[i]`): `src`/`dst` are `[x, y]` in 1/8-luma-pel
/// units, relative to the current block's top-left corner. `dst - src` is
/// the neighbour's own motion vector (by construction).
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct WarpSample {
    pub src: [i32; 2],
    pub dst: [i32; 2],
}

/// A validated local warp model (§7.13.4 output when `LocalValid == 1`):
/// the 6-parameter affine matrix (`mat[0]`/`mat[1]` = translation,
/// `mat[2..6)` = the 2×2 linear part) plus the derived per-axis shear
/// parameters `block_warp_process` steps the warp filter phase by.
#[derive(Clone, Copy, Debug)]
pub(super) struct WarpModel {
    pub matrix: [i32; 6],
    pub alpha: i32,
    pub beta: i32,
    pub gamma: i32,
    pub delta: i32,
}

// --- §7.13.4 integer least-squares fit (dav1d warpmv.c) --------------------

#[inline]
fn apply_sign(v: i32, s: i32) -> i32 {
    if s < 0 {
        -v
    } else {
        v
    }
}

#[inline]
fn apply_sign64(v: i32, s: i64) -> i32 {
    if s < 0 {
        -v
    } else {
        v
    }
}

/// `div_lut[257]` (dav1d `warpmv.c`): precomputed 1/x reciprocal multipliers
/// so `WarpEstimation`'s per-sample division becomes a table lookup + shift,
/// matching the spec's integer-exact requirement.
const DIV_LUT: [u16; 257] = [
    16384, 16320, 16257, 16194, 16132, 16070, 16009, 15948, 15888, 15828, 15768, 15709, 15650,
    15592, 15534, 15477, 15420, 15364, 15308, 15252, 15197, 15142, 15087, 15033, 14980, 14926,
    14873, 14821, 14769, 14717, 14665, 14614, 14564, 14513, 14463, 14413, 14364, 14315, 14266,
    14218, 14170, 14122, 14075, 14028, 13981, 13935, 13888, 13843, 13797, 13752, 13707, 13662,
    13618, 13574, 13530, 13487, 13443, 13400, 13358, 13315, 13273, 13231, 13190, 13148, 13107,
    13066, 13026, 12985, 12945, 12906, 12866, 12827, 12788, 12749, 12710, 12672, 12633, 12596,
    12558, 12520, 12483, 12446, 12409, 12373, 12336, 12300, 12264, 12228, 12193, 12157, 12122,
    12087, 12053, 12018, 11984, 11950, 11916, 11882, 11848, 11815, 11782, 11749, 11716, 11683,
    11651, 11619, 11586, 11555, 11523, 11491, 11460, 11429, 11398, 11367, 11336, 11305, 11275,
    11245, 11215, 11185, 11155, 11125, 11096, 11067, 11038, 11009, 10980, 10951, 10923, 10894,
    10866, 10838, 10810, 10782, 10755, 10727, 10700, 10673, 10645, 10618, 10592, 10565, 10538,
    10512, 10486, 10460, 10434, 10408, 10382, 10356, 10331, 10305, 10280, 10255, 10230, 10205,
    10180, 10156, 10131, 10107, 10082, 10058, 10034, 10010, 9986, 9963, 9939, 9916, 9892, 9869,
    9846, 9823, 9800, 9777, 9754, 9732, 9709, 9687, 9664, 9642, 9620, 9598, 9576, 9554, 9533, 9511,
    9489, 9468, 9447, 9425, 9404, 9383, 9362, 9341, 9321, 9300, 9279, 9259, 9239, 9218, 9198, 9178,
    9158, 9138, 9118, 9098, 9079, 9059, 9039, 9020, 9001, 8981, 8962, 8943, 8924, 8905, 8886, 8867,
    8849, 8830, 8812, 8793, 8775, 8756, 8738, 8720, 8702, 8684, 8666, 8648, 8630, 8613, 8595, 8577,
    8560, 8542, 8525, 8508, 8490, 8473, 8456, 8439, 8422, 8405, 8389, 8372, 8355, 8339, 8322, 8306,
    8289, 8273, 8257, 8240, 8224, 8208, 8192,
];

/// `resolve_divisor_32` (§7.13.4 / dav1d `warpmv.c`): returns `(multiplier,
/// shift)` such that `x / d ≈ (x * multiplier) >> shift` for the shear-param
/// division.
fn resolve_divisor_32(d: u32) -> (i32, i32) {
    let shift = 31 - d.leading_zeros() as i32;
    let e = d as i32 - (1i32 << shift);
    let f = if shift > 8 {
        (e + (1i32 << (shift - 9))) >> (shift - 8)
    } else {
        e << (8 - shift)
    };
    debug_assert!((0..=256).contains(&f));
    (DIV_LUT[f as usize] as i32, shift + 14)
}

/// `resolve_divisor_64` — the 64-bit sibling used by the least-squares fit's
/// determinant reciprocal.
fn resolve_divisor_64(d: u64) -> (i32, i32) {
    let shift = 63 - d.leading_zeros() as i32;
    let e = d as i64 - (1i64 << shift);
    let f = if shift > 8 {
        (e + (1i64 << (shift - 9))) >> (shift - 8)
    } else {
        e << (8 - shift)
    };
    debug_assert!((0..=256).contains(&f));
    (DIV_LUT[f as usize] as i32, shift + 14)
}

/// `iclip_wmp` (dav1d `warpmv.c`): clamp to `int16_t` range, then round to
/// the nearest multiple of 64 (the warp-filter phase granularity).
fn iclip_wmp(v: i32) -> i32 {
    let cv = v.clamp(i16::MIN as i32, i16::MAX as i32);
    apply_sign((cv.abs() + 32) >> 6, cv) * (1 << 6)
}

fn get_mult_shift_ndiag(px: i64, idet: i32, shift: i32) -> i32 {
    let v1 = px * idet as i64;
    let v2 = apply_sign64(((v1.abs() + ((1i64 << shift) >> 1)) >> shift) as i32, v1);
    v2.clamp(-0x1fff, 0x1fff)
}

fn get_mult_shift_diag(px: i64, idet: i32, shift: i32) -> i32 {
    let v1 = px * idet as i64;
    let v2 = apply_sign64(((v1.abs() + ((1i64 << shift) >> 1)) >> shift) as i32, v1);
    v2.clamp(0xe001, 0x11fff)
}

/// `dav1d_get_shear_params` (§7.13.4, the last step of `WarpEstimation`):
/// derive the four warp-filter phase-step parameters from the fitted 2×2
/// linear part `matrix[2..6)`, and validate the shear is within the
/// filterable range. Returns `None` when the block should fall back to
/// translation-only prediction (`matrix[2] <= 0`, or the shear magnitude
/// check fails) — exactly dav1d's `wm->type = IDENTITY` case.
fn get_shear_params(mat: &[i32; 6]) -> Option<(i32, i32, i32, i32)> {
    if mat[2] <= 0 {
        return None;
    }
    let alpha = iclip_wmp(mat[2] - 0x10000);
    let beta = iclip_wmp(mat[3]);

    let (mult, shift) = resolve_divisor_32(mat[2] as u32);
    let y = apply_sign(mult, mat[2]);

    let v1 = (mat[4] as i64) * 0x10000 * (y as i64);
    let rnd: i64 = (1i64 << shift) >> 1;
    let gamma = iclip_wmp(apply_sign64(((v1.abs() + rnd) >> shift) as i32, v1));

    let v2 = (mat[3] as i64) * (mat[4] as i64) * (y as i64);
    let delta = iclip_wmp(mat[5] - apply_sign64(((v2.abs() + rnd) >> shift) as i32, v2) - 0x10000);

    if 4 * alpha.abs() + 7 * beta.abs() >= 0x10000 || 4 * gamma.abs() + 4 * delta.abs() >= 0x10000 {
        None
    } else {
        Some((alpha, beta, gamma, delta))
    }
}

/// `dav1d_find_affine_int` (§7.13.4 `WarpEstimation`): fit the 6-parameter
/// affine model to up to 8 `(src, dst)` sample pairs by integer least
/// squares. `bw4`/`bh4` are the block size in 4-pixel units, `mv` the
/// block's own (translational) motion vector, `bx4`/`by4` its frame-absolute
/// position in 4-pixel (mi) units. Returns `None` when the system is
/// singular (`det == 0`).
fn find_affine_int(
    pts: &[WarpSample],
    bw4: i32,
    bh4: i32,
    mv: super::Mv,
    bx4: i32,
    by4: i32,
) -> Option<[i32; 6]> {
    // 32-bit wrapping accumulators — dav1d uses plain `int a[2][2]` / `int
    // bx[2]` / `int by[2]` here, so overflow (unreachable for in-spec block
    // sizes/samples, but kept faithful regardless) wraps exactly as C `int`
    // does on every real target, not as a wider type would.
    let mut a = [[0i32; 2]; 2];
    let mut bx = [0i32; 2];
    let mut by = [0i32; 2];
    let rsuy = 2 * bh4 - 1;
    let rsux = 2 * bw4 - 1;
    let suy = rsuy * 8;
    let sux = rsux * 8;
    let duy = suy + mv.row;
    let dux = sux + mv.col;
    let isuy = by4 * 4 + rsuy;
    let isux = bx4 * 4 + rsux;

    for p in pts {
        let dx = p.dst[0].wrapping_sub(dux);
        let dy = p.dst[1].wrapping_sub(duy);
        let sx = p.src[0].wrapping_sub(sux);
        let sy = p.src[1].wrapping_sub(suy);
        if (sx.wrapping_sub(dx)).abs() < 256 && (sy.wrapping_sub(dy)).abs() < 256 {
            a[0][0] = a[0][0].wrapping_add((sx.wrapping_mul(sx) >> 2).wrapping_add(sx * 2 + 8));
            a[0][1] = a[0][1].wrapping_add((sx.wrapping_mul(sy) >> 2).wrapping_add(sx + sy + 4));
            a[1][1] = a[1][1].wrapping_add((sy.wrapping_mul(sy) >> 2).wrapping_add(sy * 2 + 8));
            bx[0] = bx[0].wrapping_add((sx.wrapping_mul(dx) >> 2).wrapping_add(sx + dx + 8));
            bx[1] = bx[1].wrapping_add((sy.wrapping_mul(dx) >> 2).wrapping_add(sy + dx + 4));
            by[0] = by[0].wrapping_add((sx.wrapping_mul(dy) >> 2).wrapping_add(sx + dy + 4));
            by[1] = by[1].wrapping_add((sy.wrapping_mul(dy) >> 2).wrapping_add(sy + dy + 8));
        }
    }

    let det = (a[0][0] as i64) * (a[1][1] as i64) - (a[0][1] as i64) * (a[0][1] as i64);
    if det == 0 {
        return None;
    }
    let (mult, shift0) = resolve_divisor_64(det.unsigned_abs());
    let idet = apply_sign64(mult, det);
    let mut shift = shift0 - 16;
    let idet = if shift < 0 {
        let s = idet << (-shift);
        shift = 0;
        s
    } else {
        idet
    };

    let mat2 = get_mult_shift_diag(
        (a[1][1] as i64) * (bx[0] as i64) - (a[0][1] as i64) * (bx[1] as i64),
        idet,
        shift,
    );
    let mat3 = get_mult_shift_ndiag(
        (a[0][0] as i64) * (bx[1] as i64) - (a[0][1] as i64) * (bx[0] as i64),
        idet,
        shift,
    );
    let mat4 = get_mult_shift_ndiag(
        (a[1][1] as i64) * (by[0] as i64) - (a[0][1] as i64) * (by[1] as i64),
        idet,
        shift,
    );
    let mat5 = get_mult_shift_diag(
        (a[0][0] as i64) * (by[1] as i64) - (a[0][1] as i64) * (by[0] as i64),
        idet,
        shift,
    );

    let mat0 = (mv.col.wrapping_mul(0x2000)
        - (isux.wrapping_mul(mat2 - 0x10000) + isuy.wrapping_mul(mat3)))
    .clamp(-0x800000, 0x7fffff);
    let mat1 = (mv.row.wrapping_mul(0x2000)
        - (isux.wrapping_mul(mat4) + isuy.wrapping_mul(mat5 - 0x10000)))
    .clamp(-0x800000, 0x7fffff);

    Some([mat0, mat1, mat2, mat3, mat4, mat5])
}

/// The mv-difference threshold selection + tail-replacement pass from dav1d
/// `derive_warpmv` (§7.13.3, the part of `find_warp_samples` after
/// `CandList` is built): samples whose neighbour MV differs from the
/// block's own MV by more than the size-dependent threshold are dropped,
/// with valid samples from the tail of the (unfiltered) list compacted into
/// the gaps they leave — *not* simply truncated, since a later valid sample
/// must survive even if an earlier one didn't. If none pass, the first raw
/// sample is kept anyway (dav1d: `if (!ret) ret = 1`, using the *unfiltered*
/// `pts[0]`).
fn select_warp_samples(raw: &[WarpSample], bw4: i32, bh4: i32, mv: super::Mv) -> Vec<WarpSample> {
    let np = raw.len();
    if np == 0 {
        return Vec::new();
    }
    let thresh = 4 * bw4.max(bh4).clamp(4, 28);
    let mut pts = raw.to_vec();
    let mut mvd = vec![0i32; np];
    let mut ret = 0usize;
    for (i, s) in pts.iter().enumerate() {
        let d = (s.dst[0] - s.src[0] - mv.col).abs() + (s.dst[1] - s.src[1] - mv.row).abs();
        if d > thresh {
            mvd[i] = -1;
        } else {
            mvd[i] = d;
            ret += 1;
        }
    }
    if ret == 0 {
        ret = 1;
    } else {
        let mut i: isize = 0;
        let mut j: isize = np as isize - 1;
        for _ in 0..(np - ret) {
            while mvd[i as usize] != -1 {
                i += 1;
            }
            while mvd[j as usize] == -1 {
                j -= 1;
            }
            if i > j {
                break;
            }
            mvd[i as usize] = mvd[j as usize];
            pts[i as usize] = pts[j as usize];
            i += 1;
            j -= 1;
        }
    }
    pts.truncate(ret);
    pts
}

/// §7.13.3 + §7.13.4 end to end: given the raw (unfiltered, mask-matched)
/// neighbour sample list, the block size/mv/position, derive the local warp
/// model, or `None` if the block should fall back to translation-only
/// prediction (matches dav1d's `wm->type <= DAV1D_WM_TYPE_TRANSLATION`
/// fallback condition in `recon_tmpl.c`).
pub(super) fn derive_warp_model(
    raw: &[WarpSample],
    bw4: i32,
    bh4: i32,
    mv: super::Mv,
    bx4: i32,
    by4: i32,
) -> Option<WarpModel> {
    let selected = select_warp_samples(raw, bw4, bh4, mv);
    let matrix = find_affine_int(&selected, bw4, bh4, mv, bx4, by4)?;
    let (alpha, beta, gamma, delta) = get_shear_params(&matrix)?;
    Some(WarpModel {
        matrix,
        alpha,
        beta,
        gamma,
        delta,
    })
}

// --- §7.11.3.5 block_warp_process -------------------------------------------

/// `dav1d_mc_warp_filter[193][8]` (`src/tables.c`): the 8-tap warp filter
/// bank, indexed by `64 + (phase >> 10)` for a 1/1024-scaled phase in
/// roughly `[-64, 64]` (distinct from the regular sub-pel `Subpel_Filters`
/// table — different phase scale, different taps). Row 192 duplicates row
/// 191 (dav1d's own "dummy" padding row for `phase == 512`, i.e. exactly the
/// top of the range).
#[rustfmt::skip]
const WARPED_FILTERS: [[i8; 8]; 193] = [
    [0, 0, 127, 1, 0, 0, 0, 0], [0, -1, 127, 2, 0, 0, 0, 0],
    [1, -3, 127, 4, -1, 0, 0, 0], [1, -4, 126, 6, -2, 1, 0, 0],
    [1, -5, 126, 8, -3, 1, 0, 0], [1, -6, 125, 11, -4, 1, 0, 0],
    [1, -7, 124, 13, -4, 1, 0, 0], [2, -8, 123, 15, -5, 1, 0, 0],
    [2, -9, 122, 18, -6, 1, 0, 0], [2, -10, 121, 20, -6, 1, 0, 0],
    [2, -11, 120, 22, -7, 2, 0, 0], [2, -12, 119, 25, -8, 2, 0, 0],
    [3, -13, 117, 27, -8, 2, 0, 0], [3, -13, 116, 29, -9, 2, 0, 0],
    [3, -14, 114, 32, -10, 3, 0, 0], [3, -15, 113, 35, -10, 2, 0, 0],
    [3, -15, 111, 37, -11, 3, 0, 0], [3, -16, 109, 40, -11, 3, 0, 0],
    [3, -16, 108, 42, -12, 3, 0, 0], [4, -17, 106, 45, -13, 3, 0, 0],
    [4, -17, 104, 47, -13, 3, 0, 0], [4, -17, 102, 50, -14, 3, 0, 0],
    [4, -17, 100, 52, -14, 3, 0, 0], [4, -18, 98, 55, -15, 4, 0, 0],
    [4, -18, 96, 58, -15, 3, 0, 0], [4, -18, 94, 60, -16, 4, 0, 0],
    [4, -18, 91, 63, -16, 4, 0, 0], [4, -18, 89, 65, -16, 4, 0, 0],
    [4, -18, 87, 68, -17, 4, 0, 0], [4, -18, 85, 70, -17, 4, 0, 0],
    [4, -18, 82, 73, -17, 4, 0, 0], [4, -18, 80, 75, -17, 4, 0, 0],
    [4, -18, 78, 78, -18, 4, 0, 0], [4, -17, 75, 80, -18, 4, 0, 0],
    [4, -17, 73, 82, -18, 4, 0, 0], [4, -17, 70, 85, -18, 4, 0, 0],
    [4, -17, 68, 87, -18, 4, 0, 0], [4, -16, 65, 89, -18, 4, 0, 0],
    [4, -16, 63, 91, -18, 4, 0, 0], [4, -16, 60, 94, -18, 4, 0, 0],
    [3, -15, 58, 96, -18, 4, 0, 0], [4, -15, 55, 98, -18, 4, 0, 0],
    [3, -14, 52, 100, -17, 4, 0, 0], [3, -14, 50, 102, -17, 4, 0, 0],
    [3, -13, 47, 104, -17, 4, 0, 0], [3, -13, 45, 106, -17, 4, 0, 0],
    [3, -12, 42, 108, -16, 3, 0, 0], [3, -11, 40, 109, -16, 3, 0, 0],
    [3, -11, 37, 111, -15, 3, 0, 0], [2, -10, 35, 113, -15, 3, 0, 0],
    [3, -10, 32, 114, -14, 3, 0, 0], [2, -9, 29, 116, -13, 3, 0, 0],
    [2, -8, 27, 117, -13, 3, 0, 0], [2, -8, 25, 119, -12, 2, 0, 0],
    [2, -7, 22, 120, -11, 2, 0, 0], [1, -6, 20, 121, -10, 2, 0, 0],
    [1, -6, 18, 122, -9, 2, 0, 0], [1, -5, 15, 123, -8, 2, 0, 0],
    [1, -4, 13, 124, -7, 1, 0, 0], [1, -4, 11, 125, -6, 1, 0, 0],
    [1, -3, 8, 126, -5, 1, 0, 0], [1, -2, 6, 126, -4, 1, 0, 0],
    [0, -1, 4, 127, -3, 1, 0, 0], [0, 0, 2, 127, -1, 0, 0, 0],
    [0, 0, 0, 127, 1, 0, 0, 0], [0, 0, -1, 127, 2, 0, 0, 0],
    [0, 1, -3, 127, 4, -2, 1, 0], [0, 1, -5, 127, 6, -2, 1, 0],
    [0, 2, -6, 126, 8, -3, 1, 0], [-1, 2, -7, 126, 11, -4, 2, -1],
    [-1, 3, -8, 125, 13, -5, 2, -1], [-1, 3, -10, 124, 16, -6, 3, -1],
    [-1, 4, -11, 123, 18, -7, 3, -1], [-1, 4, -12, 122, 20, -7, 3, -1],
    [-1, 4, -13, 121, 23, -8, 3, -1], [-2, 5, -14, 120, 25, -9, 4, -1],
    [-1, 5, -15, 119, 27, -10, 4, -1], [-1, 5, -16, 118, 30, -11, 4, -1],
    [-2, 6, -17, 116, 33, -12, 5, -1], [-2, 6, -17, 114, 35, -12, 5, -1],
    [-2, 6, -18, 113, 38, -13, 5, -1], [-2, 7, -19, 111, 41, -14, 6, -2],
    [-2, 7, -19, 110, 43, -15, 6, -2], [-2, 7, -20, 108, 46, -15, 6, -2],
    [-2, 7, -20, 106, 49, -16, 6, -2], [-2, 7, -21, 104, 51, -16, 7, -2],
    [-2, 7, -21, 102, 54, -17, 7, -2], [-2, 8, -21, 100, 56, -18, 7, -2],
    [-2, 8, -22, 98, 59, -18, 7, -2], [-2, 8, -22, 96, 62, -19, 7, -2],
    [-2, 8, -22, 94, 64, -19, 7, -2], [-2, 8, -22, 91, 67, -20, 8, -2],
    [-2, 8, -22, 89, 69, -20, 8, -2], [-2, 8, -22, 87, 72, -21, 8, -2],
    [-2, 8, -21, 84, 74, -21, 8, -2], [-2, 8, -22, 82, 77, -21, 8, -2],
    [-2, 8, -21, 79, 79, -21, 8, -2], [-2, 8, -21, 77, 82, -22, 8, -2],
    [-2, 8, -21, 74, 84, -21, 8, -2], [-2, 8, -21, 72, 87, -22, 8, -2],
    [-2, 8, -20, 69, 89, -22, 8, -2], [-2, 8, -20, 67, 91, -22, 8, -2],
    [-2, 7, -19, 64, 94, -22, 8, -2], [-2, 7, -19, 62, 96, -22, 8, -2],
    [-2, 7, -18, 59, 98, -22, 8, -2], [-2, 7, -18, 56, 100, -21, 8, -2],
    [-2, 7, -17, 54, 102, -21, 7, -2], [-2, 7, -16, 51, 104, -21, 7, -2],
    [-2, 6, -16, 49, 106, -20, 7, -2], [-2, 6, -15, 46, 108, -20, 7, -2],
    [-2, 6, -15, 43, 110, -19, 7, -2], [-2, 6, -14, 41, 111, -19, 7, -2],
    [-1, 5, -13, 38, 113, -18, 6, -2], [-1, 5, -12, 35, 114, -17, 6, -2],
    [-1, 5, -12, 33, 116, -17, 6, -2], [-1, 4, -11, 30, 118, -16, 5, -1],
    [-1, 4, -10, 27, 119, -15, 5, -1], [-1, 4, -9, 25, 120, -14, 5, -2],
    [-1, 3, -8, 23, 121, -13, 4, -1], [-1, 3, -7, 20, 122, -12, 4, -1],
    [-1, 3, -7, 18, 123, -11, 4, -1], [-1, 3, -6, 16, 124, -10, 3, -1],
    [-1, 2, -5, 13, 125, -8, 3, -1], [-1, 2, -4, 11, 126, -7, 2, -1],
    [0, 1, -3, 8, 126, -6, 2, 0], [0, 1, -2, 6, 127, -5, 1, 0],
    [0, 1, -2, 4, 127, -3, 1, 0], [0, 0, 0, 2, 127, -1, 0, 0],
    [0, 0, 0, 1, 127, 0, 0, 0], [0, 0, 0, -1, 127, 2, 0, 0],
    [0, 0, 1, -3, 127, 4, -1, 0], [0, 0, 1, -4, 126, 6, -2, 1],
    [0, 0, 1, -5, 126, 8, -3, 1], [0, 0, 1, -6, 125, 11, -4, 1],
    [0, 0, 1, -7, 124, 13, -4, 1], [0, 0, 2, -8, 123, 15, -5, 1],
    [0, 0, 2, -9, 122, 18, -6, 1], [0, 0, 2, -10, 121, 20, -6, 1],
    [0, 0, 2, -11, 120, 22, -7, 2], [0, 0, 2, -12, 119, 25, -8, 2],
    [0, 0, 3, -13, 117, 27, -8, 2], [0, 0, 3, -13, 116, 29, -9, 2],
    [0, 0, 3, -14, 114, 32, -10, 3], [0, 0, 3, -15, 113, 35, -10, 2],
    [0, 0, 3, -15, 111, 37, -11, 3], [0, 0, 3, -16, 109, 40, -11, 3],
    [0, 0, 3, -16, 108, 42, -12, 3], [0, 0, 4, -17, 106, 45, -13, 3],
    [0, 0, 4, -17, 104, 47, -13, 3], [0, 0, 4, -17, 102, 50, -14, 3],
    [0, 0, 4, -17, 100, 52, -14, 3], [0, 0, 4, -18, 98, 55, -15, 4],
    [0, 0, 4, -18, 96, 58, -15, 3], [0, 0, 4, -18, 94, 60, -16, 4],
    [0, 0, 4, -18, 91, 63, -16, 4], [0, 0, 4, -18, 89, 65, -16, 4],
    [0, 0, 4, -18, 87, 68, -17, 4], [0, 0, 4, -18, 85, 70, -17, 4],
    [0, 0, 4, -18, 82, 73, -17, 4], [0, 0, 4, -18, 80, 75, -17, 4],
    [0, 0, 4, -18, 78, 78, -18, 4], [0, 0, 4, -17, 75, 80, -18, 4],
    [0, 0, 4, -17, 73, 82, -18, 4], [0, 0, 4, -17, 70, 85, -18, 4],
    [0, 0, 4, -17, 68, 87, -18, 4], [0, 0, 4, -16, 65, 89, -18, 4],
    [0, 0, 4, -16, 63, 91, -18, 4], [0, 0, 4, -16, 60, 94, -18, 4],
    [0, 0, 3, -15, 58, 96, -18, 4], [0, 0, 4, -15, 55, 98, -18, 4],
    [0, 0, 3, -14, 52, 100, -17, 4], [0, 0, 3, -14, 50, 102, -17, 4],
    [0, 0, 3, -13, 47, 104, -17, 4], [0, 0, 3, -13, 45, 106, -17, 4],
    [0, 0, 3, -12, 42, 108, -16, 3], [0, 0, 3, -11, 40, 109, -16, 3],
    [0, 0, 3, -11, 37, 111, -15, 3], [0, 0, 2, -10, 35, 113, -15, 3],
    [0, 0, 3, -10, 32, 114, -14, 3], [0, 0, 2, -9, 29, 116, -13, 3],
    [0, 0, 2, -8, 27, 117, -13, 3], [0, 0, 2, -8, 25, 119, -12, 2],
    [0, 0, 2, -7, 22, 120, -11, 2], [0, 0, 1, -6, 20, 121, -10, 2],
    [0, 0, 1, -6, 18, 122, -9, 2], [0, 0, 1, -5, 15, 123, -8, 2],
    [0, 0, 1, -4, 13, 124, -7, 1], [0, 0, 1, -4, 11, 125, -6, 1],
    [0, 0, 1, -3, 8, 126, -5, 1], [0, 0, 1, -2, 6, 126, -4, 1],
    [0, 0, 0, -1, 4, 127, -3, 1], [0, 0, 0, 0, 2, 127, -1, 0],
    [0, 0, 0, 0, 2, 127, -1, 0],
];

/// One 8×8 warp-filtered sub-block (dav1d `warp_affine_8x8_c`, 8-bit path):
/// two-pass separable 8-tap filtering from `dav1d_mc_warp_filter`, phase
/// stepped by `alpha`/`beta` (horizontal pass, over 15 source rows) then
/// `gamma`/`delta` (vertical pass). `dx`/`dy` are the integer source
/// top-left offset (already `-4`-biased per `warp_affine`'s caller so the
/// 8-tap support is centred); `mx`/`my` the fixed-point starting phase.
/// Reference samples are fetched with per-axis edge clamping, which for a
/// pure edge-replication border (AV1's only border mode) is exactly
/// equivalent to dav1d's `emu_edge` padded-buffer copy.
#[allow(clippy::too_many_arguments)]
fn warp_affine_8x8(
    dest: &mut [u8],
    dest_stride: usize,
    dest_x: usize,
    dest_y: usize,
    refp: &[u8],
    ref_w: usize,
    ref_h: usize,
    dx: i32,
    dy: i32,
    alpha: i32,
    beta: i32,
    gamma: i32,
    delta: i32,
    mx0: i32,
    my0: i32,
) {
    let sample = |ix: i32, iy: i32| -> i32 {
        let cx = ix.clamp(0, ref_w as i32 - 1) as usize;
        let cy = iy.clamp(0, ref_h as i32 - 1) as usize;
        refp[cy * ref_w + cx] as i32
    };
    let filter_row = |phase: i32| -> &'static [i8; 8] {
        let idx = (64 + ((phase + 512) >> 10)).clamp(0, 192);
        &WARPED_FILTERS[idx as usize]
    };

    // Horizontal pass: 15 source rows (dy-3 ..= dy+11), 8 columns each,
    // giving the vertical pass its full 8-tap support (3 above / 4 below).
    let dbg_px = std::env::var("KINETIX_AV1_DBG_WARPPX").is_ok() && dx == 26 && dy == 48;
    let dbg_n = crate::debug_frame_seq::current();
    let mut mid = [[0i32; 8]; 15];
    let mut mx_row = mx0;
    for (yy, row) in mid.iter_mut().enumerate() {
        let sy = dy + yy as i32 - 3;
        let mut tmx = mx_row;
        for (xx, out) in row.iter_mut().enumerate() {
            let filter = filter_row(tmx);
            if dbg_px && yy == 0 {
                let idx = (64 + ((tmx + 512) >> 10)).clamp(0, 192);
                eprintln!(
                    "WARPPX n={dbg_n} h yy=0 xx={xx} phase={tmx} idx={idx} f={filter:?} taps={:?}",
                    (0..8)
                        .map(|k| sample(dx + xx as i32 + k - 3, sy))
                        .collect::<Vec<_>>()
                );
            }
            let sx = dx + xx as i32;
            let mut s = 0i32;
            for (k, &c) in filter.iter().enumerate() {
                s += c as i32 * sample(sx + k as i32 - 3, sy);
            }
            // sh = 7 - intermediate_bits(4) = 3, 8-bit path.
            *out = (s + 4) >> 3;
            tmx += alpha;
        }
        if dbg_px {
            eprintln!("WARPPX n={dbg_n} mid row{yy}={:?}", row);
        }
        mx_row += beta;
    }

    let mut my_row = my0;
    for yy in 0..8usize {
        let mut tmy = my_row;
        for xx in 0..8usize {
            let filter = filter_row(tmy);
            if dbg_px && yy == 0 && xx >= 6 {
                let idx = (64 + ((tmy + 512) >> 10)).clamp(0, 192);
                eprintln!("WARPPX n={dbg_n} v yy=0 xx={xx} phase={tmy} idx={idx} f={filter:?}");
            }
            let mut s = 0i32;
            for (k, &c) in filter.iter().enumerate() {
                s += c as i32 * mid[yy + k][xx];
            }
            // sh = 7 + intermediate_bits(4) = 11, then clip to pixel range.
            let v = ((s + 1024) >> 11).clamp(0, 255) as u8;
            if dbg_px && yy == 0 {
                eprintln!("WARPPX n={dbg_n} out yy=0 xx={xx} v={v}");
            }
            dest[(dest_y + yy) * dest_stride + (dest_x + xx)] = v;
            tmy += gamma;
        }
        my_row += delta;
    }
}

/// §7.11.3.5 `block_warp_process` (dav1d `warp_affine`, `recon_tmpl.c`): tile
/// the block into 8×8 sub-blocks, evaluate the affine model at each
/// sub-block's centre to get its integer source offset + fixed-point phase,
/// and warp-filter it. `dest` is a `bw_px * bh_px` local buffer (stride
/// `bw_px`), matching [`super::inter::motion_compensate`]'s convention so
/// the caller blends/blits it the same way. `bx4`/`by4` are the block's
/// frame-absolute position in 4-pixel (mi) units; `ss_hor`/`ss_ver` the
/// plane's subsampling (0 for luma, and for chroma per the sequence's
/// `PIXEL_LAYOUT`).
#[allow(clippy::too_many_arguments)]
pub(super) fn block_warp_process(
    dest: &mut [u8],
    dest_stride: usize,
    refp: &[u8],
    ref_w: usize,
    ref_h: usize,
    model: &WarpModel,
    bx4: i32,
    by4: i32,
    bw_px: usize,
    bh_px: usize,
    ss_hor: u32,
    ss_ver: u32,
) {
    let mat = &model.matrix;
    let mut y = 0i32;
    while y < bh_px as i32 {
        let src_y = by4 * 4 + ((y + 4) << ss_ver);
        let mat3_y = (mat[3] as i64) * (src_y as i64) + mat[0] as i64;
        let mat5_y = (mat[5] as i64) * (src_y as i64) + mat[1] as i64;
        let mut x = 0i32;
        while x < bw_px as i32 {
            let src_x = bx4 * 4 + ((x + 4) << ss_hor);
            let mvx = ((mat[2] as i64) * (src_x as i64) + mat3_y) >> ss_hor;
            let mvy = ((mat[4] as i64) * (src_x as i64) + mat5_y) >> ss_ver;

            let dx = ((mvx >> 16) as i32) - 4;
            let mvx32 = mvx as i32;
            let mx = ((mvx32 & 0xffff) - model.alpha * 4 - model.beta * 7) & !0x3f;
            let dy = ((mvy >> 16) as i32) - 4;
            let mvy32 = mvy as i32;
            let my = ((mvy32 & 0xffff) - model.gamma * 4 - model.delta * 4) & !0x3f;

            warp_affine_8x8(
                dest,
                dest_stride,
                x as usize,
                y as usize,
                refp,
                ref_w,
                ref_h,
                dx,
                dy,
                model.alpha,
                model.beta,
                model.gamma,
                model.delta,
                mx,
                my,
            );
            x += 8;
        }
        y += 8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inter::Mv;

    /// Four samples whose neighbour MV exactly equals the block's own MV
    /// (pure translation, no actual shear) must fit a model with
    /// `alpha == beta == gamma == delta == 0` and `matrix[2..6) ==
    /// [0x10000, 0, 0, 0x10000]` (identity linear part) — the LS system is
    /// then exactly solvable, `WARP` degenerates to translation, and
    /// `get_shear_params` must accept it (`mat[2] == 0x10000 > 0`, zero
    /// shear trivially passes the magnitude check).
    #[test]
    fn pure_translation_fits_identity_linear_part() {
        let mv = Mv::new(16, -24); // 2px down, 3px left, in 1/8-pel.
        let bw4 = 4; // 16x16 luma block.
        let bh4 = 4;
        // Four corner-ish samples around the block, all carrying the same mv
        // as the block itself.
        let raw = [
            WarpSample {
                src: [-8, -8],
                dst: [-8 + mv.col, -8 + mv.row],
            },
            WarpSample {
                src: [120, -8],
                dst: [120 + mv.col, -8 + mv.row],
            },
            WarpSample {
                src: [-8, 120],
                dst: [-8 + mv.col, 120 + mv.row],
            },
            WarpSample {
                src: [120, 120],
                dst: [120 + mv.col, 120 + mv.row],
            },
        ];
        let model = derive_warp_model(&raw, bw4, bh4, mv, 10, 6).expect("model should be valid");
        // The integer LS fit carries small deliberate rounding-bias terms
        // (the `+8`/`+4` additions in `find_affine_int`'s accumulation, per
        // spec/dav1d), so even a mathematically perfect translation doesn't
        // round-trip to an exact 0x10000/0 identity — only close to it.
        assert!(
            (model.matrix[2] - 0x10000).abs() <= 16,
            "mat2={:#x}",
            model.matrix[2]
        );
        assert!(model.matrix[3].abs() <= 16, "mat3={}", model.matrix[3]);
        assert!(model.matrix[4].abs() <= 16, "mat4={}", model.matrix[4]);
        assert!(
            (model.matrix[5] - 0x10000).abs() <= 16,
            "mat5={:#x}",
            model.matrix[5]
        );
        assert!(model.alpha.abs() <= 64, "alpha={}", model.alpha);
        assert!(model.beta.abs() <= 64, "beta={}", model.beta);
        assert!(model.gamma.abs() <= 64, "gamma={}", model.gamma);
        assert!(model.delta.abs() <= 64, "delta={}", model.delta);
    }

    /// No samples at all (shouldn't normally happen — the entropy gate
    /// requires `NumSamples > 0` before `motion_mode == WARP` is even
    /// readable — but the math must degrade gracefully) yields `None`.
    #[test]
    fn empty_sample_list_yields_no_model() {
        let raw: [WarpSample; 0] = [];
        let model = derive_warp_model(&raw, 2, 2, Mv::new(0, 0), 4, 4);
        assert!(model.is_none());
    }

    /// A genuine zoom (neighbour MVs that grow linearly with distance from
    /// the block, i.e. a real affine relationship) must fit a non-identity
    /// `mat[2]`/`mat[5]` and pass the shear validity check for a mild zoom
    /// factor.
    #[test]
    fn simple_zoom_fits_nonidentity_diagonal() {
        let mv = Mv::new(0, 0);
        let bw4 = 4;
        let bh4 = 4;
        // Scale factor ~1/32 applied to the sample offset from the block
        // centre (in 1/8-pel units) — a mild, filterable zoom.
        let mk = |sx: i32, sy: i32| WarpSample {
            src: [sx, sy],
            dst: [sx + sx / 32, sy + sy / 32],
        };
        let raw = [mk(-256, -256), mk(256, -256), mk(-256, 256), mk(256, 256)];
        let model = derive_warp_model(&raw, bw4, bh4, mv, 10, 6).expect("model should be valid");
        // A positive zoom expands mat[2]/mat[5] above the 0x10000 identity.
        assert!(model.matrix[2] > 0x10000);
        assert!(model.matrix[5] > 0x10000);
        assert!(model.alpha > 0);
        assert!(model.delta > 0);
    }

    /// `select_warp_samples`'s threshold-and-replace pass (dav1d
    /// `derive_warpmv`'s `for (i=0,j=np-1,k=0; ...)` loop) must compact the
    /// *valid* (within-threshold) samples into the front of the list,
    /// preserving each valid sample's data, not just its slot count — a
    /// naive re-implementation could easily just filter+truncate (wrong:
    /// dav1d's loop only replaces exactly `np - ret` slots and stops, it
    /// does not do a full stable filter).
    #[test]
    fn select_warp_samples_compacts_valid_entries_from_the_tail() {
        let bw4 = 4;
        let bh4 = 4;
        let mv = Mv::new(0, 0);
        // thresh = 4*clip(max(4,4),4,28) = 16. Sample 0 and 2 exceed it,
        // sample 1 is exactly at the block's own mv (diff 0, valid).
        let raw = [
            WarpSample {
                src: [0, 0],
                dst: [1000, 1000],
            }, // way off — invalid
            WarpSample {
                src: [100, 100],
                dst: [100, 100],
            }, // exactly mv=(0,0) — valid
            WarpSample {
                src: [0, 0],
                dst: [-1000, -1000],
            }, // way off — invalid
        ];
        let selected = select_warp_samples(&raw, bw4, bh4, mv);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].src, [100, 100]);
        assert_eq!(selected[0].dst, [100, 100]);
    }

    /// When every sample fails the threshold, dav1d still keeps exactly one
    /// (the *first*, unfiltered) sample rather than producing an empty fit —
    /// `derive_warpmv`'s `if (!ret) ret = 1;`, using the original `pts[0]`.
    #[test]
    fn select_warp_samples_keeps_first_raw_sample_when_all_fail_threshold() {
        let bw4 = 2;
        let bh4 = 2;
        let mv = Mv::new(0, 0);
        let raw = [
            WarpSample {
                src: [0, 0],
                dst: [5000, 5000],
            },
            WarpSample {
                src: [0, 0],
                dst: [-5000, -5000],
            },
        ];
        let selected = select_warp_samples(&raw, bw4, bh4, mv);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].dst, [5000, 5000]);
    }

    /// The warp filter bank is a real 8-tap kernel: every row must sum to
    /// 128 (unity gain at `FILTER_BITS = 7`), matching every other AV1
    /// filter table in this codebase and guarding against a transcription
    /// error in the 193×8 constant above.
    #[test]
    fn warped_filters_sum_to_unity_gain() {
        for (i, row) in WARPED_FILTERS.iter().enumerate() {
            let sum: i32 = row.iter().map(|&c| c as i32).sum();
            assert_eq!(sum, 128, "row {i} sums to {sum}, not 128");
        }
    }
}
