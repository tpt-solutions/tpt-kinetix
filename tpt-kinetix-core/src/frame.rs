use serde::{Deserialize, Serialize};

use crate::{pixel_format::PixelFormat, timestamp::Timestamp};

/// A decoded video frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoFrame {
    /// Presentation timestamp — when this frame should be displayed.
    pub pts: Timestamp,
    /// Decode timestamp — when this frame must have been decoded by.
    pub dts: Timestamp,
    /// Raw plane data.  Layout depends on `pixel_format`.
    pub data: Vec<u8>,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Pixel / chroma-sampling format.
    pub pixel_format: PixelFormat,
    /// Whether this frame can be used as a random-access seek point.
    pub is_key_frame: bool,
}

impl VideoFrame {
    /// Computes the expected data length for a contiguous plane layout.
    ///
    /// Returns `None` if the format is unknown or the dimensions overflow.
    pub fn expected_data_len(width: u32, height: u32, pixel_format: PixelFormat) -> Option<usize> {
        let pixels = (width as usize).checked_mul(height as usize)?;
        let bits = pixels.checked_mul(pixel_format.bits_per_pixel() as usize)?;
        // Round up to a whole number of bytes.
        Some(bits.div_ceil(8))
    }
}

/// Interleaved sample format for decoded audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SampleFormat {
    /// Signed 16-bit interleaved PCM.
    S16,
    /// 32-bit float interleaved PCM.
    F32,
}

/// A decoded audio frame: a block of interleaved PCM samples.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioFrame {
    /// Presentation timestamp — when this block should be played.
    pub pts: Timestamp,
    /// Raw interleaved PCM sample bytes. Layout depends on `sample_format`.
    pub data: Vec<u8>,
    /// Sample rate in Hz (e.g. 44_100, 48_000).
    pub sample_rate: u32,
    /// Number of interleaved channels.
    pub channels: u8,
    /// Sample format of `data`.
    pub sample_format: SampleFormat,
}

impl AudioFrame {
    /// Number of samples per channel in this frame.
    pub fn samples_per_channel(&self) -> usize {
        let bytes_per_sample = match self.sample_format {
            SampleFormat::S16 => 2,
            SampleFormat::F32 => 4,
        };
        let denom = bytes_per_sample * self.channels.max(1) as usize;
        self.data.len().checked_div(denom).unwrap_or(0)
    }
}
