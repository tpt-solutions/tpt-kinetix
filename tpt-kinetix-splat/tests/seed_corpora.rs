//! Writes small valid inputs into the fuzz corpora.
//!
//! Random bytes almost always fail at a parser's first check, so the fuzzers
//! start from valid files. Run with `SPLAT_WRITE_SEEDS=1` (the output is
//! committed under `fuzz/corpus/`). Without the variable this test does nothing.

use std::path::PathBuf;

use tpt_kinetix_splat::{
    write_ply, write_sequence, write_splt, write_spz, GaussianCloud, GaussianCloud4D, Quantization,
};

fn cloud(degree: u8, n: usize) -> GaussianCloud {
    let k = tpt_kinetix_splat::sh_coeffs_for_degree(degree);
    let f = |i: usize| (i as f32 * 0.37).sin();
    GaussianCloud::new(
        (0..n).map(|i| [f(i), f(i + 1), f(i + 2)]).collect(),
        (0..n)
            .map(|i| [1.0, f(i) * 0.1, f(i + 3) * 0.1, f(i + 4) * 0.1])
            .collect(),
        (0..n)
            .map(|i| [f(i + 5) - 1.0, f(i + 6) - 1.0, f(i + 7) - 1.0])
            .collect(),
        (0..n).map(|i| f(i + 8) * 3.0).collect(),
        degree,
        (0..n * 3 * k).map(|i| f(i) * 0.5).collect(),
    )
    .unwrap()
}

fn cloud4(degree: u8, n: usize) -> GaussianCloud4D {
    let k3 = 3 * tpt_kinetix_splat::sh_coeffs_for_degree(degree);
    let f = |i: usize| (i as f32 * 0.37).sin();
    let id = [1.0, 0.0, 0.0, 0.0];
    GaussianCloud4D::new(
        (0..n)
            .map(|i| [f(i), f(i + 1), f(i + 2), 2.0 + 3.0 * f(i + 3).abs()])
            .collect(),
        (0..n).map(|_| [-1.0, -1.0, -1.0, 0.0]).collect(),
        vec![id; n],
        vec![id; n],
        (0..n).map(|i| f(i + 4)).collect(),
        degree,
        (0..n * k3).map(|i| f(i) * 0.5).collect(),
    )
    .unwrap()
}

fn write(dir: &str, name: &str, bytes: &[u8]) {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fuzz/corpus")
        .join(dir);
    std::fs::create_dir_all(&base).unwrap();
    std::fs::write(base.join(name), bytes).unwrap();
}

#[test]
fn write_seed_corpora() {
    if std::env::var_os("SPLAT_WRITE_SEEDS").is_none() {
        return;
    }
    for degree in [0u8, 1] {
        let c = cloud(degree, 4);
        write(
            "fuzz_splat_ply",
            &format!("deg{degree}.ply"),
            &write_ply(&c),
        );
        write(
            "fuzz_splat_spz",
            &format!("deg{degree}.spz"),
            &write_spz(&c).unwrap(),
        );
        write(
            "fuzz_splat_splt",
            &format!("deg{degree}_lossless.splt"),
            &write_splt(&c, Quantization::Lossless).unwrap(),
        );
        write(
            "fuzz_splat_splt",
            &format!("deg{degree}_lossy.splt"),
            &write_splt(
                &c,
                Quantization::Lossy {
                    position_bits: 16,
                    attribute_bits: 8,
                },
            )
            .unwrap(),
        );
        let c4 = cloud4(degree, 6);
        write(
            "fuzz_splat_spls",
            &format!("deg{degree}_lossless.spls"),
            &write_sequence(&c4, 2.0, 0.01, Quantization::Lossless).unwrap(),
        );
        write(
            "fuzz_splat_spls",
            &format!("deg{degree}_lossy.spls"),
            &write_sequence(
                &c4,
                2.0,
                0.01,
                Quantization::Lossy {
                    position_bits: 16,
                    attribute_bits: 8,
                },
            )
            .unwrap(),
        );
    }
}
