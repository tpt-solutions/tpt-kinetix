//! Scale-factor band offset tables (ISO/IEC 14496-3:2009 Tables 4.165 / 4.168).
//!
//! These describe, for each sampling-frequency index, how the 1024 (long window)
//! or 128 (short window) spectral lines are partitioned into scalefactor bands.
//! The values are the standard published tables (factual data, transcribable
//! from the spec); the trailing entry is always the transform length so the
//! number of bands is `table.len() - 1`.

/// Long-window (1024-line) scalefactor band offsets, indexed by the 4-bit
/// sampling-frequency index (0..=11). Index 12 (7350 Hz) is reserved.
pub const SWB_OFFSET_1024: [&[u16]; 12] = [
    &SWB_1024_96000,
    &SWB_1024_96000,
    &SWB_1024_64000,
    &SWB_1024_48000,
    &SWB_1024_48000,
    &SWB_1024_32000,
    &SWB_1024_24000,
    &SWB_1024_24000,
    &SWB_1024_16000,
    &SWB_1024_16000,
    &SWB_1024_16000,
    &SWB_1024_8000,
];

/// Short-window (128-line) scalefactor band offsets, indexed like
/// [`SWB_OFFSET_1024`].
pub const SWB_OFFSET_128: [&[u16]; 12] = [
    &SWB_128_96000,
    &SWB_128_96000,
    &SWB_128_96000,
    &SWB_128_48000,
    &SWB_128_48000,
    &SWB_128_48000,
    &SWB_128_24000,
    &SWB_128_24000,
    &SWB_128_16000,
    &SWB_128_16000,
    &SWB_128_16000,
    &SWB_128_8000,
];

/// Maximum number of long-window scalefactor bands TNS may operate on, indexed
/// by sampling-frequency index.
pub const TNS_MAX_BANDS_1024: [u8; 12] = [31, 31, 34, 40, 42, 51, 46, 46, 42, 42, 42, 39];

/// Maximum number of short-window scalefactor bands TNS may operate on.
pub const TNS_MAX_BANDS_128: [u8; 12] = [9, 9, 10, 14, 14, 14, 14, 14, 14, 14, 14, 14];

const SWB_1024_96000: &[u16] = &[
    0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 40, 44, 48, 52, 56, 64, 72, 80, 88, 96, 108, 120, 132,
    144, 156, 172, 188, 212, 240, 276, 320, 384, 448, 512, 576, 640, 704, 768, 832, 896, 960,
    1024,
];

const SWB_1024_64000: &[u16] = &[
    0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 40, 44, 48, 52, 56, 64, 72, 80, 88, 100, 112, 124, 140,
    156, 172, 192, 216, 240, 268, 304, 344, 384, 424, 464, 504, 544, 584, 624, 664, 704, 744, 784,
    824, 864, 904, 944, 984, 1024,
];

const SWB_1024_48000: &[u16] = &[
    0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 40, 48, 56, 64, 72, 80, 88, 96, 108, 120, 132, 144, 160,
    176, 196, 216, 240, 264, 292, 320, 352, 384, 416, 448, 480, 512, 544, 576, 608, 640, 672, 704,
    736, 768, 800, 832, 864, 896, 928, 1024,
];

const SWB_1024_32000: &[u16] = &[
    0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 40, 48, 56, 64, 72, 80, 88, 96, 108, 120, 132, 144, 160,
    176, 196, 216, 240, 264, 292, 320, 352, 384, 416, 448, 480, 512, 544, 576, 608, 640, 672, 704,
    736, 768, 800, 832, 864, 896, 928, 960, 992, 1024,
];

const SWB_1024_24000: &[u16] = &[
    0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 40, 44, 52, 60, 68, 76, 84, 92, 100, 108, 116, 124, 136,
    148, 160, 172, 188, 204, 220, 240, 260, 284, 308, 336, 364, 396, 432, 468, 508, 552, 600, 652,
    704, 768, 832, 896, 960, 1024,
];

const SWB_1024_16000: &[u16] = &[
    0, 8, 16, 24, 32, 40, 48, 56, 64, 72, 80, 88, 100, 112, 124, 136, 148, 160, 172, 184, 196, 212,
    228, 244, 260, 280, 300, 320, 344, 368, 396, 424, 456, 492, 532, 572, 616, 664, 716, 772, 832,
    896, 960, 1024,
];

const SWB_1024_8000: &[u16] = &[
    0, 12, 24, 36, 48, 60, 72, 84, 96, 108, 120, 132, 144, 156, 172, 188, 204, 220, 236, 252, 268,
    288, 308, 328, 348, 372, 396, 420, 448, 476, 508, 544, 580, 620, 664, 712, 764, 820, 880, 944,
    1024,
];

const SWB_128_96000: &[u16] = &[0, 4, 8, 12, 16, 20, 24, 32, 40, 48, 64, 92, 128];

const SWB_128_48000: &[u16] = &[0, 4, 8, 12, 16, 20, 28, 36, 44, 56, 68, 80, 96, 112, 128];

const SWB_128_24000: &[u16] = &[
    0, 4, 8, 12, 16, 20, 24, 28, 36, 44, 52, 64, 76, 92, 108, 128,
];

const SWB_128_16000: &[u16] = &[0, 4, 8, 12, 16, 20, 24, 28, 32, 40, 48, 60, 72, 88, 108, 128];

const SWB_128_8000: &[u16] = &[0, 4, 8, 12, 16, 20, 24, 28, 36, 44, 52, 60, 72, 88, 108, 128];

/// Number of long-window scalefactor bands for a sampling-frequency index.
#[inline]
pub fn num_swb_1024(sf_index: usize) -> usize {
    SWB_OFFSET_1024[sf_index].len() - 1
}

/// Number of short-window scalefactor bands for a sampling-frequency index.
#[inline]
pub fn num_swb_128(sf_index: usize) -> usize {
    SWB_OFFSET_128[sf_index].len() - 1
}
