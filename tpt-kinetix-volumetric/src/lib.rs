//! `tpt-kinetix-volumetric` — a point-cloud / volumetric codec for AR-VR
//! content, designed from scratch for the TPT Kinetix media processing engine.
//!
//! # Design
//!
//! `tpt-kinetix-volumetric` encodes/decodes **3D point clouds** — the dominant
//! representation for captured volumetric / AR-VR content (Depthkit, 8i, LiDAR
//! / depth fusion). It is a fundamentally different data shape from the 2D
//! frame codecs in this workspace: an unstructured set of 3D positions plus
//! per-point attributes (colour, reflectance, normal), with no fixed 2D tiling.
//!
//! v1 targets a **static single cloud** and codes geometry with a **context-
//! modeled occupancy octree** and attributes with **region-adaptive predictive
//! (lift)** (default) or **RAHT** (selectable). Both lossless and lossy
//! attribute coding are supported. The coding tools are transcribed from MPEG-I
//! G-PCC (TMC13) and wrapped in Kinetix framing (`magic b"VOLU"`), so the
//! reference software is a bit-exact conformance oracle.
//!
//! # Status
//!
//! Geometry (octree) and attribute (lift/RAHT) decode are implemented and
//! round-trip against the in-crate [`encode`] path. The decoder is **not yet
//! validated bit-exact against the TMC13 oracle**, so
//! [`VolumetricDecoderImpl::capabilities`] reports `pixel_exact: false` and
//! strict mode rejects output with [`KinetixError::NotPixelExact`]. Malformed
//! streams and any stream declaring the reserved `dynamic` flag fail with
//! [`KinetixError::Parse`] / [`KinetixError::Unsupported`] respectively. See
//! [`VolumetricDecoderImpl::capabilities`] for what is and isn't supported.
//!
//! # References
//!
//! - Design doc: `docs/volumetric-codec-design.md`

pub mod attribute;
pub mod encode;
pub mod entropy;
pub mod header;
pub mod octree;
pub mod payload;

use tpt_kinetix_core::{
    capabilities::DecoderCapabilities, error::KinetixError, frame::PointCloud, packet::Packet,
};

use crate::attribute::{decode_attributes, pack_streams, samples_per_attr};
use crate::header::parse_frame_header;
use crate::header::parse_sequence_header;
use crate::octree::decode_geometry;
use crate::payload::unframe_payload;

/// Decode interface for the volumetric (point-cloud) codec.
///
/// `decode()` returns a [`PointCloud`] — the primary decoded output, parallel
/// to a [`tpt_kinetix_core::frame::VideoFrame`] for the 2D codecs. Dynamic /
/// inter-frame streams are not supported in v1 (the `dynamic` sequence-header
/// flag is reserved for v2).
pub trait VolumetricDecoder {
    /// Decode a compressed packet into a [`PointCloud`].
    ///
    /// In strict mode, when the decoder cannot guarantee bit-exact output, this
    /// returns [`KinetixError::NotPixelExact`] instead of a reconstructed cloud.
    fn decode(&mut self, packet: &Packet) -> Result<Option<PointCloud>, KinetixError>;
}

/// Stateful volumetric-format decoder.
///
/// Feed compressed [`Packet`]s via [`VolumetricDecoder::decode`] and receive
/// decoded [`PointCloud`]s.
///
/// # Honesty contract
///
/// Geometry/attribute decode is implemented and round-trips against the in-crate
/// encoder, but it has **not** been validated bit-exact against the TMC13
/// reference oracle. Callers that require bit-exact output must enable strict
/// mode; in strict mode `decode` returns [`KinetixError::NotPixelExact`] rather
/// than reconstructed points. Check [`DecoderCapabilities::pixel_exact`] before
/// trusting output.
pub struct VolumetricDecoderImpl {
    strict: bool,
}

impl VolumetricDecoderImpl {
    /// Create a new decoder in non-strict mode.
    pub fn new() -> Self {
        Self { strict: false }
    }

    /// Enable strict mode.
    ///
    /// In strict mode, [`VolumetricDecoderImpl::decode`] returns
    /// [`KinetixError::NotPixelExact`] when the reconstructed output cannot be
    /// guaranteed bit-exact (the current state for this decoder).
    pub fn with_strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }

    /// Report what this decoder can and cannot do.
    ///
    /// Geometry (octree) and attribute (lift/RAHT) decode are implemented and
    /// round-trip against the in-crate encoder, but the decoder is **not** yet
    /// validated bit-exact against the TMC13 oracle, so `pixel_exact` is
    /// `false`. Callers requiring exact output should use strict mode.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use tpt_kinetix_volumetric::VolumetricDecoderImpl;
    ///
    /// let caps = VolumetricDecoderImpl::new().capabilities();
    /// assert!(!caps.pixel_exact);
    /// assert!(caps.is_incomplete());
    /// ```
    pub fn capabilities(&self) -> DecoderCapabilities {
        DecoderCapabilities {
            codec: "volumetric",
            pixel_exact: false,
            supports_cabac: false,
            supports_cavlc: false,
            supports_intra_prediction: false,
            supports_inter_prediction: false,
            supports_deblocking: false,
            notes: "octree geometry + lift/RAHT attribute decode implemented and \
                    round-trip tested; not yet validated bit-exact against the \
                    TMC13 oracle (strict mode rejects output)",
        }
    }
}

impl Default for VolumetricDecoderImpl {
    fn default() -> Self {
        Self::new()
    }
}

impl VolumetricDecoder for VolumetricDecoderImpl {
    fn decode(&mut self, packet: &Packet) -> Result<Option<PointCloud>, KinetixError> {
        let (rest, seq) = parse_sequence_header(&packet.data)?;
        let (rest, frame) = parse_frame_header(rest)?;

        // Empty cloud (or empty payload) is valid and decodes to no points. In
        // strict mode a *lossy* empty stream is still rejected (DECISION 4:
        // strict mode rejects lossy output), but a lossless one is fine.
        if frame.num_points == 0 || frame.payload_len == 0 {
            if self.strict && !seq.lossless {
                return Err(KinetixError::NotPixelExact(
                    "volumetric: lossy empty stream rejected in strict mode".into(),
                ));
            }
            return Ok(Some(PointCloud {
                num_points: 0,
                positions: Vec::new(),
                attributes: Vec::new(),
            }));
        }

        let payload_len = frame.payload_len as usize;
        let payload = rest.get(..payload_len).ok_or_else(|| {
            KinetixError::Parse("volumetric: payload length overruns packet".into())
        })?;

        let (occ, leaf, attr) = unframe_payload(payload)?;

        let geom = decode_geometry(occ, leaf, seq.octree_depth, frame.num_points)?;

        let num_streams: usize = seq
            .attributes
            .iter()
            .map(|a| samples_per_attr(a.kind))
            .sum();
        let streams = decode_attributes(
            attr,
            num_streams,
            frame.num_points as usize,
            seq.attribute_coding,
            seq.lossless,
        )?;
        let attributes = pack_streams(&streams, &seq.attributes, frame.num_points as usize);

        let scale = (1u32 << seq.octree_depth) as f32;
        let mut positions = Vec::with_capacity(geom.points.len() * 3);
        for c in &geom.points {
            positions.push(c[0] as f32 / scale);
            positions.push(c[1] as f32 / scale);
            positions.push(c[2] as f32 / scale);
        }

        let cloud = PointCloud {
            num_points: geom.points.len(),
            positions,
            attributes,
        };

        // DECISION 4: strict mode pairs with `lossless`. A lossy stream (the
        // quantizer step in the attribute coder is > 1) cannot guarantee
        // bit-exact output, so strict mode rejects it rather than returning
        // approximated points.
        if self.strict && !seq.lossless {
            return Err(KinetixError::NotPixelExact(
                "volumetric: lossy stream rejected in strict mode (enable lossless to decode)"
                    .into(),
            ));
        }
        Ok(Some(cloud))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::{encode_volumetric, EncodeParams};
    use crate::header::{AttributeCoding, AttributeInfo};
    use tpt_kinetix_core::error::KinetixError;
    use tpt_kinetix_core::frame::{PointAttribute, PointAttributeKind};
    use tpt_kinetix_core::timestamp::Timestamp;

    /// A minimal valid v1 static stream (sequence + frame header, empty payload).
    /// This version is lossy (lossless = 0) so strict mode rejects it.
    fn valid_static_stream() -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(header::MAGIC);
        b.push(header::VERSION);
        b.extend_from_slice(&1u32.to_be_bytes()); // max_points
        b.push(10); // octree_depth
        b.push(0); // attr_count
        b.push(0); // attribute_coding = lift
        b.push(0); // lossless = false (lossy)
        b.push(0); // dynamic = false
        b.push(8); // intra_leaf_bits
        b.push(0); // frame_type
        b.extend_from_slice(&0u32.to_be_bytes()); // num_points
        b.extend_from_slice(&0u32.to_be_bytes()); // payload_len
        b.push(0); // geometry_coding = octree
        b
    }

    fn packet_with(data: Vec<u8>) -> Packet {
        Packet {
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            data,
            stream_index: 0,
            is_key_frame: true,
        }
    }

    #[test]
    fn scaffold_reports_not_pixel_exact() {
        assert!(!VolumetricDecoderImpl::new().capabilities().pixel_exact);
    }

    #[test]
    fn strict_mode_errors_until_implemented() {
        let mut dec = VolumetricDecoderImpl::new().with_strict(true);
        assert!(matches!(
            dec.decode(&packet_with(valid_static_stream())),
            Err(KinetixError::NotPixelExact(_))
        ));
    }

    #[test]
    fn non_strict_mode_returns_cloud_for_empty_payload() {
        let mut dec = VolumetricDecoderImpl::new();
        assert!(matches!(
            dec.decode(&packet_with(valid_static_stream())),
            Ok(Some(_))
        ));
    }

    #[test]
    fn decode_rejects_dynamic_stream() {
        let mut stream = valid_static_stream();
        // `dynamic` flag is at offset 13 in the sequence header layout.
        stream[13] = 1;
        let mut dec = VolumetricDecoderImpl::new();
        assert!(matches!(
            dec.decode(&packet_with(stream)),
            Err(KinetixError::Unsupported(_))
        ));
    }

    #[test]
    fn decode_rejects_malformed_stream() {
        let mut dec = VolumetricDecoderImpl::new();
        assert!(matches!(
            dec.decode(&packet_with(vec![])),
            Err(KinetixError::Parse(_))
        ));
    }

    fn rgb_cloud(points: &[[f32; 3]], colors: &[[u8; 3]]) -> PointCloud {
        let mut positions = Vec::with_capacity(points.len() * 3);
        for p in points {
            positions.extend_from_slice(p);
        }
        let mut data = Vec::with_capacity(colors.len() * 3);
        for c in colors {
            data.extend_from_slice(c);
        }
        PointCloud {
            num_points: points.len(),
            positions,
            attributes: vec![PointAttribute {
                kind: PointAttributeKind::ColorRgb,
                bit_depth: 8,
                data,
            }],
        }
    }

    #[test]
    fn geometry_and_attributes_round_trip_lift() {
        let pts = [
            [0.1, 0.2, 0.3],
            [0.9, 0.8, 0.7],
            [0.5, 0.5, 0.5],
            [0.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
        ];
        let cols = [
            [10u8, 20, 30],
            [200, 100, 50],
            [0, 255, 128],
            [1, 2, 3],
            [250, 250, 250],
        ];
        let cloud = rgb_cloud(&pts, &cols);

        let params = EncodeParams {
            octree_depth: 8,
            attributes: vec![AttributeInfo {
                kind: PointAttributeKind::ColorRgb,
                bit_depth: 8,
            }],
            attribute_coding: AttributeCoding::Lift,
            lossless: true,
            intra_leaf_bits: 0,
        };
        let bytes = encode_volumetric(&cloud, &params);

        let mut dec = VolumetricDecoderImpl::new();
        let decoded = dec
            .decode(&packet_with(bytes))
            .expect("decode")
            .expect("some");

        assert_eq!(decoded.num_points, 5);
        // Attributes are lossless: compare the decoded color bytes (order is
        // Morton-sorted, so compare as multisets).
        let mut a: Vec<[u8; 3]> = Vec::new();
        let mut b: Vec<[u8; 3]> = Vec::new();
        for (p, col) in cols.iter().enumerate().take(5) {
            a.push([col[0], col[1], col[2]]);
            b.push([
                decoded.attributes[0].data[p * 3],
                decoded.attributes[0].data[p * 3 + 1],
                decoded.attributes[0].data[p * 3 + 2],
            ]);
        }
        a.sort();
        b.sort();
        assert_eq!(a, b);
    }

    #[test]
    fn attributes_round_trip_raht() {
        let pts = [
            [0.1, 0.2, 0.3],
            [0.9, 0.8, 0.7],
            [0.5, 0.5, 0.5],
            [0.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
            [0.3, 0.6, 0.9],
        ];
        let cols = [
            [10u8, 20, 30],
            [200, 100, 50],
            [0, 255, 128],
            [1, 2, 3],
            [250, 250, 250],
            [40, 60, 80],
        ];
        let cloud = rgb_cloud(&pts, &cols);

        let params = EncodeParams {
            octree_depth: 8,
            attributes: vec![AttributeInfo {
                kind: PointAttributeKind::ColorRgb,
                bit_depth: 8,
            }],
            attribute_coding: AttributeCoding::Raht,
            lossless: true,
            intra_leaf_bits: 0,
        };
        let bytes = encode_volumetric(&cloud, &params);

        let mut dec = VolumetricDecoderImpl::new();
        let decoded = dec
            .decode(&packet_with(bytes))
            .expect("decode")
            .expect("some");

        assert_eq!(decoded.num_points, 6);
        let mut a: Vec<[u8; 3]> = Vec::new();
        let mut b: Vec<[u8; 3]> = Vec::new();
        for (p, col) in cols.iter().enumerate().take(6) {
            a.push([col[0], col[1], col[2]]);
            b.push([
                decoded.attributes[0].data[p * 3],
                decoded.attributes[0].data[p * 3 + 1],
                decoded.attributes[0].data[p * 3 + 2],
            ]);
        }
        a.sort();
        b.sort();
        assert_eq!(a, b);
    }
}
