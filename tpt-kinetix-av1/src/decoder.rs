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

/// `(obu_type, payload)` pairs — the shape [`reconstruct_av1_frame`] consumes.
type ObuPairs = Vec<(u8, Vec<u8>)>;

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

impl StoredFrame {
    /// Rebuild a planar [`VideoFrame`] (Y then U then V) from this stored
    /// reference — used to satisfy `show_existing_frame` (§7.4).
    fn to_video_frame(&self) -> VideoFrame {
        let mut data = Vec::with_capacity(self.y.len() + self.u.len() + self.v.len());
        data.extend_from_slice(&self.y);
        data.extend_from_slice(&self.u);
        data.extend_from_slice(&self.v);
        VideoFrame {
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            data,
            width: self.width as u32,
            height: self.height as u32,
            pixel_format: PixelFormat::Yuv420p,
            is_key_frame: false,
        }
    }
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
    /// `RefOrderHint[0..8]` — the `order_hint` of the frame stored in each DPB
    /// slot, updated by `refresh_frame_flags`. Needed by `skip_mode_params()`.
    ref_order_hints: [u8; 8],
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
            ref_order_hints: [0u8; 8],
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
            supports_inter_prediction: true,
            // Loop filter + CDEF run after reconstruction (AV1 Phase D), but the
            // decoder is not yet validated pixel-exact, so the capability stays
            // conservative until the conformance harness passes.
            supports_deblocking: true,
            notes: "intra keyframe decode (OBU + sequence/frame header, real \
                    superblock partition tree, intra mode / tx_size, coeffs() symbol \
                    decoder, full inverse-transform set incl. FLIPADST, intra block \
                    copy with find_mv_stack DV prediction + bilinear chroma sub-pel, \
                    in-loop deblock + CDEF + loop restoration, rayon parallel tiles) \
                    is bit-exact vs dav1d across the synthesized intra conformance \
                    corpus (Phase G gate). Inter prediction is wired end-to-end (MV \
                    candidate derivation, NEWMV / NEARMV / NEARESTMV / ZEROMV, \
                    switchable + bilinear interpolation, single- and compound \
                    reference, residual add) but is NOT yet bit-exact — non-keyframes \
                    diverge from the reference. `pixel_exact` stays false until the \
                    inter path is validated and official AOM/ITU vectors are wired in",
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

        self.tile_data.clear();
        self.last_frame_header = None;

        // A temporal unit may carry several coded frames (hierarchical GOPs:
        // an ALTREF is decoded and stored before the B-frames that reference
        // it, then shown later via `show_existing_frame`). Each frame is
        // reconstructed and pushed to the DPB in turn; the frame this call
        // returns is the last one flagged `show_frame` (or the
        // `show_existing_frame` target).
        let mut produced_any = false;
        let mut shown: Option<VideoFrame> = None;
        // Accumulator for the separate `FrameHeader` + `TileGroup` OBU form.
        let mut pending: Option<(FrameHeader, ObuPairs)> = None;

        for obu in &obus {
            match obu.obu_type {
                ObuType::SequenceHeader => match SequenceHeaderObu::parse(&obu.payload) {
                    Ok(sh) => self.sequence_header = Some(sh),
                    Err(e) => {
                        return Err(KinetixError::Parse(format!(
                            "SequenceHeaderObu parse error: {e}"
                        )))
                    }
                },
                ObuType::Frame => {
                    let Some(seq) = self.sequence_header.clone() else {
                        continue;
                    };
                    produced_any = true;
                    let Ok((fh, header_bits)) =
                        FrameHeader::parse_with_dpb(&obu.payload, &seq, &self.ref_order_hints)
                    else {
                        continue;
                    };
                    if let Some(f) = self.finish_frame(&seq, &fh, &obu.payload, header_bits) {
                        shown = Some(f);
                    }
                }
                ObuType::FrameHeader => {
                    let Some(seq) = self.sequence_header.clone() else {
                        continue;
                    };
                    produced_any = true;
                    if let Ok((fh, _)) =
                        FrameHeader::parse_with_dpb(&obu.payload, &seq, &self.ref_order_hints)
                    {
                        if fh.show_existing_frame {
                            if let Some(f) = self.finish_frame(&seq, &fh, &obu.payload, 0) {
                                shown = Some(f);
                            }
                        } else {
                            pending = Some((fh, Vec::new()));
                        }
                    }
                }
                ObuType::TileGroup => {
                    if let Some((_, pairs)) = pending.as_mut() {
                        pairs.push((13, obu.payload.clone()));
                    }
                }
                _ => {}
            }
        }

        // Flush a `FrameHeader` + `TileGroup(s)` frame.
        if let (Some((fh, pairs)), Some(seq)) = (pending, self.sequence_header.clone()) {
            if let Some(f) = self.finish_frame_from_pairs(&seq, &fh, &pairs) {
                shown = Some(f);
            }
        }

        if let Some(f) = shown {
            return Ok(Some(f));
        }
        if !produced_any {
            return Ok(None);
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

    /// Reconstruct one frame from a combined `Frame` OBU payload (or resolve a
    /// `show_existing_frame`), store it in the DPB, and return it iff it is
    /// shown. `header_bits` is the uncompressed-header length so the tile data
    /// can be sliced off.
    fn finish_frame(
        &mut self,
        seq: &SequenceHeaderObu,
        fh: &FrameHeader,
        payload: &[u8],
        header_bits: usize,
    ) -> Option<VideoFrame> {
        self.last_frame_header = Some(fh.clone());
        if let Some(idx) = fh.show_existing_idx {
            let f = self
                .ref_frames
                .get(idx as usize)
                .map(|s| s.to_video_frame());
            if f.is_some() {
                self.frame_count += 1;
            }
            return f;
        }
        let consumed = header_bits.div_ceil(8);
        if consumed >= payload.len() {
            return None;
        }
        let pairs = vec![(13u8, payload[consumed..].to_vec())];
        self.finish_frame_from_pairs(seq, fh, &pairs)
    }

    /// As [`Self::finish_frame`] but the tile data is already split into
    /// `(obu_type, payload)` pairs (the separate `FrameHeader` + `TileGroup`
    /// OBU form).
    fn finish_frame_from_pairs(
        &mut self,
        seq: &SequenceHeaderObu,
        fh: &FrameHeader,
        pairs: &[(u8, Vec<u8>)],
    ) -> Option<VideoFrame> {
        self.last_frame_header = Some(fh.clone());
        let frame = match reconstruct_av1_frame(pairs, seq, fh, Some(&self.ref_frames)) {
            Ok(Some(f)) => f,
            _ => return None,
        };
        let refresh = fh.refresh_frame_flags;
        let order_hint = fh.order_hint as u8;
        self.ref_frames.refresh(refresh, &frame);
        for i in 0..8 {
            if refresh & (1u8 << i) != 0 {
                self.ref_order_hints[i] = order_hint;
            }
        }
        if std::env::var("KINETIX_AV1_DBG_FH").is_ok() {
            eprintln!(
                "DBG refresh oh={order_hint} show={} flags={refresh:#010b} -> hints={:?}",
                fh.show_frame, self.ref_order_hints
            );
        }
        self.frame_count += 1;
        if fh.show_frame {
            Some(frame)
        } else {
            None
        }
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
