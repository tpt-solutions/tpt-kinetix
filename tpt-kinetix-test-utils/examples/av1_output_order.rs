//! Scratch: dump the per-frame `order_hint` our AV1 decoder sees, alongside
//! the frame index dav1d emits it at. An encoder using a B-pyramid (alt-refs)
//! codes frames in a different order than it presents them; a decoder that
//! emits in coded order instead of presentation order desynchronises every
//! frame after the first reorder. Run:
//! `cargo run -p tpt-kinetix-test-utils --example av1_output_order -- <ivf>`

use tpt_kinetix_av1::Av1Decoder;
use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};
use tpt_kinetix_test_utils::reference::{dav1d_available, decode_av1_with_dav1d, split_ivf_frames};

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: av1_output_order <ivf> [max]");
    if !dav1d_available() {
        eprintln!("dav1d not available");
        return;
    }
    let max: Option<usize> = std::env::args().nth(2).and_then(|s| s.parse().ok());
    let bytes = std::fs::read(&path).unwrap();
    let w = u16::from_le_bytes([bytes[12], bytes[13]]) as usize;
    let h = u16::from_le_bytes([bytes[14], bytes[15]]) as usize;
    let ref_frames = decode_av1_with_dav1d(&bytes, w as u32, h as u32).unwrap();
    let mut dec = Av1Decoder::new();
    let mut ours: Vec<(usize, Vec<u8>)> = Vec::new();
    for (i, data) in split_ivf_frames(&bytes).iter().enumerate() {
        let pk = Packet {
            pts: Timestamp::new(i as i64, (1, 30)),
            dts: Timestamp::new(i as i64, (1, 30)),
            data: data.clone(),
            stream_index: 0,
            is_key_frame: i == 0,
        };
        match dec.decode(&pk) {
            Ok(Some(f)) => ours.push((i, f.data)),
            Ok(None) => println!("coded frame {i}: shown by a later frame (alt-ref)"),
            Err(e) => {
                println!("coded frame {i}: DECODE ERROR {e}");
                break;
            }
        }
        if let Some(m) = max {
            if ours.len() >= m {
                break;
            }
        }
    }

    // For each frame we emit, rank dav1d's output frames by how many luma
    // samples match. A non-zero best match with a *permuted* index is the
    // presentation-order permutation; a zero best match is a reconstruction
    // bug that no reordering could explain.
    for (i, (_, data)) in ours.iter().enumerate().take(20) {
        let mut scored: Vec<(usize, usize)> = ref_frames
            .iter()
            .enumerate()
            .map(|(j, r)| {
                let n = data
                    .iter()
                    .zip(r.data.iter())
                    .filter(|(a, b)| a == b)
                    .count();
                (j, n)
            })
            .collect();
        scored.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
        let total = (w * h * 3 / 2).max(1);
        println!(
            "ours[{i}] -> dav1d best {:?} (top {:.1}% of {total} samples)",
            &scored[..3.min(scored.len())],
            100.0 * scored[0].1 as f64 / total as f64
        );
    }
}
