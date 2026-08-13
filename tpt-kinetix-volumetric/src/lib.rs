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
//! This crate is a **scaffold**. Sequence / frame header parsing is implemented
//! (see [`header`]), but the geometry (octree) and attribute (lift/RAHT) decode
//! paths are not yet wired. The decoder therefore reports `pixel_exact: false`
//! and returns [`KinetixError::NotPixelExact`] in strict mode when it reaches a
//! coded payload. Malformed streams and any stream declaring the reserved
//! `dynamic` flag fail with [`KinetixError::Parse`] / [`KinetixError::Unsupported`]
//! respectively. See [`VolumetricDecoder::capabilities`] for what is and isn't
//! supported.
//!
//! # References
//!
//! - Design doc: `docs/volumetric-codec-design.md`

pub mod header;

use tpt_kinetix_core::{
    capabilities::DecoderCapabilities, error::KinetixError, frame::PointCloud, packet::Packet,
};

/// Decode interface for the volumetric (point-cloud) codec.
///
/// `decode()` returns a [`PointCloud`] — the primary decoded output, parallel
/// to a [`tpt_kinetix_core::frame::VideoFrame`] for the 2D codecs. Dynamic /
/// inter-frame streams are not supported in v1 (the `dynamic` sequence-header
/// flag is reserved for v2).
pub trait VolumetricDecoder {
    /// Decode a compressed packet into a [`PointCloud`].
    ///
    /// In non-strict mode, before the reconstruction pipeline is implemented,
    /// this returns `Ok(None)`. In strict mode it returns
    /// [`KinetixError::NotPixelExact`] instead of placeholder output.
    fn decode(&mut self, packet: &Packet) -> Result<Option<PointCloud>, KinetixError>;
}

/// Stateful volumetric-format decoder.
///
/// Feed compressed [`Packet`]s via [`VolumetricDecoder::decode`] and receive
/// decoded [`PointCloud`]s.
///
/// # Honesty contract
///
/// This decoder is **not yet implemented**. In non-strict mode it returns
/// `Ok(None)` for every packet. In strict mode it returns
/// [`KinetixError::NotPixelExact`]. Callers should check
/// [`DecoderCapabilities::pixel_exact`] before trusting output.
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
    /// [`KinetixError::NotPixelExact`] instead of placeholder/empty output.
    pub fn with_strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }

    /// Report what this decoder can and cannot do.
    ///
    /// The volumetric decoder is **not yet implemented** (geometry octree and
    /// attribute lift/RAHT decode are scaffolded but not wired). Callers should
    /// check [`DecoderCapabilities::pixel_exact`] before trusting output.
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
            notes: "sequence/frame header parsing implemented; \
                    point-cloud geometry (octree) and attribute (lift/RAHT) \
                    decode not yet wired",
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
        // Parse the headers up front so malformed or reserved streams fail
        // loudly instead of being silently treated as empty output. A valid
        // static stream reaches the (not-yet-implemented) geometry payload.
        let (rest, _seq) = header::parse_sequence_header(&packet.data)?;
        let (_rest, _frame) = header::parse_frame_header(rest)?;

        if self.strict {
            return Err(KinetixError::NotPixelExact(
                "volumetric: point-cloud geometry/attribute decode not implemented yet; \
                 see capabilities()"
                    .to_string(),
            ));
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpt_kinetix_core::error::KinetixError;
    use tpt_kinetix_core::timestamp::Timestamp;

    /// A minimal valid v1 static stream (sequence + frame header, empty payload).
    fn valid_static_stream() -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(header::MAGIC);
        b.push(header::VERSION);
        b.extend_from_slice(&1u32.to_be_bytes()); // max_points
        b.push(10); // octree_depth
        b.push(0); // attr_count
        b.push(0); // attribute_coding = lift
        b.push(1); // lossless
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
    fn non_strict_mode_returns_none_until_implemented() {
        let mut dec = VolumetricDecoderImpl::new();
        assert!(matches!(
            dec.decode(&packet_with(valid_static_stream())),
            Ok(None)
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
}
