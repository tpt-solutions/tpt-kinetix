//! Realistic-size `SPLT` check on the reference horned lizard cloud.
//!
//! Runs only when `SPZ_ORACLE_DIR` holds `out_horned.ply` (see
//! `tests/spz_oracle.rs`). Asserts lossless bit-exactness and reports the
//! size and timing of both modes.

use std::path::PathBuf;
use std::time::Instant;

use tpt_kinetix_splat::{read_ply, read_splt, write_splt, GaussianCloud, Quantization};

fn canonical(c: &GaussianCloud) -> Vec<Vec<u32>> {
    let k3 = c.sh().len() / c.len().max(1);
    let mut pts: Vec<Vec<u32>> = (0..c.len())
        .map(|i| {
            let mut v: Vec<u32> = c.positions()[i].iter().map(|x| x.to_bits()).collect();
            v.push(c.opacities()[i].to_bits());
            v.extend(c.scales()[i].iter().map(|x| x.to_bits()));
            v.extend(c.rotations()[i].iter().map(|x| x.to_bits()));
            v.extend(c.sh()[i * k3..(i + 1) * k3].iter().map(|x| x.to_bits()));
            v
        })
        .collect();
    pts.sort();
    pts
}

#[test]
fn splt_round_trips_reference_cloud() {
    let Some(dir) = std::env::var_os("SPZ_ORACLE_DIR").map(PathBuf::from) else {
        eprintln!("SPZ_ORACLE_DIR not set; skipping SPLT reference check");
        return;
    };
    let ply = std::fs::read(dir.join("out_horned.ply")).expect("reference present");
    let cloud = read_ply(&ply).expect("reference PLY parses");
    let raw_bytes = ply.len();

    let t = Instant::now();
    let lossless = write_splt(&cloud, Quantization::Lossless).unwrap();
    let t_enc = t.elapsed();
    let t = Instant::now();
    let back = read_splt(&lossless).unwrap();
    let t_dec = t.elapsed();
    assert_eq!(
        canonical(&back),
        canonical(&cloud),
        "lossless must be bit-exact up to order"
    );
    eprintln!(
        "lossless: {} bytes ({:.1}% of PLY), encode {:?}, decode {:?}",
        lossless.len(),
        100.0 * lossless.len() as f64 / raw_bytes as f64,
        t_enc,
        t_dec
    );

    for (pb, ab) in [(16, 12), (16, 10), (16, 8), (12, 8)] {
        let lossy = write_splt(
            &cloud,
            Quantization::Lossy {
                position_bits: pb,
                attribute_bits: ab,
            },
        )
        .unwrap();
        let back = read_splt(&lossy).unwrap();
        assert_eq!(back.len(), cloud.len());
        eprintln!(
            "lossy {pb}/{ab}: {} bytes ({:.1}% of PLY)",
            lossy.len(),
            100.0 * lossy.len() as f64 / raw_bytes as f64
        );
    }
}
