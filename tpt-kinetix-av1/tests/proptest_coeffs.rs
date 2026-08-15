//! Robustness properties for the rewired AV1 coefficient decode path.
//!
//! `reconstruct::decode_tile_group` now drives the real symbol decoder over
//! whatever bytes a tile group carries. Since a decoder is an attack surface,
//! the important property for arbitrary (malformed, truncated, adversarial)
//! input is that it either decodes or returns an error — never panics, hangs,
//! or reads out of bounds.

use proptest::prelude::*;

use tpt_kinetix_av1::inter::RefFrames;
use tpt_kinetix_av1::reconstruct::decode_tile_group;

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(32)
}

/// Run one tile group over a fresh set of planes, returning whether it
/// reported success. Panics (including index-out-of-bounds and arithmetic
/// overflow in debug builds) fail the property.
fn decode(data: &[u8], width: usize, height: usize, qindex: u8) -> bool {
    let uv_w = width / 2;
    let uv_h = height / 2;
    let mut y = vec![128u8; width * height];
    let mut u = vec![128u8; uv_w * uv_h];
    let mut v = vec![128u8; uv_w * uv_h];
    let mut meta = tpt_kinetix_av1::loop_filter::FrameMeta::new(width, height);
    decode_tile_group(
        data,
        width,
        height,
        8,
        qindex,
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
        false,    // enable_filter_intra
        true,     // frame_is_intra — these robustness tests exercise the intra path
        false,    // allow_high_precision_mv
        false,    // reference_select
        0,        // interpolation_filter (EIGHTTAP_REGULAR)
        [0u8; 9], // ref_to_slot
        RefFrames::empty(),
        &mut meta,
    )
    .is_ok()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    /// Arbitrary tile payloads must not panic the coefficient decoder.
    #[test]
    fn decode_tile_group_never_panics(
        data in proptest::collection::vec(any::<u8>(), 0..2048),
        qindex in any::<u8>(),
    ) {
        let _ = decode(&data, 64, 64, qindex);
    }

    /// The same, for frame sizes that are not a multiple of the placeholder
    /// block grid, where the partial right/bottom blocks are clipped.
    #[test]
    fn decode_tile_group_handles_unaligned_sizes(
        data in proptest::collection::vec(any::<u8>(), 0..1024),
        width in 2usize..40,
        height in 2usize..40,
    ) {
        let _ = decode(&data, width * 2, height * 2, 100);
    }
}

/// An empty tile payload still has to behave: the symbol decoder reads zero
/// padding past the end of the buffer rather than indexing out of bounds.
#[test]
fn empty_tile_payload_is_handled() {
    let _ = decode(&[], 64, 64, 100);
}

/// A lossless (`base_q_idx == 0`) tile takes a different transform path
/// (WHT) and a different CDF quantizer bucket; make sure it is reachable
/// without panicking too.
#[test]
fn lossless_tile_payload_is_handled() {
    let data: Vec<u8> = (0..256u32).map(|i| ((i * 37 + 11) & 0xFF) as u8).collect();
    let _ = decode(&data, 64, 64, 0);
}
