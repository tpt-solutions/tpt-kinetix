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
use crate::pns::{apply_pns, PnsRandom};
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
    #[allow(dead_code)]
    global_gain: u8,
    #[allow(dead_code)]
    pulse: Option<crate::pulse::PulseData>,
    #[allow(dead_code)]
    tns: Option<crate::tns::TnsData>,
    is_cce: bool, // true if this is a coupling channel (not output directly)
    #[allow(dead_code)]
    cpe_pair: Option<(usize, usize)>, // (left_idx, right_idx) if part of CPE
}

/// A decoded CPE pair ready for stereo processing and synthesis.
#[derive(Debug, Clone)]
struct DecodedCpe {
    instance_tag: u8,
    #[allow(dead_code)]
    left: DecodedChannel,
    #[allow(dead_code)]
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
    #[allow(dead_code)]
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
    frame_no: u64,
    /// Shared, continuously-advanced PNS pseudo-random generator (ffmpeg's
    /// `random_state`). Seeded once at construction and never reset.
    pns_rng: PnsRandom,
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
            frame_no: 0,
            pns_rng: PnsRandom::new(),
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
        let frame_no = self.frame_no;
        self.frame_no += 1;

        // Initialize decoder state from first header.
        if self.sample_rate == 0 {
            self.sample_rate = hdr.sample_rate;
            self.sf_index = hdr.sampling_frequency_index as usize;
            self.channels = vec![ChannelState::default(); hdr.channels as usize];
        }

        // Parse the raw data block.
        //
        // `frame_length` and `header_len` both come from the (untrusted) ADTS
        // header, so neither may be used to slice `packet.data` without being
        // checked against its real length first: a frame truncated in transit
        // (partial network read, damaged file) advertises a length longer than
        // the bytes actually present. This previously panicked with "range end
        // index N out of range for slice of length M" — found by
        // `tests/proptest_decode_never_panics.rs`.
        if hdr.frame_length > packet.data.len() || hdr.header_len > hdr.frame_length {
            return Err(AacError::Parse(AacParseError::UnexpectedEof));
        }
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

        if std::env::var("AAC_DBG_GG").is_ok() {
            for el in &block.elements {
                match el {
                    Element::Sce(s) => eprintln!(
                        "DBG gg SCE inst={} gg={}",
                        s.instance_tag, s.stream.global_gain
                    ),
                    Element::Cpe(c) => eprintln!(
                        "DBG gg CPE inst={} ggL={} ggR={}",
                        c.instance_tag, c.left.global_gain, c.right.global_gain
                    ),
                    _ => {}
                }
            }
        }
        for el in &block.elements {
            match el {
                Element::Sce(sce) => {
                    if std::env::var("AAC_DBG_WS").is_ok() {
                        eprintln!(
                            "DBG ws frame{} seq={:?} max_sfb={} bands={:?}",
                            self.frame_no,
                            sce.stream.ics.window_sequence,
                            sce.stream.ics.max_sfb,
                            &sce.stream.band_type[..sce.stream.ics.max_sfb.min(8) as usize]
                        );
                    }
                    let ch = Self::decode_channel_stream(
                        &sce.stream,
                        self.sf_index,
                        self.frame_no,
                        &mut self.pns_rng,
                    )?;
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
                    if std::env::var("AAC_DBG_WS").is_ok() {
                        eprintln!(
                            "DBG ws(cpe) frame{} L={:?} R={:?} ms={} maxsfbL={} maxsfbR={} tnsL={} tnsR={} pulseL={} pulseR={} predL={} predR={}",
                            self.frame_no,
                            cpe.left.ics.window_sequence,
                            cpe.right.ics.window_sequence,
                            cpe.ms_mask_present,
                            cpe.left.ics.max_sfb,
                            cpe.right.ics.max_sfb,
                            cpe.left.tns.is_some(),
                            cpe.right.tns.is_some(),
                            cpe.left.pulse.is_some(),
                            cpe.right.pulse.is_some(),
                            cpe.left.ics.predictor_data_present,
                            cpe.right.ics.predictor_data_present,
                        );
                    }
                    let left_ch = Self::decode_channel_stream(
                        &cpe.left,
                        self.sf_index,
                        self.frame_no,
                        &mut self.pns_rng,
                    )?;
                    let right_ch = Self::decode_channel_stream(
                        &cpe.right,
                        self.sf_index,
                        self.frame_no,
                        &mut self.pns_rng,
                    )?;
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
                    let ch = Self::decode_channel_stream(
                        &lfe.stream,
                        self.sf_index,
                        self.frame_no,
                        &mut self.pns_rng,
                    )?;
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
                let _right_ics = decoded_channels[right_idx].ics;
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
                // `ch.coeffs` holds 8 concatenated 128-line short-window spectra
                // (de-interleaved, per `decode_spectral_data`'s `gbase + w_idx*128`
                // layout); `short_synthesis` expects `buf` as 8 concatenated
                // 256-sample IMDCT outputs, so each 128-line window is
                // transformed separately, not the whole 1024-line buffer at once.
                for w in 0..8 {
                    self.imdct_short.transform(
                        &ch.coeffs[w * 128..(w + 1) * 128],
                        &mut buf[w * 256..(w + 1) * 256],
                    );
                }
                short_synthesis(
                    &buf,
                    &mut self.channels[ch_idx],
                    ch.ics.window_shape as usize,
                    &self.windows,
                    &mut out_buf,
                );
                self.channels[ch_idx].prev_shape = ch.ics.window_shape as u8;
            } else {
                self.imdct_long.transform(&ch.coeffs, &mut buf);
                if let Ok(spec) = std::env::var("AAC_DBG_OVERLAP") {
                    // spec = "frame:index_in_frame", e.g. "7:270" — dumps the
                    // raw pre-window IMDCT output and the carried-over overlap
                    // state around that sample, to check for catastrophic
                    // cancellation in `out[i] = overlap[i] + buf[i]*window[i]`
                    // when the underlying spectral coefficients are very large.
                    if let Some((f, idx)) = spec.split_once(':') {
                        if let (Ok(f), Ok(idx)) = (f.parse::<u64>(), idx.parse::<usize>()) {
                            if f == frame_no && ch_idx == 0 {
                                let overlap = &self.channels[ch_idx].overlap;
                                let lo = idx.saturating_sub(3);
                                let hi = (idx + 3).min(2048);
                                if idx < 1024 {
                                    let hi1 = hi.min(1024);
                                    for (i, (&b, &o)) in
                                        buf[lo..hi1].iter().zip(&overlap[lo..hi1]).enumerate()
                                    {
                                        eprintln!(
                                            "DBG overlap frame{frame_no} i={} buf[i]={b:e} overlap_in[i]={o:e}",
                                            lo + i
                                        );
                                    }
                                } else {
                                    // Second half of buf: show what will be stored as overlap.
                                    let ws = ch.ics.window_shape as usize;
                                    let w = &self.windows.long[ws];
                                    for (rel, &b) in buf[1024..hi].iter().enumerate() {
                                        let wv = w[1023 - rel];
                                        eprintln!(
                                            "DBG overlap frame{frame_no} buf[{}]={b:e} w[{}]={wv:.6} stored={:e}",
                                            rel + 1024,
                                            1023 - rel,
                                            b * wv
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
                if std::env::var("AAC_DBG_WIN").is_ok() && frame_no <= 44 && ch_idx == 0 {
                    let mut pns = 0;
                    let mut first_pns_sf = 0i32;
                    let mut first_pns_scale = 0.0f32;
                    for (i, &bt) in ch.band_type.iter().enumerate() {
                        if bt == 13 {
                            pns += 1;
                            if pns == 1 {
                                first_pns_sf = ch.scalefactor[i];
                                first_pns_scale = crate::dequant::dequant_scale(
                                    ch.global_gain,
                                    ch.scalefactor[i],
                                );
                            }
                        }
                    }
                    eprintln!(
                        "DBG frame{frame_no} seq={:?} pns_bands={pns} gg={} first_pns_sf={} first_pns_scale={:.6} prev_shape={} cur_shape={}",
                        ch.ics.window_sequence,
                        ch.global_gain,
                        first_pns_sf,
                        first_pns_scale,
                        self.channels[ch_idx].prev_shape,
                        ch.ics.window_shape
                    );
                }
                long_synthesis(
                    &buf,
                    &mut self.channels[ch_idx],
                    ch.ics.window_sequence,
                    ch.ics.window_shape as usize,
                    &self.windows,
                    &mut out_buf,
                );
                self.channels[ch_idx].prev_shape = ch.ics.window_shape as u8;
            }
            // ISO 14496-3 §4.5.2.3.6 ("Output word length"): the decoder's
            // native IMDCT/synthesis output is scaled so its integer part is
            // directly usable as 16-bit PCM, i.e. it is *not* normalized to
            // [-1, 1]. `AudioFrame`'s `SampleFormat::F32` is normalized float
            // PCM (matching e.g. ffmpeg's `fltp`), so divide by 2^15 here.
            for s in &mut out_buf {
                *s /= 32768.0;
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
            for plane in &pcm_planes {
                interleaved.push(plane[i]);
            }
        }

        // Convert f32 samples to bytes.
        //
        // Final sanitization: a successful decode must never emit NaN or
        // infinity. Individual stages already clamp their own overflow-prone
        // arithmetic (see `dequant::dequant_scale`/`dequant_coeff` and
        // `stereo`'s intensity factor), but the TNS all-pole filter and the
        // overlap-add accumulation can still amplify an already-extreme -
        // though finite - spectrum from a corrupt stream into a non-finite
        // sample. Callers reasonably assume `Ok(frame)` means usable PCM, and a
        // single NaN propagates through any downstream mixing/resampling, so
        // this replaces non-finite samples with silence rather than exporting
        // them. Guaranteed by `tests/proptest_decode_never_panics.rs`'s
        // `decoded_samples_are_finite_and_bounded`.
        let mut data = Vec::with_capacity(interleaved.len() * 4);
        for &s in &interleaved {
            let s = if s.is_finite() { s } else { 0.0 };
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
        frame_no_diag: u64,
        pns_rng: &mut PnsRandom,
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
            pns_rng,
            &mut coeffs,
        );
        if let Some(tns) = &stream.tns {
            apply_tns(tns, ics, &mut coeffs, swb);
        }
        let dbg_bands_frame = std::env::var("AAC_DBG_BANDS_FRAME")
            .ok()
            .and_then(|s| s.parse::<u64>().ok());
        if std::env::var("AAC_DBG_BANDS").is_ok()
            && dbg_bands_frame.is_none_or(|f| f == frame_no_diag)
        {
            let max_sfb = ics.max_sfb as usize;
            let mut maxc = 0.0f32;
            let mut maxbi = 0usize;
            for (k, &c) in coeffs.iter().enumerate() {
                if c.abs() > maxc {
                    maxc = c.abs();
                    maxbi = k;
                }
            }
            eprint!(
                "DBG full frame{} gg={} msf={} maxcoeff={:e} atk={} | ",
                frame_no_diag, stream.global_gain, max_sfb, maxc, maxbi
            );
            for sfb in 0..max_sfb {
                let sfv = stream.scalefactor.get(sfb).copied().unwrap_or(0);
                let scale = crate::dequant::dequant_scale(stream.global_gain, sfv);
                eprint!(
                    "{}:{} ",
                    stream.band_type.get(sfb).copied().unwrap_or(0),
                    (scale.log2() * 10.0) as i32
                );
            }
            eprintln!();
        }
        Ok(coeffs)
    }
}

/// Return the decoded channel stream for an SCE or LFE element.
#[allow(dead_code)]
fn elem_stream(el: &Element) -> &ChannelStream {
    match el {
        Element::Sce(s) => &s.stream,
        Element::Lfe(l) => &l.stream,
        _ => unreachable!("elem_stream called on non-single-channel element"),
    }
}

/// Run the filterbank (IMDCT + window + overlap-add) for one channel.
#[allow(dead_code)]
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
            state.overlap[..nflat_ls].copy_from_slice(&buf[nlong..nlong + nflat_ls]);
            for i in 0..nshort {
                state.overlap[nflat_ls + i] =
                    buf[nlong + nflat_ls + i] * w_cur_short[nshort - 1 - i];
            }
            for i in 0..nflat_ls {
                state.overlap[nflat_ls + nshort + i] = 0.0;
            }
        }
        WindowSequence::LongStop => {
            out[..nflat_ls].copy_from_slice(&state.overlap[..nflat_ls]);
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

    out[..nflat_ls].copy_from_slice(&state.overlap[..nflat_ls]);
    for i in 0..nshort {
        out[nflat_ls + i] = state.overlap[nflat_ls + i] + buf[i] * w_prev[i];
        out[nflat_ls + nshort + i] = state.overlap[nflat_ls + nshort + i]
            + buf[nshort + i] * w_cur[nshort - 1 - i]
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

#[cfg(test)]
mod synth_tests {
    use super::*;
    use crate::adts::AdtsHeader;
    use crate::mdct::Imdct;
    use crate::pns::PnsRandom;
    use crate::syntax::{Element, RawDataBlock};
    use tpt_kinetix_core::packet::Packet;
    use tpt_kinetix_core::timestamp::Timestamp;
    use tpt_kinetix_test_utils::reference::{decode_aac_with_ffmpeg, ffmpeg_available};
    use tpt_kinetix_test_utils::synthetic::encode_aac_adts_lavfi;

    /// Diagnostic (run with `-- --ignored`): localize the single-sample outlier in
    /// `sweep_stereo_44100` frame 7 by reconstructing the full 2048-sample IMDCT
    /// error of frame 7 (via TDAC: frame 7's output gives the first half, frame 8's
    /// overlap gives the second half) and projecting it onto the *full-length*
    /// orthogonal IMDCT basis. Because the synthesis operator is linear and known,
    /// this recovers exactly which spectral coefficient is wrong and what value the
    /// reference decoder produced for it — a bit-for-bit reference trace without
    /// needing ffmpeg internals.
    #[test]
    #[ignore]
    fn dbg_sweep_frame7_coeff_error_projection() {
        if !ffmpeg_available() {
            eprintln!("skip (ffmpeg unavailable)");
            return;
        }
        let adts = match encode_aac_adts_lavfi(
            "aevalsrc=exprs='sin(2*PI*(200+1900*t)*t)':s=44100:d=1.0",
            2,
            "128k",
        ) {
            Some(a) => a,
            None => {
                eprintln!("skip (encode failed)");
                return;
            }
        };

        // Split ADTS frames.
        let mut frames = Vec::new();
        let mut i = 0usize;
        while i + 7 <= adts.len() {
            if adts[i] == 0xFF && (adts[i + 1] & 0xF0) == 0xF0 {
                let fl = (((adts[i + 3] & 0x03) as usize) << 11)
                    | ((adts[i + 4] as usize) << 3)
                    | ((adts[i + 5] as usize) >> 5);
                if fl == 0 || i + fl > adts.len() {
                    break;
                }
                frames.push(adts[i..i + fl].to_vec());
                i += fl;
            } else {
                i += 1;
            }
        }
        assert!(
            frames.len() > 8,
            "expected >=9 ADTS frames, got {}",
            frames.len()
        );

        // Decode with our decoder, concatenating de-interleaved ch0 PCM.
        let mut dec = AacDecoder::new();
        let mut native_full: Vec<f32> = Vec::new();
        for f in &frames {
            let pkt = Packet {
                pts: Timestamp::NONE,
                dts: Timestamp::NONE,
                data: f.clone(),
                stream_index: 0,
                is_key_frame: true,
            };
            if let Ok(Some(frame)) = dec.decode(&pkt) {
                let s: Vec<f32> = frame
                    .data
                    .chunks_exact(4)
                    .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]) / 32768.0)
                    .collect();
                let ch0: Vec<f32> = s.iter().step_by(2).copied().collect();
                native_full.extend(ch0);
            }
        }
        let reference = decode_aac_with_ffmpeg(&adts).expect("ffmpeg decode");

        // Concatenate de-interleaved ch0 samples into full sample streams.
        let mut ref_full: Vec<f32> = Vec::new();
        for rf in &reference {
            let ch0: Vec<f32> = rf
                .data
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .step_by(2)
                .collect();
            ref_full.extend(ch0);
        }

        // Find best integer sample lag (native[i] aligns with ref[i+lag]) maximizing
        // channel-0 correlation, so frame 7/8 are compared at the same signal phase
        // as the conformance harness does (a real AAC encoder's priming delay is not
        // a multiple of 1024 samples).
        let mut best_lag = 0i64;
        let mut best_corr = f64::MIN;
        for lag in -2048i64..=2048 {
            let (n0, r0) = if lag >= 0 {
                (0usize, lag as usize)
            } else {
                ((-lag) as usize, 0usize)
            };
            if n0 >= native_full.len() || r0 >= ref_full.len() {
                continue;
            }
            let len = (native_full.len() - n0).min(ref_full.len() - r0);
            if len < 8192 {
                continue;
            }
            let (mut dot, mut nn, mut rr) = (0.0f64, 0.0, 0.0);
            for k in 0..len {
                let a = native_full[n0 + k] as f64;
                let b = ref_full[r0 + k] as f64;
                dot += a * b;
                nn += a * a;
                rr += b * b;
            }
            if nn > 0.0 && rr > 0.0 {
                let c = dot / (nn.sqrt() * rr.sqrt());
                if c > best_corr {
                    best_corr = c;
                    best_lag = lag;
                }
            }
        }
        eprintln!("best sample lag = {best_lag}, corr = {best_corr:.6}");

        // Aligned frame-7 / frame-8 ch0 regions (native offset 0, ref offset best_lag).
        let n_off = if best_lag < 0 {
            (-best_lag) as usize
        } else {
            0
        };
        let r_off = if best_lag > 0 { best_lag as usize } else { 0 };
        let grab_n = |full: &[f32], f: usize| -> Vec<f32> {
            let s = f * 1024 + n_off;
            if s + 1024 > full.len() {
                vec![0.0f32; 1024]
            } else {
                full[s..s + 1024].to_vec()
            }
        };
        let grab_r = |full: &[f32], f: usize| -> Vec<f32> {
            let s = f * 1024 + r_off;
            if s + 1024 > full.len() {
                vec![0.0f32; 1024]
            } else {
                full[s..s + 1024].to_vec()
            }
        };
        let p7 = grab_n(&native_full, 7);
        let r7 = grab_r(&ref_full, 7);
        let p8 = grab_n(&native_full, 8);
        let r8 = grab_r(&ref_full, 8);
        let ics_of = |idx: usize| -> (bool, bool) {
            let hdr = AdtsHeader::parse(&frames[idx]).unwrap();
            let payload = &frames[idx][hdr.header_len..hdr.frame_length];
            let block =
                RawDataBlock::parse(payload, hdr.sampling_frequency_index as usize).unwrap();
            for el in &block.elements {
                if let Element::Cpe(cpe) = el {
                    return (
                        cpe.left.ics.window_sequence == crate::syntax::WindowSequence::OnlyLong,
                        cpe.left.ics.window_shape,
                    );
                }
            }
            panic!("no CPE in frame {idx}");
        };

        let (only7, ws7) = ics_of(7);
        let (only8, _ws8) = ics_of(8);
        let (_only6, ws6) = ics_of(6);
        assert!(
            only7 && only8,
            "TDAC reconstruction needs OnlyLong frames 7&8"
        );

        // Surface the window-sequence context around frame 7: a window-sequence
        // transition (e.g. LongStop -> OnlyLong) is the classic cause of a
        // time-localized bump in overlap-add, since the carried overlap is windowed
        // with the *previous* frame's shape.
        for f in 4..=9 {
            let (ol, wsh) = ics_of(f);
            let seq = if ol { "OnlyLong" } else { "trans/short" };
            eprintln!("frame {f}: only_long={ol} ({seq}) window_shape={wsh}");
        }

        let windows = Windows::new();
        // prev_shape used when synthesizing frame 7 = frame 6's window_shape.
        let w_prev7 = &windows.long[ws6 as usize];
        let w_cur7 = &windows.long[ws7 as usize];

        let f8max = p8
            .iter()
            .zip(&r8)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        eprintln!(
            "frame7 ch0 residual max = {:.3e}",
            p7.iter()
                .zip(&r7)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0f32, f32::max)
        );
        eprintln!(
            "frame8 ch0 residual max = {f8max:.3e} (leakage into frame-7 2nd-half reconstruction)"
        );

        // Reconstruct the full 2048-sample IMDCT error of frame 7.
        // First half (i in 0..1024): out7[i] = overlap7[i] + buf7[i]*w_prev7[i];
        // overlap7 is frame-6-derived and assumed exact, so buf7_err[i] =
        // (out7_native[i]-out7_ref[i]) / w_prev7[i].
        // Second half (i in 1024..2048): becomes overlap8, i.e.
        // buf7_err[1024+i] = (out8_native[i]-out8_ref[i]) / w_cur7[1023-i]
        // (subject to frame-8 current-frame leakage, reported above).
        let mut buf7_err = [0.0f64; 2048];
        for i in 0..1024 {
            let wk = w_prev7[i];
            buf7_err[i] = if wk.abs() > 1e-3 {
                (p7[i] - r7[i]) as f64 / wk as f64
            } else {
                0.0
            };
        }
        for j in 0..1024 {
            let wk = w_cur7[1023 - j];
            buf7_err[1024 + j] = if wk.abs() > 1e-3 {
                (p8[j] - r8[j]) as f64 / wk as f64
            } else {
                0.0
            };
        }

        // Project onto the full-length orthogonal IMDCT basis.
        let imdct = Imdct::new(1024);
        let mut err = [0f64; 1024];
        let mut best_b = 0usize;
        let mut best_e = 0.0f64;
        for b in 0..1024 {
            let mut unit = [0f32; 1024];
            unit[b] = 1.0;
            let mut col = [0f32; 2048];
            imdct.transform(&unit, &mut col);
            let mut num = 0.0f64;
            let mut den = 0.0f64;
            for i in 0..2048 {
                let c = col[i] as f64;
                num += buf7_err[i] * c;
                den += c * c;
            }
            let e = if den > 0.0 { num / den } else { 0.0 };
            err[b] = e;
            if e.abs() > best_e {
                best_e = e.abs();
                best_b = b;
            }
        }

        // Reference (post-M/S) coeffs for frame 7's left channel.
        let hdr = AdtsHeader::parse(&frames[7]).unwrap();
        let payload = &frames[7][hdr.header_len..hdr.frame_length];
        let block = RawDataBlock::parse(payload, hdr.sampling_frequency_index as usize).unwrap();
        let mut cpe = None;
        for el in &block.elements {
            if let Element::Cpe(c) = el {
                cpe = Some(c.clone());
            }
        }
        let cpe = cpe.expect("CPE in frame 7");
        let mut rng = PnsRandom::new();
        let mut mid = AacDecoder::decode_channel_stream(
            &cpe.left,
            hdr.sampling_frequency_index as usize,
            7,
            &mut rng,
        )
        .unwrap();
        let mut side = AacDecoder::decode_channel_stream(
            &cpe.right,
            hdr.sampling_frequency_index as usize,
            7,
            &mut rng,
        )
        .unwrap();
        let swb = crate::tables::SWB_OFFSET_1024[hdr.sampling_frequency_index as usize];
        crate::stereo::apply_stereo(
            &mut mid,
            &mut side,
            &cpe.left.ics,
            &cpe.left.band_type,
            &cpe.right.band_type,
            &cpe.left.scalefactor,
            &cpe.right.scalefactor,
            cpe.ms_mask_present,
            &cpe.ms_mask,
            swb,
        );

        eprintln!(
            "FRAME7 dominant coeff error: bin {best_b} err={:e} postMS_native={:e} ref_est={:e}",
            err[best_b],
            mid[best_b],
            mid[best_b] - err[best_b] as f32
        );
        let mut idxs: Vec<usize> = (0..1024).collect();
        idxs.sort_by(|&a, &b| err[b].abs().partial_cmp(&err[a].abs()).unwrap());
        for &b in idxs.iter().take(15) {
            if err[b].abs() > 1.0 {
                eprintln!(
                    "   bin {b}: err={:e} postMS_native={:e} ref_est={:e}",
                    err[b],
                    mid[b],
                    mid[b] - err[b] as f32
                );
            }
        }
    }

    /// TDAC projection for frame 24 (TNS frame) to identify the spectral coefficient
    /// error that leaks into frame 25 via overlap-add. The sweep_stereo outlier is
    /// localized to frame 25 sample 488; frame 24 has TNS, frame 25 doesn't.
    #[test]
    #[ignore]
    fn dbg_sweep_frame24_tns_coeff_error() {
        if !ffmpeg_available() {
            eprintln!("skip (ffmpeg unavailable)");
            return;
        }
        let adts = match encode_aac_adts_lavfi(
            "aevalsrc=exprs='sin(2*PI*(200+1900*t)*t)':s=44100:d=1.0",
            2,
            "128k",
        ) {
            Some(a) => a,
            None => {
                eprintln!("skip (encode failed)");
                return;
            }
        };

        let mut frames = Vec::new();
        let mut i = 0usize;
        while i + 7 <= adts.len() {
            if adts[i] == 0xFF && (adts[i + 1] & 0xF0) == 0xF0 {
                let fl = (((adts[i + 3] & 0x03) as usize) << 11)
                    | ((adts[i + 4] as usize) << 3)
                    | ((adts[i + 5] as usize) >> 5);
                if fl == 0 || i + fl > adts.len() {
                    break;
                }
                frames.push(adts[i..i + fl].to_vec());
                i += fl;
            } else {
                i += 1;
            }
        }
        assert!(frames.len() > 25, "need >=26 frames, got {}", frames.len());

        let mut dec = AacDecoder::new();
        let mut native_full: Vec<f32> = Vec::new();
        for f in &frames {
            let pkt = Packet {
                pts: Timestamp::NONE,
                dts: Timestamp::NONE,
                data: f.clone(),
                stream_index: 0,
                is_key_frame: true,
            };
            if let Ok(Some(frame)) = dec.decode(&pkt) {
                let s: Vec<f32> = frame
                    .data
                    .chunks_exact(4)
                    .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]) / 32768.0)
                    .collect();
                let ch0: Vec<f32> = s.iter().step_by(2).copied().collect();
                native_full.extend(ch0);
            }
        }
        let reference = decode_aac_with_ffmpeg(&adts).expect("ffmpeg decode");
        let mut ref_full: Vec<f32> = Vec::new();
        for rf in &reference {
            let ch0: Vec<f32> = rf
                .data
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .step_by(2)
                .collect();
            ref_full.extend(ch0);
        }

        // Find best integer sample lag.
        let mut best_lag = 0i64;
        let mut best_corr = f64::MIN;
        for lag in -2048i64..=2048 {
            let (n0, r0) = if lag >= 0 {
                (0usize, lag as usize)
            } else {
                ((-lag) as usize, 0usize)
            };
            if n0 >= native_full.len() || r0 >= ref_full.len() {
                continue;
            }
            let len = (native_full.len() - n0).min(ref_full.len() - r0);
            if len < 8192 {
                continue;
            }
            let (mut dot, mut nn, mut rr) = (0.0f64, 0.0, 0.0);
            for k in 0..len {
                let a = native_full[n0 + k] as f64;
                let b = ref_full[r0 + k] as f64;
                dot += a * b;
                nn += a * a;
                rr += b * b;
            }
            if nn > 0.0 && rr > 0.0 {
                let c = dot / (nn.sqrt() * rr.sqrt());
                if c > best_corr {
                    best_corr = c;
                    best_lag = lag;
                }
            }
        }
        eprintln!("best lag = {best_lag}, corr = {best_corr:.6}");

        let n_off = if best_lag < 0 {
            (-best_lag) as usize
        } else {
            0
        };
        let r_off = if best_lag > 0 { best_lag as usize } else { 0 };
        let grab_n = |full: &[f32], f: usize| -> Vec<f32> {
            let s = f * 1024 + n_off;
            if s + 1024 > full.len() {
                vec![0.0f32; 1024]
            } else {
                full[s..s + 1024].to_vec()
            }
        };
        let grab_r = |full: &[f32], f: usize| -> Vec<f32> {
            let s = f * 1024 + r_off;
            if s + 1024 > full.len() {
                vec![0.0f32; 1024]
            } else {
                full[s..s + 1024].to_vec()
            }
        };
        let p24 = grab_n(&native_full, 24);
        let r24 = grab_r(&ref_full, 24);
        let p25 = grab_n(&native_full, 25);
        let r25 = grab_r(&ref_full, 25);

        let ics_of = |idx: usize| -> (bool, bool) {
            let hdr = AdtsHeader::parse(&frames[idx]).unwrap();
            let payload = &frames[idx][hdr.header_len..hdr.frame_length];
            let block =
                RawDataBlock::parse(payload, hdr.sampling_frequency_index as usize).unwrap();
            for el in &block.elements {
                if let Element::Cpe(cpe) = el {
                    return (
                        cpe.left.ics.window_sequence == WindowSequence::OnlyLong,
                        cpe.left.ics.window_shape,
                    );
                }
            }
            panic!("no CPE in frame {idx}");
        };
        let (only24, ws24) = ics_of(24);
        let (only25, ws25) = ics_of(25);
        let (_only23, ws23) = ics_of(23);
        eprintln!(
            "frame24: only_long={only24} shape={ws24} | frame25: only_long={only25} shape={ws25}"
        );

        let windows = Windows::new();
        let w_prev24 = &windows.long[ws23 as usize];
        let w_cur24 = &windows.long[ws24 as usize];

        // Reconstruct the full 2048-sample IMDCT error of frame 24.
        let mut buf24_err = [0.0f64; 2048];
        for i in 0..1024 {
            let wk = w_prev24[i];
            buf24_err[i] = if wk.abs() > 1e-3 {
                (p24[i] - r24[i]) as f64 / wk as f64
            } else {
                0.0
            };
        }
        for j in 0..1024 {
            let wk = w_cur24[1023 - j];
            buf24_err[1024 + j] = if wk.abs() > 1e-3 {
                (p25[j] - r25[j]) as f64 / wk as f64
            } else {
                0.0
            };
        }

        // Project onto IMDCT basis.
        let imdct = Imdct::new(1024);
        let mut err = [0f64; 1024];
        let mut best_b = 0usize;
        let mut best_e = 0.0f64;
        for b in 0..1024 {
            let mut unit = [0f32; 1024];
            unit[b] = 1.0;
            let mut col = [0f32; 2048];
            imdct.transform(&unit, &mut col);
            let mut num = 0.0f64;
            let mut den = 0.0f64;
            for i in 0..2048 {
                let c = col[i] as f64;
                num += buf24_err[i] * c;
                den += c * c;
            }
            let e = if den > 0.0 { num / den } else { 0.0 };
            err[b] = e;
            if e.abs() > best_e {
                best_e = e.abs();
                best_b = b;
            }
        }

        // Get native post-TNS coeffs for frame 24.
        let hdr = AdtsHeader::parse(&frames[24]).unwrap();
        let payload = &frames[24][hdr.header_len..hdr.frame_length];
        let block = RawDataBlock::parse(payload, hdr.sampling_frequency_index as usize).unwrap();
        let mut cpe = None;
        for el in &block.elements {
            if let Element::Cpe(c) = el {
                cpe = Some(c.clone());
            }
        }
        let cpe = cpe.expect("CPE in frame 24");
        let mut rng = PnsRandom::new();
        let mid = AacDecoder::decode_channel_stream(
            &cpe.left,
            hdr.sampling_frequency_index as usize,
            24,
            &mut rng,
        )
        .unwrap();
        let side = AacDecoder::decode_channel_stream(
            &cpe.right,
            hdr.sampling_frequency_index as usize,
            24,
            &mut rng,
        )
        .unwrap();

        // Also decode WITHOUT TNS to isolate whether the error is from TNS.
        let swb = crate::tables::SWB_OFFSET_1024[hdr.sampling_frequency_index as usize];
        let mut coeffs_no_tns = cpe.left.coeffs;
        if let Some(p) = &cpe.left.pulse {
            crate::pulse::apply_pulse(p, swb, &mut coeffs_no_tns);
        }
        let gindex = crate::dequant::group_base_offsets(&cpe.left.ics);
        let mut rng2 = PnsRandom::new();
        crate::pns::apply_pns(
            &cpe.left.ics,
            &cpe.left.band_type,
            &cpe.left.scalefactor,
            swb,
            cpe.left.global_gain,
            &gindex,
            &mut rng2,
            &mut coeffs_no_tns,
        );
        // (intentionally skip TNS)

        // Apply M/S stereo to see if the error appears after M/S.
        let mut mid_ms = mid;
        let mut side_ms = side;
        crate::stereo::apply_stereo(
            &mut mid_ms,
            &mut side_ms,
            &cpe.left.ics,
            &cpe.left.band_type,
            &cpe.right.band_type,
            &cpe.left.scalefactor,
            &cpe.right.scalefactor,
            cpe.ms_mask_present,
            &cpe.ms_mask,
            swb,
        );

        eprintln!("\nCoefficients at error bins (pre-M/S vs post-M/S):");
        for b in [97, 99, 101, 103, 105, 107, 109, 111] {
            eprintln!(
                "  bin {b}: preMS_L={:e} preMS_R={:e} postMS_L={:e} postMS_R={:e}",
                mid[b], side[b], mid_ms[b], side_ms[b]
            );
        }

        // Print band types and ms_mask for the error bins.
        let max_sfb = cpe.left.ics.max_sfb as usize;
        let swb = crate::tables::SWB_OFFSET_1024[hdr.sampling_frequency_index as usize];
        eprintln!("\nBand types and ms_mask for error bins:");
        for sfb in 0..max_sfb {
            if sfb + 1 >= swb.len() {
                break;
            }
            let line_start = swb[sfb] as usize;
            let line_end = swb[sfb + 1] as usize;
            // Check if this band overlaps with the error region (97-111).
            if line_start <= 111 && line_end >= 97 {
                let l_bt = cpe.left.band_type.get(sfb).copied().unwrap_or(0);
                let r_bt = cpe.right.band_type.get(sfb).copied().unwrap_or(0);
                let ms_bit = match cpe.ms_mask_present {
                    0 => "N/A",
                    2 => "ALL",
                    _ => {
                        if cpe.ms_mask.get(sfb).copied().unwrap_or(false) {
                            "ms"
                        } else {
                            "lr"
                        }
                    }
                };
                eprintln!(
                    "  sfb {sfb}: lines {line_start}-{line_end} L_bt={l_bt} R_bt={r_bt} ms={ms_bit}"
                );
            }
        }

        // Check if right channel is zero everywhere or just at error bins.
        let r_energy: f64 = side.iter().map(|&x| (x as f64).powi(2)).sum();
        let r_max = side.iter().map(|&x| x.abs()).fold(0.0f32, f32::max);
        let r_nonzero = side.iter().filter(|&&x| x != 0.0).count();
        eprintln!("\nRight channel: energy={:.3e} max_abs={:.3e} nonzero={}/1024", r_energy, r_max, r_nonzero);
        let l_energy: f64 = mid.iter().map(|&x| (x as f64).powi(2)).sum();
        let l_max = mid.iter().map(|&x| x.abs()).fold(0.0f32, f32::max);
        let l_nonzero = mid.iter().filter(|&&x| x != 0.0).count();
        eprintln!("Left channel: energy={:.3e} max_abs={:.3e} nonzero={}/1024", l_energy, l_max, l_nonzero);

        // Check right channel band types across all bands.
        let max_sfb_r = cpe.right.ics.max_sfb as usize;
        eprintln!("\nRight channel band types (all bands):");
        for sfb in 0..max_sfb_r {
            let r_bt = cpe.right.band_type.get(sfb).copied().unwrap_or(0);
            if r_bt != 0 {
                eprintln!("  sfb {sfb}: R_bt={r_bt}");
            }
        }
        let r_bt_nonzero = cpe.right.band_type.iter().filter(|&&x| x != 0).count();
        eprintln!("Right channel: {} non-zero band types out of {}", r_bt_nonzero, cpe.right.band_type.len());

        // Check right channel raw coeffs (before any processing).
        let r_raw_nonzero = cpe.right.coeffs.iter().filter(|&&x| x != 0.0).count();
        eprintln!("Right channel raw coeffs: {} non-zero out of 1024", r_raw_nonzero);
        let l_raw_nonzero = cpe.left.coeffs.iter().filter(|&&x| x != 0.0).count();
        eprintln!("Left channel raw coeffs: {} non-zero out of 1024", l_raw_nonzero);

        // Print right channel sections.
        eprintln!("\nRight channel sections:");
        for (gi, grp) in cpe.right.sections.groups.iter().enumerate() {
            for (si, sec) in grp.iter().enumerate() {
                eprintln!("  g{gi}/s{si}: sect_cb={} sect_len={}", sec.sect_cb, sec.sect_len);
            }
        }
        eprintln!("\nLeft channel sections:");
        for (gi, grp) in cpe.left.sections.groups.iter().enumerate() {
            for (si, sec) in grp.iter().enumerate() {
                eprintln!("  g{gi}/s{si}: sect_cb={} sect_len={}", sec.sect_cb, sec.sect_len);
            }
        }

        // Re-parse the raw data block and track bitstream position around channel decodes.
        let mut reader = crate::bitreader::BitReader::new(payload);
        // Skip to the CPE element.
        let _id = reader.read_bits(3).unwrap();
        let _tag = reader.read_bits(4);
        let _common_window = reader.read_bit().unwrap() != 0;
        let _ics = crate::syntax::IcsInfo::parse(&mut reader).unwrap();
        let _ms_mask_present = reader.read_bits(2).unwrap() as u8;
        let num_groups = _ics.num_window_groups();
        let max_sfb = _ics.max_sfb as usize;
        if _ms_mask_present == 1 {
            for _ in 0..(num_groups * max_sfb) {
                reader.read_bit().unwrap();
            }
        }
        let pos_before_left = reader.bit_position();
        // We can't easily re-parse the left channel without re-running the full parse,
        // so instead let's compare the left and right channel's first few raw coeffs.
        eprintln!("\nLeft channel first 16 raw coeffs: {:?}", &cpe.left.coeffs[..16]);
        eprintln!("Right channel first 16 raw coeffs: {:?}", &cpe.right.coeffs[..16]);

        // Check right channel scalefactors and global_gain.
        eprintln!("\nLeft global_gain={}, Right global_gain={}", cpe.left.global_gain, cpe.right.global_gain);
        eprintln!("Left scalefactors (first 10): {:?}", &cpe.left.scalefactor[..10.min(cpe.left.scalefactor.len())]);
        eprintln!("Right scalefactors (first 10): {:?}", &cpe.right.scalefactor[..10.min(cpe.right.scalefactor.len())]);

        // Check if right channel has any non-zero scalefactors.
        let r_sf_nonzero = cpe.right.scalefactor.iter().filter(|&&x| x != 0).count();
        eprintln!("Right scalefactors: {} non-zero out of {}", r_sf_nonzero, cpe.right.scalefactor.len());
        let l_sf_nonzero = cpe.left.scalefactor.iter().filter(|&&x| x != 0).count();
        eprintln!("Left scalefactors: {} non-zero out of {}", l_sf_nonzero, cpe.left.scalefactor.len());

        // Check if right channel sections match left channel sections.
        eprintln!("\nSection comparison:");
        eprintln!("Left: {} groups", cpe.left.sections.groups.len());
        eprintln!("Right: {} groups", cpe.right.sections.groups.len());
        for (gi, (lgrp, rgrp)) in cpe.left.sections.groups.iter().zip(cpe.right.sections.groups.iter()).enumerate() {
            eprintln!("  Group {gi}: Left {} sections, Right {} sections", lgrp.len(), rgrp.len());
            for (si, (ls, rs)) in lgrp.iter().zip(rgrp.iter()).enumerate() {
                eprintln!("    s{si}: Left(cb={},len={}) Right(cb={},len={})", ls.sect_cb, ls.sect_len, rs.sect_cb, rs.sect_len);
            }
        }

        // Re-parse and track bitstream position around the channel pair.
        let mut reader = crate::bitreader::BitReader::new(payload);
        let _id = reader.read_bits(3).unwrap();
        let _tag = reader.read_bits(4);
        let _common_window = reader.read_bit().unwrap() != 0;
        let shared_ics = crate::syntax::IcsInfo::parse(&mut reader).unwrap();
        let _ms_mask_present = reader.read_bits(2).unwrap() as u8;
        if _ms_mask_present == 1 {
            let bits = shared_ics.num_window_groups() * (shared_ics.max_sfb as usize);
            for _ in 0..bits {
                reader.read_bit().unwrap();
            }
        }
        let pos_after_ms = reader.bit_position();
        // Parse left global_gain
        let _lg = reader.read_bits(8).unwrap() as u8;
        // Parse left sections
        for _g in 0..shared_ics.num_window_groups() {
            let mut covered = 0usize;
            while covered < shared_ics.max_sfb as usize {
                let _sect_cb = reader.read_bits(4).unwrap() as u8;
                let _sect_len = reader.read_section_length(5).unwrap() as usize;
                covered += _sect_len;
            }
        }
        // Skip left scalefactors, pulse, tns, gain_control, spectral_data
        // This is complex; instead let's just compare the raw coeffs positions
        eprintln!("\nBitstream position after ics+ms: {pos_after_ms}");
        eprintln!("Payload length: {} bytes = {} bits", payload.len(), payload.len() * 8);

        // Decode the right channel's first quad manually to see what bits are being read.
        // First, find the bitstream position where the right channel's spectral data starts.
        // We'll do this by re-parsing the left channel completely.
        let mut reader2 = crate::bitreader::BitReader::new(payload);
        // Skip to CPE
        let _id = reader2.read_bits(3).unwrap();
        let _tag = reader2.read_bits(4);
        let _common_window = reader2.read_bit().unwrap() != 0;
        let shared_ics2 = crate::syntax::IcsInfo::parse(&mut reader2).unwrap();
        let _ms_mask_present2 = reader2.read_bits(2).unwrap() as u8;
        if _ms_mask_present2 == 1 {
            let bits = shared_ics2.num_window_groups() * (shared_ics2.max_sfb as usize);
            for _ in 0..bits {
                reader2.read_bit().unwrap();
            }
        }
        // Parse left channel stream
        let _lg2 = reader2.read_bits(8).unwrap() as u8;
        // Left uses shared ics
        // Parse left sections
        for _g in 0..shared_ics2.num_window_groups() {
            let mut covered = 0usize;
            while covered < shared_ics2.max_sfb as usize {
                let _sect_cb = reader2.read_bits(4).unwrap() as u8;
                let _sect_len = reader2.read_section_length(5).unwrap() as usize;
                covered += _sect_len;
            }
        }
        // Skip left scalefactors
        for _ in 0..shared_ics2.num_window_groups() * (shared_ics2.max_sfb as usize) {
            let _sf = crate::codebooks::decode_scalefactor(&mut reader2);
        }
        // Skip left pulse
        let pulse_present = reader2.read_bit().unwrap() != 0;
        if pulse_present {
            // Parse pulse data
            let _npulse = reader2.read_bits(2).unwrap() as u8;
            let _pulse_start_sfb = reader2.read_bits(6).unwrap() as u8;
            for _ in 0..=_npulse {
                let _pulse_offset = reader2.read_bits(5).unwrap() as u8;
                let _pulse_amp = reader2.read_bits(4).unwrap() as u8;
            }
        }
        // Skip left tns
        let tns_present = reader2.read_bit().unwrap() != 0;
        if tns_present {
            // Parse TNS - this is complex, skip for now
            eprintln!("Left channel has TNS - skipping detailed parse");
        } else {
            // Skip gain control
            let _gc = reader2.read_bit();
        }
        let pos_before_left_spectral = reader2.bit_position();
        eprintln!("Bitstream position before left spectral data: {pos_before_left_spectral}");

        // Now skip the left spectral data
        // Left sections: cb=10 len=1, cb=6 len=9, cb=11 len=18, cb=4 len=13
        let swb = crate::tables::SWB_OFFSET_1024[hdr.sampling_frequency_index as usize];
        let left_sections = &cpe.left.sections;
        let mut sfb = 0usize;
        for sec in &left_sections.groups[0] {
            for _ in 0..sec.sect_len as usize {
                if sfb >= shared_ics2.max_sfb as usize { break; }
                if sfb + 1 >= swb.len() { break; }
                let width = (swb[sfb + 1] - swb[sfb]) as usize;
                let mut bin = 0usize;
                while bin < width {
                    if (1..=4).contains(&sec.sect_cb) {
                        // quad - decode but discard
                        let _q = crate::codebooks::decode_spectral_quad(&mut reader2, sec.sect_cb);
                    } else if (5..=11).contains(&sec.sect_cb) {
                        let _q1 = crate::codebooks::decode_spectral_quad(&mut reader2, sec.sect_cb);
                        let _q2 = crate::codebooks::decode_spectral_quad(&mut reader2, sec.sect_cb);
                    }
                    bin += 4;
                }
                sfb += 1;
            }
        }
        let pos_after_left_spectral = reader2.bit_position();
        eprintln!("Bitstream position after left spectral data: {pos_after_left_spectral}");

        // Now parse right channel
        let _rg = reader2.read_bits(8).unwrap() as u8;
        // Parse right sections
        for _g in 0..shared_ics2.num_window_groups() {
            let mut covered = 0usize;
            while covered < shared_ics2.max_sfb as usize {
                let _sect_cb = reader2.read_bits(4).unwrap() as u8;
                let _sect_len = reader2.read_section_length(5).unwrap() as usize;
                covered += _sect_len;
            }
        }
        // Skip right scalefactors
        for _ in 0..shared_ics2.num_window_groups() * (shared_ics2.max_sfb as usize) {
            let _sf = crate::codebooks::decode_scalefactor(&mut reader2);
        }
        // Skip right pulse
        let pulse_present_r = reader2.read_bit().unwrap() != 0;
        // Skip right tns
        let tns_present_r = reader2.read_bit().unwrap() != 0;
        let _gc_r = reader2.read_bit();
        let pos_before_right_spectral = reader2.bit_position();
        eprintln!("Bitstream position before right spectral data: {pos_before_right_spectral}");

        // Now decode the right channel's first quad manually
        let right_sections = &cpe.right.sections;
        let mut sfb_r = 0usize;
        let mut first_quad = None;
        for sec in &right_sections.groups[0] {
            for _ in 0..sec.sect_len as usize {
                if sfb_r >= shared_ics2.max_sfb as usize { break; }
                if sfb_r + 1 >= swb.len() { break; }
                let width = (swb[sfb_r + 1] - swb[sfb_r]) as usize;
                let mut bin = 0usize;
                while bin < width {
                    if (1..=4).contains(&sec.sect_cb) {
                        let q = crate::codebooks::decode_spectral_quad(&mut reader2, sec.sect_cb);
                        if first_quad.is_none() {
                            first_quad = q;
                        }
                    } else if (5..=11).contains(&sec.sect_cb) {
                        let _q1 = crate::codebooks::decode_spectral_quad(&mut reader2, sec.sect_cb);
                        let _q2 = crate::codebooks::decode_spectral_quad(&mut reader2, sec.sect_cb);
                    }
                    bin += 4;
                }
                sfb_r += 1;
            }
        }
        eprintln!("Right channel first quad (manual decode): {:?}", first_quad);
        eprintln!("Right channel section codebook: {}", right_sections.groups[0][0].sect_cb);

        // Peek at the bits at the right channel's spectral data position.
        let mut reader3 = crate::bitreader::BitReader::new(payload);
        reader3.seek_to_bit(pos_before_right_spectral);
        let peek_bits = reader3.peek(32).unwrap_or(0);
        eprintln!("Bits at right spectral position ({pos_before_right_spectral}): {:032b}", peek_bits);
        eprintln!("Byte at position: byte[{}] = {:02x}", pos_before_right_spectral / 8, payload[pos_before_right_spectral / 8]);

        // Also peek at the bits at the left channel's spectral data position for comparison.
        let mut reader4 = crate::bitreader::BitReader::new(payload);
        reader4.seek_to_bit(pos_before_left_spectral);
        let peek_left = reader4.peek(32).unwrap_or(0);
        eprintln!("Bits at left spectral position ({pos_before_left_spectral}): {:032b}", peek_left);

        // Manually decode the first codeword from the right channel's position.
        let mut reader5 = crate::bitreader::BitReader::new(payload);
        reader5.seek_to_bit(pos_before_right_spectral);
        let book1 = crate::codebooks::SPECTRAL_BOOKS[1];
        let mut cur: u32 = 0;
        let mut nbits: u32 = 0;
        let mut found = None;
        for _ in 0..20 {
            let bit = reader5.read_bit().unwrap() as u32;
            cur = (cur << 1) | bit;
            nbits += 1;
            let mask = (1u32 << nbits) - 1;
            for (i, &(code, len)) in book1.iter().enumerate() {
                if (len as u32) == nbits && (cur & mask) == code {
                    found = Some((i, code, len, nbits));
                    break;
                }
            }
            if found.is_some() { break; }
        }
        eprintln!("Right channel first codeword: idx={:?}", found);

        // Also decode the first codeword from the left channel's position for comparison.
        let mut reader6 = crate::bitreader::BitReader::new(payload);
        reader6.seek_to_bit(pos_before_left_spectral);
        let book10 = crate::codebooks::SPECTRAL_BOOKS[10];
        let mut cur6: u32 = 0;
        let mut nbits6: u32 = 0;
        let mut found6 = None;
        for _ in 0..20 {
            let bit = reader6.read_bit().unwrap() as u32;
            cur6 = (cur6 << 1) | bit;
            nbits6 += 1;
            let mask = (1u32 << nbits6) - 1;
            for (i, &(code, len)) in book10.iter().enumerate() {
                if (len as u32) == nbits6 && (cur6 & mask) == code {
                    found6 = Some((i, code, len, nbits6));
                    break;
                }
            }
            if found6.is_some() { break; }
        }
        eprintln!("Left channel first codeword (book 10): idx={:?}", found6);

        // Check if the right channel's spectral data position is correct.
        // The payload is 387 bytes. Let's look at the bytes around position 354.
        eprintln!("\nBytes around right spectral position (354):");
        for i in 350..370 {
            if i < payload.len() {
                eprintln!("  byte[{i}] = {:02x} ({:08b})", payload[i], payload[i]);
            }
        }

        // The right channel has 40 bands of codebook 1 (quad) + 1 band of NOISE_HCB.
        // For a quad codebook, each band needs at least 1 bit per quad (the zero codeword).
        // Let's count how many quads are in 40 bands.
        let swb_r = crate::tables::SWB_OFFSET_1024[hdr.sampling_frequency_index as usize];
        let mut total_quads = 0usize;
        for sfb in 0..40 {
            if sfb + 1 >= swb_r.len() { break; }
            let width = (swb_r[sfb + 1] - swb_r[sfb]) as usize;
            total_quads += width / 4;
        }
        eprintln!("Right channel: {total_quads} quads in 40 bands");
        eprintln!("Available bits for right spectral: {} bits ({} bytes)", payload.len() * 8 - pos_before_right_spectral, payload.len() - pos_before_right_spectral / 8);

        // Check if the right channel's spectral data might be at a different position.
        // Let's look for non-zero data in the payload after the left channel.
        eprintln!("\nSearching for non-zero spectral data after left channel:");
        let mut reader7 = crate::bitreader::BitReader::new(payload);
        reader7.seek_to_bit(pos_after_left_spectral);
        // Skip right global_gain (8 bits)
        let _rg2 = reader7.read_bits(8).unwrap();
        // The right channel's global_gain should be 108 (same as left)
        eprintln!("Right global_gain (re-read): {_rg2}");
        // Parse right sections
        for _g in 0..shared_ics2.num_window_groups() {
            let mut covered = 0usize;
            while covered < shared_ics2.max_sfb as usize {
                let _sect_cb = reader7.read_bits(4).unwrap() as u8;
                let _sect_len = reader7.read_section_length(5).unwrap() as usize;
                covered += _sect_len;
            }
        }
        // Skip right scalefactors
        for _ in 0..shared_ics2.num_window_groups() * (shared_ics2.max_sfb as usize) {
            let _sf = crate::codebooks::decode_scalefactor(&mut reader7);
        }
        // Skip right pulse/tns/gc
        let _pp = reader7.read_bit();
        let _tt = reader7.read_bit();
        let _gg = reader7.read_bit();
        let pos_right_spectral_reparsed = reader7.bit_position();
        eprintln!("Right spectral position (reparsed): {pos_right_spectral_reparsed}");
        let peek_right = reader7.peek(32).unwrap_or(0);
        eprintln!("Bits at reparsed right spectral position: {:032b}", peek_right);

        // Check the bitstream position before parsing the right channel's sections.
        let mut reader8 = crate::bitreader::BitReader::new(payload);
        reader8.seek_to_bit(pos_after_left_spectral);
        let _rg3 = reader8.read_bits(8).unwrap();
        // Parse right sections
        let mut right_sects = Vec::new();
        for _g in 0..shared_ics2.num_window_groups() {
            let mut covered = 0usize;
            while covered < shared_ics2.max_sfb as usize {
                let sect_cb = reader8.read_bits(4).unwrap() as u8;
                let sect_len = reader8.read_section_length(5).unwrap() as usize;
                right_sects.push((sect_cb, sect_len));
                covered += sect_len;
            }
        }
        eprintln!("Right sections (reparsed): {:?}", right_sects);
        eprintln!("Right sections (original): {:?}", cpe.right.sections.groups[0].iter().map(|s| (s.sect_cb, s.sect_len)).collect::<Vec<_>>());

        // Check the bitstream position after parsing the right channel's sections.
        let pos_after_right_sections = reader8.bit_position();
        eprintln!("Bitstream position after right sections: {pos_after_right_sections}");

        // Now let's check if the right channel's sections match between reparse and original.
        let orig_sects: Vec<(u8, u32)> = cpe.right.sections.groups[0].iter().map(|s| (s.sect_cb, s.sect_len)).collect();
        let reparse_sects: Vec<(u8, usize)> = right_sects;
        eprintln!("Sections match: {}", orig_sects.iter().map(|(c,l)| (*c, *l as usize)).collect::<Vec<_>>() == reparse_sects);

        // The sections don't match! This means the left channel's spectral data
        // consumed the wrong number of bits, causing the right channel to be parsed
        // from the wrong position.
        // Let's check the bitstream position after the left channel's spectral data
        // in the original parse.
        eprintln!("\nLeft spectral data bit consumption:");
        eprintln!("  Reparse: {} -> {} ({} bits)", pos_before_left_spectral, pos_after_left_spectral, pos_after_left_spectral - pos_before_left_spectral);

        // Now let's decode the right channel using the REPARSED sections (which are correct).
        let mut reader9 = crate::bitreader::BitReader::new(payload);
        reader9.seek_to_bit(pos_after_right_sections);
        // Skip right scalefactors
        for _ in 0..shared_ics2.num_window_groups() * (shared_ics2.max_sfb as usize) {
            let _sf = crate::codebooks::decode_scalefactor(&mut reader9);
        }
        // Skip right pulse/tns/gc
        let _pp2 = reader9.read_bit();
        let _tt2 = reader9.read_bit();
        let _gg2 = reader9.read_bit();
        let pos_right_spectral_correct = reader9.bit_position();
        eprintln!("Right spectral position (correct): {pos_right_spectral_correct}");
        let peek_right_correct = reader9.peek(32).unwrap_or(0);
        eprintln!("Bits at correct right spectral position: {:032b}", peek_right_correct);

        // Decode the first quad from the correct position
        let mut reader10 = crate::bitreader::BitReader::new(payload);
        reader10.seek_to_bit(pos_right_spectral_correct);
        let first_quad_correct = crate::codebooks::decode_spectral_quad(&mut reader10, 5);
        eprintln!("First quad at correct position (book 5): {:?}", first_quad_correct);

        eprintln!("\nCoefficients at error bins (with TNS vs without TNS):");
        for b in [97, 99, 101, 103, 105, 107, 109, 111] {
            eprintln!(
                "  bin {b}: with_TNS={:e} without_TNS={:e} diff={:e}",
                mid[b],
                coeffs_no_tns[b],
                mid[b] - coeffs_no_tns[b]
            );
        }
        eprintln!(
            "frame24 has_tns={} has_tnsR={}",
            cpe.left.tns.is_some(),
            cpe.right.tns.is_some()
        );

        // Print TNS filter parameters for frame 24 left channel.
        if let Some(tns) = &cpe.left.tns {
            eprintln!("frame24 TNS: n_filt={:?}", tns.n_filt);
            for (gi, gf) in tns.filters.iter().enumerate() {
                for (fi, f) in gf.iter().enumerate() {
                    eprintln!(
                        "  filter[{gi}/{fi}]: order={} length={} direction={} coef={:?}",
                        f.order,
                        f.length,
                        f.direction,
                        &f.coef[..f.order as usize]
                    );
                }
            }
        }

        eprintln!(
            "FRAME24 dominant coeff error: bin {best_b} err={:e} native={:e} ref_est={:e}",
            err[best_b],
            mid[best_b],
            mid[best_b] - err[best_b] as f32
        );
        let mut idxs: Vec<usize> = (0..1024).collect();
        idxs.sort_by(|&a, &b| err[b].abs().partial_cmp(&err[a].abs()).unwrap());
        for &b in idxs.iter().take(10) {
            if err[b].abs() > 1.0 {
                eprintln!(
                    "   bin {b}: err={:e} native={:e} ref_est={:e}",
                    err[b],
                    mid[b],
                    mid[b] - err[b] as f32
                );
            }
        }
    }

    /// Replicates ffmpeg's `imdct_and_windowing` (AAC EIGHT_SHORT path) overlap-add
    /// arithmetic exactly and asserts our `short_synthesis` matches it: the flat
    /// 448-sample left/right guard regions are copied from the carried overlap,
    /// each short window's 256-sample IMDCT tail is windowed with the previous
    /// and current short windows and summed, and the overlap is advanced for the
    /// next frame. This locks the short-block synthesis (the path the wideband
    /// `noise_mono` fixture exercises via its EIGHT_SHORT frames, while the
    /// passing 440 Hz tone uses OnlyLong) against the reference.
    #[allow(clippy::needless_range_loop)]
    #[test]
    fn short_synthesis_matches_ffmpeg_reference() {
        let windows = Windows::new();
        let prev_shape = 1usize;
        let cur_shape = 0usize;
        let mut state = ChannelState {
            overlap: [0.0f32; 1024],
            prev_shape: prev_shape as u8,
            init: true,
        };
        for i in 0..1024 {
            state.overlap[i] = (i as f32 - 512.0) * 0.001;
        }
        let mut buf = [0.0f32; 2048];
        for i in 0..2048 {
            buf[i] = ((i as f32) - 1000.0) * 0.01;
        }

        let nflat_ls = 448usize;
        let nshort = 128usize;
        let trans = 64usize;
        let nlong = 1024usize;
        let w_prev = &windows.short[prev_shape];
        let w_cur = &windows.short[cur_shape];

        let mut out_ours = [0.0f32; 1024];
        let ov = state.overlap; // snapshot BEFORE the call mutates it
        short_synthesis(&buf, &mut state, cur_shape, &windows, &mut out_ours);

        // --- ffmpeg reference replica (uses the pre-call overlap snapshot) ---
        let mut out_exp = [0.0f32; 1024];
        out_exp[..nflat_ls].copy_from_slice(&ov[..nflat_ls]);
        for i in 0..nshort {
            out_exp[nflat_ls + i] = ov[nflat_ls + i] + buf[i] * w_prev[i];
            out_exp[nflat_ls + nshort + i] = ov[nflat_ls + nshort + i]
                + buf[nshort + i] * w_cur[nshort - 1 - i]
                + buf[2 * nshort + i] * w_cur[i];
            out_exp[nflat_ls + 2 * nshort + i] = ov[nflat_ls + 2 * nshort + i]
                + buf[3 * nshort + i] * w_cur[nshort - 1 - i]
                + buf[4 * nshort + i] * w_cur[i];
            out_exp[nflat_ls + 3 * nshort + i] = ov[nflat_ls + 3 * nshort + i]
                + buf[5 * nshort + i] * w_cur[nshort - 1 - i]
                + buf[6 * nshort + i] * w_cur[i];
            if i < trans {
                out_exp[nflat_ls + 4 * nshort + i] = ov[nflat_ls + 4 * nshort + i]
                    + buf[7 * nshort + i] * w_cur[nshort - 1 - i]
                    + buf[8 * nshort + i] * w_cur[i];
            }
        }
        let mut ov_exp = [0.0f32; 1024];
        for i in 0..nshort {
            if i >= trans {
                ov_exp[nflat_ls + 4 * nshort + i - nlong] =
                    buf[7 * nshort + i] * w_cur[nshort - 1 - i] + buf[8 * nshort + i] * w_cur[i];
            }
            ov_exp[nflat_ls + 5 * nshort + i - nlong] =
                buf[9 * nshort + i] * w_cur[nshort - 1 - i] + buf[10 * nshort + i] * w_cur[i];
            ov_exp[nflat_ls + 6 * nshort + i - nlong] =
                buf[11 * nshort + i] * w_cur[nshort - 1 - i] + buf[12 * nshort + i] * w_cur[i];
            ov_exp[nflat_ls + 7 * nshort + i - nlong] =
                buf[13 * nshort + i] * w_cur[nshort - 1 - i] + buf[14 * nshort + i] * w_cur[i];
            ov_exp[nflat_ls + 8 * nshort + i - nlong] =
                buf[15 * nshort + i] * w_cur[nshort - 1 - i];
        }
        for i in 0..nflat_ls {
            ov_exp[nflat_ls + nshort + i] = 0.0;
        }

        for i in 0..1024 {
            assert!(
                (out_ours[i] - out_exp[i]).abs() < 1e-3,
                "out[{}]: ours={} exp={}",
                i,
                out_ours[i],
                out_exp[i]
            );
        }
        for i in 0..1024 {
            assert!(
                (state.overlap[i] - ov_exp[i]).abs() < 1e-3,
                "overlap[{}]: ours={} exp={}",
                i,
                state.overlap[i],
                ov_exp[i]
            );
        }
    }

    /// Replicates ffmpeg's `imdct_and_windowing` OnlyLong overlap-add and asserts
    /// our `long_synthesis` matches it.
    #[allow(clippy::needless_range_loop)]
    #[test]
    fn long_synthesis_matches_ffmpeg_reference() {
        let windows = Windows::new();
        let prev_shape = 1usize;
        let cur_shape = 0usize;
        let mut state = ChannelState {
            overlap: [0.0f32; 1024],
            prev_shape: prev_shape as u8,
            init: true,
        };
        for i in 0..1024 {
            state.overlap[i] = (i as f32 - 512.0) * 0.001;
        }
        let mut buf = [0.0f32; 2048];
        for i in 0..2048 {
            buf[i] = ((i as f32) - 1000.0) * 0.01;
        }
        let nlong = 1024usize;

        let mut out_ours = [0.0f32; 1024];
        let ov = state.overlap; // snapshot BEFORE the call mutates it
        long_synthesis(
            &buf,
            &mut state,
            WindowSequence::OnlyLong,
            cur_shape,
            &windows,
            &mut out_ours,
        );

        let w_prev_long = &windows.long[prev_shape];
        let w_cur_long = &windows.long[cur_shape];
        let mut out_exp = [0.0f32; 1024];
        for i in 0..nlong {
            out_exp[i] = ov[i] + buf[i] * w_prev_long[i];
        }
        let mut ov_exp = [0.0f32; 1024];
        for i in 0..nlong {
            ov_exp[i] = buf[nlong + i] * w_cur_long[nlong - 1 - i];
        }

        for i in 0..1024 {
            assert!(
                (out_ours[i] - out_exp[i]).abs() < 1e-3,
                "out[{}]: ours={} exp={}",
                i,
                out_ours[i],
                out_exp[i]
            );
        }
        for i in 0..1024 {
            assert!(
                (state.overlap[i] - ov_exp[i]).abs() < 1e-3,
                "overlap[{}]: ours={} exp={}",
                i,
                state.overlap[i],
                ov_exp[i]
            );
        }
    }
}
