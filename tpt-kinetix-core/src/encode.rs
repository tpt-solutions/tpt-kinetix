//! Shared encoder configuration types.
//!
//! Encoders across the Kinetix engine (currently the `rav1e`-backed AV1 encoder
//! in `tpt-kinetix-av1`) accept their tuning parameters through the codec-agnostic
//! [`EncodeConfig`] defined here, so that higher layers (pipeline, CLI) can
//! express encode intent without depending on any specific codec crate.
//!
//! The [`RateControl`] enum captures the two rate-control strategies supported
//! by the initial release, and [`SpeedPreset`] maps human-friendly presets onto
//! the numeric speed knobs used by real encoders.

use serde::{Deserialize, Serialize};

/// How the encoder should allocate bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RateControl {
    /// Constant-quality mode driven by a quantizer value (0 = best quality,
    /// 255 = worst quality). This is `rav1e`'s CQP mode.
    ConstantQuality {
        /// Quantizer index (0..=255).
        quantizer: u8,
    },
    /// Target-bitrate mode in bits per second.
    Bitrate {
        /// Target bitrate in bits per second.
        bits_per_second: u32,
    },
}

impl Default for RateControl {
    fn default() -> Self {
        RateControl::ConstantQuality { quantizer: 100 }
    }
}

/// Human-friendly speed / quality trade-off preset.
///
/// Presets map onto a numeric speed value via [`SpeedPreset::to_speed`], where
/// `0` is the slowest / highest quality and `10` is the fastest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SpeedPreset {
    /// Highest quality, slowest encode (speed 0).
    Slowest,
    /// Balanced default (speed 6).
    #[default]
    Medium,
    /// Fastest encode, lowest quality (speed 10).
    Fastest,
    /// An explicit numeric speed value (clamped to 0..=10).
    Custom(u8),
}

impl SpeedPreset {
    /// Map the preset to a numeric speed value in the range `0..=10`.
    ///
    /// # Examples
    ///
    /// ```
    /// use tpt_kinetix_core::encode::SpeedPreset;
    ///
    /// assert_eq!(SpeedPreset::Slowest.to_speed(), 0);
    /// assert_eq!(SpeedPreset::Medium.to_speed(), 6);
    /// assert_eq!(SpeedPreset::Fastest.to_speed(), 10);
    /// assert_eq!(SpeedPreset::Custom(20).to_speed(), 10); // clamped
    /// ```
    pub fn to_speed(self) -> u8 {
        match self {
            SpeedPreset::Slowest => 0,
            SpeedPreset::Medium => 6,
            SpeedPreset::Fastest => 10,
            SpeedPreset::Custom(v) => v.min(10),
        }
    }
}

/// Codec-agnostic video encode configuration.
///
/// Codec-specific encoders translate this into their own configuration type.
///
/// # Examples
///
/// ```
/// use tpt_kinetix_core::encode::{EncodeConfig, RateControl, SpeedPreset};
///
/// let cfg = EncodeConfig {
///     width: 1920,
///     height: 1080,
///     rate_control: RateControl::Bitrate { bits_per_second: 6_000_000 },
///     speed: SpeedPreset::Medium,
///     keyframe_interval: 240,
/// };
/// assert_eq!(cfg.speed.to_speed(), 6);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncodeConfig {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Rate-control strategy.
    pub rate_control: RateControl,
    /// Speed / quality preset.
    pub speed: SpeedPreset,
    /// Maximum interval between keyframes, in frames.
    pub keyframe_interval: u64,
}

impl Default for EncodeConfig {
    fn default() -> Self {
        Self {
            width: 640,
            height: 480,
            rate_control: RateControl::default(),
            speed: SpeedPreset::default(),
            keyframe_interval: 240,
        }
    }
}

impl EncodeConfig {
    /// Returns the quantizer to use, defaulting sensibly when in bitrate mode.
    pub fn quantizer(&self) -> u8 {
        match self.rate_control {
            RateControl::ConstantQuality { quantizer } => quantizer,
            // rav1e uses the quantizer as an upper bound in bitrate mode; a
            // neutral mid value is a safe default.
            RateControl::Bitrate { .. } => 100,
        }
    }

    /// Returns the target bitrate in bits per second (0 in constant-quality mode).
    pub fn bitrate(&self) -> u32 {
        match self.rate_control {
            RateControl::ConstantQuality { .. } => 0,
            RateControl::Bitrate { bits_per_second } => bits_per_second,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_constant_quality() {
        let cfg = EncodeConfig::default();
        assert_eq!(cfg.bitrate(), 0);
        assert_eq!(cfg.quantizer(), 100);
        assert_eq!(cfg.speed.to_speed(), 6);
    }

    #[test]
    fn bitrate_mode_reports_bitrate() {
        let cfg = EncodeConfig {
            rate_control: RateControl::Bitrate {
                bits_per_second: 5_000_000,
            },
            ..Default::default()
        };
        assert_eq!(cfg.bitrate(), 5_000_000);
    }

    #[test]
    fn custom_speed_clamps() {
        assert_eq!(SpeedPreset::Custom(255).to_speed(), 10);
        assert_eq!(SpeedPreset::Custom(3).to_speed(), 3);
    }
}
