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

/// The kind of per-point attribute carried by a [`PointCloud`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PointAttributeKind {
    /// 8- or 10-16-bit RGB colour (three samples per point at the attribute's
    /// bit depth).
    ColorRgb,
    /// Scalar reflectance / intensity (e.g. LiDAR), 8-16 bit, one sample per
    /// point.
    Reflectance,
    /// Unit normal vector (nx, ny, nz), quantized to the attribute bit depth,
    /// three samples per point.
    Normal,
}

/// A single per-point attribute channel of a [`PointCloud`].
///
/// `data` holds the packed per-point values; `bit_depth` records the
/// quantization resolution (8 for typical AR/VR capture, 10-16 for HDR /
/// scientific capture, per the volumetric codec design DECISION 4). The packing
/// matches `kind`: colour and normal are `3 * num_points` samples, reflectance
/// is `num_points`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PointAttribute {
    /// Semantic kind of this attribute channel.
    pub kind: PointAttributeKind,
    /// Quantization resolution in bits (8-16).
    pub bit_depth: u8,
    /// Packed per-point attribute values.
    pub data: Vec<u8>,
}

/// A decoded volumetric point cloud.
///
/// This is the primary output type of `tpt-kinetix-volumetric`, parallel to
/// [`VideoFrame`] for the 2D codecs. A cloud is an *unstructured* set of 3D
/// points: `positions` holds `3 * num_points` `f32` values (x, y, z per point)
/// and `attributes` holds zero or more per-point attribute channels (colour,
/// reflectance, normal). There is no fixed 2D tiling — the data shape is
/// fundamentally 3D, which is why a separate representation from a pixel frame
/// is required.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PointCloud {
    /// Number of points in the cloud.
    pub num_points: usize,
    /// Flat `f32` position buffer: `positions[3 * i + 0..3]` is point `i`'s
    /// (x, y, z) coordinates.
    pub positions: Vec<f32>,
    /// Per-point attribute channels (colour, reflectance, normal, ...).
    pub attributes: Vec<PointAttribute>,
}

impl PointCloud {
    /// Expected length of `positions` (`3 * num_points`).
    #[must_use]
    pub fn expected_positions_len(num_points: usize) -> usize {
        num_points.saturating_mul(3)
    }

    /// `true` if this cloud carries no points.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.num_points == 0
    }

    /// Number of attribute channels carried by this cloud.
    #[must_use]
    pub fn attribute_count(&self) -> usize {
        self.attributes.len()
    }
}
