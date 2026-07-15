//! AV1 decoder state machine.

use kinetix_core::{
    error::KinetixError, frame::VideoFrame, packet::Packet, pixel_format::PixelFormat,
    timestamp::Timestamp,
};

use crate::obu::{parse_obu_sequence, ObuType, SequenceHeaderObu};

/// Stateful AV1 decoder.
pub struct Av1Decoder {
    sequence_header: Option<SequenceHeaderObu>,
    frame_count: u64,
}

impl Av1Decoder {
    pub fn new() -> Self {
        Self {
            sequence_header: None,
            frame_count: 0,
        }
    }

    /// Decode a compressed AV1 [`Packet`] into a [`VideoFrame`].
    ///
    /// Parses OBUs from `packet.data`.  Handles SequenceHeader OBUs to learn
    /// the stream dimensions.  For Frame / FrameHeader OBUs a placeholder grey
    /// frame is returned (full AV1 frame reconstruction is out of Phase 4 scope).
    pub fn decode(&mut self, packet: &Packet) -> Result<Option<VideoFrame>, KinetixError> {
        let obus = parse_obu_sequence(&packet.data);

        let mut produced_frame = false;

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
                    produced_frame = true;
                }
                _ => {}
            }
        }

        if !produced_frame {
            return Ok(None);
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
        // Y = 128 (grey), Cb = 128, Cr = 128 — mid-grey in limited-range YCbCr
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
        // No buffered frames in the placeholder decoder.
        Ok(Vec::new())
    }

    /// Returns the parsed sequence header if one has been seen.
    pub fn sequence_header(&self) -> Option<&SequenceHeaderObu> {
        self.sequence_header.as_ref()
    }
}

impl Default for Av1Decoder {
    fn default() -> Self {
        Self::new()
    }
}
