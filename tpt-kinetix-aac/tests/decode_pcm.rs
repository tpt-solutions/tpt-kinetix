//! End-to-end PCM decode test for `tpt-kinetix-aac`.
//!
//! Generates a real AAC-LC ADTS elementary stream with `ffmpeg`, feeds each
//! ADTS frame through [`AacDecoder::decode`], and asserts that the decoder
//! produces real, non-silent interleaved f32 PCM (not the previous parse-only
//! placeholder output).
//!
//! The test is skipped (not failed) when `ffmpeg` is unavailable so it stays
//! green on runners without the binary.

use tpt_kinetix_aac::{adts, AacDecoder};
use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};
use tpt_kinetix_test_utils::synthetic::minimal_aac_adts;

fn packet_from(data: Vec<u8>) -> Packet {
    Packet {
        pts: Timestamp::NONE,
        dts: Timestamp::NONE,
        data,
        stream_index: 0,
        is_key_frame: true,
    }
}

#[test]
fn decodes_real_pcm_from_adts_stream() {
    let sample_rate = 44_100u32;
    let channels = 2u8;

    let Some(stream) = minimal_aac_adts(sample_rate, channels, 0.25) else {
        eprintln!("skipping: ffmpeg unavailable or AAC encode failed");
        return;
    };

    // Split the ADTS stream into individual frames to sanity-check the stream
    // is well-formed before feeding it to the decoder.
    let frames = adts::iter_frames(&stream);
    assert!(
        !frames.is_empty(),
        "expected at least one ADTS frame from the generated stream"
    );

    let mut dec = AacDecoder::new();
    let mut decoded_frames = 0usize;
    let mut total_samples = 0usize;
    let mut saw_nonzero = false;

    // Walk the stream by frame_length, handing each whole ADTS frame (header +
    // payload) to the decoder so it learns config from the header itself.
    let mut pos = 0usize;
    while pos < stream.len() {
        let hdr = match adts::AdtsHeader::parse(&stream[pos..]) {
            Ok(h) => h,
            Err(_) => break,
        };
        let end = pos + hdr.frame_length;
        if end > stream.len() {
            break;
        }
        let frame_bytes = stream[pos..end].to_vec();
        pos = end;

        if let Some(audio) = dec.decode(&packet_from(frame_bytes)).unwrap() {
            assert_eq!(audio.sample_rate, sample_rate, "decoded sample rate");
            assert_eq!(audio.channels, channels, "decoded channel count");
            assert!(!audio.data.is_empty(), "decoded PCM must not be empty");

            decoded_frames += 1;
            total_samples += audio.samples_per_channel();

            // f32 little-endian interleaved samples: at least one should be
            // non-zero for a 440 Hz tone (proving real reconstruction).
            for chunk in audio.data.chunks_exact(4) {
                let s = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                if s.abs() > 1e-4 {
                    saw_nonzero = true;
                }
            }
        }
    }

    assert!(decoded_frames > 0, "decoder produced no PCM frames");
    assert!(
        total_samples > 0,
        "decoder produced frames but zero samples per channel"
    );
    assert!(
        saw_nonzero,
        "decoded PCM was entirely silent — reconstruction likely not working"
    );
}
