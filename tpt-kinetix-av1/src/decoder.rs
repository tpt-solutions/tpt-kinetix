//! AV1 decoder state machine.
//!
//! Parses OBU sequences, extracts the sequence header and frame header, and
//! stores tile group data for future reconstruction. Currently emits
//! placeholder grey frames — full tile/frame reconstruction is future work.

use tpt_kinetix_core::{
    capabilities::DecoderCapabilities, error::KinetixError, frame::VideoFrame, packet::Packet,
    pixel_format::PixelFormat, timestamp::Timestamp,
};

use crate::{
    frame::FrameHeader,
    obu::{parse_obu_sequence, ObuType, SequenceHeaderObu},
};

/// Parsed tile group data: tile index and raw payload bytes.
#[derive(Debug, Clone)]
pub struct TileData {
    pub tile_index: usize,
    pub payload: Vec<u8>,
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
}

impl Av1Decoder {
    pub fn new() -> Self {
        Self {
            sequence_header: None,
            frame_count: 0,
            strict: false,
            last_frame_header: None,
            tile_data: Vec::new(),
        }
    }

    /// Reports what this decoder can and cannot do.
    ///
    /// The AV1 decoder is **not yet pixel-exact**: it parses OBUs, sequence
    /// headers, and frame headers, and stores tile group data, but emits
    /// placeholder grey frames rather than real reconstructed pixels.
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
            supports_cabac: false,
            supports_cavlc: false,
            supports_intra_prediction: false,
            supports_inter_prediction: false,
            supports_deblocking: false,
            notes: "OBU + sequence header + frame header parsing; tile group \
                    payloads stored but not reconstructed; emits placeholder \
                    grey frames",
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
    /// Parses OBUs from `packet.data`. Handles SequenceHeader OBUs to learn
    /// the stream dimensions, parses FrameHeader OBUs for frame-level metadata,
    /// and stores TileGroup OBU payloads for future tile reconstruction.
    ///
    /// Currently returns a placeholder grey frame — full AV1 frame
    /// reconstruction is future work.
    pub fn decode(&mut self, packet: &Packet) -> Result<Option<VideoFrame>, KinetixError> {
        let obus = parse_obu_sequence(&packet.data);

        let mut produced_frame = false;
        self.tile_data.clear();
        self.last_frame_header = None;
        let mut tile_index = 0usize;

        for obu in &obus {
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
                    // Parse the frame header from the OBU payload.
                    if let Some(ref seq) = self.sequence_header {
                        match FrameHeader::parse(&obu.payload, seq) {
                            Ok(fh) => {
                                self.last_frame_header = Some(fh);
                            }
                            Err(KinetixError::Unsupported(ref msg))
                                if msg.contains("show_existing_frame") =>
                            {
                                // show_existing_frame not yet supported; skip
                                // this frame and try to use the stored reference.
                            }
                            Err(_) => {
                                // Frame header parse failed; still produce a
                                // placeholder frame so the pipeline doesn't stall.
                            }
                        }
                    }
                    produced_frame = true;
                }
                ObuType::TileGroup => {
                    // Store tile group payloads for future reconstruction.
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

        if self.strict {
            return Err(KinetixError::NotPixelExact(
                "AV1: tile/frame reconstruction not implemented; only OBU/sequence \
                 header/frame header parsing (see Av1Decoder::capabilities)"
                    .to_string(),
            ));
        }

        // TODO(phase-4): decode tiles in parallel
        // tiles.par_iter_mut().for_each(|tile| decode_tile(tile));

        let (width, height) = self
            .sequence_header
            .as_ref()
            .map(|sh| (sh.frame_width(), sh.frame_height()))
            .unwrap_or((0, 0));

        if width == 0 || height == 0 {
            return Ok(None);
        }

        // Placeholder: return a grey yuv420p frame of the correct dimensions.
        let y_size = (width as usize) * (height as usize);
        let uv_size = y_size / 4;
        let mut data = vec![128u8; y_size + uv_size + uv_size];
        for p in data.iter_mut() {
            *p = 128;
        }

        let frame_no = self.frame_count;
        self.frame_count += 1;

        let pts = Timestamp::new(frame_no as i64, (1, 90_000));
        let frame = VideoFrame {
            pts,
            dts: pts,
            data,
            width,
            height,
            pixel_format: PixelFormat::Yuv420p,
            is_key_frame: packet.is_key_frame,
        };

        Ok(Some(frame))
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
}

impl Default for Av1Decoder {
    fn default() -> Self {
        Self::new()
    }
}
