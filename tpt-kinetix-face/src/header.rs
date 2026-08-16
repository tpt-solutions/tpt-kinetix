//! Sequence / frame header parsing (DECISION 3 bitstream layout).
//!
//! Headers are **byte-aligned** (framing budget is spent on the rANS payload,
//! not the headers — same rationale as `tpt-kinetix-lean` / `tpt-kinetix-vision`).
//! Parsing is pure byte-level structure; the parameter payload itself is rANS-coded
//! and decoded in a later step.
//!
//! A decoder that receives a header it cannot honor (wrong magic, unsupported
//! version, or a `basis_hash` it cannot match) must **fail loudly** rather than
//! emit a wrong face (DECISION 8 honesty contract). The hash *match* check needs
//! the basis loader (implementation-order step 1) and is performed by the caller;
//! this module validates structure and surfaces the pinned hash for verification.

use nom::{
    bytes::complete::take,
    number::complete::{be_u16, be_u32, be_u8},
    IResult, Parser,
};

/// Magic bytes at the start of every face sequence: `b"FACE"`.
pub const FACE_MAGIC: &[u8; 4] = b"FACE";

/// Format version this crate implements.
pub const FACE_VERSION: u8 = 1;

/// Header parse / validation errors.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FaceHeaderError {
    /// Leading bytes were not `FACE_MAGIC`.
    #[error("face: bad magic (expected b\"FACE\")")]
    BadMagic,
    /// `version` field is higher than [`FACE_VERSION`].
    #[error("face: unsupported version {found} (max supported {max})")]
    UnsupportedVersion {
        /// The version found in the stream.
        found: u8,
        /// Maximum version this crate supports.
        max: u8,
    },
    /// Input ended before a complete header could be read.
    #[error("face: truncated header (need {needed} more bytes)")]
    Truncated {
        /// Extra bytes required to finish parsing.
        needed: usize,
    },
}

/// Per-group base quant steps, in header order.
///
/// Order is fixed by DECISION 3: `[identity, expression, pose, illumination,
/// appearance]`. Quantization step `q` maps to `2.powi(-(q as i32))`-style scaling
/// with `quant_precision` fractional bits; the exact dequant formula lives with
/// the parameter-vector decode (implementation-order step 3).
pub type GroupQp = [u8; 5];

/// Sequence-level flags (DECISION 3 `flags` u8).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SequenceFlags {
    /// `bit0` — a sparse-landmark companion block rides each frame (DECISION 1).
    pub landmark_companion: bool,
    /// `bit1` — an optional v2 neural-texture refinement layer is declared. A v1
    /// decoder without that model still decodes the rasterizer path (DECISION 2/8).
    pub neural_texture_layer: bool,
}

impl SequenceFlags {
    /// Pack into the on-wire `u8`.
    pub fn to_u8(self) -> u8 {
        let mut v = 0u8;
        if self.landmark_companion {
            v |= 1 << 0;
        }
        if self.neural_texture_layer {
            v |= 1 << 1;
        }
        v
    }

    /// Unpack from the on-wire `u8`. Unused bits are ignored (forward-compatible).
    pub fn from_u8(v: u8) -> Self {
        Self {
            landmark_companion: v & (1 << 0) != 0,
            neural_texture_layer: v & (1 << 1) != 0,
        }
    }
}

/// Frame-level flags (extension beyond the doc's `frame_type` byte).
///
/// The design lists `group_qp_override` as optional without specifying its
/// signal bit; we encode it here so the framing stays byte-aligned and
/// self-describing. `bit0` = inter frame, `bit1` = `group_qp_override` present.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrameFlags {
    /// `false` = key (full vector incl. identity), `true` = inter (deltas only).
    pub inter: bool,
    /// Whether [`FrameHeader::group_qp_override`] is present in this frame.
    pub has_qp_override: bool,
}

impl FrameFlags {
    /// Pack into the on-wire `u8`.
    pub fn to_u8(self) -> u8 {
        let mut v = 0u8;
        if self.inter {
            v |= 1 << 0;
        }
        if self.has_qp_override {
            v |= 1 << 1;
        }
        v
    }

    /// Unpack from the on-wire `u8`. Unused bits are ignored (forward-compatible).
    pub fn from_u8(v: u8) -> Self {
        Self {
            inter: v & (1 << 0) != 0,
            has_qp_override: v & (1 << 1) != 0,
        }
    }
}

/// Sequence header — one per file/stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaceSequenceHeader {
    /// Format version (`FACE_VERSION` for a v1 stream).
    pub version: u8,
    /// Index into the built-in 3DMM basis set (shape/expression/mean-albedo/SH).
    pub asset_basis_id: u8,
    /// Truncated hash of the exact basis asset; a decoder with a mismatched basis
    /// must reject rather than silently render a wrong face (DECISION 3).
    pub basis_hash: [u8; 8],
    /// Max frame width (arena sizing).
    pub max_width: u16,
    /// Max frame height (arena sizing).
    pub max_height: u16,
    /// Sequence-level feature flags.
    pub flags: SequenceFlags,
    /// Fractional bits for coefficient quant steps (0 = integer).
    pub quant_precision: u8,
    /// Per-group base quant steps: `[identity, expression, pose, illumination, appearance]`.
    pub group_qp: GroupQp,
}

/// Frame header — one per frame, precedes the rANS parameter payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaceFrameHeader {
    /// Frame classification flags (inter / qp-override-present).
    pub flags: FrameFlags,
    /// Frame width (≤ sequence `max_width`).
    pub width: u16,
    /// Frame height (≤ sequence `max_height`).
    pub height: u16,
    /// Delta reference: `0` = previous frame, `1` = last key (error resilience / scene cut).
    pub ref_mode: u8,
    /// Per-frame quant tweak, present only when [`FrameFlags::has_qp_override`].
    pub group_qp_override: Option<GroupQp>,
    /// Length of the rANS-coded parameter payload that follows this header.
    pub payload_len: u32,
}

fn parse_sequence_header(
    input: &[u8],
) -> IResult<&[u8], FaceSequenceHeader, nom::error::Error<&[u8]>> {
    let (input, _magic) = take(4usize)(input)?;
    let (
        input,
        (
            version,
            asset_basis_id,
            basis_hash,
            max_width,
            max_height,
            flags,
            quant_precision,
            group_qp,
        ),
    ) = (
        be_u8,
        be_u8,
        take(8usize),
        be_u16,
        be_u16,
        be_u8,
        be_u8,
        take(5usize),
    )
        .parse(input)?;

    let mut hash = [0u8; 8];
    hash.copy_from_slice(basis_hash);
    let mut qp = [0u8; 5];
    qp.copy_from_slice(group_qp);

    Ok((
        input,
        FaceSequenceHeader {
            version,
            asset_basis_id,
            basis_hash: hash,
            max_width,
            max_height,
            flags: SequenceFlags::from_u8(flags),
            quant_precision,
            group_qp: qp,
        },
    ))
}

fn parse_frame_header(input: &[u8]) -> IResult<&[u8], FaceFrameHeader, nom::error::Error<&[u8]>> {
    let (input, (flags, width, height, ref_mode)) = (be_u8, be_u16, be_u16, be_u8).parse(input)?;
    let frame_flags = FrameFlags::from_u8(flags);
    let (input, group_qp_override) = if frame_flags.has_qp_override {
        let (input, qp) = take(5usize)(input)?;
        let mut q = [0u8; 5];
        q.copy_from_slice(qp);
        (input, Some(q))
    } else {
        (input, None)
    };
    let (input, payload_len) = be_u32(input)?;
    Ok((
        input,
        FaceFrameHeader {
            flags: frame_flags,
            width,
            height,
            ref_mode,
            group_qp_override,
            payload_len,
        },
    ))
}

/// Parse and validate a [`FaceSequenceHeader`].
///
/// Returns [`FaceHeaderError::BadMagic`] / [`FaceHeaderError::UnsupportedVersion`]
/// for structurally-invalid streams, and [`FaceHeaderError::Truncated`] if the
/// buffer ended mid-header. The `basis_hash` is validated by the caller against a
/// loaded basis (DECISION 8).
pub fn read_sequence_header(buf: &[u8]) -> Result<FaceSequenceHeader, FaceHeaderError> {
    if buf.len() < 4 || &buf[..4] != FACE_MAGIC {
        return Err(FaceHeaderError::BadMagic);
    }
    let (_, header) =
        parse_sequence_header(buf).map_err(|_: nom::Err<nom::error::Error<&[u8]>>| {
            FaceHeaderError::Truncated { needed: 1 }
        })?;
    if header.version > FACE_VERSION {
        return Err(FaceHeaderError::UnsupportedVersion {
            found: header.version,
            max: FACE_VERSION,
        });
    }
    Ok(header)
}

/// Parse a [`FaceFrameHeader`] from the bytes immediately following the sequence
/// header (or a frame boundary).
///
/// Returns [`FaceHeaderError::Truncated`] if the buffer ended mid-header.
pub fn read_frame_header(buf: &[u8]) -> Result<FaceFrameHeader, FaceHeaderError> {
    let (_, header) =
        parse_frame_header(buf).map_err(|_: nom::Err<nom::error::Error<&[u8]>>| {
            FaceHeaderError::Truncated { needed: 1 }
        })?;
    Ok(header)
}

fn write_be_bytes(out: &mut Vec<u8>, header: &FaceSequenceHeader) {
    out.extend_from_slice(FACE_MAGIC);
    out.push(header.version);
    out.push(header.asset_basis_id);
    out.extend_from_slice(&header.basis_hash);
    out.extend_from_slice(&header.max_width.to_be_bytes());
    out.extend_from_slice(&header.max_height.to_be_bytes());
    out.push(SequenceFlags::to_u8(header.flags));
    out.push(header.quant_precision);
    out.extend_from_slice(&header.group_qp);
}

fn write_frame_bytes(out: &mut Vec<u8>, header: &FaceFrameHeader) {
    out.push(FrameFlags::to_u8(header.flags));
    out.extend_from_slice(&header.width.to_be_bytes());
    out.extend_from_slice(&header.height.to_be_bytes());
    out.push(header.ref_mode);
    if let Some(qp) = &header.group_qp_override {
        out.extend_from_slice(qp);
    }
    out.extend_from_slice(&header.payload_len.to_be_bytes());
}

/// Serialize a [`FaceSequenceHeader`] to its exact on-wire byte form.
///
/// Primarily for encoder use and round-trip tests; mirrors [`read_sequence_header`].
pub fn write_sequence_header(header: &FaceSequenceHeader) -> Vec<u8> {
    let mut out = Vec::with_capacity(32);
    write_be_bytes(&mut out, header);
    out
}

/// Serialize a [`FaceFrameHeader`] to its exact on-wire byte form.
///
/// Primarily for encoder use and round-trip tests; mirrors [`read_frame_header`].
pub fn write_frame_header(header: &FaceFrameHeader) -> Vec<u8> {
    let mut out = Vec::with_capacity(16);
    write_frame_bytes(&mut out, header);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_sequence() -> FaceSequenceHeader {
        FaceSequenceHeader {
            version: FACE_VERSION,
            asset_basis_id: 2,
            basis_hash: [0xAA, 0xBB, 0xCC, 0xDD, 0x01, 0x02, 0x03, 0x04],
            max_width: 1920,
            max_height: 1080,
            flags: SequenceFlags {
                landmark_companion: true,
                neural_texture_layer: false,
            },
            quant_precision: 4,
            group_qp: [10, 12, 8, 6, 9],
        }
    }

    fn sample_frame() -> FaceFrameHeader {
        FaceFrameHeader {
            flags: FrameFlags {
                inter: true,
                has_qp_override: true,
            },
            width: 1280,
            height: 720,
            ref_mode: 0,
            group_qp_override: Some([1, 2, 3, 4, 5]),
            payload_len: 4096,
        }
    }

    #[test]
    fn sequence_header_round_trips() {
        let h = sample_sequence();
        let bytes = write_sequence_header(&h);
        let parsed = read_sequence_header(&bytes).expect("valid header");
        assert_eq!(parsed, h);
    }

    #[test]
    fn frame_header_round_trips() {
        let h = sample_frame();
        let bytes = write_frame_header(&h);
        let parsed = read_frame_header(&bytes).expect("valid header");
        assert_eq!(parsed, h);
    }

    #[test]
    fn bad_magic_is_rejected() {
        let mut bytes = write_sequence_header(&sample_sequence());
        bytes[0] = b'X';
        assert!(matches!(
            read_sequence_header(&bytes),
            Err(FaceHeaderError::BadMagic)
        ));
    }

    #[test]
    fn unsupported_version_is_rejected() {
        let mut h = sample_sequence();
        h.version = FACE_VERSION + 1;
        let bytes = write_sequence_header(&h);
        assert!(matches!(
            read_sequence_header(&bytes),
            Err(FaceHeaderError::UnsupportedVersion { .. })
        ));
    }

    #[test]
    fn truncated_sequence_is_rejected() {
        let bytes = write_sequence_header(&sample_sequence())[..10].to_vec();
        assert!(matches!(
            read_sequence_header(&bytes),
            Err(FaceHeaderError::Truncated { .. })
        ));
    }

    #[test]
    fn truncated_frame_is_rejected() {
        let bytes = write_frame_header(&sample_frame())[..4].to_vec();
        assert!(matches!(
            read_frame_header(&bytes),
            Err(FaceHeaderError::Truncated { .. })
        ));
    }

    #[test]
    fn key_frame_has_no_qp_override() {
        let h = FaceFrameHeader {
            flags: FrameFlags {
                inter: false,
                has_qp_override: false,
            },
            width: 640,
            height: 480,
            ref_mode: 1,
            group_qp_override: None,
            payload_len: 0,
        };
        let bytes = write_frame_header(&h);
        let parsed = read_frame_header(&bytes).expect("valid header");
        assert_eq!(parsed, h);
        assert!(!parsed.flags.inter);
        assert!(parsed.group_qp_override.is_none());
    }

    #[test]
    fn flags_round_trip() {
        let f = SequenceFlags {
            landmark_companion: true,
            neural_texture_layer: true,
        };
        assert_eq!(SequenceFlags::from_u8(f.to_u8()), f);
        let ff = FrameFlags {
            inter: true,
            has_qp_override: false,
        };
        assert_eq!(FrameFlags::from_u8(ff.to_u8()), ff);
    }
}
