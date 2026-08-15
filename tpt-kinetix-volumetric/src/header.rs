//! Volumetric bitstream header parsing.
//!
//! Framing follows DECISION 6 (C) of `docs/volumetric-codec-design.md`:
//! G-PCC-faithful coding tools wrapped in Kinetix framing. Headers are
//! **byte-aligned, big-endian** (same convention as `tpt-kinetix-lean`) and
//! introduced by the `b"VOLU"` magic. All multi-byte integers are big-endian.
//!
//! # Sequence header layout
//!
//! ```text
//! offset 0  : magic            [u8; 4]  = b"VOLU"
//! offset 4  : version          u8       (= 1)
//! offset 5  : max_points       u32 BE
//! offset 9  : octree_depth     u8
//! offset 10 : attr_count       u8
//! offset 11 : attribute_coding u8       (0 = lift, 1 = RAHT)
//! offset 12 : lossless         u8       (0 / 1)
//! offset 13 : dynamic          u8       (0 = static v1; != 0 reserved for v2)
//! offset 14 : intra_leaf_bits  u8
//! then attr_count * (kind u8, bit_depth u8)
//! ```
//!
//! # Frame header layout (immediately after the sequence header)
//!
//! ```text
//! offset 0  : frame_type       u8       (0 = static key frame)
//! offset 1  : num_points       u32 BE
//! offset 5  : payload_len      u32 BE
//! offset 9  : geometry_coding  u8       (0 = occupancy octree)
//! ```

use nom::{
    bytes::complete::tag,
    number::complete::{be_u32, be_u8},
    sequence::Tuple,
    IResult, Parser,
};
use tpt_kinetix_core::error::KinetixError;
use tpt_kinetix_core::frame::PointAttributeKind;

/// Magic bytes prefixing every volumetric stream (`VOLU`).
pub const MAGIC: &[u8; 4] = b"VOLU";

/// Bitstream format version understood by this decoder.
pub const VERSION: u8 = 1;

/// Hard ceiling on points per cloud (DECISION 8): bounds octree depth + arena
/// so a malformed stream cannot force unbounded allocation.
pub const MAX_POINTS: u32 = 10_000_000;

/// Attribute transform used by the decoder (DECISION 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttributeCoding {
    /// Region-adaptive predictive / lift (v1 default).
    Lift,
    /// Region-adaptive hierarchical transform (selectable).
    Raht,
}

/// A single per-point attribute described by the sequence header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttributeInfo {
    /// Semantic kind of the attribute channel.
    pub kind: PointAttributeKind,
    /// Quantization resolution in bits (8-16).
    pub bit_depth: u8,
}

/// Serialize an attribute kind to its on-wire byte.
pub fn attribute_kind_byte(kind: PointAttributeKind) -> u8 {
    match kind {
        PointAttributeKind::ColorRgb => 0,
        PointAttributeKind::Reflectance => 1,
        PointAttributeKind::Normal => 2,
    }
}

/// Sequence-level (decode-once) parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SequenceHeader {
    /// Format version (must equal [`VERSION`]).
    pub version: u8,
    /// Declared maximum number of points in the cloud.
    pub max_points: u32,
    /// Maximum octree depth (sets leaf precision).
    pub octree_depth: u8,
    /// Attribute channels declared by the stream.
    pub attributes: Vec<AttributeInfo>,
    /// Attribute transform in use.
    pub attribute_coding: AttributeCoding,
    /// Whether attributes are coded losslessly (DECISION 4).
    pub lossless: bool,
    /// Whether the stream is a dynamic (inter-frame) sequence (reserved for v2).
    pub dynamic: bool,
    /// Bits used to code each point's intra-leaf position.
    pub intra_leaf_bits: u8,
}

/// Per-frame parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    /// 0 = static key frame. Any other value is reserved for v2.
    pub frame_type: u8,
    /// Number of points in this frame.
    pub num_points: u32,
    /// Length in bytes of the coded geometry + attribute payload that follows.
    pub payload_len: u32,
    /// 0 = occupancy octree. Any other value is reserved.
    pub geometry_coding: u8,
}

fn map_err<'a, T>(
    what: &'static str,
    r: IResult<&'a [u8], T>,
) -> Result<(&'a [u8], T), KinetixError> {
    r.map_err(|e| KinetixError::Parse(format!("volumetric {what}: {e:?}")))
}

/// Parse the sequence header from the start of a stream.
///
/// Returns the parsed header and the remaining byte slice (the frame header and
/// payload). Rejects unknown versions, dynamic streams (DECISION 5), unknown
/// attribute kinds/codings, and point counts above [`MAX_POINTS`] (DECISION 8).
pub fn parse_sequence_header(input: &[u8]) -> Result<(&[u8], SequenceHeader), KinetixError> {
    let (rest, _) = map_err("magic", tag(MAGIC.as_slice()).parse(input))?;
    let (
        rest,
        (version, max_points, octree_depth, attr_count, coding, lossless, dynamic, intra_leaf),
    ) = map_err(
        "sequence header",
        (be_u8, be_u32, be_u8, be_u8, be_u8, be_u8, be_u8, be_u8).parse(rest),
    )?;

    if version != VERSION {
        return Err(KinetixError::Parse(format!(
            "volumetric: unsupported version {version}, expected {VERSION}"
        )));
    }
    if dynamic != 0 {
        return Err(KinetixError::Unsupported(
            "volumetric: dynamic point-cloud streams are reserved for v2; \
             the v1 decoder rejects them"
                .to_string(),
        ));
    }
    if max_points > MAX_POINTS {
        return Err(KinetixError::Unsupported(format!(
            "volumetric: declared max_points {max_points} exceeds the {MAX_POINTS}-point cap"
        )));
    }
    // Bound octree depth so `1 << octree_depth` (used to size the cube during
    // decode) cannot overflow a `u32` (DECISION 8 budget is 10-12).
    if octree_depth > 24 {
        return Err(KinetixError::Unsupported(format!(
            "volumetric: octree_depth {octree_depth} exceeds the supported maximum of 24"
        )));
    }

    let attribute_coding = match coding {
        0 => AttributeCoding::Lift,
        1 => AttributeCoding::Raht,
        other => {
            return Err(KinetixError::Parse(format!(
                "volumetric: unknown attribute coding {other}"
            )))
        }
    };

    let mut attributes = Vec::with_capacity(attr_count as usize);
    let mut cur = rest;
    for _ in 0..attr_count {
        let (r, (kind_byte, bit_depth)) =
            map_err("attribute info", (be_u8, be_u8).parse(cur))?;
        let kind = match kind_byte {
            0 => PointAttributeKind::ColorRgb,
            1 => PointAttributeKind::Reflectance,
            2 => PointAttributeKind::Normal,
            other => {
                return Err(KinetixError::Parse(format!(
                    "volumetric: unknown attribute kind {other}"
                )))
            }
        };
        attributes.push(AttributeInfo { kind, bit_depth });
        cur = r;
    }

    let header = SequenceHeader {
        version,
        max_points,
        octree_depth,
        attributes,
        attribute_coding,
        lossless: lossless != 0,
        dynamic: dynamic != 0,
        intra_leaf_bits: intra_leaf,
    };
    Ok((cur, header))
}

/// Parse the frame header that immediately follows the sequence header.
pub fn parse_frame_header(input: &[u8]) -> Result<(&[u8], FrameHeader), KinetixError> {
    let (rest, (frame_type, num_points, payload_len, geometry_coding)) = map_err(
        "frame header",
        (be_u8, be_u32, be_u32, be_u8).parse(input),
    )?;
    if frame_type != 0 {
        return Err(KinetixError::Unsupported(format!(
            "volumetric: frame_type {frame_type} is reserved for v2; v1 only supports static frames"
        )));
    }
    if num_points > MAX_POINTS {
        return Err(KinetixError::Unsupported(format!(
            "volumetric: frame declares {num_points} points, exceeding the {MAX_POINTS}-point cap"
        )));
    }
    if geometry_coding != 0 {
        return Err(KinetixError::Unsupported(format!(
            "volumetric: geometry_coding {geometry_coding} is reserved; v1 only supports octree"
        )));
    }
    Ok((
        rest,
        FrameHeader {
            frame_type,
            num_points,
            payload_len,
            geometry_coding,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_sequence(attr_kinds: &[(u8, u8)]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(MAGIC);
        b.push(VERSION);
        b.extend_from_slice(&1_000u32.to_be_bytes()); // max_points
        b.push(10); // octree_depth
        b.push(attr_kinds.len() as u8);
        b.push(0); // attribute_coding = lift
        b.push(1); // lossless
        b.push(0); // dynamic = false
        b.push(8); // intra_leaf_bits
        for (k, d) in attr_kinds {
            b.push(*k);
            b.push(*d);
        }
        b
    }

    #[test]
    fn parses_static_lift_lossless_rgb_sequence() {
        let bytes = build_sequence(&[(0, 8)]);
        let (rest, seq) = parse_sequence_header(&bytes).unwrap();
        assert_eq!(seq.version, VERSION);
        assert_eq!(seq.max_points, 1_000);
        assert_eq!(seq.octree_depth, 10);
        assert_eq!(seq.attribute_coding, AttributeCoding::Lift);
        assert!(seq.lossless);
        assert!(!seq.dynamic);
        assert_eq!(seq.intra_leaf_bits, 8);
        assert_eq!(seq.attributes.len(), 1);
        assert_eq!(seq.attributes[0].kind, PointAttributeKind::ColorRgb);
        assert_eq!(seq.attributes[0].bit_depth, 8);
        assert!(rest.is_empty());
    }

    #[test]
    fn rejects_bad_magic() {
        let mut b = build_sequence(&[(0, 8)]);
        b[0] = b'X';
        assert!(matches!(
            parse_sequence_header(&b),
            Err(KinetixError::Parse(_))
        ));
    }

    #[test]
    fn rejects_dynamic_stream() {
        let mut b = build_sequence(&[(0, 8)]);
        b[13] = 1; // dynamic = true
        assert!(matches!(
            parse_sequence_header(&b),
            Err(KinetixError::Unsupported(_))
        ));
    }

    #[test]
    fn rejects_excessive_octree_depth() {
        let mut b = build_sequence(&[(0, 8)]);
        b[9] = 30; // octree_depth = 30 (> 24)
        assert!(matches!(
            parse_sequence_header(&b),
            Err(KinetixError::Unsupported(_))
        ));
    }

    #[test]
    fn parses_frame_header() {
        let mut b = Vec::new();
        b.push(0); // frame_type
        b.extend_from_slice(&500u32.to_be_bytes());
        b.extend_from_slice(&42u32.to_be_bytes());
        b.push(0); // geometry_coding = octree
        let (rest, frame) = parse_frame_header(&b).unwrap();
        assert_eq!(frame.frame_type, 0);
        assert_eq!(frame.num_points, 500);
        assert_eq!(frame.payload_len, 42);
        assert_eq!(frame.geometry_coding, 0);
        assert!(rest.is_empty());
    }

    #[test]
    fn rejects_reserved_frame_type() {
        let mut b = Vec::new();
        b.push(1); // reserved frame_type
        b.extend_from_slice(&0u32.to_be_bytes());
        b.extend_from_slice(&0u32.to_be_bytes());
        b.push(0);
        assert!(matches!(
            parse_frame_header(&b),
            Err(KinetixError::Unsupported(_))
        ));
    }
}
