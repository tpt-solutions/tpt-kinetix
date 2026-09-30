use serde::{Deserialize, Serialize};

/// Supported pixel / chroma-sampling formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PixelFormat {
    /// 4:2:0 planar YUV — the most common format for H.264 and AV1 content.
    Yuv420p,
    /// 4:2:2 planar YUV.
    Yuv422p,
    /// 4:4:4 planar YUV — full-chroma, used for lossless / high-quality workflows.
    Yuv444p,
    /// Monochrome planar greyscale — a single luma plane (AV1 `mono_chrome`).
    Gray,
    /// 24-bit packed RGB (R, G, B byte order).
    Rgb24,
    /// 24-bit packed BGR (B, G, R byte order).
    Bgr24,
    /// 4:2:0 planar YUV, 10 bits per sample stored as little-endian 16-bit words.
    Yuv420p10le,
    /// 4:2:0 planar YUV, 12 bits per sample stored as little-endian 16-bit words.
    Yuv420p12le,
    /// Monochrome planar greyscale, 10 bits per sample stored as little-endian
    /// 16-bit words.
    Gray10le,
    /// Monochrome planar greyscale, 12 bits per sample stored as little-endian
    /// 16-bit words.
    Gray12le,
}

impl PixelFormat {
    /// Returns the number of planes for this format.
    pub fn num_planes(self) -> usize {
        match self {
            PixelFormat::Yuv420p
            | PixelFormat::Yuv422p
            | PixelFormat::Yuv444p
            | PixelFormat::Yuv420p10le
            | PixelFormat::Yuv420p12le => 3,
            PixelFormat::Gray | PixelFormat::Gray10le | PixelFormat::Gray12le => 1,
            PixelFormat::Rgb24 | PixelFormat::Bgr24 => 1,
        }
    }

    /// Returns the number of bits per pixel (averaged across all planes).
    ///
    /// # Examples
    ///
    /// ```
    /// use tpt_kinetix_core::PixelFormat;
    /// assert_eq!(PixelFormat::Yuv420p.bits_per_pixel(), 12);
    /// ```
    pub fn bits_per_pixel(self) -> u32 {
        match self {
            PixelFormat::Yuv420p => 12,
            PixelFormat::Yuv422p => 16,
            PixelFormat::Yuv444p => 24,
            PixelFormat::Gray => 8,
            PixelFormat::Rgb24 | PixelFormat::Bgr24 => 24,
            // 16-bit storage per sample, 4:2:0: 1.5 samples per pixel.
            PixelFormat::Yuv420p10le | PixelFormat::Yuv420p12le => 24,
            PixelFormat::Gray10le | PixelFormat::Gray12le => 16,
        }
    }
}

impl std::fmt::Display for PixelFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            PixelFormat::Yuv420p => "yuv420p",
            PixelFormat::Yuv422p => "yuv422p",
            PixelFormat::Yuv444p => "yuv444p",
            PixelFormat::Gray => "gray",
            PixelFormat::Rgb24 => "rgb24",
            PixelFormat::Bgr24 => "bgr24",
            PixelFormat::Yuv420p10le => "yuv420p10le",
            PixelFormat::Yuv420p12le => "yuv420p12le",
            PixelFormat::Gray10le => "gray10le",
            PixelFormat::Gray12le => "gray12le",
        };
        f.write_str(s)
    }
}
