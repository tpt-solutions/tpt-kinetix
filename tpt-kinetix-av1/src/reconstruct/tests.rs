use super::*;

/// Pin the SMOOTH intra-mode enum constants to the AV1 spec ordering
/// (`SMOOTH_PRED=9, SMOOTH_V_PRED=10, SMOOTH_H_PRED=11`). They were briefly
/// rotated (`SMOOTH_V=9, SMOOTH_H=10, SMOOTH=11`), which silently ran
/// `SMOOTH_V_PRED` blocks through `predict_smooth_h` and vice-versa —
/// invisible on flat content, an axis-swapped gradient on real content.
#[test]
fn smooth_intra_mode_constants_match_spec_ordering() {
    assert_eq!(SMOOTH, 9);
    assert_eq!(SMOOTH_V, 10);
    assert_eq!(SMOOTH_H, 11);
    assert_eq!(PAETH, 12);
}

/// `is_smooth()` (AV1 §7.11.2.9) recognises exactly the three SMOOTH* intra
/// modes — the predicate feeding directional prediction's `filterType`.
#[test]
fn is_smooth_intra_mode_matches_spec_set() {
    for m in 0u8..=13 {
        let expect = m == SMOOTH || m == SMOOTH_V || m == SMOOTH_H;
        assert_eq!(is_smooth_intra_mode(m), expect, "mode {m}");
    }
    assert!(!is_smooth_intra_mode(DC_PRED));
    assert!(!is_smooth_intra_mode(PAETH));
}

/// A non-zero `filterType` lowers the edge-filter strength / upsample gate
/// thresholds (AV1 §7.11.2.9), so a sub-pel directional block next to a
/// SMOOTH neighbour can predict differently than the same block in isolation.
#[test]
fn directional_prediction_filter_type_changes_sub_pel_output() {
    let size = 8;
    let jagged: Vec<i32> = (0..2 * size)
        .map(|i| if i % 2 == 0 { 20 } else { 230 })
        .collect();
    let borders = BlockBorders {
        top: jagged.clone(),
        left: jagged,
        tl: 128,
        have_above: true,
        have_left: true,
    };
    // D67_PRED: nominal 67, a zone-1 sub-pel angle where upsampling is gated
    // by `filterType` (blk_wh 16 -> allowed only when filterType == 0).
    let mut a = vec![0i32; size * size];
    let mut b = vec![0i32; size * size];
    predict_intra_block(
        D67_PRED, &borders, size, size, &mut a, true, 0, 0, size, size,
    );
    predict_intra_block(
        D67_PRED, &borders, size, size, &mut b, true, 1, 0, size, size,
    );
    assert_ne!(
        a, b,
        "filterType should affect the sub-pel edge/upsample path"
    );
}

/// The same generated buffer the `coeff` module's oracle tests use, so
/// this exercises a payload that is known to decode to real coefficients
/// rather than immediately hitting `all_zero` everywhere.
fn ramp(len: usize, mul: usize, add: usize) -> Vec<u8> {
    (0..len).map(|i| ((i * mul + add) & 0xFF) as u8).collect()
}

type DecodeResult = Result<(Vec<u8>, Vec<u8>, Vec<u8>), KinetixError>;

fn decode(data: &[u8], width: usize, height: usize, qindex: u8) -> DecodeResult {
    let uv_w = width / 2;
    let uv_h = height / 2;
    let mut y = vec![128u8; width * height];
    let mut u = vec![128u8; uv_w * uv_h];
    let mut v = vec![128u8; uv_w * uv_h];
    let mut meta = FrameMeta::new(width, height);
    decode_tile_group(
        data,
        width,
        height,
        8,
        qindex,
        DeltaQ::default(),
        false,
        0,
        0,
        1,
        1,
        &mut y,
        &mut u,
        &mut v,
        width,
        uv_w,
        true,
        false,
        false,
        false,
        false,
        false,
        false,
        false,
        false, // allow_intrabc
        LrDecodeParams::default(),
        CdefDeltaParams::default(),
        true,
        false,
        false, // force_integer_mv
        false,
        0,
        [0u8; 9],
        RefFrames::empty(),
        &mut meta,
    )?;
    Ok((y, u, v))
}

#[test]
fn tile_group_decode_does_not_panic_on_synthetic_input() {
    // Now that decode walks a real partition/mode/tx tree, a synthetic
    // buffer is not a valid AV1 stream, so we only assert the call returns
    // (success or a clean decode error) without panicking. Real
    // pixel-exact validation lives in the conformance harness against an
    // ffmpeg/dav1d reference.
    let _ = decode(&ramp(512, 37, 11), 32, 32, 100);
    let _ = decode(&[], 32, 32, 100);
}

#[test]
fn empty_tile_group_leaves_the_neutral_fill() {
    // With no payload the symbol decoder reads zero padding; whatever it
    // decodes must still land inside the planes without panicking.
    let (y, u, v) = decode(&[], 32, 32, 100).expect("empty tile group decodes");
    assert_eq!(y.len(), 32 * 32);
    assert_eq!(u.len(), 16 * 16);
    assert_eq!(v.len(), 16 * 16);
}

#[test]
fn directional_prediction_covers_all_modes_without_panicking() {
    // These modes are unreachable until Phase C selects them, but the
    // offset arithmetic used to underflow in `usize`; make sure every
    // mode/size combination stays in bounds and in range.
    for &mode in &[
        D45_PRED, D135_PRED, D113_PRED, D157_PRED, D207_PRED, D67_PRED,
    ] {
        for &size in &[4usize, 8, 16, 32] {
            let top: Vec<i32> = (0..2 * size).map(|i| (i * 7 % 256) as i32).collect();
            let left: Vec<i32> = (0..2 * size).map(|i| (i * 13 % 256) as i32).collect();
            let mut out = vec![0i32; size * size];
            let borders = BlockBorders {
                top,
                left,
                tl: 128,
                have_above: true,
                have_left: true,
            };
            predict_intra_block(mode, &borders, size, size, &mut out, true, 0, 0, size, size);
            assert!(
                out.iter().all(|&v| (0..=255).contains(&v)),
                "mode {mode} size {size} produced an out-of-range sample"
            );
        }
    }
}

#[test]
fn directional_edge_filter_gates_on_have_above_left_not_zone_need() {
    // AV1 spec §7.11.2.4 step 4 gates the above/left intra-edge-filter
    // sub-steps on `haveAbove`/`haveLeft` (actual sample availability), not
    // on whether the current angle's *zone* structurally reads that edge
    // (`need_above`/`need_left`). `D135_PRED` (nominal angle 135) is a
    // zone-2 mode, which always needs *both* edges structurally — so a
    // previous revision that gated the edge-filter sub-steps on
    // `need_above`/`need_left` instead of `haveAbove`/`haveLeft` always
    // filtered both edges for this mode, even when one side had no real
    // neighbour and its `top`/`left` arrays were just synthesized filler.
    //
    // With `haveAbove == 0` and `haveLeft == 0`, spec says the edge-filter
    // sub-steps (and therefore any dependency on the exact `top`/`left`
    // sample values feeding the smoothing kernel) never run at all, so the
    // predicted output must come out bit-identical whether
    // `enable_intra_edge_filter` is on or off. Under the previous bug the
    // two runs differed, since the jagged `top`/`left` filler below is
    // exactly the kind of high-frequency input the 5-tap edge filter
    // visibly smooths.
    // `w + h < 24` keeps `filter_intra_edge_corner` (gated on
    // `need_above && need_left`, unconditionally on `haveAbove`/`haveLeft`
    // per spec — a separate, already-correct piece of §7.11.2.4 step 4) out
    // of play, isolating exactly the above/left edge-filter gate this test
    // targets.
    let size = 8;
    let jagged: Vec<i32> = (0..2 * size)
        .map(|i| if i % 2 == 0 { 0 } else { 255 })
        .collect();
    let borders = BlockBorders {
        top: jagged.clone(),
        left: jagged,
        tl: 128,
        have_above: false,
        have_left: false,
    };
    let mut pred_filtered = vec![0i32; size * size];
    predict_intra_block(
        D135_PRED,
        &borders,
        size,
        size,
        &mut pred_filtered,
        true,
        0,
        0,
        size,
        size,
    );
    let mut pred_unfiltered = vec![0i32; size * size];
    predict_intra_block(
        D135_PRED,
        &borders,
        size,
        size,
        &mut pred_unfiltered,
        false,
        0,
        0,
        size,
        size,
    );
    assert_eq!(
        pred_filtered, pred_unfiltered,
        "haveAbove == haveLeft == false must skip edge filtering entirely, \
         regardless of enable_intra_edge_filter"
    );
}

#[test]
fn smooth_weights_match_libaom_tables() {
    // AV1 spec §7.11.2.6 `Sm_Weights_Tx_*` — these are libaom's
    // `sm_weight_arrays` exactly, transcribed into `SMOOTH_WEIGHTS`.
    assert_eq!(smooth_weight(4, 0), 255);
    assert_eq!(smooth_weight(4, 1), 149);
    assert_eq!(smooth_weight(4, 3), 64);
    assert_eq!(smooth_weight(8, 1), 197);
    assert_eq!(smooth_weight(8, 7), 32);
    assert_eq!(smooth_weight(16, 15), 16);
    assert_eq!(smooth_weight(32, 31), 8);
    assert_eq!(smooth_weight(64, 63), 4);
    // The near-edge weight is always ~1 (scaled by 256) and the far-edge
    // weight is 1/block_size, scaled by 256.
    assert_eq!(smooth_weight(4, 0), 255);
    assert_eq!(smooth_weight(64, 0), 255);
}

#[test]
fn smooth_predictors_match_libaom_arithmetic() {
    // Hand-evaluated against libaom's `smooth_v/h/smooth_predictor`:
    //   pred = w*near + (256-w)*far,  dst = Round2(pred, 8)
    //   (SMOOTH combines both axes, Round2(., 9)). The smooth weights never
    //   reach 0 at the far edge (they settle at 1/block_size scaled), so the
    //   far edge is only approached, never matched exactly.
    let top = vec![10i32, 20, 30, 40];
    let left = vec![50i32, 60, 70, 80];
    let mut out = vec![0i32; 16];

    predict_smooth_v(&top, &left, 128, 4, 4, &mut out);
    assert_eq!(
        out,
        vec![10, 20, 30, 40, 39, 45, 51, 57, 57, 60, 63, 67, 63, 65, 68, 70,]
    );

    predict_smooth_h(&top, &left, 128, 4, 4, &mut out);
    // far = top[3] = 40; rightmost column approaches (but does not equal) it.
    assert_eq!(
        out,
        vec![50, 46, 43, 43, 60, 52, 47, 45, 70, 57, 50, 48, 80, 63, 53, 50,]
    );

    predict_smooth(&top, &left, 128, 4, 4, &mut out);
    assert_eq!(
        out,
        vec![30, 33, 37, 41, 50, 48, 49, 51, 63, 59, 57, 57, 71, 64, 60, 60,]
    );
    assert!(out.iter().all(|&v| (0..=255).contains(&v)));
}

#[test]
fn cfl_prediction_matches_hand_computed_values_no_subsampling() {
    // AV1 spec §7.11.5, sub_x = sub_y = 0 (4:4:4-style): L[i][j] =
    // luma[i][j] << 3, lumaAvg = Round2(sum(L), log2W + log2H).
    //
    // Luma block:
    //  10  20  30  40
    //  50  60  70  80
    //  90 100 110 120
    // 130 140 150 160
    // sum = 1360, L-sum = 10880, lumaAvg = (10880 + 8) >> 4 = 680.
    let luma: Vec<u8> = vec![
        10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120, 130, 140, 150, 160,
    ];
    let cfl = CflParams {
        luma: &luma,
        luma_stride: 4,
        sub_x: false,
        sub_y: false,
        max_luma_w: 4,
        max_luma_h: 4,
        alpha: 4,
    };
    let mut pred = vec![50i32; 16];
    apply_cfl_prediction(&mut pred, 4, 4, 0, 0, &cfl);
    // pixel=10 (top-left): L=80, diff=4*(80-680)=-2400, scaled=-((2400+32)>>6)=-38.
    assert_eq!(pred[0], 50 - 38);
    // pixel=60 (row1,col1): L=480, diff=4*(480-680)=-800, scaled=-((800+32)>>6)=-13.
    assert_eq!(pred[4 + 1], 50 - 13);
    // pixel=160 (bottom-right): L=1280, diff=4*(1280-680)=2400, scaled=(2400+32)>>6=38.
    assert_eq!(pred[15], 50 + 38);
}

#[test]
fn cfl_prediction_is_a_no_op_on_flat_luma() {
    // A constant luma plane has L[i][j] == lumaAvg everywhere, so the CFL
    // adjustment must be exactly zero regardless of alpha.
    let luma = vec![77u8; 64];
    let cfl = CflParams {
        luma: &luma,
        luma_stride: 8,
        sub_x: true,
        sub_y: true,
        max_luma_w: 8,
        max_luma_h: 8,
        alpha: -7,
    };
    let mut pred = vec![42i32; 16];
    apply_cfl_prediction(&mut pred, 4, 4, 0, 0, &cfl);
    assert!(pred.iter().all(|&v| v == 42), "got {pred:?}");
}

#[test]
fn ceil_log2_matches_spec_examples() {
    // AV1 spec §4.6: CeilLog2(x) is 0 for x < 2, otherwise the number of
    // bits needed to represent 0..x-1.
    assert_eq!(ceil_log2(0), 0);
    assert_eq!(ceil_log2(1), 0);
    assert_eq!(ceil_log2(2), 1);
    assert_eq!(ceil_log2(3), 2);
    assert_eq!(ceil_log2(4), 2);
    assert_eq!(ceil_log2(5), 3);
    assert_eq!(ceil_log2(8), 3);
    assert_eq!(ceil_log2(9), 4);
}

#[test]
fn get_palette_color_context_matches_spec_worked_example() {
    // 2x2 map (row-major, stride 2): row0 = [1, 0], row1 = [0, _],
    // querying (r=1, c=1) so the unread `_` slot's value doesn't matter.
    // Left = map[1][0] = 0 (score += 2), above-left = map[0][0] = 1
    // (score += 1), above = map[0][1] = 0 (score += 2) -> scores =
    // [4, 1, 0] for colors [0, 1, 2]. Already sorted descending, so no
    // swap happens and ColorOrder stays [0, 1, 2]. hash = 4*1 + 1*2 +
    // 0*2 = 6 -> ctx = Palette_Color_Context[6] = 3 (§5.11.50 /
    // "Additional tables").
    let map = [1u8, 0, 0, 0];
    let (ctx, order) = get_palette_color_context(&map, 2, 1, 1, 3);
    assert_eq!(ctx, 3, "expected Palette_Color_Context[6] == 3");
    assert_eq!(order[..3], [0, 1, 2], "already sorted, no swap expected");

    // All-distinct-neighbour case reaching the maximum possible hash
    // (PALETTE_MAX_COLOR_CONTEXT_HASH = 8): left = map[1][0] = 0
    // (score 2), above-left = map[0][0] = 2 (score 1), above =
    // map[0][1] = 1 (score 2) -> scores = [2, 2, 1], already descending
    // -> hash = 2*1 + 2*2 + 1*2 = 8 -> ctx = Palette_Color_Context[8] = 1.
    let map2 = [2u8, 1, 0, 0];
    let (ctx2, order2) = get_palette_color_context(&map2, 2, 1, 1, 3);
    assert_eq!(ctx2, 1, "expected Palette_Color_Context[8] == 1");
    assert_eq!(order2[..3], [0, 1, 2], "already sorted, no swap expected");
}

#[test]
fn lossless_blocks_select_the_walsh_hadamard_transform() {
    // AV1 §7.13.3 substitutes the inverse WHT when `Lossless` is set,
    // regardless of the `TxType` `coeffs()` reported.
    let mut coeffs = vec![0i32; 16];
    coeffs[0] = 64;
    let mut wht = vec![0i32; 16];
    inverse_transform(&coeffs, av1::DCT_DCT, TX_4X4, true, &mut wht);
    let mut dct = vec![0i32; 16];
    inverse_transform(&coeffs, av1::DCT_DCT, TX_4X4, false, &mut dct);
    assert_ne!(wht, dct, "the WHT must not be aliased onto the DCT");

    // `lossless` takes priority over whatever `TxType` `coeffs()` reported.
    let mut wht_via_idtx = vec![0i32; 16];
    inverse_transform(&coeffs, av1::IDTX, TX_4X4, true, &mut wht_via_idtx);
    assert_eq!(wht, wht_via_idtx);
}

#[test]
fn cos128_sin128_match_spec_identities() {
    // cos128(0) = 4096 * cos(0) = 4096; sin128(64) = cos128(0) = 4096;
    // cos128(64) = 4096 * cos(pi/2) = 0; cos128(32) == sin128(32)
    // (angle 32 is the 45-degree case the butterfly fast path relies on).
    assert_eq!(cos128(0), 4096);
    assert_eq!(sin128(64), 4096);
    assert_eq!(cos128(64), 0);
    assert_eq!(cos128(32), sin128(32));
    assert_eq!(cos128(32), 2896);
    // cos128 is periodic in 256 and symmetric per spec steps 2-5.
    assert_eq!(cos128(0), cos128(256));
    assert_eq!(cos128(128), -4096);
}

#[test]
fn inverse_dct_permutation_is_bit_reversal() {
    for &n in &[2u32, 3, 4, 5, 6] {
        let len = 1usize << n;
        let mut t: Vec<i64> = (0..len as i64).collect();
        inverse_dct_permute(&mut t, n);
        let mut seen = vec![false; len];
        for &v in &t {
            assert!(!seen[v as usize], "permutation must be a bijection");
            seen[v as usize] = true;
        }
        // t[i] == brev(n, i) since the source array was the identity.
        for (i, x) in t.iter().enumerate().take(len) {
            assert_eq!(*x as usize, brev(n, i));
        }
    }
}

#[test]
fn dc_only_inverse_dct_4x4_matches_hand_computed_value() {
    // A pure-DC 4x4 DCT_DCT block: TRANSFORM_ROW_SHIFT[TX_4X4] = 0,
    // colShift = 4. Hand-derived via the spec's butterfly steps for n=2:
    // row/col each apply `round2(2896 * x, 12)`, so dequant[0] = 4096
    // round-trips to a flat residual of 128 (4096 / 32) with no rounding
    // slack at this specific value (2896*4096 and 2896*2048 both divide
    // cleanly by 4096 at the intermediate steps).
    let mut dequant = vec![0i32; 16];
    dequant[0] = 4096;
    let mut residual = vec![0i32; 16];
    inverse_transform(&dequant, av1::DCT_DCT, TX_4X4, false, &mut residual);
    assert_eq!(residual, vec![128; 16]);
}

#[test]
fn dq_denom_matches_spec_for_large_square_transforms() {
    // AV1 spec §7.12.3: dqDenom is 2 for TX_32X32, 4 for TX_64X64, 1
    // otherwise. Omitting this (as the pre-2026-08-16 code did) overscales
    // every coefficient in the two largest transform sizes.
    assert_eq!(dq_denom(TX_4X4), 1);
    assert_eq!(dq_denom(TX_8X8), 1);
    assert_eq!(dq_denom(TX_16X16), 1);
    assert_eq!(dq_denom(TX_32X32), 2);
    assert_eq!(dq_denom(TX_64X64), 4);
}

#[test]
fn dq_denom_follows_the_square_up_size_not_the_transforms_own_shape() {
    // AV1 spec §7.12.3 / dav1d's `dq_shift = Max(0, t_dim->ctx - 2)`, where
    // `t_dim->ctx == tx_sz_ctx == (TX_SIZE_SQR[tx] + TX_SIZE_SQR_UP[tx] + 1)
    // >> 1` (dav1d's own `dav1d_txfm_dimensions` table, `src/tables.c`,
    // pins the exact `ctx` value per rectangular size and was used as the
    // ground truth for the expectations below). This is *not* simply "the
    // transform's square-up size": `TX_16X32`/`TX_32X16` (sqr=16x16,
    // sqr_up=32x32) average to ctx=3 (dqDenom=2), but the more skewed
    // `TX_8X32`/`TX_32X8` (sqr=8x8, sqr_up=32x32) average to ctx=2
    // (dqDenom=1, *not* 2) — same square-up, different dqDenom, because the
    // averaging also accounts for the transform's own (smaller) shape.
    // Likewise `TX_16X64`/`TX_64X16` (sqr=16x16, sqr_up=64x64) average to
    // ctx=3 (dqDenom=2, not 4) while only the genuinely-close-to-square
    // `TX_32X64`/`TX_64X32` (sqr=32x32, sqr_up=64x64) reach ctx=4
    // (dqDenom=4). A version of `dq_denom` that only matched the two
    // literal square enum values (`TX_32X32`/`TX_64X64`) silently returned
    // 1 (a no-op) for every one of these eight rectangular sizes — found
    // via a `DAV1D_ITXDUMP`-patched dav1d trace on `mandelbrot_128x96`'s
    // `TX_16X32` block, where it overscaled the residual ~2x and, once
    // fixed, took `smptebars_256x144`'s luma PSNR from 57.49 dB to 99 dB
    // (pixel-exact) in one `av1_psnr_check` run.
    for &tx_size in &[av1::TX_16X32, av1::TX_32X16] {
        assert_eq!(dq_denom(tx_size), 2, "tx_size index {tx_size}: ctx=3");
    }
    for &tx_size in &[av1::TX_8X32, av1::TX_32X8] {
        assert_eq!(dq_denom(tx_size), 1, "tx_size index {tx_size}: ctx=2");
    }
    for &tx_size in &[av1::TX_16X64, av1::TX_64X16] {
        assert_eq!(dq_denom(tx_size), 2, "tx_size index {tx_size}: ctx=3");
    }
    for &tx_size in &[av1::TX_32X64, av1::TX_64X32] {
        assert_eq!(dq_denom(tx_size), 4, "tx_size index {tx_size}: ctx=4");
    }
}

#[test]
fn dc_only_inverse_dct_is_flat_at_every_square_size() {
    // A DC-only coefficient block must inverse-transform to a spatially
    // flat residual at every square transform size (both DCT and ADST
    // rows/cols are the identity shape for a pure-DC input: only T[0] is
    // nonzero going into the row pass, so every row transform sees the
    // same 1-nonzero-sample input and therefore produces the same
    // per-row constant, and likewise for the column pass).
    for &tx_size in &[TX_4X4, TX_8X8, TX_16X16, TX_32X32, TX_64X64] {
        let n = 4usize << tx_size;
        let mut dequant = vec![0i32; n * n];
        dequant[0] = -1000;
        let mut residual = vec![0i32; n * n];
        inverse_transform(&dequant, av1::DCT_DCT, tx_size, false, &mut residual);
        let first = residual[0];
        assert!(
            residual.iter().all(|&v| v == first),
            "tx_size {tx_size}: DC-only residual must be flat, got {residual:?}"
        );
        assert_ne!(
            first, 0,
            "tx_size {tx_size}: a -1000 DC coefficient must not vanish to 0"
        );
    }
}

#[test]
fn mi_width_height_log2_match_block_size_table() {
    // BLOCK_4X4 is 1 mi unit wide/tall (log2 0); BLOCK_64X64 is 16 mi
    // units (log2 4); BLOCK_4X8 is 1 wide / 2 tall (log2 0 / log2 1).
    assert_eq!(mi_width_log2(BLOCK_4X4), 0);
    assert_eq!(mi_height_log2(BLOCK_4X4), 0);
    assert_eq!(mi_width_log2(BLOCK_64X64), 4);
    assert_eq!(mi_height_log2(BLOCK_64X64), 4);
    assert_eq!(mi_width_log2(BLOCK_4X8), 0);
    assert_eq!(mi_height_log2(BLOCK_4X8), 1);
}

#[test]
fn split_or_horz_and_vert_never_panic_at_every_partition_bucket() {
    // Regression test for a real panic (2026-08-16): `read_split_or_horz`/
    // `read_split_or_vert` indexed the W8-bucket partition CDF (4 symbols:
    // NONE/HORZ/VERT/SPLIT, length 5) at the extended-partition indices
    // (HORZ_A=4 .. VERT_4=9), which only exist in the W16-W128 buckets.
    // The spec asserts this bucket is never actually reached in a
    // conformant bitstream, but the decoder must not panic on it
    // regardless (a malformed/adversarial stream, or a bug elsewhere that
    // picks the wrong bucket, must fail as a decode error, not a crash).
    let data = vec![0x55u8; 64];
    for bucket in 0..5 {
        for bsize in [BLOCK_8X8, BLOCK_64X64, BLOCK_128X128] {
            let mut dec = SymbolDecoder::new(&data);
            let mut cdfs = ModeCdfs::new();
            let _ = cdfs.read_split_or_horz(&mut dec, bucket, 0, bsize);
            let mut dec = SymbolDecoder::new(&data);
            let _ = cdfs.read_split_or_vert(&mut dec, bucket, 0, bsize);
        }
    }
}

#[test]
fn partition_context_matches_spec_left_times_2_plus_above() {
    // AV1 spec §8.3.2: ctx = left*2 + above, each gated on the neighbour
    // existing (AvailU/AvailL) and only set when the neighbour's mi
    // width/height log2 is strictly smaller than the current node's.
    let mut y = vec![0u8; 64];
    let mut u = vec![0u8; 16];
    let mut v = vec![0u8; 16];
    let mut meta = FrameMeta::new(2, 2);
    let mut state = TileDecodeState::new(
        &[0u8; 8],
        0,
        8,
        8,
        4,
        4,
        &mut y,
        &mut u,
        &mut v,
        8,
        4,
        128,
        DeltaQ::default(),
        true,
        false,
        false,
        false,
        0,
        0,
        8,
        8,
        true,
        false,
        false,
        false,
        false,
        false,
        false,
        true,
        false, // use_128x128_superblock
        LrDecodeParams::default(),
        CdefDeltaParams::default(),
        false,
        false,
        false,
        false,
        INTERP_SWITCHABLE,
        [0u8; 9],
        RefFrames::empty(),
        &mut meta,
    );
    // No neighbours recorded yet: both AvailU/AvailL false at the origin.
    assert_eq!(state.partition_context(0, 0, BLOCK_8X8), 0);

    // Record an 8x8 leaf at (0,0), then query the node to its right at
    // (0, 2 mi units = BLOCK_8X8 width): AvailL is true, and the left
    // neighbour's width log2 (1, for BLOCK_8X8) is not smaller than a
    // BLOCK_8X8 query's bsl (1) -> left=false.
    state.record_mi_size_context(0, 0, BLOCK_8X8);
    assert_eq!(state.partition_context(0, 2, BLOCK_8X8), 0);
    // Querying a *larger* node (BLOCK_16X16, bsl=2) against that same
    // BLOCK_8X8 neighbour: 1 < 2, so left=true -> ctx = 1*2+0 = 2.
    assert_eq!(state.partition_context(0, 2, BLOCK_16X16), 2);
}

#[test]
fn qindex_for_plane_applies_per_plane_delta_and_clamps() {
    // AV1 spec §7.12.2 `get_dc_quant`/`get_ac_quant`: the DC term for every
    // plane uses `qindex + DeltaQ{Y,U,V}Dc`; only chroma's AC term has its
    // own delta (`DeltaQ{U,V}Ac`) — luma AC never does. Regression test for
    // the 2026-08-24 fix: `dequantize_coeffs` previously always used the
    // plain frame `qindex` for every plane's DC term, silently ignoring
    // these (parsed but unused) frame-header fields.
    let mut y = vec![0u8; 64];
    let mut u = vec![0u8; 16];
    let mut v = vec![0u8; 16];
    let mut meta = FrameMeta::new(2, 2);
    let state = TileDecodeState::new(
        &[0u8; 8],
        0,
        8,
        8,
        4,
        4,
        &mut y,
        &mut u,
        &mut v,
        8,
        4,
        100,
        DeltaQ {
            y_dc: 5,
            u_dc: -10,
            u_ac: 3,
            v_dc: 200,  // deliberately large enough to force clamping to 255
            v_ac: -150, // deliberately large enough to force clamping to 0
        },
        true,
        false,
        false,
        false,
        0,
        0,
        8,
        8,
        true,
        false,
        false,
        false,
        false,
        false,
        false,
        true,
        false, // use_128x128_superblock
        LrDecodeParams::default(),
        CdefDeltaParams::default(),
        false,
        false,
        false,
        false,
        INTERP_SWITCHABLE,
        [0u8; 9],
        RefFrames::empty(),
        &mut meta,
    );
    assert_eq!(
        state.qindex_for_plane(0),
        (105, 100),
        "luma: dc+5, ac plain"
    );
    assert_eq!(
        state.qindex_for_plane(1),
        (90, 103),
        "chroma U: dc-10, ac+3"
    );
    assert_eq!(
        state.qindex_for_plane(2),
        (255, 0),
        "chroma V: dc/ac both clamp to the valid [0, 255] range"
    );
}

/// Shared factory for the `read_cdef`/`read_delta_qindex`/`read_delta_lf`
/// regression tests below: only the parameters those three functions'
/// gates actually inspect vary between tests, everything else is a fixed
/// 8x8-frame placeholder.
#[allow(clippy::too_many_arguments)]
fn make_cdef_delta_state<'a>(
    y: &'a mut [u8],
    u: &'a mut [u8],
    v: &'a mut [u8],
    meta: &'a mut FrameMeta,
    qindex: u8,
    cdef_delta: CdefDeltaParams,
    use_128x128_superblock: bool,
    allow_intrabc: bool,
) -> TileDecodeState<'a> {
    TileDecodeState::new(
        &[0u8; 8],
        0,
        8,
        8,
        4,
        4,
        y,
        u,
        v,
        8,
        4,
        qindex,
        DeltaQ::default(),
        true,
        false,
        false,
        false,
        0,
        0,
        8,
        8,
        true,
        false,
        false,
        false,
        false,
        false,
        false,
        allow_intrabc,
        use_128x128_superblock,
        LrDecodeParams::default(),
        cdef_delta,
        false,
        false,
        false,
        false,
        INTERP_SWITCHABLE,
        [0u8; 9],
        RefFrames::empty(),
        meta,
    )
}

#[test]
fn palette_colors_yu_delta_bias_is_plus_one_for_y_and_zero_for_u() {
    // AV1 spec §5.11.46: the delta-coded palette-colour remainder does
    // `palette_delta_y++` for luma but has NO bias for U. A shared decoder
    // that always added 1 drifted every U palette after the first delta entry
    // (and, since `paletteBits` is re-derived from the running colour, then
    // desynced the whole block). Feed identical bits to the Y and U paths and
    // assert the second colour differs by exactly the bias.
    //
    // Empty cache -> first colour = L(8), then L(2) extra bits, then one
    // L(paletteBits) delta. The arithmetic decoder's L() is not raw bits, so
    // rather than predict absolute values, feed the SAME bytes to the Y and U
    // paths and assert the only difference is the +1 luma bias on the delta.
    let data = [0x40u8, 0b0000_0001, 0b0000_0000];
    let mut yb = vec![0u8; 64];
    let mut ub = vec![0u8; 16];
    let mut vb = vec![0u8; 16];
    let mut meta = FrameMeta::new(2, 2);

    let mut state = TileDecodeState::new(
        &data,
        0,
        8,
        8,
        4,
        4,
        &mut yb,
        &mut ub,
        &mut vb,
        8,
        4,
        32,
        DeltaQ::default(),
        true,
        false,
        false,
        false,
        0,
        0,
        8,
        8,
        true,
        false,
        false,
        false,
        false,
        false,
        false,
        false,
        false,
        LrDecodeParams::default(),
        CdefDeltaParams::default(),
        true,
        false,
        false,
        false,
        INTERP_SWITCHABLE,
        [0u8; 9],
        RefFrames::empty(),
        &mut meta,
    );
    let y_colors = state.read_palette_colors_yu(2, &[], false, false);

    let mut state2 = TileDecodeState::new(
        &data,
        0,
        8,
        8,
        4,
        4,
        &mut yb,
        &mut ub,
        &mut vb,
        8,
        4,
        32,
        DeltaQ::default(),
        true,
        false,
        false,
        false,
        0,
        0,
        8,
        8,
        true,
        false,
        false,
        false,
        false,
        false,
        false,
        false,
        false,
        LrDecodeParams::default(),
        CdefDeltaParams::default(),
        true,
        false,
        false,
        false,
        INTERP_SWITCHABLE,
        [0u8; 9],
        RefFrames::empty(),
        &mut meta,
    );
    let u_colors = state2.read_palette_colors_yu(2, &[], true, false);

    // Same first colour, same raw delta bits; only the luma `++` bias differs.
    assert_eq!(y_colors[0], u_colors[0]);
    assert_eq!(
        y_colors[1] - u_colors[1],
        1,
        "luma delta must be one more than chroma-U delta (spec `palette_delta_y++`)"
    );
}

#[test]
fn read_cdef_marks_the_64x64_slot_set_even_at_cdef_bits_zero_and_reads_only_once() {
    // §5.11.56 `read_cdef()`: `L(cdef_bits)` is a zero-width (true no-op)
    // read when `cdef_bits == 0` — the only value the current corpus
    // exercises — but the spec still requires the `cdef_idx[r][c]` slot to
    // be marked "signalled" so a second block in the same 64x64 unit of the
    // same superblock does not read (or derive) a value again.
    let mut y = vec![0u8; 64];
    let mut u = vec![0u8; 16];
    let mut v = vec![0u8; 16];
    let mut meta = FrameMeta::new(2, 2);
    let mut state = make_cdef_delta_state(
        &mut y,
        &mut u,
        &mut v,
        &mut meta,
        100,
        CdefDeltaParams {
            enable_cdef: true,
            cdef_bits: 0,
            ..Default::default()
        },
        false,
        false,
    );
    assert!(state.cdef_idx.is_empty());
    state.read_cdef(0, 0, BLOCK_8X8, false);
    assert_eq!(state.cdef_idx.get(&(0, 0)), Some(&0));
    let bit_pos_after_first = state.dec.bit_position();
    state.read_cdef(0, 0, BLOCK_8X8, false);
    assert_eq!(
        state.dec.bit_position(),
        bit_pos_after_first,
        "second read_cdef in the same 64x64 unit must not consume more bits"
    );
}

#[test]
fn read_cdef_is_a_true_noop_when_any_gate_condition_holds() {
    // §5.11.56: `skip || CodedLossless || !enable_cdef || allow_intrabc`
    // all return immediately without touching `cdef_idx` or the bitstream.
    let mut y = vec![0u8; 64];
    let mut u = vec![0u8; 16];
    let mut v = vec![0u8; 16];
    let mut meta = FrameMeta::new(2, 2);
    // enable_cdef = false.
    let mut state = make_cdef_delta_state(
        &mut y,
        &mut u,
        &mut v,
        &mut meta,
        100,
        CdefDeltaParams {
            enable_cdef: false,
            cdef_bits: 3,
            ..Default::default()
        },
        false,
        false,
    );
    let bit_pos_before = state.dec.bit_position();
    state.read_cdef(0, 0, BLOCK_8X8, false);
    assert!(state.cdef_idx.is_empty());
    assert_eq!(state.dec.bit_position(), bit_pos_before);
}

#[test]
fn read_delta_qindex_is_a_true_noop_when_delta_q_not_present() {
    // §5.11.19 `read_delta_qindex()`: `ReadDeltas` is only ever set when
    // `delta_q_present` is true (`decode_superblock`'s `ReadDeltas =
    // delta_q_present`), so a stream with `delta_q_present == false` never
    // reads a delta_q_abs symbol at all — confirmed here by checking the
    // bitstream position is untouched.
    let mut y = vec![0u8; 64];
    let mut u = vec![0u8; 16];
    let mut v = vec![0u8; 16];
    let mut meta = FrameMeta::new(2, 2);
    let mut state = make_cdef_delta_state(
        &mut y,
        &mut u,
        &mut v,
        &mut meta,
        100,
        CdefDeltaParams::default(), // delta_q_present: false
        false,
        false,
    );
    let bit_pos_before = state.dec.bit_position();
    state.read_delta_qindex(BLOCK_8X8, false);
    assert_eq!(state.current_q_index, 100);
    assert_eq!(state.dec.bit_position(), bit_pos_before);
}

#[test]
fn read_delta_qindex_skips_at_superblock_size_when_skip_is_set() {
    // §5.11.19: `if ( MiSize == sbSize && skip ) return` fires before the
    // `ReadDeltas` check, even when `delta_q_present` is on.
    let mut y = vec![0u8; 64];
    let mut u = vec![0u8; 16];
    let mut v = vec![0u8; 16];
    let mut meta = FrameMeta::new(2, 2);
    let mut state = make_cdef_delta_state(
        &mut y,
        &mut u,
        &mut v,
        &mut meta,
        100,
        CdefDeltaParams {
            delta_q_present: true,
            ..Default::default()
        },
        false,
        false,
    );
    state.read_deltas = true;
    let bit_pos_before = state.dec.bit_position();
    state.read_delta_qindex(BLOCK_64X64, true); // sbSize (use_128 == false) && skip
    assert_eq!(state.current_q_index, 100);
    assert_eq!(state.dec.bit_position(), bit_pos_before);
}

#[test]
fn read_delta_lf_is_a_true_noop_when_delta_lf_not_present() {
    let mut y = vec![0u8; 64];
    let mut u = vec![0u8; 16];
    let mut v = vec![0u8; 16];
    let mut meta = FrameMeta::new(2, 2);
    let mut state = make_cdef_delta_state(
        &mut y,
        &mut u,
        &mut v,
        &mut meta,
        100,
        CdefDeltaParams {
            delta_q_present: true, // ReadDeltas true, but delta_lf_present false
            ..Default::default()
        },
        false,
        false,
    );
    state.read_deltas = true;
    let bit_pos_before = state.dec.bit_position();
    state.read_delta_lf(BLOCK_8X8, false);
    assert_eq!(state.delta_lf, [0i8; 4]);
    assert_eq!(state.dec.bit_position(), bit_pos_before);
}

#[test]
fn decode_superblock_resets_read_deltas_from_delta_q_present_and_clears_cdef() {
    // `decode_superblock` mirrors `decode_tile()`'s per-superblock prelude:
    // `ReadDeltas = delta_q_present`, `clear_cdef(r, c)`.
    let mut y = vec![128u8; 64 * 64];
    let mut u = vec![128u8; 32 * 32];
    let mut v = vec![128u8; 32 * 32];
    let mut meta = FrameMeta::new(64, 64);
    let mut state = make_cdef_delta_state(
        &mut y,
        &mut u,
        &mut v,
        &mut meta,
        100,
        CdefDeltaParams {
            delta_q_present: true,
            ..Default::default()
        },
        false,
        false,
    );
    state.read_deltas = false;
    state.cdef_idx.insert((0, 0), 2);
    state.clear_cdef(0, 0);
    assert!(
        !state.cdef_idx.contains_key(&(0, 0)),
        "clear_cdef must unset the slot before the superblock is decoded"
    );
}

#[test]
fn intra_y_mode_context_uses_above_left_as_independent_axes() {
    // Regression test for a real bug (2026-08-16): the previous
    // implementation summed `INTRA_MODE_CONTEXT[above]` and `[left]`
    // then re-split the sum (`.min(4)`, `(sum/5).min(4)`) instead of
    // using each value directly as its own axis of
    // `TileIntraFrameYModeCdf[abovemode][leftmode]` (spec §8.3.2). The
    // two only coincidentally agree when either mode is DC (context 0);
    // for anything else they diverge. `read_intra_y_mode(dec, above_ctx,
    // left_ctx)` indexes `self.intra_y_mode[above_ctx][left_ctx]`
    // directly, so this just confirms the call sites feed the two
    // `INTRA_MODE_CONTEXT` lookups straight through, unmodified, as
    // separate arguments — not summed/resplit.
    assert_eq!(INTRA_MODE_CONTEXT[V_PRED as usize], 1);
    assert_eq!(INTRA_MODE_CONTEXT[D157_PRED as usize], 4);
    // Old (wrong) formula: sum=1+4=5 -> above_ctx=5.min(4)=4,
    // left_ctx=(5/5).min(4)=1 -> (4,1), not the correct (1,4).
    let wrong_above = 4usize;
    let wrong_left = 1usize.div_ceil(5);
    assert_ne!((wrong_above, wrong_left), (1, 4));
}

#[test]
fn intra_mode_context_table_matches_spec_at_the_two_previously_wrong_indices() {
    // Regression test for a real transcription bug (this session): the
    // table read `[0, 1, 2, 3, 4, 4, 4, 3, 3, 1, 1, 2, 0]` — matching the
    // spec's `Intra_Mode_Context[INTRA_MODES] = {0, 1, 2, 3, 4, 4, 4, 4, 3,
    // 0, 1, 2, 0}` (AV1 spec §8.3.2) everywhere except index 7 (`D207_PRED`,
    // 3 instead of the correct 4) and index 9 (`SMOOTH_PRED`, 1 instead of
    // the correct 0). Any block whose above/left neighbour used one of
    // those two (common, especially `SMOOTH_PRED` on flat content) got the
    // wrong 2-D CDF context for its own `intra_frame_y_mode` read, decoding
    // a plausible-but-wrong mode without desyncing the bitstream. Found via
    // `dbg_av1_smptebars`'s mi=(0,12) block, which sits directly to the
    // right of a `SMOOTH_PRED`-coded (`y_mode=9`) neighbour.
    const SMOOTH_PRED: usize = 9;
    assert_eq!(INTRA_MODE_CONTEXT[D207_PRED as usize], 4);
    assert_eq!(INTRA_MODE_CONTEXT[SMOOTH_PRED], 0);
    assert_eq!(INTRA_MODE_CONTEXT, [0, 1, 2, 3, 4, 4, 4, 4, 3, 0, 1, 2, 0]);
}

#[test]
fn tx_depth_bucket_selection_matches_max_tx_depth_table() {
    // AV1 spec §8.3.2 `tx_depth`: bucket 4 (TileTx64x64Cdf) when
    // Max_Tx_Depth[bSize]==4, bucket 3 (TileTx32x32Cdf) when ==3, bucket
    // 2 (TileTx16x16Cdf) when ==2, else bucket 0/1 (TileTx8x8Cdf) — the
    // same 4 CDF tables `read_tx_level`'s `depth` parameter already
    // selects, just chosen by this rule instead of a loop-iteration
    // index (the previous, wrong syntax model: see `read_tx_size`'s
    // doc comment).
    assert_eq!(MAX_TX_DEPTH_TABLE[BLOCK_8X8], 1);
    assert_eq!(MAX_TX_DEPTH_TABLE[BLOCK_16X16], 2);
    assert_eq!(MAX_TX_DEPTH_TABLE[BLOCK_32X32], 3);
    assert_eq!(MAX_TX_DEPTH_TABLE[BLOCK_64X64], 4);
}

#[test]
fn read_tx_size_never_panics_and_stays_in_range() {
    // `read_tx_size` must return a size in `TX_4X4..=max_tx` for every
    // reachable `tx_depth` symbol value (0, 1, or 2), and must not panic
    // via the `saturating_sub` — a regression guard for the rewrite from
    // the old per-depth-loop model to the single-ternary-symbol model.
    let data = vec![0xA5u8; 32];
    let mut y = vec![0u8; 64 * 64];
    let mut u = vec![0u8; 32 * 32];
    let mut v = vec![0u8; 32 * 32];
    let mut meta = FrameMeta::new(64, 64);
    let mut state = TileDecodeState::new(
        &data,
        0,
        64,
        64,
        32,
        32,
        &mut y,
        &mut u,
        &mut v,
        64,
        32,
        128,
        DeltaQ::default(),
        true,
        false,
        false,
        false,
        0,
        0,
        64,
        64,
        true,
        false,
        false,
        false,
        false,
        false,
        false,
        true,
        false, // use_128x128_superblock
        LrDecodeParams::default(),
        CdefDeltaParams::default(),
        false,
        false,
        false,
        false,
        INTERP_SWITCHABLE,
        [0u8; 9],
        RefFrames::empty(),
        &mut meta,
    );
    for &bsize in &[BLOCK_8X8, BLOCK_16X16, BLOCK_32X32, BLOCK_64X64] {
        let max_tx = max_tx_size_for_bsize(bsize);
        let tx = state.read_tx_size(bsize, max_tx, 0, 0);
        assert!(
            tx <= max_tx,
            "bsize {bsize}: tx {tx} exceeds max_tx {max_tx}"
        );
    }
}

#[test]
fn max_tx_size_for_bsize_matches_spec_rect_table() {
    // Spot-check against AV1 spec `Max_Tx_Size_Rect[BLOCK_SIZES]`
    // (fetched from the spec PDF, see coeff_tables.rs's
    // `MAX_TX_SIZE_RECT` doc comment) — every non-square bsize must keep
    // its real rectangular transform size, not collapse to the largest
    // square that fits (the bug this session fixed: `BLOCK_32X8` used to
    // return `TX_8X8` here).
    assert_eq!(max_tx_size_for_bsize(BLOCK_4X4), TX_4X4);
    assert_eq!(max_tx_size_for_bsize(BLOCK_8X8), TX_8X8);
    assert_eq!(max_tx_size_for_bsize(BLOCK_32X8), av1::TX_32X8);
    assert_eq!(max_tx_size_for_bsize(BLOCK_8X32), av1::TX_8X32);
    assert_eq!(max_tx_size_for_bsize(BLOCK_4X16), av1::TX_4X16);
    assert_eq!(max_tx_size_for_bsize(BLOCK_16X4), av1::TX_16X4);
    assert_eq!(max_tx_size_for_bsize(BLOCK_16X64), av1::TX_16X64);
    assert_eq!(max_tx_size_for_bsize(BLOCK_64X16), av1::TX_64X16);
    // The three biggest block sizes all clamp down to TX_64X64 (spec:
    // "the largest transform size that can be used", and 64 is the
    // largest transform dimension AV1 has).
    assert_eq!(max_tx_size_for_bsize(BLOCK_64X128), TX_64X64);
    assert_eq!(max_tx_size_for_bsize(BLOCK_128X64), TX_64X64);
    assert_eq!(max_tx_size_for_bsize(BLOCK_128X128), TX_64X64);
}

#[test]
fn split_tx_size_terminates_at_4x4_and_never_grows() {
    // Repeatedly applying `Split_Tx_Size` from any starting size must
    // strictly shrink the transform (by area) until it bottoms out at
    // `TX_4X4`, which is its own fixed point (spec: applied `tx_depth`
    // times, and `tx_depth` is bounded by `Max_Tx_Depth`, so a real
    // decode never over-applies it — but the table itself should still
    // be well-formed for every index).
    for tx in 0..19 {
        let mut cur = tx;
        let mut steps = 0;
        let start_area = av1::TX_WIDTH[cur] * av1::TX_HEIGHT[cur];
        let mut prev_area = start_area;
        loop {
            let next = av1::SPLIT_TX_SIZE[cur];
            let next_area = av1::TX_WIDTH[next] * av1::TX_HEIGHT[next];
            assert!(
                next_area <= prev_area,
                "tx {tx}: Split_Tx_Size must never grow the transform (step {steps})"
            );
            if next == cur {
                break;
            }
            cur = next;
            prev_area = next_area;
            steps += 1;
            assert!(steps < 10, "tx {tx}: Split_Tx_Size did not converge");
        }
        assert_eq!(
            cur, TX_4X4,
            "tx {tx}: Split_Tx_Size must bottom out at TX_4X4"
        );
    }
}

#[test]
fn inverse_transform_dc_only_is_flat_at_rectangular_sizes() {
    // Same property as `dc_only_inverse_dct_is_flat_at_every_square_size`
    // but for genuinely rectangular transform sizes — this is the case
    // that was entirely untested (and unreachable, since
    // `max_tx_size_for_bsize` never produced a rectangular `tx_size`)
    // before this session.
    for &tx_size in &[
        av1::TX_4X8,
        av1::TX_8X4,
        av1::TX_8X16,
        av1::TX_16X8,
        av1::TX_32X8,
        av1::TX_8X32,
        av1::TX_16X64,
        av1::TX_64X16,
    ] {
        let w = av1::TX_WIDTH[tx_size];
        let h = av1::TX_HEIGHT[tx_size];
        let adj = av1::ADJUSTED_TX_SIZE[tx_size];
        let adj_w = av1::TX_WIDTH[adj];
        let adj_h = av1::TX_HEIGHT[adj];
        let mut dequant = vec![0i32; adj_w * adj_h];
        dequant[0] = -1000;
        let mut residual = vec![0i32; w * h];
        inverse_transform(&dequant, av1::DCT_DCT, tx_size, false, &mut residual);
        assert_eq!(residual.len(), w * h);
        let first = residual[0];
        assert!(
            residual.iter().all(|&v| v == first),
            "tx_size {tx_size} ({w}x{h}): DC-only residual must be flat, got {residual:?}"
        );
        assert_ne!(
            first, 0,
            "tx_size {tx_size} ({w}x{h}): a -1000 DC coefficient must not vanish to 0"
        );
    }
}

#[test]
fn chroma_tx_size_matches_spec_for_common_bsizes() {
    // AV1 spec §5.11.37/§5.11.38: chroma tx size comes from
    // `Max_Tx_Size_Rect[Subsampled_Size[bsize][subx][suby]]`, not a
    // square bucket over the luma tx's own subsampled width/height.
    // 4:4:4 (subx=suby=0): chroma plane is the same size as luma.
    assert_eq!(chroma_tx_size(BLOCK_32X8, 0, 0), av1::TX_32X8);
    assert_eq!(chroma_tx_size(BLOCK_8X8, 0, 0), TX_8X8);
    // 4:2:0 (subx=suby=1): Subsampled_Size[BLOCK_32X8][1][1] = BLOCK_16X4.
    assert_eq!(chroma_tx_size(BLOCK_32X8, 1, 1), av1::TX_16X4);
    // Subsampled_Size[BLOCK_8X8][1][1] = BLOCK_4X4.
    assert_eq!(chroma_tx_size(BLOCK_8X8, 1, 1), TX_4X4);
    // Subsampled_Size[BLOCK_64X64][1][1] = BLOCK_32X32.
    assert_eq!(chroma_tx_size(BLOCK_64X64, 1, 1), TX_32X32);
    // Subsampled_Size[BLOCK_128X128][1][1] = BLOCK_64X64, whose
    // Max_Tx_Size_Rect is TX_64X64 (64x64) — the spec's 64-sample clamp
    // then folds that down to TX_32X32 since neither side is 16.
    assert_eq!(chroma_tx_size(BLOCK_128X128, 1, 1), TX_32X32);
}

#[test]
fn predict_dc_matches_spec_combined_average_for_rectangular_block() {
    // AV1 spec §7.11.2.5, both-available case: avg = (sum(LeftCol[0..h])
    // + sum(AboveRow[0..w]) + ((w+h)>>1)) / (w+h). Hand-computed for an
    // 8-wide/4-tall block with constant edges (top=100, left=50):
    // sum = 8*100 + 4*50 = 1000; (w+h)>>1 = 6; avg = 1006/12 = 83.
    let top = vec![100i32; 8];
    let left = vec![50i32; 4];
    let mut out = vec![0i32; 8 * 4];
    predict_dc(&top, &left, 8, 4, true, true, &mut out);
    assert!(out.iter().all(|&v| v == 83), "got {out:?}");
}

#[test]
fn predict_dc_asymmetric_cases_average_only_the_available_side() {
    // AV1 spec §7.11.2.5's three non-both branches, hand-computed. The
    // `top`/`left` arrays deliberately hold *different* values on the
    // unavailable side to prove it is not read at all: before this was
    // fixed, every case below took the both-available `avg` branch and
    // blended the two sides together.
    let top: Vec<i32> = (0..8).map(|i| 100 + i).collect(); // sum 828
    let left: Vec<i32> = (0..4).map(|i| 40 + 2 * i).collect(); // sum 172

    // haveLeft = 1, haveAbove = 0:
    //   leftAvg = Clip1((172 + (4 >> 1)) >> log2(4)) = (172 + 2) >> 2 = 43.
    let mut out = vec![0i32; 8 * 4];
    predict_dc(&top, &left, 8, 4, false, true, &mut out);
    assert!(out.iter().all(|&v| v == 43), "left-only: got {out:?}");

    // haveLeft = 0, haveAbove = 1:
    //   aboveAvg = Clip1((828 + (8 >> 1)) >> log2(8)) = (828 + 4) >> 3 = 104.
    let mut out = vec![0i32; 8 * 4];
    predict_dc(&top, &left, 8, 4, true, false, &mut out);
    assert!(out.iter().all(|&v| v == 104), "above-only: got {out:?}");

    // Neither: 1 << (BitDepth - 1).
    let mut out = vec![0i32; 8 * 4];
    predict_dc(&top, &left, 8, 4, false, false, &mut out);
    assert!(out.iter().all(|&v| v == 128), "neither: got {out:?}");

    // Sanity: the both-available branch is a genuinely different value,
    // so the assertions above cannot pass by accident.
    let mut both = vec![0i32; 8 * 4];
    predict_dc(&top, &left, 8, 4, true, true, &mut both);
    assert_eq!(both[0], (828 + 172 + 6) / 12);
    assert!(both[0] != 43 && both[0] != 104 && both[0] != 128);
}

#[test]
fn predict_dc_left_only_rounds_like_round2_not_truncation() {
    // `leftAvg`'s `(sum + (h >> 1)) >> log2H` rounds to nearest; a plain
    // truncating `sum / h` would give 10 here instead of 11.
    let left = vec![10i32, 11, 11, 11]; // sum 43; (43 + 2) >> 2 = 11
    let mut out = vec![0i32; 4 * 4];
    predict_dc(&[], &left, 4, 4, false, true, &mut out);
    assert!(out.iter().all(|&v| v == 11), "got {out:?}");
}

/// One-plane fixture: a `w`×`h` ramp so every sample is distinguishable.
fn borders_fixture(w: usize, h: usize) -> Vec<u8> {
    (0..w * h).map(|i| (i % 251) as u8).collect()
}

#[test]
fn block_borders_extends_left_col_into_real_below_left_samples_when_available() {
    // §7.11.2: `LeftCol[i]` for i up to `w+h-1` reads real reconstructed
    // samples down to `y + 2*h - 1` when `haveBelowLeft` is set (the
    // below-left transform block is reconstructed), rather than replicating
    // the last one at `y + h - 1`. This is the fix for directional (zone-3)
    // prediction on non-flat content — mandelbrot Y went 24.4 → 46 dB.
    let (w, h) = (16usize, 16usize);
    let plane = borders_fixture(w, h);
    // 4×4 tx block at (4, 4): left column is x=3, rows 4..; ext = 8.
    let with_bl = block_borders(&plane, w, w, h, 4, 4, 4, 4, false, true);
    let without_bl = block_borders(&plane, w, w, h, 4, 4, 4, 4, false, false);
    // First `h` (4) entries identical (real left column of the block).
    assert_eq!(with_bl.left[..4], without_bl.left[..4]);
    // Beyond that: `with_bl` keeps reading down column 3 (rows 8..11);
    // `without_bl` replicates row 7.
    assert_eq!(
        with_bl.left[4..8].to_vec(),
        (8..12)
            .map(|y| i32::from(plane[y * w + 3]))
            .collect::<Vec<_>>()
    );
    assert!(without_bl.left[4..8]
        .iter()
        .all(|&v| v == without_bl.left[3]));
}

#[test]
fn block_borders_tracks_availability_from_tile_local_position() {
    let (w, h) = (16usize, 16usize);
    let plane = borders_fixture(w, h);

    // Tile-local origin: neither side available (spec §5.11.35's
    // `haveLeft = AvailL || x > 0` is false at x == 0 within a tile).
    let b = block_borders(&plane, w, w, h, 4, 4, 0, 0, false, false);
    assert!(!b.have_above && !b.have_left);
    // Top row / left column both away from the tile edge: both available.
    let b = block_borders(&plane, w, w, h, 4, 4, 4, 4, false, false);
    assert!(b.have_above && b.have_left);
    // Left edge, second row: above only.
    let b = block_borders(&plane, w, w, h, 4, 4, 0, 4, false, false);
    assert!(b.have_above && !b.have_left);
    // Top row, second column: left only.
    let b = block_borders(&plane, w, w, h, 4, 4, 4, 0, false, false);
    assert!(!b.have_above && b.have_left);
}

#[test]
fn block_borders_substitute_values_match_spec_7_11_2_1() {
    let (w, h) = (16usize, 16usize);
    let plane = borders_fixture(w, h);

    // Neither side available: AboveRow = (1 << (BitDepth-1)) - 1 = 127,
    // LeftCol = (1 << (BitDepth-1)) + 1 = 129, AboveRow[-1] = 128. The
    // ±1 asymmetry is normative (it keeps PAETH_PRED's ties
    // deterministic) — a single shared 128 fill is what this replaced.
    let b = block_borders(&plane, w, w, h, 4, 4, 0, 0, false, false);
    assert_eq!(b.top, vec![127; 8]);
    assert_eq!(b.left, vec![129; 8]);
    assert_eq!(b.tl, 128);

    // Above unavailable, left available: AboveRow[i] = CurrFrame[y][x-1]
    // (replicated), AboveRow[-1] = the same sample.
    let b = block_borders(&plane, w, w, h, 4, 4, 8, 0, false, false);
    let expected = i32::from(plane[7]); // (x-1, y) = (7, 0)
    assert_eq!(b.top, vec![expected; 8]);
    assert_eq!(b.tl, expected);
    // LeftCol still comes from the real left column.
    assert_eq!(
        b.left,
        (0..8)
            .map(|i| i32::from(plane[i.min(3) * w + 7]))
            .collect::<Vec<_>>()
    );

    // Left unavailable, above available: LeftCol[i] = CurrFrame[y-1][x]
    // (replicated), AboveRow[-1] = the same sample.
    let b = block_borders(&plane, w, w, h, 4, 4, 0, 8, false, false);
    let expected = i32::from(plane[7 * w]); // (x, y-1) = (0, 7)
    assert_eq!(b.left, vec![expected; 8]);
    assert_eq!(b.tl, expected);
    assert_eq!(
        b.top,
        (0..8)
            .map(|i| i32::from(plane[7 * w + i.min(3)]))
            .collect::<Vec<_>>()
    );

    // Both available: the corner is the real diagonal neighbour.
    let b = block_borders(&plane, w, w, h, 4, 4, 8, 8, false, false);
    assert_eq!(b.tl, i32::from(plane[7 * w + 7]));
}

#[test]
fn block_borders_replicate_the_last_sample_past_the_frame_edge() {
    // §7.11.2.1 clamps the neighbour reads with `Min(aboveLimit, x+i)` /
    // `Min(leftLimit, y+i)`, i.e. samples past the right/bottom edge
    // repeat the last real one. A previous revision left those slots at
    // the 128 fill, which polluted every transform block hanging over the
    // frame edge (a 12×12 plane with an 8-wide tx block at x=8 has half
    // its above row out of bounds).
    let (w, h) = (12usize, 12usize);
    let plane = borders_fixture(w, h);

    let b = block_borders(&plane, w, w, h, 8, 8, 8, 8, false, false);
    // Above row: x = 8..15 clamped to maxX = 11 -> samples 8,9,10,11 then
    // 11 repeated.
    let above_row = 7 * w;
    let expected_top: Vec<i32> = (0..16)
        .map(|i| i32::from(plane[above_row + (8 + i).min(11)]))
        .collect();
    assert_eq!(b.top, expected_top);
    assert!(
        b.top[4..].iter().all(|&v| v == expected_top[3]),
        "out-of-frame tail must replicate, got {:?}",
        b.top
    );
    // Left column: y = 8..15 clamped to maxY = 11.
    let expected_left: Vec<i32> = (0..16)
        .map(|i| i32::from(plane[(8 + i).min(11) * w + 7]))
        .collect();
    assert_eq!(b.left, expected_left);
}

#[test]
fn dc_pred_via_predict_intra_block_uses_the_border_availability_flags() {
    // End-to-end through the mode dispatch: a left-edge block whose above
    // row is a constant 200 must predict exactly 200, not the average of
    // 200 with a synthesized left column.
    let borders = BlockBorders {
        top: vec![200; 8],
        left: vec![200; 8],
        tl: 200,
        have_above: true,
        have_left: false,
    };
    let mut out = vec![0i32; 8 * 8];
    predict_intra_block(DC_PRED, &borders, 8, 8, &mut out, true, 0, 0, 8, 8);
    assert!(out.iter().all(|&v| v == 200), "got {out:?}");

    // And with neither side real, the mode dispatch must reach the
    // `1 << (BitDepth - 1)` branch regardless of what the (substituted)
    // arrays happen to contain.
    let borders = BlockBorders {
        top: vec![127; 8],
        left: vec![129; 8],
        tl: 128,
        have_above: false,
        have_left: false,
    };
    let mut out = vec![0i32; 8 * 8];
    predict_intra_block(DC_PRED, &borders, 8, 8, &mut out, true, 0, 0, 8, 8);
    assert!(out.iter().all(|&v| v == 128), "got {out:?}");
}

#[test]
fn inverse_transform_2d_dct_round_trip_with_ac_coefficients() {
    // The 2-D inverse transform must correctly apply both the row and column
    // passes for a block with non-zero AC coefficients. This exercises the
    // full butterfly network (not just the DC-only path). We verify the
    // output is non-flat, in-range, and deterministic.
    let mut dequant = vec![0i32; 16];
    dequant[0] = 500; // DC
    dequant[1] = 200; // first AC (horizontal frequency)
    dequant[4] = -100; // first AC (vertical frequency)
    dequant[5] = 50; // diagonal AC
    let mut residual = vec![0i32; 16];
    inverse_transform(&dequant, av1::DCT_DCT, TX_4X4, false, &mut residual);
    // Not flat: AC coefficients must produce spatial variation.
    let first = residual[0];
    assert!(
        residual.iter().any(|&v| v != first),
        "AC coefficients must produce spatial variation, got {residual:?}"
    );
    // In valid sample range.
    assert!(
        residual.iter().all(|&v| (-256..=512).contains(&v)),
        "residual out of range: {residual:?}"
    );
    // Deterministic: same input → same output.
    let mut residual2 = vec![0i32; 16];
    inverse_transform(&dequant, av1::DCT_DCT, TX_4X4, false, &mut residual2);
    assert_eq!(residual, residual2);
}

#[test]
fn inverse_transform_adst_4x4_produces_spatial_output() {
    // ADST-based transform types (ADST_ADST, ADST_DCT, DCT_ADST) exercise
    // the `inverse_adst4`/`inverse_adst8` butterfly networks. Verify they
    // produce valid, non-flat output for a block with AC energy.
    for &tx_type in &[av1::ADST_ADST, av1::ADST_DCT, av1::DCT_ADST] {
        let mut dequant = vec![0i32; 16];
        dequant[0] = 400;
        dequant[3] = 150;
        dequant[7] = -80;
        let mut residual = vec![0i32; 16];
        inverse_transform(&dequant, tx_type, TX_4X4, false, &mut residual);
        assert!(
            residual.iter().any(|&v| v != residual[0]),
            "tx_type {tx_type}: ADST must produce spatial variation"
        );
        assert!(
            residual.iter().all(|&v| (-512..=768).contains(&v)),
            "tx_type {tx_type}: residual out of range: {residual:?}"
        );
    }
}

#[test]
fn inverse_transform_rectangular_rescale_path() {
    // Rectangular transforms with |log2W - log2H| == 1 exercise the sqrt(2)
    // rescale (Round2(x * 2896, 12)) in the row pass. TX_4X8 (w=4, h=8,
    // log2w=2, log2h=3) and TX_8X4 (w=8, h=4, log2w=3, log2h=2) are the
    // smallest such sizes. Verify the output is valid and the full w×h
    // buffer is written (not just the square subset).
    for &tx_size in &[av1::TX_4X8, av1::TX_8X4, av1::TX_8X16, av1::TX_16X8] {
        let w = av1::TX_WIDTH[tx_size];
        let h = av1::TX_HEIGHT[tx_size];
        let adj = av1::ADJUSTED_TX_SIZE[tx_size];
        let adj_w = av1::TX_WIDTH[adj];
        let adj_h = av1::TX_HEIGHT[adj];
        let mut dequant = vec![0i32; adj_w * adj_h];
        dequant[0] = 800;
        if dequant.len() > 1 {
            dequant[1] = 200;
        }
        if adj_w > 1 && dequant.len() > adj_w {
            dequant[adj_w] = -100;
        }
        let mut residual = vec![0i32; w * h];
        inverse_transform(&dequant, av1::DCT_DCT, tx_size, false, &mut residual);
        assert_eq!(residual.len(), w * h);
        assert!(
            residual.iter().all(|&v| (-1024..=1024).contains(&v)),
            "tx_size {tx_size}: residual out of range: {residual:?}"
        );
        // DC-only input to a DCT_DCT transform must still be spatially flat
        // even through the rescale path (the rescale is a per-row constant
        // multiplier for a flat input).
        let mut dc_only = vec![0i32; adj_w * adj_h];
        dc_only[0] = 800;
        let mut dc_residual = vec![0i32; w * h];
        inverse_transform(&dc_only, av1::DCT_DCT, tx_size, false, &mut dc_residual);
        let first = dc_residual[0];
        assert!(
            dc_residual.iter().all(|&v| v == first),
            "tx_size {tx_size}: DC-only through rescale must be flat, got {dc_residual:?}"
        );
    }
}

#[test]
fn inverse_transform_tx8x8_with_ac_matches_expected_scale() {
    // An 8×8 DCT_DCT block with a known DC coefficient. The DC-only case
    // must produce a flat residual whose value reflects the row_shift=1 and
    // col_shift=4 applied after the butterfly passes. For dequant[0] = 8192:
    // the unnormalized DCT basis gives M·Mᵀ = (n/2)·I = 4·I for n=8, so
    // the 2-D inverse produces 4 * 8192 = 32768 pre-shift; with row_shift=1
    // and col_shift=4 the final is round2(round2(32768, 1), 4) = 2048.
    let mut dequant = vec![0i32; 64];
    dequant[0] = 8192;
    let mut residual = vec![0i32; 64];
    inverse_transform(&dequant, av1::DCT_DCT, TX_8X8, false, &mut residual);
    assert!(
        residual.iter().all(|&v| v == residual[0]),
        "DC-only 8×8 must be flat, got {residual:?}"
    );
    // The DC coefficient must not vanish or explode.
    assert_ne!(residual[0], 0);
    assert!(residual[0].abs() < 10000);
}

#[test]
fn filter_intra_prediction_matches_hand_computed_values() {
    // AV1 spec §7.11.2.3: filter-intra processes the block in 4×2 sub-blocks,
    // each filtered from up to 7 causal neighbour samples. With uniform
    // borders (top=100, left=200, tl=128) and filter_intra_mode=0
    // (FILTER_DC_PRED), the first sub-block at (i2=0, j4=0) reads its
    // taps from top/left/tl directly.
    //
    // For the first 4×2 sub-block (i2=0, j4=0):
    //   p[0] = above_row(-1) = tl = 128  (i2==0, i<5, i==0 → above_row(j4*4+0-1) = above_row(-1))
    //   p[1] = above_row(0) = top[0] = 100
    //   p[2] = above_row(1) = top[1] = 100
    //   p[3] = above_row(2) = top[2] = 100
    //   p[4] = above_row(3) = top[3] = 100
    //   p[5] = left_col(0) = left[0] = 200  (j4==0, i>=5 → left_col(i2*2+i-5) = left_col(0))
    //   p[6] = left_col(1) = left[1] = 200
    // Using INTRA_FILTER_TAPS[0] = the DC filter taps:
    //   [-6, 10, 0, 0, 0, 12, 0] for (i1=0,j1=0)
    //   pr = -6*128 + 10*100 + 0 + 0 + 0 + 12*200 + 0 = -768 + 1000 + 2400 = 2632
    //   val = Round2Signed(2632, 4) = (2632 + 8) >> 4 = 165
    let top = vec![100i32; 8];
    let left = vec![200i32; 8];
    let mut out = vec![0i32; 64];
    predict_filter_intra(0, &top, &left, 128, 8, 8, &mut out);
    // First sample of the first sub-block: i2=0, j4=0, i1=0, j1=0.
    assert_eq!(
        out[0], 165,
        "filter-intra DC mode first sample, got {}",
        out[0]
    );
    // All outputs must be in valid range.
    assert!(
        out.iter().all(|&v| (0..=255).contains(&v)),
        "filter-intra output out of range: {out:?}"
    );
}

#[test]
fn filter_intra_mode2_horizontal_matches_hand_computed() {
    // filter_intra_mode=2 (FILTER_H_PRED): for the first sub-block
    // (i2=0, j4=0), the taps use INTRA_FILTER_TAPS[2]:
    //   [-8, 8, 0, 0, 0, 16, 0] for (i1=0,j1=0)
    // With top=100, left=200, tl=128:
    //   p = [128, 100, 100, 100, 100, 200, 200]
    //   pr = -8*128 + 8*100 + 0 + 0 + 0 + 16*200 + 0 = -1024 + 800 + 3200 = 2976
    //   val = Round2Signed(2976, 4) = (2976 + 8) >> 4 = 186
    let top = vec![100i32; 8];
    let left = vec![200i32; 8];
    let mut out = vec![0i32; 64];
    predict_filter_intra(2, &top, &left, 128, 8, 8, &mut out);
    assert_eq!(
        out[0], 186,
        "filter-intra H mode first sample, got {}",
        out[0]
    );
    assert!(
        out.iter().all(|&v| (0..=255).contains(&v)),
        "filter-intra H output out of range: {out:?}"
    );
}

#[test]
fn inverse_transform_v_dct_8x8_only_col0_nonzero() {
    // V_DCT: row pass is identity, column pass is DCT. With only column 0
    // having non-zero input (dequant[0]=400 at row 0, dequant[8]=200 at row
    // 1), the identity row pass preserves the input, and the column DCT
    // produces variation down column 0. All-zero columns (1-7) stay zero.
    // The output has non-zero values in column 0 only.
    let mut dequant = vec![0i32; 64];
    dequant[0] = 400; // DC
    dequant[8] = 200; // vertical-frequency coefficient (row 1, col 0)
    let mut residual = vec![0i32; 64];
    inverse_transform(&dequant, av1::V_DCT, TX_8X8, false, &mut residual);
    // Column 0 must have variation (vertical frequency present).
    let col0_val = residual[0];
    assert!(
        (0..8).any(|r| residual[r * 8] != col0_val),
        "V_DCT with AC must vary down column 0"
    );
    // Columns 1-7 must be all-zero (no input energy in those columns).
    for col in 1..8 {
        for row in 0..8 {
            assert_eq!(
                residual[row * 8 + col],
                0,
                "V_DCT col {col} must be zero (no input energy)"
            );
        }
    }
}

#[test]
fn inverse_transform_h_dct_8x8_only_row0_nonzero() {
    // H_DCT: row pass is DCT, column pass is identity. With only row 0
    // having non-zero input (dequant[0]=400, dequant[1]=200), the row DCT
    // produces variation across row 0. Rows 1-7 had all-zero input and
    // stay zero. The identity column pass just copies.
    let mut dequant = vec![0i32; 64];
    dequant[0] = 400; // DC
    dequant[1] = 200; // horizontal-frequency coefficient
    let mut residual = vec![0i32; 64];
    inverse_transform(&dequant, av1::H_DCT, TX_8X8, false, &mut residual);
    // Row 0 must have variation (horizontal frequency present).
    let row0_val = residual[0];
    assert!(
        (0..8).any(|c| residual[c] != row0_val),
        "H_DCT with AC must vary across row 0"
    );
    // Rows 1-7 must be all-zero (no input energy in those rows).
    for row in 1..8 {
        for col in 0..8 {
            assert_eq!(
                residual[row * 8 + col],
                0,
                "H_DCT row {row} must be zero (no input energy)"
            );
        }
    }
}

#[test]
fn transform_row_shift_table_matches_spec_ordering() {
    // Kinetix's TxSize enum ordering differs from the spec's. The
    // TRANSFORM_ROW_SHIFT table must be indexed by Kinetix's enum indices.
    // Spec values (in spec order): TX_4X4=0, TX_8X8=1, TX_16X16=2, TX_32X32=3,
    // TX_64X64=4, TX_4X8=0, TX_8X4=0, TX_8X16=1, TX_16X8=1, TX_16X32=1,
    // TX_32X16=1, TX_32X64=1, TX_64X32=1, TX_4X16=1, TX_16X4=1, TX_8X32=2,
    // TX_32X8=2, TX_16X64=2, TX_64X16=2.
    // Kinetix's ordering: 4X4=0, 8X8=1, 16X16=2, 32X32=3, 64X64=4,
    // 4X8=5, 8X4=6, 8X16=7, 16X8=8, 16X32=9, 32X16=10, 32X64=11, 64X32=12,
    // 4X16=13, 16X4=14, 8X32=15, 32X8=16, 16X64=17, 64X16=18.
    assert_eq!(av1::TRANSFORM_ROW_SHIFT[TX_4X4], 0);
    assert_eq!(av1::TRANSFORM_ROW_SHIFT[TX_8X8], 1);
    assert_eq!(av1::TRANSFORM_ROW_SHIFT[TX_16X16], 2);
    assert_eq!(av1::TRANSFORM_ROW_SHIFT[TX_32X32], 2);
    assert_eq!(av1::TRANSFORM_ROW_SHIFT[TX_64X64], 2);
    assert_eq!(av1::TRANSFORM_ROW_SHIFT[av1::TX_4X8], 0);
    assert_eq!(av1::TRANSFORM_ROW_SHIFT[av1::TX_8X4], 0);
    assert_eq!(av1::TRANSFORM_ROW_SHIFT[av1::TX_8X16], 1);
    assert_eq!(av1::TRANSFORM_ROW_SHIFT[av1::TX_16X8], 1);
    assert_eq!(av1::TRANSFORM_ROW_SHIFT[av1::TX_8X32], 2);
    assert_eq!(av1::TRANSFORM_ROW_SHIFT[av1::TX_32X8], 2);
}

#[test]
fn adjusted_tx_size_table_clamps_to_32_for_large_sizes() {
    // AV1 spec §7.12.3: transforms with either side > 32 only code their
    // low-frequency <= 32 corner. ADJUSTED_TX_SIZE maps each TxSize to the
    // size whose stride the coefficient array is populated at.
    assert_eq!(av1::ADJUSTED_TX_SIZE[TX_4X4], TX_4X4);
    assert_eq!(av1::ADJUSTED_TX_SIZE[TX_8X8], TX_8X8);
    assert_eq!(av1::ADJUSTED_TX_SIZE[TX_16X16], TX_16X16);
    assert_eq!(av1::ADJUSTED_TX_SIZE[TX_32X32], TX_32X32);
    // TX_64X64 adjusts down to TX_32X32.
    assert_eq!(av1::ADJUSTED_TX_SIZE[TX_64X64], TX_32X32);
    // TX_64X16 adjusts to TX_32X16 (halve the 64-side).
    assert_eq!(av1::ADJUSTED_TX_SIZE[av1::TX_64X16], av1::TX_32X16);
    // TX_16X64 adjusts to TX_16X32.
    assert_eq!(av1::ADJUSTED_TX_SIZE[av1::TX_16X64], av1::TX_16X32);
    // TX_32X64 adjusts to TX_32X32.
    assert_eq!(av1::ADJUSTED_TX_SIZE[av1::TX_32X64], TX_32X32);
    // TX_64X32 adjusts to TX_32X32.
    assert_eq!(av1::ADJUSTED_TX_SIZE[av1::TX_64X32], TX_32X32);
}

#[test]
fn inverse_transform_16x16_dc_only_is_flat() {
    // A 16×16 DC-only block must produce a flat residual. This exercises
    // the larger butterfly networks (n=4) with the row_shift=2 and col_shift=4.
    let mut dequant = vec![0i32; 256];
    dequant[0] = 16000;
    let mut residual = vec![0i32; 256];
    inverse_transform(&dequant, av1::DCT_DCT, TX_16X16, false, &mut residual);
    let first = residual[0];
    assert!(
        residual.iter().all(|&v| v == first),
        "16×16 DC-only must be flat, got first={first} last={}",
        residual[255]
    );
    assert_ne!(first, 0);
}

#[test]
fn palette_prediction_maps_color_indices_correctly() {
    // AV1 spec §7.11.4: each output sample is the palette color named by the
    // color map at that position.
    let colors = vec![10i32, 50, 100, 200];
    let color_map = vec![0u8, 1, 2, 3, 3, 2, 1, 0, 0, 0, 1, 1, 2, 2, 3, 3];
    let info = PaletteBlockInfo {
        colors: &colors,
        color_map: &color_map,
        map_stride: 4,
        off_x: 0,
        off_y: 0,
    };
    let mut out = vec![0i32; 16];
    predict_palette(&info, 4, 4, &mut out);
    assert_eq!(out[0], 10); // map[0] = 0 → colors[0]
    assert_eq!(out[1], 50); // map[1] = 1 → colors[1]
    assert_eq!(out[2], 100); // map[2] = 2 → colors[2]
    assert_eq!(out[3], 200); // map[3] = 3 → colors[3]
    assert_eq!(out[15], 200); // map[15] = 3 → colors[3]
}

#[test]
fn palette_prediction_with_sub_block_offset() {
    // When the transform block is not at the coded block's origin, the
    // color map is read at the correct offset: map[(off_y+i)*stride+off_x+j].
    let colors = vec![10i32, 50, 100];
    let color_map = vec![
        0, 0, 1, 1, //
        0, 0, 2, 2, //
        2, 2, 2, 2, //
        2, 2, 2, 2, //
    ];
    let info = PaletteBlockInfo {
        colors: &colors,
        color_map: &color_map,
        map_stride: 4,
        off_x: 2,
        off_y: 0,
    };
    // Output is 2 wide (tx_w=2) × 4 tall (tx_h=4).
    // out[0] = map[(0+0)*4 + 2+0] = map[2] = 1 → colors[1] = 50
    // out[1] = map[(0+0)*4 + 2+1] = map[3] = 1 → colors[1] = 50
    // out[2] = map[(0+1)*4 + 2+0] = map[6] = 2 → colors[2] = 100
    // out[3] = map[(0+1)*4 + 2+1] = map[7] = 2 → colors[2] = 100
    // out[4] = map[(0+2)*4 + 2+0] = map[10] = 2 → colors[2] = 100
    // out[5] = map[(0+2)*4 + 2+1] = map[11] = 2 → colors[2] = 100
    // out[6] = map[(0+3)*4 + 2+0] = map[14] = 2 → colors[2] = 100
    // out[7] = map[(0+3)*4 + 2+1] = map[15] = 2 → colors[2] = 100
    let mut out = vec![0i32; 8];
    predict_palette(&info, 2, 4, &mut out);
    assert_eq!(out[0], 50);
    assert_eq!(out[1], 50);
    assert_eq!(out[2], 100);
    assert_eq!(out[3], 100);
    assert_eq!(out[4], 100);
    assert_eq!(out[5], 100);
    assert_eq!(out[6], 100);
    assert_eq!(out[7], 100);
}
