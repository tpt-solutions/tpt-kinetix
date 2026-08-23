//! Phase 6 exit criterion: the full native decode pipeline must never panic on
//! untrusted input.
//!
//! `proptest_aac.rs` already covers `RawDataBlock::parse` and the bit reader in
//! isolation. This file drives `AacDecoder::decode` end-to-end, which reaches
//! considerably further: scalefactor/spectral Huffman decode, dequantization,
//! PNS/TNS/pulse, CCE coupling, stereo reconstruction, and the IMDCT +
//! overlap-add filterbank.
//!
//! That distinction matters concretely — two real out-of-bounds panics
//! (`stereo.rs`'s `swb[sfb + 1]`, `pulse.rs`'s `swb[pulse.start_sfb]`) lived in
//! those later stages and were unreachable until upstream parsing was fixed
//! enough to get past it, so a parse-only property test could not have found
//! them. See `fuzz/fuzz_targets/fuzz_aac_decode.rs` for the coverage-guided
//! counterpart.

use proptest::prelude::*;
use tpt_kinetix_aac::AacDecoder;
use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(256)
}

fn packet(data: Vec<u8>) -> Packet {
    Packet {
        pts: Timestamp::NONE,
        dts: Timestamp::NONE,
        data,
        stream_index: 0,
        is_key_frame: true,
    }
}

/// A syntactically valid ADTS header wrapping `payload`, so a meaningful share
/// of generated inputs get past the header check and actually reach the
/// spectral-decode and filterbank stages.
fn adts_frame(sample_rate_index: u8, channel_config: u8, payload: &[u8]) -> Vec<u8> {
    let frame_len = 7 + payload.len();
    let mut frame = vec![
        0xFF,
        0xF1, // syncword, MPEG-4, no CRC
        // profile = AAC-LC (01), sample-rate index, private=0, channel cfg high bit
        0x40 | ((sample_rate_index & 0x0F) << 2) | ((channel_config >> 2) & 0x01),
        ((channel_config & 0x03) << 6) | ((frame_len >> 11) & 0x03) as u8,
        ((frame_len >> 3) & 0xFF) as u8,
        (((frame_len & 0x07) << 5) as u8) | 0x1F,
        0xFC,
    ];
    frame.extend_from_slice(payload);
    frame
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    /// Arbitrary bytes: decode must return Ok/Err, never panic.
    #[test]
    fn decode_arbitrary_bytes_never_panics(
        data in proptest::collection::vec(any::<u8>(), 0..2048)
    ) {
        let mut dec = AacDecoder::new();
        let _ = dec.decode(&packet(data));
    }

    /// Arbitrary payloads behind a *valid* ADTS header, so the fuzzed bits land
    /// in `raw_data_block` and the reconstruction stages rather than being
    /// rejected at the header.
    #[test]
    fn decode_valid_adts_header_with_arbitrary_payload_never_panics(
        sri in 3u8..=12,
        ch in 1u8..=2,
        payload in proptest::collection::vec(any::<u8>(), 0..1024),
    ) {
        let mut dec = AacDecoder::new();
        let _ = dec.decode(&packet(adts_frame(sri, ch, &payload)));
    }

    /// Multiple frames through one decoder instance: exercises the persistent
    /// per-channel overlap-add buffers and window-shape history across frame
    /// boundaries, including transitions between long and eight-short windows.
    #[test]
    fn decode_multiple_frames_never_panics(
        frames in proptest::collection::vec(
            proptest::collection::vec(any::<u8>(), 0..256),
            1..8,
        ),
    ) {
        let mut dec = AacDecoder::new();
        for payload in &frames {
            let _ = dec.decode(&packet(adts_frame(4, 2, payload)));
        }
    }

    /// Truncating a frame at every prefix length must not panic — a common
    /// real-world failure mode (partial network reads / damaged files) and the
    /// regime that historically produced `UnexpectedEof` rather than clean
    /// errors.
    #[test]
    fn decode_truncated_frames_never_panics(
        payload in proptest::collection::vec(any::<u8>(), 8..256),
        cut in 0usize..256,
    ) {
        let full = adts_frame(4, 2, &payload);
        let cut = cut.min(full.len());
        let mut dec = AacDecoder::new();
        let _ = dec.decode(&packet(full[..cut].to_vec()));
    }

    /// Any successful decode must produce **finite** samples.
    ///
    /// This is a correctness invariant, not just a panic check, and it caught
    /// three real bugs: `dequant_scale`'s `2^(0.25·q)` and intensity stereo's
    /// `2^(-0.25·is_pos)` both overflowed `f32` to `+inf` on
    /// bitstream-controlled exponents, and an infinite scale then became NaN
    /// downstream (`inf * 0.0`, or `l - r` in M/S stereo), poisoning the whole
    /// output frame while still returning `Ok`.
    ///
    /// Only finiteness is asserted, deliberately. These inputs are random bits
    /// behind a valid ADTS header, which can legitimately encode enormous
    /// scalefactor / `global_gain` combinations, so a *finite but very large*
    /// sample is the decoder faithfully reproducing a corrupt stream rather than
    /// a bug — asserting any particular magnitude ceiling here just encodes an
    /// arbitrary threshold that random search will eventually cross. Real-signal
    /// amplitude correctness is covered where it belongs, against an actual
    /// reference decoder, by `tests/conformance_aac.rs`.
    #[test]
    fn decoded_samples_are_always_finite(
        sri in 3u8..=12,
        ch in 1u8..=2,
        payload in proptest::collection::vec(any::<u8>(), 0..1024),
    ) {
        let mut dec = AacDecoder::new();
        if let Ok(Some(frame)) = dec.decode(&packet(adts_frame(sri, ch, &payload))) {
            for c in frame.data.chunks_exact(4) {
                let s = f32::from_le_bytes([c[0], c[1], c[2], c[3]]);
                prop_assert!(
                    s.is_finite(),
                    "decoded a non-finite sample ({s}) from a successful decode"
                );
            }
        }
    }
}
