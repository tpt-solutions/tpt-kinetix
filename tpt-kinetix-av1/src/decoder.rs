//! AV1 decoder state machine.
//!
//! Parses OBU sequences, extracts the sequence header and frame header, and
//! performs frame reconstruction. The [`Av1Decoder`] now delegates real
//! reconstruction to [`crate::reconstruct`] for intra-coded keyframes;
//! inter frames and unsupported features still return an error in strict mode.
//!
//! **Decoder capabilities**: `pixel_exact` is `false` until the full
//! reconstruction path is validated against `dav1d` reference output.

use tpt_kinetix_core::{
    capabilities::DecoderCapabilities, error::KinetixError, frame::VideoFrame, packet::Packet,
    pixel_format::PixelFormat, timestamp::Timestamp,
};

use crate::{
    frame::FrameHeader,
    obu::{parse_obu_sequence, ObuType, SequenceHeaderObu},
    reconstruct::reconstruct_av1_frame,
};

/// Parsed tile group data: tile index and raw payload bytes.
#[derive(Debug, Clone)]
pub struct TileData {
    pub tile_index: usize,
    pub payload: Vec<u8>,
}

/// A decoded reference frame retained in the decoder's picture buffer.
///
/// AV1 keeps up to eight reference pictures (the `LAST`/`GOLDEN`/`ALTREF`
/// family); this stores one slot's worth of planar YUV (4:2:0) samples.
#[derive(Clone)]
pub struct StoredFrame {
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    pub width: usize,
    pub height: usize,
}

/// Reference frame buffer (AV1 §7.20): eight slots indexed by
/// `refresh_frame_flags`.
///
/// Phase E scaffolding: this is the storage inter prediction will draw
/// reconstructed reference pictures from once motion compensation lands. It is
/// populated after every successfully reconstructed frame so the buffer tracks
/// `refresh_frame_flags` exactly as the spec mandates, even before inter blocks
/// are decoded.
#[derive(Default)]
pub struct RefFrameStore {
    slots: [Option<StoredFrame>; 8],
}

impl RefFrameStore {
    pub fn new() -> Self {
        Self {
            slots: Default::default(),
        }
    }

    /// Store `frame` into every slot whose bit is set in `refresh_flags`
    /// (AV1 §7.20 / `refresh_frame_flags` semantics).
    pub fn refresh(&mut self, refresh_flags: u8, frame: &VideoFrame) {
        let (y, u, v) = split_planes(frame);
        for i in 0..8 {
            if refresh_flags & (1u8 << i) != 0 {
                self.slots[i] = Some(StoredFrame {
                    y: y.clone(),
                    u: u.clone(),
                    v: v.clone(),
                    width: frame.width as usize,
                    height: frame.height as usize,
                });
            }
        }
    }

    /// Read the reference frame stored in slot `slot` (0..8).
    pub fn get(&self, slot: usize) -> Option<&StoredFrame> {
        self.slots.get(slot).and_then(|s| s.as_ref())
    }

    /// Number of currently populated reference slots.
    pub fn populated(&self) -> usize {
        self.slots.iter().filter(|s| s.is_some()).count()
    }
}

/// Split a packed 4:2:0 [`VideoFrame`] into its three component planes.
fn split_planes(f: &VideoFrame) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let w = f.width as usize;
    let h = f.height as usize;
    let ysz = w * h;
    let uvsz = (w / 2) * (h / 2);
    let y = f.data[..ysz].to_vec();
    let u = f.data[ysz..ysz + uvsz].to_vec();
    let v = f.data[ysz + uvsz..ysz + 2 * uvsz].to_vec();
    (y, u, v)
}

/// Stateful AV1 decoder.
pub struct Av1Decoder {
    sequence_header: Option<SequenceHeaderObu>,
    frame_count: u64,
    /// When `true`, [`Av1Decoder::decode`] returns
    /// [`KinetixError::NotPixelExact`] instead of emitting placeholder grey
    /// frames. Off by default so existing pipelines keep working.
    strict: bool,
    /// Most recently parsed frame header.
    last_frame_header: Option<FrameHeader>,
    /// Tile group payloads from the most recent packet.
    tile_data: Vec<TileData>,
    /// Reference frame buffer (AV1 §7.20), populated after each reconstructed
    /// frame (Phase E).
    ref_frames: RefFrameStore,
}

impl Av1Decoder {
    pub fn new() -> Self {
        Self {
            sequence_header: None,
            frame_count: 0,
            strict: false,
            last_frame_header: None,
            tile_data: Vec::new(),
            ref_frames: RefFrameStore::new(),
        }
    }

    /// Reports what this decoder can and cannot do.
    ///
    /// The AV1 decoder **attempts** pixel-exact decode for intra-coded
    /// keyframes using the reconstruction pipeline in [`crate::reconstruct`],
    /// but is **not yet validated** against `dav1d` reference output, so
    /// `pixel_exact` remains `false` until the conformance harness passes.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use tpt_kinetix_av1::Av1Decoder;
    ///
    /// let caps = Av1Decoder::new().capabilities();
    /// assert!(!caps.pixel_exact);
    /// ```
    pub fn capabilities(&self) -> DecoderCapabilities {
        DecoderCapabilities {
            codec: "AV1",
            pixel_exact: false,
            supports_cabac: true,
            supports_cavlc: true,
            supports_intra_prediction: true,
            supports_inter_prediction: false,
            // Loop filter + CDEF run after reconstruction (AV1 Phase D), but the
            // decoder is not yet validated pixel-exact, so the capability stays
            // conservative until the conformance harness passes.
            supports_deblocking: true,
            notes: "OBU + sequence header + frame header + per-tile reconstruction \
                    (real superblock partition tree, intra mode / tx_size, coeffs() \
                    symbol decoder) for intra keyframes; in-loop deblock + CDEF wired \
                    (Phase D); rayon parallel tile decode (Phase F); reference frame \
                    buffer (Phase E) populated but inter prediction / motion \
                    compensation not yet implemented, so output is not pixel-exact",
        }
    }

    /// Enable strict mode.
    ///
    /// In strict mode, [`Av1Decoder::decode`] returns
    /// [`KinetixError::NotPixelExact`] whenever it would otherwise emit a
    /// placeholder frame.
    pub fn set_strict(&mut self, strict: bool) {
        self.strict = strict;
    }

    /// Builder-style variant of [`Av1Decoder::set_strict`].
    pub fn with_strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }

    /// Decode a compressed AV1 [`Packet`] into a [`VideoFrame`].
    ///
    /// Parses OBUs from `packet.data`. For intra-coded keyframes, performs
    /// full tile-group reconstruction (inverse transform + intra prediction)
    /// via [`crate::reconstruct::reconstruct_av1_frame`]. Falls back to
    /// strict-mode `NotPixelExact` for unsupported frame types.
    pub fn decode(&mut self, packet: &Packet) -> Result<Option<VideoFrame>, KinetixError> {
        let obus = parse_obu_sequence(&packet.data);

        let mut produced_frame = false;
        self.tile_data.clear();
        self.last_frame_header = None;
        let mut tile_index = 0usize;
        let mut obu_pairs: Vec<(u8, Vec<u8>)> = Vec::new();

        for obu in &obus {
            obu_pairs.push((obu.obu_type as u8, obu.payload.clone()));
            match obu.obu_type {
                ObuType::SequenceHeader => match SequenceHeaderObu::parse(&obu.payload) {
                    Ok(sh) => {
                        self.sequence_header = Some(sh);
                    }
                    Err(e) => {
                        return Err(KinetixError::Parse(format!(
                            "SequenceHeaderObu parse error: {e}"
                        )));
                    }
                },
                ObuType::Frame | ObuType::FrameHeader => {
                    if let Some(ref seq) = self.sequence_header {
                        match FrameHeader::parse(&obu.payload, seq) {
                            Ok((fh, header_bits)) => {
                                self.last_frame_header = Some(fh);
                                // A combined `Frame` OBU (type 6) carries the
                                // uncompressed header *followed by* the tile
                                // group data. Slice that remainder off and feed
                                // it to reconstruction as if it were a standalone
                                // TileGroup OBU (type 13) — this is what
                                // `reconstruct_av1_frame` collects. For a single
                                // tile the tile group begins immediately after
                                // the (byte-aligned) uncompressed header.
                                let consumed = (header_bits + 7) / 8;
                                if consumed < obu.payload.len() {
                                    let tg = obu.payload[consumed..].to_vec();
                                    obu_pairs.push((13, tg.clone()));
                                    self.tile_data.push(TileData {
                                        tile_index: tile_index,
                                        payload: tg,
                                    });
                                    tile_index += 1;
                                }
                            }
                            Err(KinetixError::Unsupported(ref msg))
                                if msg.contains("show_existing_frame") => {}
                            Err(_) => {}
                        }
                    }
                    produced_frame = true;
                }
                ObuType::TileGroup => {
                    self.tile_data.push(TileData {
                        tile_index,
                        payload: obu.payload.clone(),
                    });
                    tile_index += 1;
                    produced_frame = true;
                }
                _ => {}
            }
        }

        if !produced_frame {
            return Ok(None);
        }

        // Attempt reconstruction via the new pipeline.
        if let (Some(seq), Some(fh)) = (&self.sequence_header, &self.last_frame_header) {
            match reconstruct_av1_frame(&obu_pairs, seq, fh) {
                Ok(Some(frame)) => {
                    // Phase E: track the reconstructed frame in the reference
                    // buffer per `refresh_frame_flags` so inter prediction can
                    // later draw from it.
                    let refresh = self
                        .last_frame_header
                        .as_ref()
                        .map(|fh| fh.refresh_frame_flags)
                        .unwrap_or(0);
                    self.ref_frames.refresh(refresh, &frame);
                    self.frame_count += 1;
                    return Ok(Some(frame));
                }
                Ok(None) => {}
                Err(e) => {
                    if self.strict {
                        return Err(KinetixError::NotPixelExact(format!(
                            "AV1 reconstruction failed: {e}"
                        )));
                    }
                }
            }
        }

        if self.strict {
            return Err(KinetixError::NotPixelExact(
                "AV1: tile/frame reconstruction not yet complete (see Av1Decoder::capabilities)"
                    .to_string(),
            ));
        }

        // Fallback: grey placeholder frame
        let (width, height) = self
            .sequence_header
            .as_ref()
            .map(|sh| (sh.frame_width(), sh.frame_height()))
            .unwrap_or((0, 0));

        if width == 0 || height == 0 {
            return Ok(None);
        }

        let y_size = (width as usize) * (height as usize);
        let uv_size = y_size / 4;
        let data = vec![128u8; y_size + uv_size + uv_size];

        let frame_no = self.frame_count;
        self.frame_count += 1;

        let pts = Timestamp::new(frame_no as i64, (1, 90_000));
        Ok(Some(VideoFrame {
            pts,
            dts: pts,
            data,
            width,
            height,
            pixel_format: PixelFormat::Yuv420p,
            is_key_frame: packet.is_key_frame,
        }))
    }

    /// Flush any buffered frames.
    pub fn flush(&mut self) -> Result<Vec<VideoFrame>, KinetixError> {
        Ok(Vec::new())
    }

    /// Returns the parsed sequence header if one has been seen.
    pub fn sequence_header(&self) -> Option<&SequenceHeaderObu> {
        self.sequence_header.as_ref()
    }

    /// Returns the most recently parsed frame header, if any.
    pub fn last_frame_header(&self) -> Option<&FrameHeader> {
        self.last_frame_header.as_ref()
    }

    /// Returns the tile group payloads from the most recent packet.
    pub fn tile_data(&self) -> &[TileData] {
        &self.tile_data
    }

    /// Returns the decoder's reference frame buffer (AV1 §7.20), populated
    /// after each reconstructed frame. Phase E scaffolding for inter prediction.
    pub fn ref_frames(&self) -> &RefFrameStore {
        &self.ref_frames
    }
}

impl Default for Av1Decoder {
    fn default() -> Self {
        Self::new()
    }
}
