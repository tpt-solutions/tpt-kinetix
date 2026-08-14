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
use crate::syntax::{AacParseError, ChannelStream, Element, IcsInfo, RawDataBlock, WindowSequence};
use crate::tables::{SWB_OFFSET_1024, SWB_OFFSET_128};
use crate::tns::apply_tns;
use crate::window::build_window;

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
            long: [build_window(1024, false, false), build_window(1024, true, false)],
            short: [build_window(128, false, true), build_window(128, true, true)],
        }
    }
}

/// Per-output-channel overlap-add and window-shape state.
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

    /// Decode one ADTS frame, returning one 1024-sample-per-channel PCM frame.
    pub fn decode(&mut self, packet: &Packet) -> Result<Option<AudioFrame>, AacError> {
        let header = AdtsHeader::parse(&packet.data)?;
        if header.frame_length > packet.data.len() {
            return Ok(None);
        }
        let sf_index = header.sampling_frequency_index as usize;
        self.sf_index = sf_index;
        self.sample_rate = header.sample_rate;
        let payload = &packet.data[header.header_len..header.frame_length];

        let block = RawDataBlock::parse(payload, sf_index)?;

        let mut frame_channels: Vec<[f32; 1024]> = Vec::new();
        for el in &block.elements {
            match el {
                Element::Sce(_) | Element::Lfe(_) => {
                    let ch = frame_channels.len();
                    let stream = elem_stream(el);
                    let mut coeffs = stream.coeffs;
                    let swb = if stream.ics.window_sequence.is_eight_short() {
                        SWB_OFFSET_128[sf_index]
                    } else {
                        SWB_OFFSET_1024[sf_index]
                    };
                    if let Some(p) = &stream.pulse {
                        apply_pulse(p, swb, &mut coeffs);
                    }
                    let gindex = group_base_offsets(&stream.ics);
                    apply_pns(&stream.ics, &stream.band_type, &stream.scalefactor, swb, stream.global_gain, &gindex, &mut coeffs);
                    if let Some(tns) = &stream.tns {
                        apply_tns(tns, &stream.ics, &mut coeffs, swb);
                    }
                    let pcm = self.synthesize_channel(ch, &stream.ics, &coeffs);
                    frame_channels.push(pcm);
                }
                Element::Cpe(cpe) => {
                    let ch_l = frame_channels.len();
                    let ch_r = ch_l + 1;

                    let mut l = cpe.left.coeffs;
                    let mut r = cpe.right.coeffs;

                    let swb = if cpe.left.ics.window_sequence.is_eight_short() {
                        SWB_OFFSET_128[sf_index]
                    } else {
                        SWB_OFFSET_1024[sf_index]
                    };
                    if let Some(p) = &cpe.left.pulse {
                        apply_pulse(p, swb, &mut l);
                    }
                    if let Some(p) = &cpe.right.pulse {
                        apply_pulse(p, swb, &mut r);
                    }
                    let gindex_l = group_base_offsets(&cpe.left.ics);
                    let gindex_r = group_base_offsets(&cpe.right.ics);
                    apply_pns(&cpe.left.ics, &cpe.left.band_type, &cpe.left.scalefactor, swb, cpe.left.global_gain, &gindex_l, &mut l);
                    apply_pns(&cpe.right.ics, &cpe.right.band_type, &cpe.right.scalefactor, swb, cpe.right.global_gain, &gindex_r, &mut r);

                    apply_stereo(
                        &mut l,
                        &mut r,
                        &cpe.left.ics,
                        &cpe.left.band_type,
                        &cpe.right.band_type,
                        &cpe.left.scalefactor,
                        &cpe.right.scalefactor,
                        cpe.ms_mask_present,
                        &cpe.ms_mask,
                        swb,
                    );

                    if let Some(tns) = &cpe.left.tns {
                        apply_tns(tns, &cpe.left.ics, &mut l, swb);
                    }
                    if let Some(tns) = &cpe.right.tns {
                        apply_tns(tns, &cpe.right.ics, &mut r, swb);
                    }

                    let pcm_l = self.synthesize_channel(ch_l, &cpe.left.ics, &l);
                    let pcm_r = self.synthesize_channel(ch_r, &cpe.right.ics, &r);
                    frame_channels.push(pcm_l);
                    frame_channels.push(pcm_r);
                }
                Element::Fil(_) | Element::End => {}
            }
        }

        let nch = frame_channels.len();
        if nch == 0 {
            return Ok(None);
        }
        let mut interleaved = Vec::with_capacity(1024 * nch);
        for s in 0..1024 {
            for c in 0..nch {
                interleaved.push(frame_channels[c][s]);
            }
        }

        let mut data = Vec::with_capacity(interleaved.len() * 4);
        for &s in &interleaved {
            data.extend_from_slice(&s.to_le_bytes());
        }

        Ok(Some(AudioFrame {
            pts: packet.pts,
            data,
            sample_rate: header.sample_rate,
            channels: header.channels.max(1),
            sample_format: SampleFormat::F32,
        }))
    }

    /// Run the filterbank for one output channel, reusing overlap state.
    fn synthesize_channel(
        &mut self,
        ch: usize,
        ics: &IcsInfo,
        coeffs: &[f32; 1024],
    ) -> [f32; 1024] {
        if self.channels.len() <= ch {
            self.channels.resize_with(ch + 1, ChannelState::default);
        }
        let im_l = &self.imdct_long;
        let im_s = &self.imdct_short;
        let win = &self.windows;
        let st: &mut ChannelState = &mut self.channels[ch];
        synthesize(im_l, im_s, win, st, ics, coeffs)
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
            imdct_short.transform(&coeffs[w * 128..w * 128 + 128], &mut buf[w * 256..w * 256 + 256]);
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
                state.overlap[nflat_ls + i] = buf[nlong + nflat_ls + i] * w_cur_short[nshort - 1 - i];
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
                out[nflat_ls + i] = state.overlap[nflat_ls + i] + buf[nflat_ls + i] * w_prev_short[i];
            }
            for i in 0..nflat_ls {
                out[nflat_ls + nshort + i] = state.overlap[nflat_ls + nshort + i] + buf[nflat_ls + nshort + i];
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
        state.overlap[nflat_ls + 8 * nshort + i - nlong] = buf[nshort * 15 + i] * w_cur[nshort - 1 - i];
    }
    for i in 0..nflat_ls {
        state.overlap[nflat_ls + nshort + i] = 0.0;
    }
}
