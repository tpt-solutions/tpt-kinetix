//! VP9 inverse transforms (§8.5): IDCT/iADST of sizes 4/8/16 and the IDCT-32,
//! plus the lossless Walsh–Hadamard transform, added into the prediction with
//! the reference decoder's exact rounding.

/// Transform types (spec §8.5.1 / reference enum ordering).
pub const DCT_DCT: usize = 0;
pub const ADST_DCT: usize = 1;
pub const DCT_ADST: usize = 2;
pub const ADST_ADST: usize = 3;

const C1: i64 = 8192; // 1 << 13 rounding used inside the 1-D passes

#[inline]
fn rsh14(v: i64) -> i32 {
    ((v + C1) >> 14) as i32
}

fn idct4_1d(inp: &[i32], out: &mut [i32]) {
    let i = |x: usize| inp[x];
    let t0 = rsh14((i(0) + i(2)) as i64 * 11585);
    let t1 = rsh14((i(0) - i(2)) as i64 * 11585);
    let t2 = rsh14(i(1) as i64 * 6270 - i(3) as i64 * 15137);
    let t3 = rsh14(i(1) as i64 * 15137 + i(3) as i64 * 6270);
    out[0] = t0 + t3;
    out[1] = t1 + t2;
    out[2] = t1 - t2;
    out[3] = t0 - t3;
}

fn iadst4_1d(inp: &[i32], out: &mut [i32]) {
    let i = |x: usize| inp[x] as i64;
    let t0 = 5283 * i(0) + 15212 * i(2) + 9929 * i(3);
    let t1 = 9929 * i(0) - 5283 * i(2) - 15212 * i(3);
    let t2 = 13377 * (i(0) - i(2) + i(3));
    let t3 = 13377 * i(1);
    out[0] = ((t0 + t3 + C1) >> 14) as i32;
    out[1] = ((t1 + t3 + C1) >> 14) as i32;
    out[2] = ((t2 + C1) >> 14) as i32;
    out[3] = ((t0 + t1 - t3 + C1) >> 14) as i32;
}

fn idct8_1d(inp: &[i32], out: &mut [i32]) {
    let i = |x: usize| inp[x] as i64;
    let t0a = rsh14((i(0) + i(4)) * 11585);
    let t1a = rsh14((i(0) - i(4)) * 11585);
    let t2a = rsh14(i(2) * 6270 - i(6) * 15137);
    let t3a = rsh14(i(2) * 15137 + i(6) * 6270);
    let t4a = rsh14(i(1) * 3196 - i(7) * 16069);
    let t5a = rsh14(i(5) * 13623 - i(3) * 9102);
    let t6a = rsh14(i(5) * 9102 + i(3) * 13623);
    let t7a = rsh14(i(1) * 16069 + i(7) * 3196);

    let t0 = t0a + t3a;
    let t1 = t1a + t2a;
    let t2 = t1a - t2a;
    let t3 = t0a - t3a;
    let t4 = t4a + t5a;
    let t5a = t4a - t5a;
    let t7 = t7a + t6a;
    let t6a = t7a - t6a;
    let t5 = rsh14((t6a - t5a) as i64 * 11585);
    let t6 = rsh14((t6a + t5a) as i64 * 11585);

    out[0] = t0 + t7;
    out[1] = t1 + t6;
    out[2] = t2 + t5;
    out[3] = t3 + t4;
    out[4] = t3 - t4;
    out[5] = t2 - t5;
    out[6] = t1 - t6;
    out[7] = t0 - t7;
}

fn iadst8_1d(inp: &[i32], out: &mut [i32]) {
    // Literal port of libvpx iadst8_c, including its input permutation
    // (x0 = in[7], x1 = in[0], ...) and per-stage rounding.
    let mut x0 = inp[7] as i64;
    let mut x1 = inp[0] as i64;
    let mut x2 = inp[5] as i64;
    let mut x3 = inp[2] as i64;
    let mut x4 = inp[3] as i64;
    let mut x5 = inp[4] as i64;
    let mut x6 = inp[1] as i64;
    let mut x7 = inp[6] as i64;

    // stage 1
    let s0 = 16305 * x0 + 1606 * x1;
    let s1 = 1606 * x0 - 16305 * x1;
    let s2 = 14449 * x2 + 7723 * x3;
    let s3 = 7723 * x2 - 14449 * x3;
    let s4 = 10394 * x4 + 12665 * x5;
    let s5 = 12665 * x4 - 10394 * x5;
    let s6 = 4756 * x6 + 15679 * x7;
    let s7 = 15679 * x6 - 4756 * x7;
    x0 = i64::from(rsh14(s0 + s4));
    x1 = i64::from(rsh14(s1 + s5));
    x2 = i64::from(rsh14(s2 + s6));
    x3 = i64::from(rsh14(s3 + s7));
    x4 = i64::from(rsh14(s0 - s4));
    x5 = i64::from(rsh14(s1 - s5));
    x6 = i64::from(rsh14(s2 - s6));
    x7 = i64::from(rsh14(s3 - s7));

    // stage 2
    let s0 = x0;
    let s1 = x1;
    let s2 = x2;
    let s3 = x3;
    let s4 = 15137 * x4 + 6270 * x5;
    let s5 = 6270 * x4 - 15137 * x5;
    let s6 = -6270 * x6 + 15137 * x7;
    let s7 = 15137 * x6 + 6270 * x7;
    let u0 = s0 + s2;
    let u1 = s1 + s3;
    let v2 = s0 - s2;
    let v3 = s1 - s3;
    let x4 = i64::from(rsh14(s4 + s6));
    let x5 = i64::from(rsh14(s5 + s7));
    let x6 = i64::from(rsh14(s4 - s6));
    let x7 = i64::from(rsh14(s5 - s7));

    // stage 3
    let x2 = i64::from(rsh14(11585 * (v2 + v3)));
    let x3 = i64::from(rsh14(11585 * (v2 - v3)));
    let x6n = i64::from(rsh14(11585 * (x6 + x7)));
    let x7n = i64::from(rsh14(11585 * (x6 - x7)));
    let (x6, x7) = (x6n, x7n);

    out[0] = u0 as i32;
    out[1] = -x4 as i32;
    out[2] = x6 as i32;
    out[3] = -x2 as i32;
    out[4] = x3 as i32;
    out[5] = -x7 as i32;
    out[6] = x5 as i32;
    out[7] = -u1 as i32;
}

fn idct16_1d(inp: &[i32], out: &mut [i32]) {
    let i = |x: usize| inp[x] as i64;
    let r = |v: i64| rsh14(v);

    let a0 = r((i(0) + i(8)) * 11585);
    let a1 = r((i(0) - i(8)) * 11585);
    let a2 = r(i(4) * 6270 - i(12) * 15137);
    let a3 = r(i(4) * 15137 + i(12) * 6270);
    let a4 = r(i(2) * 3196 - i(14) * 16069);
    let a5 = r(i(10) * 13623 - i(6) * 9102);
    let a6 = r(i(10) * 9102 + i(6) * 13623);
    let a7 = r(i(2) * 16069 + i(14) * 3196);
    let a8 = r(i(1) * 1606 - i(15) * 16305);
    let a9 = r(i(9) * 12665 - i(7) * 10394);
    let a10 = r(i(5) * 7723 - i(11) * 14449);
    let a11 = r(i(13) * 15679 - i(3) * 4756);
    let a12 = r(i(13) * 4756 + i(3) * 15679);
    let a13 = r(i(5) * 14449 + i(11) * 7723);
    let a14 = r(i(9) * 10394 + i(7) * 12665);
    let a15 = r(i(1) * 16305 + i(15) * 1606);

    let t0 = a0 + a3;
    let t1 = a1 + a2;
    let t2 = a1 - a2;
    let t3 = a0 - a3;
    let t4 = a4 + a5;
    let t5 = a4 - a5;
    let t6 = a7 - a6;
    let t7 = a7 + a6;
    let t8 = a8 + a9;
    let t9 = a8 - a9;
    let t10 = a11 - a10;
    let t11 = a11 + a10;
    let t12 = a12 + a13;
    let t13 = a12 - a13;
    let t14 = a15 - a14;
    let t15 = a15 + a14;

    let t5a = r((t6 - t5) as i64 * 11585);
    let t6a = r((t6 + t5) as i64 * 11585);
    let t9a = r(t14 as i64 * 6270 - t9 as i64 * 15137);
    let t14a = r(t14 as i64 * 15137 + t9 as i64 * 6270);
    let t10a = r(-(t13 as i64 * 15137 + t10 as i64 * 6270));
    let t13a = r(t13 as i64 * 6270 - t10 as i64 * 15137);

    let t0a = t0 + t7;
    let t1a = t1 + t6a;
    let t2a = t2 + t5a;
    let t3a = t3 + t4;
    let t4 = t3 - t4;
    let t5 = t2 - t5a;
    let t6 = t1 - t6a;
    let t7 = t0 - t7;
    let t8a = t8 + t11;
    let t9 = t9a + t10a;
    let t10 = t9a - t10a;
    let t11a = t8 - t11;
    let t12a = t15 - t12;
    let t13 = t14a - t13a;
    let t14 = t14a + t13a;
    let t15a = t15 + t12;

    let t10a = r((t13 - t10) as i64 * 11585);
    let t13a = r((t13 + t10) as i64 * 11585);
    let t11 = r((t12a - t11a) as i64 * 11585);
    let t12 = r((t12a + t11a) as i64 * 11585);

    out[0] = t0a + t15a;
    out[1] = t1a + t14;
    out[2] = t2a + t13a;
    out[3] = t3a + t12;
    out[4] = t4 + t11;
    out[5] = t5 + t10a;
    out[6] = t6 + t9;
    out[7] = t7 + t8a;
    out[8] = t7 - t8a;
    out[9] = t6 - t9;
    out[10] = t5 - t10a;
    out[11] = t4 - t11;
    out[12] = t3a - t12;
    out[13] = t2a - t13a;
    out[14] = t1a - t14;
    out[15] = t0a - t15a;
}

fn idct32_1d(inp: &[i32], out: &mut [i32]) {
    let i = |x: usize| inp[x] as i64;
    let r = |v: i64| rsh14(v);
    let mut a = [0i32; 32];

    a[0] = r((i(0) + i(16)) * 11585);
    a[1] = r((i(0) - i(16)) * 11585);
    a[2] = r(i(8) * 6270 - i(24) * 15137);
    a[3] = r(i(8) * 15137 + i(24) * 6270);
    a[4] = r(i(4) * 3196 - i(28) * 16069);
    a[5] = r(i(20) * 13623 - i(12) * 9102);
    a[6] = r(i(20) * 9102 + i(12) * 13623);
    a[7] = r(i(4) * 16069 + i(28) * 3196);
    a[8] = r(i(2) * 1606 - i(30) * 16305);
    a[9] = r(i(18) * 12665 - i(14) * 10394);
    a[10] = r(i(10) * 7723 - i(22) * 14449);
    a[11] = r(i(26) * 15679 - i(6) * 4756);
    a[12] = r(i(26) * 4756 + i(6) * 15679);
    a[13] = r(i(10) * 14449 + i(22) * 7723);
    a[14] = r(i(18) * 10394 + i(14) * 12665);
    a[15] = r(i(2) * 16305 + i(30) * 1606);
    a[16] = r(i(1) * 804 - i(31) * 16364);
    a[17] = r(i(17) * 12140 - i(15) * 11003);
    a[18] = r(i(9) * 7005 - i(23) * 14811);
    a[19] = r(i(25) * 15426 - i(7) * 5520);
    a[20] = r(i(5) * 3981 - i(27) * 15893);
    a[21] = r(i(21) * 14053 - i(11) * 8423);
    a[22] = r(i(13) * 9760 - i(19) * 13160);
    a[23] = r(i(29) * 16207 - i(3) * 2404);
    a[24] = r(i(29) * 2404 + i(3) * 16207);
    a[25] = r(i(13) * 13160 + i(19) * 9760);
    a[26] = r(i(21) * 8423 + i(11) * 14053);
    a[27] = r(i(5) * 15893 + i(27) * 3981);
    a[28] = r(i(25) * 5520 + i(7) * 15426);
    a[29] = r(i(9) * 14811 + i(23) * 7005);
    a[30] = r(i(17) * 11003 + i(15) * 12140);
    a[31] = r(i(1) * 16364 + i(31) * 804);

    let g = |k: usize| a[k];
    let t0 = g(0) + g(3);
    let t1 = g(1) + g(2);
    let t2 = g(1) - g(2);
    let t3 = g(0) - g(3);
    let t4 = g(4) + g(5);
    let t5 = g(4) - g(5);
    let t6 = g(7) - g(6);
    let t7 = g(7) + g(6);
    let t8 = g(8) + g(9);
    let t9 = g(8) - g(9);
    let t10 = g(11) - g(10);
    let t11 = g(11) + g(10);
    let t12 = g(12) + g(13);
    let t13 = g(12) - g(13);
    let t14 = g(15) - g(14);
    let t15 = g(15) + g(14);
    let t16 = g(16) + g(17);
    let t17 = g(16) - g(17);
    let t18 = g(19) - g(18);
    let t19 = g(19) + g(18);
    let t20 = g(20) + g(21);
    let t21 = g(20) - g(21);
    let t22 = g(23) - g(22);
    let t23 = g(23) + g(22);
    let t24 = g(24) + g(25);
    let t25 = g(24) - g(25);
    let t26 = g(27) - g(26);
    let t27 = g(27) + g(26);
    let t28 = g(28) + g(29);
    let t29 = g(28) - g(29);
    let t30 = g(31) - g(30);
    let t31 = g(31) + g(30);

    let t5a = r((t6 - t5) as i64 * 11585);
    let t6a = r((t6 + t5) as i64 * 11585);
    let t9a = r(t14 as i64 * 6270 - t9 as i64 * 15137);
    let t14a = r(t14 as i64 * 15137 + t9 as i64 * 6270);
    let t10a = r(-(t13 as i64 * 15137 + t10 as i64 * 6270));
    let t13a = r(t13 as i64 * 6270 - t10 as i64 * 15137);
    let t17a = r(t30 as i64 * 3196 - t17 as i64 * 16069);
    let t30a = r(t30 as i64 * 16069 + t17 as i64 * 3196);
    let t18a = r(-(t29 as i64 * 16069 + t18 as i64 * 3196));
    let t29a = r(t29 as i64 * 3196 - t18 as i64 * 16069);
    let t21a = r(t26 as i64 * 13623 - t21 as i64 * 9102);
    let t26a = r(t26 as i64 * 9102 + t21 as i64 * 13623);
    let t22a = r(-(t25 as i64 * 9102 + t22 as i64 * 13623));
    let t25a = r(t25 as i64 * 13623 - t22 as i64 * 9102);

    let t0a = t0 + t7;
    let t1a = t1 + t6a;
    let t2a = t2 + t5a;
    let t3a = t3 + t4;
    let t4a = t3 - t4;
    let t5 = t2 - t5a;
    let t6 = t1 - t6a;
    let t7a = t0 - t7;
    let t8a = t8 + t11;
    let t9 = t9a + t10a;
    let t10 = t9a - t10a;
    let t11a = t8 - t11;
    let t12a = t15 - t12;
    let t13 = t14a - t13a;
    let t14 = t14a + t13a;
    let t15a = t15 + t12;
    let t16a = t16 + t19;
    let t17 = t17a + t18a;
    let t18 = t17a - t18a;
    let t19a = t16 - t19;
    let t20a = t23 - t20;
    let t21 = t22a - t21a;
    let t22 = t22a + t21a;
    let t23a = t23 + t20;
    let t24a = t24 + t27;
    let t25 = t25a + t26a;
    let t26 = t25a - t26a;
    let t27a = t24 - t27;
    let t28a = t31 - t28;
    let t29 = t30a - t29a;
    let t30 = t30a + t29a;
    let t31a = t31 + t28;

    let t10a = r((t13 - t10) as i64 * 11585);
    let t13a = r((t13 + t10) as i64 * 11585);
    let t11 = r((t12a - t11a) as i64 * 11585);
    let t12 = r((t12a + t11a) as i64 * 11585);
    let t18a = r(t29 as i64 * 6270 - t18 as i64 * 15137);
    let t29a = r(t29 as i64 * 15137 + t18 as i64 * 6270);
    let t19 = r(t28a as i64 * 6270 - t19a as i64 * 15137);
    let t28 = r(t28a as i64 * 15137 + t19a as i64 * 6270);
    let t20 = r(-(t27a as i64 * 15137 + t20a as i64 * 6270));
    let t27 = r(t27a as i64 * 6270 - t20a as i64 * 15137);
    let t21a = r(-(t26 as i64 * 15137 + t21 as i64 * 6270));
    let t26a = r(t26 as i64 * 6270 - t21 as i64 * 15137);

    let t0 = t0a + t15a;
    let t1 = t1a + t14;
    let t2 = t2a + t13a;
    let t3 = t3a + t12;
    let t4 = t4a + t11;
    let t5a = t5 + t10a;
    let t6a = t6 + t9;
    let t7 = t7a + t8a;
    let t8 = t7a - t8a;
    let t9a = t6 - t9;
    let t10 = t5 - t10a;
    let t11a = t4a - t11;
    let t12a = t3a - t12;
    let t13 = t2a - t13a;
    let t14a = t1a - t14;
    let t15 = t0a - t15a;
    let t16 = t16a + t23a;
    let t17a = t17 + t22;
    let t18 = t18a + t21a;
    let t19a = t19 + t20;
    let t20a = t19 - t20;
    let t21 = t18a - t21a;
    let t22a = t17 - t22;
    let t23 = t16a - t23a;
    let t24 = t31a - t24a;
    let t25a = t30 - t25;
    let t26 = t29a - t26a;
    let t27a = t28 - t27;
    let t28a = t28 + t27;
    let t29 = t29a + t26a;
    let t30a = t30 + t25;
    let t31 = t31a + t24a;

    let t20 = r((t27a - t20a) as i64 * 11585);
    let t27 = r((t27a + t20a) as i64 * 11585);
    let t21a = r((t26 - t21) as i64 * 11585);
    let t26a = r((t26 + t21) as i64 * 11585);
    let t22 = r((t25a - t22a) as i64 * 11585);
    let t25 = r((t25a + t22a) as i64 * 11585);
    let t23a = r((t24 - t23) as i64 * 11585);
    let t24a = r((t24 + t23) as i64 * 11585);

    out[0] = t0 + t31;
    out[1] = t1 + t30a;
    out[2] = t2 + t29;
    out[3] = t3 + t28a;
    out[4] = t4 + t27;
    out[5] = t5a + t26a;
    out[6] = t6a + t25;
    out[7] = t7 + t24a;
    out[8] = t8 + t23a;
    out[9] = t9a + t22;
    out[10] = t10 + t21a;
    out[11] = t11a + t20;
    out[12] = t12a + t19a;
    out[13] = t13 + t18;
    out[14] = t14a + t17a;
    out[15] = t15 + t16;
    out[16] = t15 - t16;
    out[17] = t14a - t17a;
    out[18] = t13 - t18;
    out[19] = t12a - t19a;
    out[20] = t11a - t20;
    out[21] = t10 - t21a;
    out[22] = t9a - t22;
    out[23] = t8 - t23a;
    out[24] = t7 - t24a;
    out[25] = t6a - t25;
    out[26] = t5a - t26a;
    out[27] = t4 - t27;
    out[28] = t3 - t28a;
    out[29] = t2 - t29;
    out[30] = t1 - t30a;
    out[31] = t0 - t31;
}

fn iadst16_1d(inp: &[i32], out: &mut [i32]) {
    // Literal port of libvpx iadst16_c, including its input permutation and
    // per-stage rounding (cospi_N_64 values inlined).
    let mut x0 = inp[15] as i64;
    let mut x1 = inp[0] as i64;
    let mut x2 = inp[13] as i64;
    let mut x3 = inp[2] as i64;
    let mut x4 = inp[11] as i64;
    let mut x5 = inp[4] as i64;
    let mut x6 = inp[9] as i64;
    let mut x7 = inp[6] as i64;
    let mut x8 = inp[7] as i64;
    let mut x9 = inp[8] as i64;
    let mut x10 = inp[5] as i64;
    let mut x11 = inp[10] as i64;
    let mut x12 = inp[3] as i64;
    let mut x13 = inp[12] as i64;
    let mut x14 = inp[1] as i64;
    let mut x15 = inp[14] as i64;

    // stage 1
    let s0 = x0 * 16364 + x1 * 804;
    let s1 = x0 * 804 - x1 * 16364;
    let s2 = x2 * 15893 + x3 * 3981;
    let s3 = x2 * 3981 - x3 * 15893;
    let s4 = x4 * 14811 + x5 * 7005;
    let s5 = x4 * 7005 - x5 * 14811;
    let s6 = x6 * 13160 + x7 * 9760;
    let s7 = x6 * 9760 - x7 * 13160;
    let s8 = x8 * 11003 + x9 * 12140;
    let s9 = x8 * 12140 - x9 * 11003;
    let s10 = x10 * 8423 + x11 * 14053;
    let s11 = x10 * 14053 - x11 * 8423;
    let s12 = x12 * 5520 + x13 * 15426;
    let s13 = x12 * 15426 - x13 * 5520;
    let s14 = x14 * 2404 + x15 * 16207;
    let s15 = x14 * 16207 - x15 * 2404;

    x0 = i64::from(rsh14(s0 + s8));
    x1 = i64::from(rsh14(s1 + s9));
    x2 = i64::from(rsh14(s2 + s10));
    x3 = i64::from(rsh14(s3 + s11));
    x4 = i64::from(rsh14(s4 + s12));
    x5 = i64::from(rsh14(s5 + s13));
    x6 = i64::from(rsh14(s6 + s14));
    x7 = i64::from(rsh14(s7 + s15));
    x8 = i64::from(rsh14(s0 - s8));
    x9 = i64::from(rsh14(s1 - s9));
    x10 = i64::from(rsh14(s2 - s10));
    x11 = i64::from(rsh14(s3 - s11));
    x12 = i64::from(rsh14(s4 - s12));
    x13 = i64::from(rsh14(s5 - s13));
    x14 = i64::from(rsh14(s6 - s14));
    x15 = i64::from(rsh14(s7 - s15));

    // stage 2
    let s0 = x0;
    let s1 = x1;
    let s2 = x2;
    let s3 = x3;
    let s4 = x4;
    let s5 = x5;
    let s6 = x6;
    let s7 = x7;
    let s8 = x8 * 16069 + x9 * 3196;
    let s9 = x8 * 3196 - x9 * 16069;
    let s10 = x10 * 9102 + x11 * 13623;
    let s11 = x10 * 13623 - x11 * 9102;
    let s12 = -x12 * 3196 + x13 * 16069;
    let s13 = x12 * 16069 + x13 * 3196;
    let s14 = -x14 * 13623 + x15 * 9102;
    let s15 = x14 * 9102 + x15 * 13623;

    x0 = s0 + s4;
    x1 = s1 + s5;
    x2 = s2 + s6;
    x3 = s3 + s7;
    x4 = s0 - s4;
    x5 = s1 - s5;
    x6 = s2 - s6;
    x7 = s3 - s7;
    x8 = i64::from(rsh14(s8 + s12));
    x9 = i64::from(rsh14(s9 + s13));
    x10 = i64::from(rsh14(s10 + s14));
    x11 = i64::from(rsh14(s11 + s15));
    x12 = i64::from(rsh14(s8 - s12));
    x13 = i64::from(rsh14(s9 - s13));
    x14 = i64::from(rsh14(s10 - s14));
    x15 = i64::from(rsh14(s11 - s15));

    // stage 3
    let s0 = x0;
    let s1 = x1;
    let s2 = x2;
    let s3 = x3;
    let s4 = x4 * 15137 + x5 * 6270;
    let s5 = x4 * 6270 - x5 * 15137;
    let s6 = -x6 * 6270 + x7 * 15137;
    let s7 = x6 * 15137 + x7 * 6270;
    let s8 = x8;
    let s9 = x9;
    let s10 = x10;
    let s11 = x11;
    let s12 = x12 * 15137 + x13 * 6270;
    let s13 = x12 * 6270 - x13 * 15137;
    let s14 = -x14 * 6270 + x15 * 15137;
    let s15 = x14 * 15137 + x15 * 6270;

    x0 = s0 + s2;
    x1 = s1 + s3;
    x2 = s0 - s2;
    x3 = s1 - s3;
    x4 = i64::from(rsh14(s4 + s6));
    x5 = i64::from(rsh14(s5 + s7));
    x6 = i64::from(rsh14(s4 - s6));
    x7 = i64::from(rsh14(s5 - s7));
    x8 = s8 + s10;
    x9 = s9 + s11;
    x10 = s8 - s10;
    x11 = s9 - s11;
    x12 = i64::from(rsh14(s12 + s14));
    x13 = i64::from(rsh14(s13 + s15));
    x14 = i64::from(rsh14(s12 - s14));
    x15 = i64::from(rsh14(s13 - s15));

    // stage 4
    let x2n = i64::from(rsh14(-11585 * (x2 + x3)));
    let x3n = i64::from(rsh14(11585 * (x2 - x3)));
    let x6n = i64::from(rsh14(11585 * (x6 + x7)));
    let x7n = i64::from(rsh14(11585 * (-x6 + x7)));
    let x10n = i64::from(rsh14(11585 * (x10 + x11)));
    let x11n = i64::from(rsh14(11585 * (-x10 + x11)));
    let x14n = i64::from(rsh14(-11585 * (x14 + x15)));
    let x15n = i64::from(rsh14(11585 * (x14 - x15)));
    let (x2, x3) = (x2n, x3n);
    let (x6, x7) = (x6n, x7n);
    let (x10, x11) = (x10n, x11n);
    let (x14, x15) = (x14n, x15n);

    out[0] = x0 as i32;
    out[1] = -x8 as i32;
    out[2] = x12 as i32;
    out[3] = -x4 as i32;
    out[4] = x6 as i32;
    out[5] = x14 as i32;
    out[6] = x10 as i32;
    out[7] = x2 as i32;
    out[8] = x3 as i32;
    out[9] = x11 as i32;
    out[10] = x15 as i32;
    out[11] = x7 as i32;
    out[12] = x5 as i32;
    out[13] = -x13 as i32;
    out[14] = x9 as i32;
    out[15] = -x1 as i32;
}

fn iwht4_1d(inp: &[i32], out: &mut [i32], pass: u32) {
    let (t0, t1, t2, t3) = if pass == 0 {
        (inp[0] >> 2, inp[3] >> 2, inp[1] >> 2, inp[2] >> 2)
    } else {
        (inp[0], inp[3], inp[1], inp[2])
    };
    let mut t0 = t0 + t2;
    let mut t3 = t3 - t1;
    let t4 = (t0 - t3) >> 1;
    let t1 = t4 - t1;
    let t2 = t4 - t2;
    t0 -= t1;
    t3 += t2;
    out[0] = t0;
    out[1] = t1;
    out[2] = t2;
    out[3] = t3;
}

/// Final per-size rounding shift (`bits`): 4 for 4x4, 5 for 8x8, 6 for
/// 16x16/32x32, 0 for lossless.
#[inline]
fn add_to_pixel(dst: &mut u8, v: i32, bits: u32) {
    let s = if bits == 0 {
        v
    } else {
        // (int)(out + (1U << (bits-1))) >> bits — unsigned add, arithmetic shift
        let u = (v as u32).wrapping_add(1u32 << (bits - 1));
        (u as i32) >> bits
    };
    *dst = (((*dst as i32) + s).clamp(0, 255)) as u8;
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Wht is expressed through use_wht in run_1d
enum Xf {
    Dct,
    Adst,
    Wht,
}

/// Perform the inverse transform of `coeffs` (raster-layout dequantized
/// coefficients for one transform block) and add the result into
/// `dst[dst_off..]` with stride `stride`.
///
/// `tx`: 0..3 = 4x4/8x8/16x16/32x32, 4 = lossless (WHT 4x4). `tx_type` is one
/// of the `DCT_DCT`..`ADST_ADST` constants. Pass order and per-pass rounding
/// mirror the reference `itxfm_wrapper` templates.
pub fn inverse_transform_add(
    tx: usize,
    tx_type: usize,
    eob: usize,
    coeffs: &[i32],
    dst: &mut [u8],
    dst_off: usize,
    stride: usize,
) {
    let (sz, bits, has_dconly) = match tx {
        0 => (4usize, 4u32, true),
        1 => (8, 5, true),
        2 => (16, 6, true),
        3 => (32, 6, false),
        _ => (4, 0, false), // lossless
    };

    // DC-only shortcut for the pure-DCT shapes that support it (the lossless
    // WHT and the 32x32 DCT do not take it in the reference).
    if has_dconly && eob == 1 && tx_type == DCT_DCT {
        let t0 = rsh14(i64::from(coeffs[0]) * 11585);
        let t = rsh14(i64::from(t0) * 11585);
        for j in 0..sz {
            for i in 0..sz {
                add_to_pixel(&mut dst[dst_off + j * stride + i], t, bits);
            }
        }
        return;
    }

    let (a_kind, b_kind) = if tx == 4 || tx == 3 {
        (Xf::Dct, Xf::Dct)
    } else {
        match tx_type {
            DCT_DCT => (Xf::Dct, Xf::Dct),
            ADST_DCT => (Xf::Dct, Xf::Adst),
            DCT_ADST => (Xf::Adst, Xf::Dct),
            _ => (Xf::Adst, Xf::Adst),
        }
    };
    let use_wht = tx == 4;

    let mut block = vec![0i32; sz * sz];
    block.copy_from_slice(&coeffs[..sz * sz]);
    let mut tmp = vec![0i32; sz * sz];
    let mut out = vec![0i32; sz];
    let mut col = vec![0i32; sz];

    // Pass A: rows of the block (the reference transforms row vectors
    // first), written out as rows of `tmp`.
    for i in 0..sz {
        for k in 0..sz {
            col[k] = block[i * sz + k];
        }
        run_1d(use_wht, a_kind, &col, &mut out, 0);
        for k in 0..sz {
            tmp[i * sz + k] = out[k];
        }
    }
    // Pass B: COLUMNS of `tmp` (the reference reads tmp + i with stride sz),
    // added into destination columns.
    for i in 0..sz {
        let tcol: Vec<i32> = (0..sz).map(|k| tmp[k * sz + i]).collect();
        run_1d(use_wht, b_kind, &tcol, &mut out, 1);
        for j in 0..sz {
            add_to_pixel(&mut dst[dst_off + j * stride + i], out[j], bits);
        }
    }
}

fn run_1d(use_wht: bool, kind: Xf, inp: &[i32], out: &mut [i32], pass: u32) {
    if use_wht {
        iwht4_1d(inp, out, pass);
        return;
    }
    match (inp.len(), kind) {
        (4, Xf::Dct) => idct4_1d(inp, out),
        (4, _) => iadst4_1d(inp, out),
        (8, Xf::Dct) => idct8_1d(inp, out),
        (8, _) => iadst8_1d(inp, out),
        (16, Xf::Dct) => idct16_1d(inp, out),
        (16, _) => iadst16_1d(inp, out),
        _ => idct32_1d(inp, out),
    }
}

#[cfg(test)]
mod wht_tests {
    // NOTE: a DC-only lossless coefficient block is NOT a valid encoder
    // output (libvpx's fwht4x4 of a constant block scatters into all four
    // row positions per pass), so there is no hand-derivable "correct"
    // result to assert here; lossless correctness is covered end-to-end by
    // the ffmpeg-gated conformance clips (tests/conformance_vp9.rs).
}
