//! `SPLS` sequences carried in MP4: one `spls` sample per time slab.

use tpt_kinetix_ops::{mp4_to_spls, spls_to_mp4};
use tpt_kinetix_splat::{write_sequence, GaussianCloud4D, Quantization};

const ID: [f32; 4] = [1.0, 0.0, 0.0, 0.0];

fn scene(n: usize) -> GaussianCloud4D {
    let f = |i: usize| (i as f32 * 0.37).sin();
    GaussianCloud4D::new(
        (0..n)
            .map(|i| [f(i), f(i + 1), f(i + 2), 1.0 + 9.0 * (f(i + 3) + 1.0) / 2.0])
            .collect(),
        vec![[-1.0, -1.0, -1.0, 0.0]; n],
        vec![ID; n],
        vec![ID; n],
        (0..n).map(|i| f(i + 4)).collect(),
        1,
        (0..n * 12).map(|i| f(i) * 0.3).collect(),
    )
    .unwrap()
}

fn round_trip(quant: Quantization) {
    let spls = write_sequence(&scene(40), 2.5, 0.01, quant).unwrap();
    let mp4 = spls_to_mp4(&spls).unwrap();
    assert!(mp4.windows(4).any(|w| w == b"spls"), "spls sample entry");
    assert!(
        mp4.windows(4).any(|w| w == b"splc"),
        "splc configuration box"
    );
    let back = mp4_to_spls(&mp4).unwrap();
    assert_eq!(back, spls, "SPLS must come back byte-identical");
}

#[test]
fn lossless_sequence_round_trips_through_mp4() {
    round_trip(Quantization::Lossless);
}

#[test]
fn lossy_sequence_round_trips_through_mp4() {
    round_trip(Quantization::Lossy {
        position_bits: 16,
        attribute_bits: 8,
    });
}

#[test]
fn rejects_non_splat_input() {
    assert!(spls_to_mp4(b"not a sequence").is_err());
    assert!(mp4_to_spls(b"\0\0\0\x08free").is_err());
}

/// Writes an MP4 to `SPLAT_MP4_OUT` for external checks (for example
/// `ffprobe`). Does nothing when the variable is unset.
#[test]
fn write_mp4_for_external_check() {
    let Some(path) = std::env::var_os("SPLAT_MP4_OUT") else {
        return;
    };
    let spls = write_sequence(&scene(40), 2.5, 0.01, Quantization::Lossless).unwrap();
    std::fs::write(path, spls_to_mp4(&spls).unwrap()).unwrap();
}
