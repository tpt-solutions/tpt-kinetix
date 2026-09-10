//! Compound inter-prediction neighbour-context derivations (AV1 §8.3.2),
//! ported branch-for-branch from dav1d `src/env.h` (`get_comp_dir_ctx`,
//! `av1_get_*_ref_ctx`, `get_mask_comp_ctx`, `get_jnt_comp_ctx`).
//!
//! dav1d reference numbering is used throughout (`LAST = 0 … GOLDEN = 3`,
//! `BWDREF = 4 … ALTREF = 6`); the caller converts Kinetix's `LAST_FRAME = 2 …
//! ALTREF_FRAME = 8` names by subtracting 2. `ref0`/`ref1` are only meaningful
//! when `!intra`.

/// One neighbour edge (the mi cell directly above, or directly to the left, of
/// the current block's top-left corner).
#[derive(Clone, Copy)]
pub(super) struct CompEdge {
    pub intra: bool,
    /// dav1d `BlockContext::comp_type`: 0 = none, 1 = weighted-avg, 2 = avg,
    /// 3 = seg/diffwtd, 4 = wedge.
    pub comp_type: u8,
    /// dav1d ref numbering (0..6), or -1 when unavailable / intra.
    pub ref0: i32,
    pub ref1: i32,
}

impl CompEdge {
    pub(super) const NA: CompEdge = CompEdge {
        intra: true,
        comp_type: 0,
        ref0: -1,
        ref1: -1,
    };
    #[inline]
    fn is_comp(&self) -> bool {
        self.comp_type != 0
    }
    /// dav1d `has_uni_comp(edge, off)`.
    #[inline]
    fn has_uni_comp(&self) -> bool {
        (self.ref0 < 4) == (self.ref1 < 4)
    }
}

/// `>= 4U` in dav1d — true for backward refs *and* for intra (`-1` as u32).
#[inline]
fn ref_ge4_u(r: i32) -> bool {
    (r as u32) >= 4
}

/// dav1d `get_comp_ctx` — context for the `is_comp` (`comp_mode`) flag.
pub(super) fn comp_ctx(a: CompEdge, l: CompEdge, have_top: bool, have_left: bool) -> usize {
    if have_top {
        if have_left {
            if a.is_comp() {
                if l.is_comp() {
                    4
                } else {
                    2 + ref_ge4_u(l.ref0) as usize
                }
            } else if l.is_comp() {
                2 + ref_ge4_u(a.ref0) as usize
            } else {
                ((l.ref0 >= 4) ^ (a.ref0 >= 4)) as usize
            }
        } else if a.is_comp() {
            3
        } else {
            (a.ref0 >= 4) as usize
        }
    } else if have_left {
        if l.is_comp() {
            3
        } else {
            (l.ref0 >= 4) as usize
        }
    } else {
        1
    }
}

/// dav1d `get_comp_dir_ctx` — context for `comp_reference_type`
/// (UNIDIR vs BIDIR).
pub(super) fn comp_dir_ctx(a: CompEdge, l: CompEdge, have_top: bool, have_left: bool) -> usize {
    if have_top && have_left {
        if a.intra && l.intra {
            return 2;
        }
        if a.intra || l.intra {
            let edge = if a.intra { l } else { a };
            if !edge.is_comp() {
                return 2;
            }
            return 1 + 2 * edge.has_uni_comp() as usize;
        }
        let a_comp = a.is_comp();
        let l_comp = l.is_comp();
        let a_ref0 = a.ref0;
        let l_ref0 = l.ref0;
        if !a_comp && !l_comp {
            1 + 2 * ((a_ref0 >= 4) == (l_ref0 >= 4)) as usize
        } else if !a_comp || !l_comp {
            let edge = if a_comp { a } else { l };
            if !edge.has_uni_comp() {
                return 1;
            }
            3 + ((a_ref0 >= 4) == (l_ref0 >= 4)) as usize
        } else {
            let a_uni = a.has_uni_comp();
            let l_uni = l.has_uni_comp();
            if !a_uni && !l_uni {
                return 0;
            }
            if !a_uni || !l_uni {
                return 2;
            }
            3 + ((a_ref0 == 4) == (l_ref0 == 4)) as usize
        }
    } else if have_top || have_left {
        let edge = if have_left { l } else { a };
        if edge.intra {
            return 2;
        }
        if !edge.is_comp() {
            return 2;
        }
        4 * edge.has_uni_comp() as usize
    } else {
        2
    }
}

/// Shared "count refs on each side, compare" helper used by several dav1d
/// `av1_get_*_ref_ctx` functions.
#[inline]
fn cmp3(c0: i32, c1: i32) -> usize {
    if c0 == c1 {
        1
    } else if c0 < c1 {
        0
    } else {
        2
    }
}

fn for_each_edge_ref(
    a: CompEdge,
    l: CompEdge,
    have_top: bool,
    have_left: bool,
    mut f: impl FnMut(i32),
) {
    if have_top && !a.intra {
        f(a.ref0);
        if a.is_comp() {
            f(a.ref1);
        }
    }
    if have_left && !l.intra {
        f(l.ref0);
        if l.is_comp() {
            f(l.ref1);
        }
    }
}

/// dav1d `av1_get_ref_ctx` (= `av1_get_uni_p_ctx`).
pub(super) fn ref_ctx(a: CompEdge, l: CompEdge, have_top: bool, have_left: bool) -> usize {
    let mut cnt = [0i32; 2];
    for_each_edge_ref(a, l, have_top, have_left, |r| cnt[(r >= 4) as usize] += 1);
    cmp3(cnt[0], cnt[1])
}

/// dav1d `av1_get_fwd_ref_ctx`.
pub(super) fn fwd_ref_ctx(a: CompEdge, l: CompEdge, have_top: bool, have_left: bool) -> usize {
    let mut cnt = [0i32; 4];
    for_each_edge_ref(a, l, have_top, have_left, |r| {
        if (0..4).contains(&r) {
            cnt[r as usize] += 1;
        }
    });
    let c0 = cnt[0] + cnt[1];
    let c2 = cnt[2] + cnt[3];
    cmp3(c0, c2)
}

/// dav1d `av1_get_fwd_ref_1_ctx`.
pub(super) fn fwd_ref_1_ctx(a: CompEdge, l: CompEdge, have_top: bool, have_left: bool) -> usize {
    let mut cnt = [0i32; 2];
    for_each_edge_ref(a, l, have_top, have_left, |r| {
        if (0..2).contains(&r) {
            cnt[r as usize] += 1;
        }
    });
    cmp3(cnt[0], cnt[1])
}

/// dav1d `av1_get_fwd_ref_2_ctx` (= `av1_get_uni_p2_ctx`).
pub(super) fn fwd_ref_2_ctx(a: CompEdge, l: CompEdge, have_top: bool, have_left: bool) -> usize {
    let mut cnt = [0i32; 2];
    for_each_edge_ref(a, l, have_top, have_left, |r| {
        if r == 2 || r == 3 {
            cnt[(r - 2) as usize] += 1;
        }
    });
    cmp3(cnt[0], cnt[1])
}

/// dav1d `av1_get_bwd_ref_ctx`.
pub(super) fn bwd_ref_ctx(a: CompEdge, l: CompEdge, have_top: bool, have_left: bool) -> usize {
    let mut cnt = [0i32; 3];
    for_each_edge_ref(a, l, have_top, have_left, |r| {
        if r >= 4 {
            cnt[(r - 4) as usize] += 1;
        }
    });
    let c1 = cnt[1] + cnt[0];
    if cnt[2] == c1 {
        1
    } else if c1 < cnt[2] {
        0
    } else {
        2
    }
}

/// dav1d `av1_get_bwd_ref_1_ctx`.
pub(super) fn bwd_ref_1_ctx(a: CompEdge, l: CompEdge, have_top: bool, have_left: bool) -> usize {
    let mut cnt = [0i32; 3];
    for_each_edge_ref(a, l, have_top, have_left, |r| {
        if r >= 4 {
            cnt[(r - 4) as usize] += 1;
        }
    });
    cmp3(cnt[0], cnt[1])
}

/// dav1d `av1_get_uni_p1_ctx`.
pub(super) fn uni_p1_ctx(a: CompEdge, l: CompEdge, have_top: bool, have_left: bool) -> usize {
    let mut cnt = [0i32; 3];
    for_each_edge_ref(a, l, have_top, have_left, |r| {
        if (1..4).contains(&r) {
            cnt[(r - 1) as usize] += 1;
        }
    });
    let c1 = cnt[1] + cnt[2];
    cmp3(cnt[0], c1)
}

/// dav1d `get_mask_comp_ctx` — context for `mask_comp` (seg/wedge vs jnt-avg).
#[allow(dead_code)]
pub(super) fn mask_comp_ctx(a: CompEdge, l: CompEdge) -> usize {
    let side = |e: CompEdge| -> i32 {
        if e.comp_type >= 3 {
            1
        } else if e.ref0 == 6 {
            3
        } else {
            0
        }
    };
    (side(a) + side(l)).min(5) as usize
}

/// dav1d `get_jnt_comp_ctx` — context for the `jnt_comp` (distance-weighted)
/// flag. `d_equal` is `abs(poc_diff(ref0, cur)) == abs(poc_diff(cur, ref1))`.
#[allow(dead_code)]
pub(super) fn jnt_comp_ctx(d_equal: bool, a: CompEdge, l: CompEdge) -> usize {
    let side = |e: CompEdge| -> usize { (e.comp_type >= 2 || e.ref0 == 6) as usize };
    3 * d_equal as usize + side(a) + side(l)
}
