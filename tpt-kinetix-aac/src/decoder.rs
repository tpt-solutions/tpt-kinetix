//! Native AAC-LC decoder (no third-party codec dependency).
//!
//! Decodes ADTS-framed AAC-LC into interleaved `f32` PCM. The pipeline per AAC
//! frame is: parse the raw data block → per-channel Huffman spectral decode +
//! inverse quantize → pulse / PNS → joint stereo (M/S, intensity) → TNS → IMDCT
//! with sine/KBD windows and 50% overlap-add.

use tpt_kinetix_core::frame::{AudioFrame, SampleFormat};
use tpt_kinetix_core::packet::Packet;

use crate::adts::AdtsHeader;
use crate::dequant::group_base_offsets;
use crate::mdct::Imdct;
use crate::pns::apply_pns;
use crate::pulse::apply_pulse;
use crate::stereo::apply_stereo;
use crate::syntax::{
    AacParseError, ChannelStream, CouplingChannelElement, Element, IcsInfo, RawDataBlock,
    WindowSequence,
};
use crate::tables::{SWB_OFFSET_1024, SWB_OFFSET_128};
use crate::tns::apply_tns;
use crate::window::build_window;

/// A decoded channel ready for CCE coupling and synthesis.
#[derive(Debug, Clone)]
struct DecodedChannel {
    instance_tag: u8,
    ics: IcsInfo,
    coeffs: [f32; 1024],
    band_type: Vec<u8>,
    scalefactor: Vec<i32>,
    global_gain: u8,
    pulse: Option<crate::pulse::PulseData>,
    tns: Option<crate::tns::TnsData>,
    is_cce: bool, // true if this is a coupling channel (not output directly)
    cpe_pair: Option<(usize, usize)>, // (left_idx, right_idx) if part of CPE
}

/// A decoded CPE pair ready for stereo processing and synthesis.
#[derive(Debug, Clone)]
struct DecodedCpe {
    instance_tag: u8,
    left: DecodedChannel,
    right: DecodedChannel,
    ms_mask_present: u8,
    ms_mask: Vec<bool>,
}

/// Errors raised by [`AacDecoder`].
#[derive(Debug, thiserror::Error)]
pub enum AacError {
    /// The ADTS header could not be parsed.
    #[error("ADTS header error: {0}")]
    Adts(#[from] crate::adts::AdtsError),
    /// The raw data block could not be parsed.
    #[error("AAC syntax error: {0}")]
    Parse(#[from] AacParseError),
}

/// Precomputed synthesis windows for both shapes and both lengths.
struct Windows {
    long: [Vec<f32>; 2],
    short: [Vec<f32>; 2],
}

impl Windows {
    fn new() -> Self {
        Windows {
            long: [
                build_window(1024, false, false),
                build_window(1024, true, false),
            ],
            short: [
                build_window(128, false, true),
                build_window(128, true, true),
            ],
        }
    }
}

/// Per-output-channel overlap-add and window-shape state.
#[derive(Clone)]
struct ChannelState {
    overlap: [f32; 1024],
    prev_shape: u8,
    init: bool,
}

impl Default for ChannelState {
    fn default() -> Self {
        ChannelState {
            overlap: [0.0f32; 1024],
            prev_shape: 0,
            init: false,
        }
    }
}

/// A native AAC-LC decoder.
pub struct AacDecoder {
    imdct_long: Imdct,
    imdct_short: Imdct,
    windows: Windows,
    channels: Vec<ChannelState>,
    sample_rate: u32,
    sf_index: usize,
}

impl Default for AacDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl AacDecoder {
    /// Create a new decoder. Configuration is learned from the first ADTS header.
    pub fn new() -> Self {
        AacDecoder {
            imdct_long: Imdct::new(1024),
            imdct_short: Imdct::new(128),
            windows: Windows::new(),
            channels: Vec::new(),
            sample_rate: 0,
            sf_index: 4,
        }
    }

    /// Report what this decoder can do today.
    pub fn capabilities(&self) -> tpt_kinetix_core::capabilities::DecoderCapabilities {
        tpt_kinetix_core::capabilities::DecoderCapabilities {
            codec: "AAC-LC",
            pixel_exact: false, // not yet sample-exact vs reference
            supports_cabac: false,
            supports_cavlc: false,
            supports_intra_prediction: false,
            supports_inter_prediction: false,
            supports_deblocking: false,
            notes: "native AAC-LC decoder (ADTS framing, Huffman, IMDCT, TNS, PNS, stereo); conformance validation pending",
        }
    }

    /// Decode one ADTS frame, returning one 1024-sample-per-channel PCM frame.
    pub fn decode(&mut self, packet: &Packet) -> Result<Option<AudioFrame>, AacError> {
        let hdr = AdtsHeader::parse(&packet.data)?;

        // Initialize decoder state from first header.
        if self.sample_rate == 0 {
            self.sample_rate = hdr.sample_rate;
            self.sf_index = hdr.sampling_frequency_index as usize;
            self.channels = vec![ChannelState::default(); hdr.channels as usize];
        }

        // Parse the raw data block.
        let payload = &packet.data[hdr.header_len..hdr.frame_length];
        let block = RawDataBlock::parse(payload, hdr.sampling_frequency_index as usize)?;

        // Two-pass decode:
        // Pass 1: Decode all channel streams to frequency-domain coefficients.
        // Pass 2: Apply CCE coupling (coupling channels -> target channels).
        // Pass 3: Apply stereo (M/S, intensity) for CPEs.
        // Pass 4: IMDCT + windowing + overlap-add.

        // --- Pass 1: Collect all decoded channels ---
        let mut decoded_channels: Vec<DecodedChannel> = Vec::new();
        let mut decoded_cpes: Vec<DecodedCpe> = Vec::new();
        let mut cce_elements: Vec<CouplingChannelElement> = Vec::new();

        for el in &block.elements {
            match el {
                Element::Sce(sce) => {
                    let ch = Self::decode_channel_stream(&sce.stream, self.sf_index)?;
                    decoded_channels.push(DecodedChannel {
                        instance_tag: sce.instance_tag,
                        ics: sce.stream.ics,
                        coeffs: ch,
                        band_type: sce.stream.band_type.clone(),
                        scalefactor: sce.stream.scalefactor.clone(),
                        global_gain: sce.stream.global_gain,
                        pulse: sce.stream.pulse.clone(),
                        tns: sce.stream.tns.clone(),
                        is_cce: false,
                        cpe_pair: None,
                    });
                }
                Element::Cpe(cpe) => {
                    let left_ch = Self::decode_channel_stream(&cpe.left, self.sf_index)?;
                    let right_ch = Self::decode_channel_stream(&cpe.right, self.sf_index)?;
                    let left_idx = decoded_channels.len();
                    let right_idx = left_idx + 1;
                    decoded_channels.push(DecodedChannel {
                        instance_tag: cpe.instance_tag,
                        ics: cpe.left.ics,
                        coeffs: left_ch,
                        band_type: cpe.left.band_type.clone(),
                        scalefactor: cpe.left.scalefactor.clone(),
                        global_gain: cpe.left.global_gain,
                        pulse: cpe.left.pulse.clone(),
                        tns: cpe.left.tns.clone(),
                        is_cce: false,
                        cpe_pair: Some((left_idx, right_idx)),
                    });
                    decoded_channels.push(DecodedChannel {
                        instance_tag: cpe.instance_tag,
                        ics: cpe.right.ics,
                        coeffs: right_ch,
                        band_type: cpe.right.band_type.clone(),
                        scalefactor: cpe.right.scalefactor.clone(),
                        global_gain: cpe.right.global_gain,
                        pulse: cpe.right.pulse.clone(),
                        tns: cpe.right.tns.clone(),
                        is_cce: false,
                        cpe_pair: Some((left_idx, right_idx)),
                    });
                    decoded_cpes.push(DecodedCpe {
                        instance_tag: cpe.instance_tag,
                        left: decoded_channels[left_idx].clone(),
                        right: decoded_channels[right_idx].clone(),
                        ms_mask_present: cpe.ms_mask_present,
                        ms_mask: cpe.ms_mask.clone(),
                    });
                }
                Element::Cce(cce) => {
                    cce_elements.push(cce.clone());
                }
                Element::Lfe(lfe) => {
                    let ch = Self::decode_channel_stream(&lfe.stream, self.sf_index)?;
                    decoded_channels.push(DecodedChannel {
                        instance_tag: lfe.instance_tag,
                        ics: lfe.stream.ics,
                        coeffs: ch,
                        band_type: lfe.stream.band_type.clone(),
                        scalefactor: lfe.stream.scalefactor.clone(),
                        global_gain: lfe.stream.global_gain,
                        pulse: lfe.stream.pulse.clone(),
                        tns: lfe.stream.tns.clone(),
                        is_cce: false,
                        cpe_pair: None,
                    });
                }
                Element::Fil(_) | Element::End => {}
            }
        }

        // --- Pass 2: Apply CCE coupling ---
        // Build a map from instance_tag to decoded_channels index.
        let mut tag_to_idx: std::collections::HashMap<u8, usize> = std::collections::HashMap::new();
        for (idx, ch) in decoded_channels.iter().enumerate() {
            if !ch.is_cce {
                tag_to_idx.insert(ch.instance_tag, idx);
            }
        }

        // Pass 2: Apply CCE coupling (placeholder - full implementation TODO).
        // The CCE element contains gain elements that describe how to mix a coupling
        // channel into target channels, but the coupling channel's own spectral data
        // is carried in a separate SCE/CPE that shares the same instance_tag.
        // For now, we just acknowledge the CCE exists; full coupling requires:
        // 1. Finding the coupling channel by instance_tag in decoded_channels
        // 2. Applying gain_element_lists to mix into target channels
        // TODO: Implement full CCE coupling logic.

        // --- Pass 3: Apply stereo (M/S, intensity) for CPEs ---
        for cpe in &decoded_cpes {
            // Find the indices in decoded_channels (they're consecutive).
            // The cpe_pair field points to them.
            // We need to find the actual mutable references.
            // Since we cloned into decoded_cpes, we need to find them again.
            // For simplicity, we'll re-find by instance_tag and position.
            // In practice, CPE channels are stored consecutively with same instance_tag.
            let indices: Vec<usize> = decoded_channels
                .iter()
                .enumerate()
                .filter(|(_, ch)| ch.instance_tag == cpe.instance_tag && !ch.is_cce)
                .map(|(idx, _)| idx)
                .collect();
            if indices.len() == 2 {
                let left_idx = indices[0];
                let right_idx = indices[1];
                let swb = if decoded_channels[left_idx]
                    .ics
                    .window_sequence
                    .is_eight_short()
                {
                    SWB_OFFSET_128[self.sf_index]
                } else {
                    SWB_OFFSET_1024[self.sf_index]
                };

                // Extract the data we need before mutable borrows to avoid borrow checker issues.
                let left_ics = decoded_channels[left_idx].ics;
                let right_ics = decoded_channels[right_idx].ics;
                let left_band_type = decoded_channels[left_idx].band_type.clone();
                let right_band_type = decoded_channels[right_idx].band_type.clone();
                let left_scalefactor = decoded_channels[left_idx].scalefactor.clone();
                let right_scalefactor = decoded_channels[right_idx].scalefactor.clone();

                // Use split_at_mut to get two mutable references to non-overlapping elements.
                let (left, right) = if left_idx < right_idx {
                    let (left_part, right_part) = decoded_channels.split_at_mut(right_idx);
                    (&mut left_part[left_idx], &mut right_part[0])
                } else {
                    let (left_part, right_part) = decoded_channels.split_at_mut(left_idx);
                    (&mut left_part[0], &mut right_part[right_idx])
                };

                apply_stereo(
                    &mut left.coeffs,
                    &mut right.coeffs,
                    &left_ics,
                    &left_band_type,
                    &right_band_type,
                    &left_scalefactor,
                    &right_scalefactor,
                    cpe.ms_mask_present,
                    &cpe.ms_mask,
                    swb,
                );
            }
        }

        // --- Pass 4: IMDCT + windowing + overlap-add ---
        // Collect output channels in order (non-CCE channels only).
        let output_channels: Vec<_> = decoded_channels.iter().filter(|ch| !ch.is_cce).collect();

        // Ensure we have enough channel states.
        while self.channels.len() < output_channels.len() {
            self.channels.push(ChannelState::default());
        }

        let mut pcm_planes: Vec<Vec<f32>> = Vec::with_capacity(output_channels.len());
        for (ch_idx, ch) in output_channels.iter().enumerate() {
            let mut buf = [0.0f32; 2048];
            let mut out_buf = [0.0f32; 1024];
            if ch.ics.window_sequence.is_eight_short() {
                self.imdct_short.transform(&ch.coeffs, &mut buf);
                short_synthesis(
                    &buf,
                    &mut self.channels[ch_idx],
                    ch.ics.window_shape as usize,
                    &self.windows,
                    &mut out_buf,
                );
            } else {
                self.imdct_long.transform(&ch.coeffs, &mut buf);
                long_synthesis(
                    &buf,
                    &mut self.channels[ch_idx],
                    ch.ics.window_sequence,
                    ch.ics.window_shape as usize,
                    &self.windows,
                    &mut out_buf,
                );
            }
            pcm_planes.push(out_buf.to_vec());
        }

        if pcm_planes.is_empty() {
            return Ok(None);
        }

        // Interleave channels.
        let ch_count = pcm_planes.len();
        let mut interleaved = Vec::with_capacity(1024 * ch_count);
        for i in 0..1024 {
            for ch in 0..ch_count {
                interleaved.push(pcm_planes[ch][i]);
            }
        }

        // Convert f32 samples to bytes.
        let mut data = Vec::with_capacity(interleaved.len() * 4);
        for &s in &interleaved {
            data.extend_from_slice(&s.to_le_bytes());
        }

        Ok(Some(AudioFrame {
            pts: packet.pts,
            data,
            sample_rate: self.sample_rate,
            channels: ch_count as u8,
            sample_format: SampleFormat::F32,
        }))
    }

    /// Decode a single ChannelStream to frequency-domain coefficients.
    fn decode_channel_stream(
        stream: &ChannelStream,
        sf_index: usize,
    ) -> Result<[f32; 1024], AacParseError> {
        let ics = &stream.ics;
        let swb = if ics.window_sequence.is_eight_short() {
            SWB_OFFSET_128[sf_index]
        } else {
            SWB_OFFSET_1024[sf_index]
        };
        let mut coeffs = stream.coeffs;
        if let Some(p) = &stream.pulse {
            apply_pulse(p, swb, &mut coeffs);
        }
        let gindex = group_base_offsets(ics);
        apply_pns(
            ics,
            &stream.band_type,
            &stream.scalefactor,
            swb,
            stream.global_gain,
            &gindex,
            &mut coeffs,
        );
        if let Some(tns) = &stream.tns {
            apply_tns(tns, ics, &mut coeffs, swb);
        }
        Ok(coeffs)
    }
}

/// Return the decoded channel stream for an SCE or LFE element.
fn elem_stream(el: &Element) -> &ChannelStream {
    match el {
        Element::Sce(s) => &s.stream,
        Element::Lfe(l) => &l.stream,
        _ => unreachable!("elem_stream called on non-single-channel element"),
    }
}

/// Run the filterbank (IMDCT + window + overlap-add) for one channel.
fn synthesize(
    imdct_long: &Imdct,
    imdct_short: &Imdct,
    windows: &Windows,
    state: &mut ChannelState,
    ics: &IcsInfo,
    coeffs: &[f32; 1024],
) -> [f32; 1024] {
    let ws = ics.window_shape as usize;
    if !state.init {
        state.prev_shape = ws as u8;
        state.init = true;
    }

    let mut out = [0.0f32; 1024];
    if ics.window_sequence.is_eight_short() {
        let mut buf = [0.0f32; 2048];
        for w in 0..8 {
            imdct_short.transform(
                &coeffs[w * 128..w * 128 + 128],
                &mut buf[w * 256..w * 256 + 256],
            );
        }
        short_synthesis(&buf, state, ws, windows, &mut out);
    } else {
        let mut buf = [0.0f32; 2048];
        imdct_long.transform(coeffs, &mut buf);
        long_synthesis(&buf, state, ics.window_sequence, ws, windows, &mut out);
    }
    state.prev_shape = ws as u8;
    out
}

fn long_synthesis(
    buf: &[f32; 2048],
    state: &mut ChannelState,
    seq: WindowSequence,
    ws: usize,
    windows: &Windows,
    out: &mut [f32; 1024],
) {
    let nlong = 1024;
    let nshort = 128;
    let nflat_ls = (nlong - nshort) / 2; // 448
    let w_prev_long = &windows.long[state.prev_shape as usize];
    let w_cur_long = &windows.long[ws];
    let w_prev_short = &windows.short[state.prev_shape as usize];
    let w_cur_short = &windows.short[ws];

    match seq {
        WindowSequence::OnlyLong => {
            for i in 0..nlong {
                out[i] = state.overlap[i] + buf[i] * w_prev_long[i];
            }
            for i in 0..nlong {
                state.overlap[i] = buf[nlong + i] * w_cur_long[nlong - 1 - i];
            }
        }
        WindowSequence::LongStart => {
            for i in 0..nlong {
                out[i] = state.overlap[i] + buf[i] * w_prev_long[i];
            }
            for i in 0..nflat_ls {
                state.overlap[i] = buf[nlong + i];
            }
            for i in 0..nshort {
                state.overlap[nflat_ls + i] =
                    buf[nlong + nflat_ls + i] * w_cur_short[nshort - 1 - i];
            }
            for i in 0..nflat_ls {
                state.overlap[nflat_ls + nshort + i] = 0.0;
            }
        }
        WindowSequence::LongStop => {
            for i in 0..nflat_ls {
                out[i] = state.overlap[i];
            }
            for i in 0..nshort {
                out[nflat_ls + i] =
                    state.overlap[nflat_ls + i] + buf[nflat_ls + i] * w_prev_short[i];
            }
            for i in 0..nflat_ls {
                out[nflat_ls + nshort + i] =
                    state.overlap[nflat_ls + nshort + i] + buf[nflat_ls + nshort + i];
            }
            for i in 0..nlong {
                state.overlap[i] = buf[nlong + i] * w_cur_long[nlong - 1 - i];
            }
        }
        WindowSequence::EightShort => unreachable!("handled by short_synthesis"),
    }
}

fn short_synthesis(
    buf: &[f32; 2048],
    state: &mut ChannelState,
    ws: usize,
    windows: &Windows,
    out: &mut [f32; 1024],
) {
    let nlong = 1024;
    let nshort = 128;
    let nflat_ls = (nlong - nshort) / 2; // 448
    let trans = nshort / 2; // 64
    let w_prev = &windows.short[state.prev_shape as usize];
    let w_cur = &windows.short[ws];

    for i in 0..nflat_ls {
        out[i] = state.overlap[i];
    }
    for i in 0..nshort {
        out[nflat_ls + i] = state.overlap[nflat_ls + i] + buf[nshort * 0 + i] * w_prev[i];
        out[nflat_ls + nshort + i] = state.overlap[nflat_ls + nshort + i]
            + buf[nshort * 1 + i] * w_cur[nshort - 1 - i]
            + buf[nshort * 2 + i] * w_cur[i];
        out[nflat_ls + 2 * nshort + i] = state.overlap[nflat_ls + 2 * nshort + i]
            + buf[nshort * 3 + i] * w_cur[nshort - 1 - i]
            + buf[nshort * 4 + i] * w_cur[i];
        out[nflat_ls + 3 * nshort + i] = state.overlap[nflat_ls + 3 * nshort + i]
            + buf[nshort * 5 + i] * w_cur[nshort - 1 - i]
            + buf[nshort * 6 + i] * w_cur[i];
        if i < trans {
            out[nflat_ls + 4 * nshort + i] = state.overlap[nflat_ls + 4 * nshort + i]
                + buf[nshort * 7 + i] * w_cur[nshort - 1 - i]
                + buf[nshort * 8 + i] * w_cur[i];
        }
    }

    for i in 0..nshort {
        if i >= trans {
            state.overlap[nflat_ls + 4 * nshort + i - nlong] =
                buf[nshort * 7 + i] * w_cur[nshort - 1 - i] + buf[nshort * 8 + i] * w_cur[i];
        }
        state.overlap[nflat_ls + 5 * nshort + i - nlong] =
            buf[nshort * 9 + i] * w_cur[nshort - 1 - i] + buf[nshort * 10 + i] * w_cur[i];
        state.overlap[nflat_ls + 6 * nshort + i - nlong] =
            buf[nshort * 11 + i] * w_cur[nshort - 1 - i] + buf[nshort * 12 + i] * w_cur[i];
        state.overlap[nflat_ls + 7 * nshort + i - nlong] =
            buf[nshort * 13 + i] * w_cur[nshort - 1 - i] + buf[nshort * 14 + i] * w_cur[i];
        state.overlap[nflat_ls + 8 * nshort + i - nlong] =
            buf[nshort * 15 + i] * w_cur[nshort - 1 - i];
    }
    for i in 0..nflat_ls {
        state.overlap[nflat_ls + nshort + i] = 0.0;
    }
}
