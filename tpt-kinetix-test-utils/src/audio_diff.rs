//! Audio-frame comparison helpers.
//!
//! Analogues of [`crate::pixel_diff`] for decoded audio: compare two
//! [`AudioFrame`]s sample-by-sample and report tolerance / max difference.
//! Used by the AAC conformance harness to diff Kinetix's PCM against an
//! `ffmpeg` reference decode.
//!
//! Both frames are expected to be interleaved PCM with the same
//! [`tpt_kinetix_core::frame::SampleFormat`]; mismatched geometry yields
//! `None` rather than a panic.

use tpt_kinetix_core::frame::{AudioFrame, SampleFormat};

/// Returns `true` if every interleaved sample in `a` and `b` differs by at most
/// `max_diff` (in the native sample space).
///
/// Returns `false` if the frames have different sample rates, channel counts,
/// sample formats, or data lengths (those are not "within tolerance" — they are
/// incomparable).
pub fn pcm_within_tolerance(a: &AudioFrame, b: &AudioFrame, max_diff: f32) -> bool {
    if a.sample_rate != b.sample_rate
        || a.channels != b.channels
        || a.sample_format != b.sample_format
        || a.data.len() != b.data.len()
    {
        return false;
    }

    match a.sample_format {
        SampleFormat::F32 => {
            let a_s = read_f32(a);
            let b_s = read_f32(b);
            a_s.iter()
                .zip(b_s.iter())
                .all(|(&x, &y)| (x - y).abs() <= max_diff)
        }
        SampleFormat::S16 => {
            let a_s = read_s16(a);
            let b_s = read_s16(b);
            a_s.iter()
                .zip(b_s.iter())
                .all(|(&x, &y)| (x as f32 - y as f32).abs() <= max_diff)
        }
    }
}

/// Returns the maximum absolute sample difference between `a` and `b`.
///
/// Returns `None` if the two frames are incomparable (different sample rates,
/// channel counts, sample formats, or data lengths).
pub fn pcm_max_abs_diff(a: &AudioFrame, b: &AudioFrame) -> Option<f32> {
    if a.sample_rate != b.sample_rate
        || a.channels != b.channels
        || a.sample_format != b.sample_format
        || a.data.len() != b.data.len()
    {
        return None;
    }

    match a.sample_format {
        SampleFormat::F32 => {
            let a_s = read_f32(a);
            let b_s = read_f32(b);
            a_s.iter()
                .zip(b_s.iter())
                .map(|(&x, &y)| (x - y).abs())
                .fold(None, |acc, d| Some(acc.map_or(d, |m| m.max(d))))
        }
        SampleFormat::S16 => {
            let a_s = read_s16(a);
            let b_s = read_s16(b);
            a_s.iter()
                .zip(b_s.iter())
                .map(|(&x, &y)| (x as f32 - y as f32).abs())
                .fold(None, |acc, d| Some(acc.map_or(d, |m| m.max(d))))
        }
    }
}

/// Count the number of interleaved samples where `a` and `b` differ.
///
/// Returns 0 when the frames are identical or incomparable.
pub fn pcm_diff_count(a: &AudioFrame, b: &AudioFrame) -> usize {
    if a.sample_rate != b.sample_rate
        || a.channels != b.channels
        || a.sample_format != b.sample_format
        || a.data.len() != b.data.len()
    {
        return 0;
    }

    match a.sample_format {
        SampleFormat::F32 => {
            let a_s = read_f32(a);
            let b_s = read_f32(b);
            a_s.iter().zip(b_s.iter()).filter(|(&x, &y)| x != y).count()
        }
        SampleFormat::S16 => {
            let a_s = read_s16(a);
            let b_s = read_s16(b);
            a_s.iter().zip(b_s.iter()).filter(|(&x, &y)| x != y).count()
        }
    }
}

fn read_f32(f: &AudioFrame) -> Vec<f32> {
    f.data
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn read_s16(f: &AudioFrame) -> Vec<i16> {
    f.data
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpt_kinetix_core::timestamp::Timestamp;

    fn f32_frame(samples: &[f32]) -> AudioFrame {
        let mut data = Vec::with_capacity(samples.len() * 4);
        for &s in samples {
            data.extend_from_slice(&s.to_le_bytes());
        }
        AudioFrame {
            pts: Timestamp::NONE,
            data,
            sample_rate: 44_100,
            channels: 1,
            sample_format: SampleFormat::F32,
        }
    }

    #[test]
    fn identical_frames_within_tolerance() {
        let f = f32_frame(&[0.0, 1.0, -1.0, 0.5]);
        assert!(pcm_within_tolerance(&f, &f, 0.0));
        assert_eq!(pcm_max_abs_diff(&f, &f), Some(0.0));
        assert_eq!(pcm_diff_count(&f, &f), 0);
    }

    #[test]
    fn close_frames_within_loose_tolerance() {
        let a = f32_frame(&[0.0, 1.0]);
        let b = f32_frame(&[0.1, 0.9]);
        assert!(pcm_within_tolerance(&a, &b, 0.2));
        let d = pcm_max_abs_diff(&a, &b).unwrap();
        assert!(d > 0.09 && d < 0.11, "max diff was {d}");
    }

    #[test]
    fn mismatched_geometry_is_incomparable() {
        let a = f32_frame(&[0.0, 1.0]);
        let mut b = a.clone();
        b.sample_rate = 48_000;
        assert!(!pcm_within_tolerance(&a, &b, 0.0));
        assert_eq!(pcm_max_abs_diff(&a, &b), None);
    }
}
