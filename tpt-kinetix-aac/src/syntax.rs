//! Parse-only AAC syntax structs and raw-data-block element dispatch.
//!
//! This module parses the *structure* of an AAC raw data block (the payload that
//! follows an ADTS header or an `AudioSpecificConfig` in MP4/FLV): the
//! individual-channel-stream metadata ([`IcsInfo`]), the per-window-group
//! section maps ([`SectionData`]), and the top-level syntactic-element dispatch
//! (SCE / CPE / LFE / FIL / END).
//!
//! It is intentionally **decode-only**: scale factors and spectral coefficients
//! are not reconstructed. When a stream needs Huffman `scale_factor_data` /
//! `spectral_data` decode (i.e. any section uses a non-zero codebook, or pulse /
//! TNS / gain-control data is present) the parser returns
//! [`AacParseError::Unsupported`] rather than silently producing wrong output.
//! This keeps the surface honest and panic-free on untrusted input.

use crate::bitreader::BitReader;
use crate::dequant::{decode_spectral_data, expand_band_types};
use crate::pulse::{parse_pulse, PulseData};
use crate::scalefactors::decode_scalefactors;
use crate::tables::{SWB_OFFSET_1024, SWB_OFFSET_128, TNS_MAX_BANDS_1024, TNS_MAX_BANDS_128};
use crate::tns::{parse_tns, TnsData};

/// Errors raised while parsing AAC syntax.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AacParseError {
    /// The bitstream ended in the middle of a syntax element.
    #[error("unexpected end of AAC bitstream")]
    UnexpectedEof,
    /// `window_sequence` was not one of the four defined values.
    #[error("invalid AAC window_sequence value")]
    BadWindowSequence,
    /// A section codebook codeword could not be decoded.
    #[error("invalid section codebook codeword")]
    BadSectionCodebook,
    /// A section length ran past the scalefactor bands for its group.
    #[error("section length exceeds available scalefactor bands")]
    SectionLengthOverflow,
    /// A syntactic element id was outside 0..=7.
    #[error("invalid syntactic element id")]
    BadElementId,
    /// A feature outside Phase 1's parse-only scope was signalled.
    #[error("unsupported AAC feature: {0}")]
    Unsupported(&'static str),
}

/// The four AAC window sequences (ISO 14496-3, 4.4.1.1 / Table 4.16).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowSequence {
    /// Only long windows (the common AAC-LC case).
    OnlyLong = 0,
    /// Long window, start of a transition.
    LongStart = 1,
    /// Eight short windows.
    EightShort = 2,
    /// Long window, end of a transition.
    LongStop = 3,
}

impl WindowSequence {
    fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::OnlyLong),
            1 => Some(Self::LongStart),
            2 => Some(Self::EightShort),
            3 => Some(Self::LongStop),
            _ => None,
        }
    }

    /// Whether this sequence uses eight short windows.
    pub fn is_eight_short(self) -> bool {
        matches!(self, Self::EightShort)
    }
}

/// Individual Channel Stream information (`ics_info()`, ISO 14496-3 4.4.1.1).
///
/// Note this does **not** carry `sampling_frequency_index` — that is known from
/// the framing/ASC and is not part of the in-band `ics_info` syntax.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IcsInfo {
    /// Window sequence (2 bits).
    pub window_sequence: WindowSequence,
    /// `window_shape` flag (1 bit).
    pub window_shape: bool,
    /// `max_sfb` — number of scalefactor bands (6 bits for long, 4 for short).
    pub max_sfb: u8,
    /// `scale_factor_grouping` (7 bits), only meaningful for eight short windows.
    pub scale_factor_grouping: u8,
    /// `predictor_data_present` (1 bit, long windows only).
    pub predictor_data_present: bool,
    /// `predictor_reset` flag (1 bit) when `predictor_data_present`.
    pub predictor_reset_mode: Option<u8>,
}

impl IcsInfo {
    /// Number of window groups this configuration produces.
    ///
    /// Eight short windows are partitioned into window groups by
    /// `scale_factor_grouping` (a set bit at position `i` joins window `i+1` to
    /// window `i`'s group); every non-short sequence is a single group.
    pub fn num_window_groups(&self) -> usize {
        if !self.window_sequence.is_eight_short() {
            return 1;
        }
        let mut n = 1usize;
        for i in 0..7 {
            // The first grouping bit read (MSB of the 7-bit field) joins windows
            // 0 and 1; bit `i` joins window `i` and `i+1`.
            if (self.scale_factor_grouping >> (6 - i)) & 1 == 1 {
                // extend current group
            } else {
                n += 1;
            }
        }
        n
    }

    /// Length (number of short windows) of window group `g`.
    pub fn group_len(&self, g: usize) -> usize {
        if !self.window_sequence.is_eight_short() {
            return 1;
        }
        let groups = self.window_groups();
        groups.get(g).copied().unwrap_or(1)
    }

    /// Per-group short-window lengths (sums to 8 for `EIGHT_SHORT_SEQUENCE`).
    fn window_groups(&self) -> Vec<usize> {
        if !self.window_sequence.is_eight_short() {
            return vec![1];
        }
        let mut groups = vec![1usize];
        for i in 0..7 {
            if (self.scale_factor_grouping >> (6 - i)) & 1 == 1 {
                if let Some(last) = groups.last_mut() {
                    *last += 1;
                }
            } else {
                groups.push(1);
            }
        }
        groups
    }

    /// Parse `ics_info()` from `reader`.
    pub fn parse(reader: &mut BitReader) -> Result<Self, AacParseError> {
        // `ics_reserved_bit` (ISO 14496-3 Table 4.6): always present, always 0.
        let _ics_reserved_bit = reader.read_bit().ok_or(AacParseError::UnexpectedEof)?;

        let window_sequence =
            WindowSequence::from_u8(reader.read_bits(2).ok_or(AacParseError::UnexpectedEof)? as u8)
                .ok_or(AacParseError::BadWindowSequence)?;
        let window_shape = reader.read_bit().ok_or(AacParseError::UnexpectedEof)? != 0;

        let (max_sfb, scale_factor_grouping, predictor_present) =
            if window_sequence.is_eight_short() {
                let max_sfb = reader.read_bits(4).ok_or(AacParseError::UnexpectedEof)? as u8;
                let grouping = reader.read_bits(7).ok_or(AacParseError::UnexpectedEof)? as u8;
                (max_sfb, grouping, false)
            } else {
                let max_sfb = reader.read_bits(6).ok_or(AacParseError::UnexpectedEof)? as u8;
                let predictor_present = reader.read_bit().ok_or(AacParseError::UnexpectedEof)? != 0;
                (max_sfb, 0, predictor_present)
            };

        // `predictor_data_present` (AAC Main-profile prediction; never set by an
        // AAC-LC encoder, but must still be parsed correctly for spec/hostile-input
        // correctness): `predictor_reset` (1 bit), then `predictor_reset_group_number`
        // (5 bits) if set, then one `prediction_used[sfb]` bit per band up to
        // `min(max_sfb, PRED_SFB_MAX=40)`.
        let (predictor_data_present, predictor_reset_mode) = if predictor_present {
            let predictor_reset = reader.read_bit().ok_or(AacParseError::UnexpectedEof)? != 0;
            if predictor_reset {
                let _reset_group_number =
                    reader.read_bits(5).ok_or(AacParseError::UnexpectedEof)?;
            }
            let pred_sfb_max = (max_sfb as usize).min(40);
            for _ in 0..pred_sfb_max {
                let _prediction_used = reader.read_bit().ok_or(AacParseError::UnexpectedEof)?;
            }
            (true, Some(predictor_reset as u8))
        } else {
            (false, None)
        };

        Ok(IcsInfo {
            window_sequence,
            window_shape,
            max_sfb,
            scale_factor_grouping,
            predictor_data_present,
            predictor_reset_mode,
        })
    }
}

/// A single section: a codebook applied to a run of `sect_len` scalefactor bands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Section {
    /// Section codebook `sect_cb` (0 = ZERO_HCB, 1..=11 spectral, 13/14 intensity).
    pub sect_cb: u8,
    /// Number of scalefactor bands covered by this section.
    pub sect_len: u32,
}

/// Per-window-group section maps (`section_data()`, ISO 14496-3 4.4.3.1).
///
/// `groups[g]` holds the sections covering `ics.max_sfb` scalefactor bands in
/// group `g`. The sum of `sect_len` across a group's sections always equals
/// `max_sfb`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionData {
    /// One section list per window group.
    pub groups: Vec<Vec<Section>>,
}

/// Decode a `sect_cb` codebook index: a plain fixed-width 4-bit field (ISO
/// 14496-3 §4.4.3.1's `section_data()` pseudocode reads `sect_cb; 4`, not a
/// prefix code). Values 0..=11 are the ZERO_HCB / spectral Huffman codebooks,
/// 12 is reserved, 13/14 are the intensity-stereo codebooks, and 15 is
/// NOISE_HCB (perceptual noise substitution).
fn decode_section_cb(reader: &mut BitReader) -> Result<u8, AacParseError> {
    reader
        .read_bits(4)
        .map(|v| v as u8)
        .ok_or(AacParseError::UnexpectedEof)
}

impl SectionData {
    /// Number of bits used to code each `sect_len` increment (ISO 14496-3
    /// §4.4.3.1). This is a fixed width keyed only on the window sequence —
    /// 5 bits for long windows, 3 for eight-short — never on `max_sfb` (matching
    /// e.g. ffmpeg's `decode_band_types`: `bits = num_windows == 8 ? 3 : 5`).
    fn section_len_bits(ics: &IcsInfo) -> u32 {
        if ics.window_sequence.is_eight_short() {
            3
        } else {
            5
        }
    }

    /// Parse `section_data()` for the given [`IcsInfo`].
    pub fn parse(reader: &mut BitReader, ics: &IcsInfo) -> Result<Self, AacParseError> {
        let num_groups = ics.num_window_groups();
        let max_sfb = ics.max_sfb as usize;
        let sect_len_bits = Self::section_len_bits(ics);
        let mut groups = Vec::with_capacity(num_groups);
        for _g in 0..num_groups {
            let mut sections = Vec::new();
            let mut covered = 0usize;
            // Bound the number of sections so a hostile or desynced stream cannot
            // drive an infinite loop (e.g. a run of zero-length sections). A
            // valid stream needs at most `max_sfb` non-zero sections; the small
            // fixed slack also accommodates the occasional zero-length section
            // some encoders (and the ffmpeg reference decoder) emit.
            let mut iterations = 0usize;
            let iteration_cap = max_sfb + 64;
            while covered < max_sfb {
                iterations += 1;
                if iterations > iteration_cap {
                    return Err(AacParseError::SectionLengthOverflow);
                }
                let sect_cb = decode_section_cb(reader)?;
                let sect_len = reader
                    .read_section_length(sect_len_bits)
                    .ok_or(AacParseError::UnexpectedEof)? as usize;
                // A zero-length section is legal (the ffmpeg reference decoder
                // accepts it); it covers no scalefactor bands. More generally a
                // section may extend past `max_sfb` — the ffmpeg decoder reads the
                // full section structure and simply ignores the excess bands, so
                // we record the true length and let the loop end once `covered`
                // reaches/passes `max_sfb`. Downstream band loops use the section
                // structure as the source of truth and skip out-of-range bands.
                sections.push(Section {
                    sect_cb,
                    sect_len: sect_len as u32,
                });
                covered += sect_len;
            }
            groups.push(sections);
        }
        Ok(SectionData { groups })
    }
}

/// A single channel's fully decoded stream: global gain, ICS, sections, the
/// dequantized frequency-ordered spectrum, and the parsed pulse / TNS side data.
#[derive(Debug, Clone)]
pub struct ChannelStream {
    /// `global_gain` (8 bits).
    pub global_gain: u8,
    /// Per-channel `ics_info()`.
    pub ics: IcsInfo,
    /// Per-group section maps.
    pub sections: SectionData,
    /// Dequantized, frequency-ordered spectral coefficients (1024 lines).
    pub coeffs: [f32; 1024],
    /// Band (section) codebook per `(group * max_sfb + sfb)`.
    pub band_type: Vec<u8>,
    /// Scalefactor / intensity position / noise energy per `(group * max_sfb + sfb)`.
    pub scalefactor: Vec<i32>,
    /// Parsed TNS data, if present.
    pub tns: Option<TnsData>,
    /// Parsed pulse data, if present.
    pub pulse: Option<PulseData>,
}

impl ChannelStream {
    /// Parse an `individual_channel_stream()`, optionally reusing a shared
    /// `ics_info()` (the CPE `common_window` case).
    ///
    /// `sf_index` is the 4-bit sampling-frequency index used to select the
    /// scalefactor-band offset tables.
    fn parse(
        reader: &mut BitReader,
        shared_ics: Option<&IcsInfo>,
        sf_index: usize,
    ) -> Result<Self, AacParseError> {
        let global_gain = reader.read_bits(8).ok_or(AacParseError::UnexpectedEof)? as u8;
        let ics = match shared_ics {
            Some(shared) => *shared,
            None => IcsInfo::parse(reader)?,
        };
        let sections = SectionData::parse(reader, &ics)?;

        let band_type = expand_band_types(&sections, &ics);
        let scalefactor = decode_scalefactors(reader, &ics, &sections, &band_type)?;

        let pulse = reader.read_bit().ok_or(AacParseError::UnexpectedEof)? != 0;
        let pulse = if pulse {
            Some(parse_pulse(reader)?)
        } else {
            None
        };

        let tns = reader.read_bit().ok_or(AacParseError::UnexpectedEof)? != 0;
        let tns = if tns {
            let tns_max = if ics.window_sequence.is_eight_short() {
                TNS_MAX_BANDS_128[sf_index]
            } else {
                TNS_MAX_BANDS_1024[sf_index]
            };
            Some(parse_tns(reader, &ics, tns_max)?)
        } else {
            None
        };

        // `gain_control_data_present` (ISO 14496-3 Table 4.50) is read
        // unconditionally in `individual_channel_stream()` — it is not gated on
        // profile/object type. It is only ever set by an SSR (Scalable Sample
        // Rate) encoder; no AAC-LC encoder sets it, but the flag bit itself is
        // always present and must be consumed to stay in sync.
        let gain_control_data_present = reader.read_bit().ok_or(AacParseError::UnexpectedEof)? != 0;
        if gain_control_data_present {
            // `gain_control_data()` (SSR-only) is not implemented; real AAC-LC
            // streams never set this flag.
            return Err(AacParseError::Unsupported("gain_control_data (SSR)"));
        }

        let swb_long = SWB_OFFSET_1024[sf_index];
        let swb_short = SWB_OFFSET_128[sf_index];
        let coeffs = decode_spectral_data(
            reader,
            &ics,
            &sections,
            &band_type,
            &scalefactor,
            swb_long,
            swb_short,
            global_gain,
        )?;

        Ok(ChannelStream {
            global_gain,
            ics,
            sections,
            coeffs,
            band_type,
            scalefactor,
            tns,
            pulse,
        })
    }
}

/// Single Channel Element (id 0).
#[derive(Debug, Clone)]
pub struct SingleChannelElement {
    /// `element_instance_tag` (4 bits).
    pub instance_tag: u8,
    /// The decoded channel stream.
    pub stream: ChannelStream,
}

/// Channel Pair Element (id 1).
#[derive(Debug, Clone)]
pub struct ChannelPairElement {
    /// `element_instance_tag` (4 bits).
    pub instance_tag: u8,
    /// `common_window` flag (1 bit).
    pub common_window: bool,
    /// Shared `ics_info()` when `common_window` is set.
    pub ics: Option<IcsInfo>,
    /// `ms_mask_present` (2 bits).
    pub ms_mask_present: u8,
    /// Per-(group, sfb) mid/side mask bits (only when `ms_mask_present == 1`).
    pub ms_mask: Vec<bool>,
    /// Left channel stream.
    pub left: ChannelStream,
    /// Right channel stream.
    pub right: ChannelStream,
}

/// Low Frequency Element (id 3) — structurally an SCE with TNS-only constraint.
#[derive(Debug, Clone)]
pub struct LfeElement {
    /// `element_instance_tag` (4 bits).
    pub instance_tag: u8,
    /// The decoded channel stream.
    pub stream: ChannelStream,
}

/// Fill Element (id 6).
#[derive(Debug, Clone)]
pub struct FillElement {
    /// `element_instance_tag` (4 bits).
    pub instance_tag: u8,
    /// Raw fill payload bytes.
    pub payload: Vec<u8>,
}

/// Coupling Channel Element (id 2, ISO 14496-3 §4.4.4.3).
#[derive(Debug, Clone)]
pub struct CouplingChannelElement {
    /// `element_instance_tag` (4 bits).
    pub instance_tag: u8,
    /// `common_window` flag (1 bit).
    pub common_window: bool,
    /// Shared `ics_info()` when `common_window` is set.
    pub ics: Option<IcsInfo>,
    /// Number of gain element lists.
    pub num_gain_element_lists: u8,
    /// Gain element lists for each target channel.
    pub gain_element_lists: Vec<GainElementList>,
}

/// Gain element list for CCE coupling.
#[derive(Debug, Clone)]
pub struct GainElementList {
    /// `gain_element_scale` (1 bit).
    pub gain_element_scale: bool,
    /// Number of gain elements.
    pub num_gain_elements: u8,
    /// The gain elements.
    pub gain_elements: Vec<GainElement>,
}

/// Individual gain element for CCE coupling.
#[derive(Debug, Clone)]
pub struct GainElement {
    /// `cce_gain` (3 bits).
    pub cce_gain: u8,
    /// `cce_scale` (4 bits).
    pub cce_scale: u8,
    /// Target channel tag.
    pub target_tag: u8,
    /// `gain_element_index` (4 bits) - index into gain_element_lists.
    pub gain_element_index: u8,
}

/// A parsed AAC syntactic element.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum Element {
    /// Single Channel Element.
    Sce(SingleChannelElement),
    /// Channel Pair Element.
    Cpe(ChannelPairElement),
    /// Coupling Channel Element.
    Cce(CouplingChannelElement),
    /// Low Frequency Element.
    Lfe(LfeElement),
    /// Fill Element.
    Fil(FillElement),
    /// End Element (id 7) — terminates the raw data block.
    End,
}

/// A parsed AAC raw data block: an ordered list of syntactic elements.
#[derive(Debug, Clone)]
pub struct RawDataBlock {
    /// Elements in stream order, ending at `Element::End` (if present).
    pub elements: Vec<Element>,
}

/// Skip a `data_stream_element()` (DSE, id 4) in place.
///
/// Reads the instance tag and the (possibly escaped) byte count, then discards
/// the payload bytes. DSEs carry ancillary data (e.g. metadata) and carry no
/// channels, so the decoder simply ignores them.
fn skip_data_stream_element(reader: &mut BitReader) -> Result<(), AacParseError> {
    // ffmpeg's `skip_data_stream_element`: byte_align_flag(1) then count(8, with
    // a 255 escape to 16 bits), then the payload bytes, then `byte_alignment()`
    // only when the flag was set. The 4-bit `element_instance_tag` preceding
    // this was already consumed by the dispatch in `RawDataBlock::parse`.
    let byte_align_flag = reader.read_bit().ok_or(AacParseError::UnexpectedEof)? != 0;
    let mut count = reader.read_bits(8).ok_or(AacParseError::UnexpectedEof)? as usize;
    if count == 255 {
        count += reader.read_bits(8).ok_or(AacParseError::UnexpectedEof)? as usize;
    }
    for _ in 0..count {
        reader.read_u8().ok_or(AacParseError::UnexpectedEof)?;
    }
    if byte_align_flag {
        reader.byte_align();
    }
    Ok(())
}

/// Skip a `program_config_element()` (PCE, id 5) in place.
///
/// Reads every field of the (fixed-shape, count-driven) PCE so the exact bit
/// length is consumed, then byte-aligns. PCEs describe the channel topology and
/// are redundant with the ADTS header's `channel_config` for AAC-LC, so the
/// decoder ignores them.
fn skip_program_config_element(reader: &mut BitReader) -> Result<(), AacParseError> {
    let _object_type = reader.read_bits(2).ok_or(AacParseError::UnexpectedEof)?;
    let _sf = reader.read_bits(4).ok_or(AacParseError::UnexpectedEof)?;
    let num_front = reader.read_bits(4).ok_or(AacParseError::UnexpectedEof)? as usize;
    let num_side = reader.read_bits(4).ok_or(AacParseError::UnexpectedEof)? as usize;
    let num_back = reader.read_bits(4).ok_or(AacParseError::UnexpectedEof)? as usize;
    let num_lfe = reader.read_bits(2).ok_or(AacParseError::UnexpectedEof)? as usize;
    let num_assoc = reader.read_bits(3).ok_or(AacParseError::UnexpectedEof)? as usize;
    let num_cc = reader.read_bits(4).ok_or(AacParseError::UnexpectedEof)? as usize;

    let mono_present = reader.read_bit().ok_or(AacParseError::UnexpectedEof)?;
    if mono_present != 0 {
        let _ = reader.read_bits(4).ok_or(AacParseError::UnexpectedEof)?;
    }
    let stereo_present = reader.read_bit().ok_or(AacParseError::UnexpectedEof)?;
    if stereo_present != 0 {
        let _ = reader.read_bits(4).ok_or(AacParseError::UnexpectedEof)?;
    }
    let matrix_present = reader.read_bit().ok_or(AacParseError::UnexpectedEof)?;
    if matrix_present != 0 {
        let _ = reader.read_bits(2).ok_or(AacParseError::UnexpectedEof)?;
        let _ = reader.read_bit().ok_or(AacParseError::UnexpectedEof)?;
    }
    for _ in 0..num_front {
        let _ = reader.read_bit().ok_or(AacParseError::UnexpectedEof)?;
        let _ = reader.read_bits(4).ok_or(AacParseError::UnexpectedEof)?;
    }
    for _ in 0..num_side {
        let _ = reader.read_bit().ok_or(AacParseError::UnexpectedEof)?;
        let _ = reader.read_bits(4).ok_or(AacParseError::UnexpectedEof)?;
    }
    for _ in 0..num_back {
        let _ = reader.read_bit().ok_or(AacParseError::UnexpectedEof)?;
        let _ = reader.read_bits(4).ok_or(AacParseError::UnexpectedEof)?;
    }
    for _ in 0..num_lfe {
        let _ = reader.read_bits(4).ok_or(AacParseError::UnexpectedEof)?;
    }
    for _ in 0..num_assoc {
        let _ = reader.read_bits(4).ok_or(AacParseError::UnexpectedEof)?;
    }
    for _ in 0..num_cc {
        let _ = reader.read_bit().ok_or(AacParseError::UnexpectedEof)?;
        let _ = reader.read_bits(4).ok_or(AacParseError::UnexpectedEof)?;
    }
    let comment_bytes = reader.read_bits(8).ok_or(AacParseError::UnexpectedEof)? as usize;
    for _ in 0..comment_bytes {
        let _ = reader.read_u8().ok_or(AacParseError::UnexpectedEof)?;
    }
    reader.byte_align();
    Ok(())
}

impl RawDataBlock {
    /// Parse a raw data block from `data`, dispatching on each syntactic element
    /// id until `END` or the bitstream is exhausted. `sf_index` is the 4-bit
    /// sampling-frequency index (used to select scalefactor-band tables).
    ///
    /// Returns [`AacParseError::Unsupported`] for a reserved `sf_index`: the
    /// field is 4 bits (0..=15) but only 0..=11 name a real sample rate, and
    /// every scalefactor-band / TNS table is sized 12. Validating once here
    /// keeps all the downstream `TABLE[sf_index]` lookups infallible.
    pub fn parse(data: &[u8], sf_index: usize) -> Result<Self, AacParseError> {
        if sf_index >= SWB_OFFSET_1024.len() {
            return Err(AacParseError::Unsupported(
                "reserved sampling_frequency_index",
            ));
        }
        let mut reader = BitReader::new(data);
        let mut elements = Vec::new();
        let mut _element_count = 0;
        #[allow(clippy::while_let_loop)]
        loop {
            let id = match reader.read_bits(3) {
                Some(id) => id as u8,
                None => break,
            };
            _element_count += 1;
            match id {
                0 => {
                    let tag = reader.read_bits(4).ok_or(AacParseError::UnexpectedEof)? as u8;
                    let stream = ChannelStream::parse(&mut reader, None, sf_index)?;
                    elements.push(Element::Sce(SingleChannelElement {
                        instance_tag: tag,
                        stream,
                    }));
                }
                1 => {
                    let tag = reader.read_bits(4).ok_or(AacParseError::UnexpectedEof)? as u8;
                    let common_window = reader.read_bit().ok_or(AacParseError::UnexpectedEof)? != 0;
                    let mut ms_mask_present = 0u8;
                    let mut ms_mask = Vec::new();
                    let shared = if common_window {
                        let ics = IcsInfo::parse(&mut reader)?;
                        ms_mask_present =
                            reader.read_bits(2).ok_or(AacParseError::UnexpectedEof)? as u8;
                        if ms_mask_present == 1 {
                            let bits = ics.num_window_groups() * (ics.max_sfb as usize);
                            for _ in 0..bits {
                                ms_mask.push(
                                    reader.read_bit().ok_or(AacParseError::UnexpectedEof)? != 0,
                                );
                            }
                        }
                        Some(ics)
                    } else {
                        None
                    };
                    let left = ChannelStream::parse(&mut reader, shared.as_ref(), sf_index)?;
                    let right = ChannelStream::parse(&mut reader, shared.as_ref(), sf_index)?;
                    elements.push(Element::Cpe(ChannelPairElement {
                        instance_tag: tag,
                        common_window,
                        ics: shared,
                        ms_mask_present,
                        ms_mask,
                        left,
                        right,
                    }));
                }
                3 => {
                    let tag = reader.read_bits(4).ok_or(AacParseError::UnexpectedEof)? as u8;
                    let stream = ChannelStream::parse(&mut reader, None, sf_index)?;
                    elements.push(Element::Lfe(LfeElement {
                        instance_tag: tag,
                        stream,
                    }));
                }
                6 => {
                    // Fill element (TYPE_FIL). ffmpeg's `put_bitstream_info` does
                    // *not* follow the ISO `fill_element()` `element_instance_tag(4)`
                    // layout: it writes `TYPE_FIL(3) + len(4) + [escape(8)] +
                    // ext_type(4)`, where the 4-bit field is the fill payload
                    // *byte length* (with `15` as an escape sentinel) and the
                    // decoder skips `8*len - 4` bits of payload. A spec-literal
                    // parse (instance_tag then count) desyncs here because the
                    // version-string fill that ffmpeg emits in frame 1 lands a
                    // whole fill payload early and corrupts every following
                    // element. Mirror ffmpeg's decoder (`decode_extension_payload`
                    // EXT_FILL → `decode_fill(gb, 8*cnt - 4)`) so the opaque fill
                    // is skipped exactly.
                    let mut fill_len =
                        reader.read_bits(4).ok_or(AacParseError::UnexpectedEof)? as usize;
                    if fill_len == 15 {
                        // ffmpeg's escape arithmetic is `cnt += ret - 1` with
                        // `cnt` (and `ret`) as signed ints: a base count of 15
                        // plus (esc - 1). Mirror that exactly (== 14 + esc) with
                        // saturating math so a corrupt stream can never overflow
                        // the later `fill_len * 8`.
                        let esc = reader.read_bits(8).ok_or(AacParseError::UnexpectedEof)? as usize;
                        fill_len = 14usize.saturating_add(esc);
                    }
                    // extension type (4 bits); AAC-LC only carries EXT_FILL here.
                    reader.read_bits(4).ok_or(AacParseError::UnexpectedEof)?;
                    let fill_bits = fill_len.saturating_mul(8).saturating_sub(4);
                    reader.skip(fill_bits as u32);
                    elements.push(Element::Fil(FillElement {
                        instance_tag: 0,
                        payload: Vec::new(),
                    }));
                }
                7 => {
                    elements.push(Element::End);
                    break;
                }
                2 => {
                    let tag = reader.read_bits(4).ok_or(AacParseError::UnexpectedEof)? as u8;
                    let common_window = reader.read_bit().ok_or(AacParseError::UnexpectedEof)? != 0;
                    let shared = if common_window {
                        Some(IcsInfo::parse(&mut reader)?)
                    } else {
                        None
                    };
                    let num_gain_element_lists =
                        reader.read_bits(4).ok_or(AacParseError::UnexpectedEof)? as u8;
                    let mut gain_element_lists =
                        Vec::with_capacity((num_gain_element_lists + 1) as usize);
                    let mut cce_truncated = false;
                    for _list_idx in 0..=num_gain_element_lists {
                        let gain_element_scale =
                            reader.read_bit().ok_or(AacParseError::UnexpectedEof)? != 0;
                        let num_gain_elements =
                            reader.read_bits(4).ok_or(AacParseError::UnexpectedEof)? as u8;
                        let mut gain_elements =
                            Vec::with_capacity((num_gain_elements + 1) as usize);
                        for _ge_idx in 0..=num_gain_elements {
                            let cce_gain = match reader.read_bits(3) {
                                Some(v) => v as u8,
                                None => {
                                    cce_truncated = true;
                                    break;
                                }
                            };
                            let cce_scale = match reader.read_bits(4) {
                                Some(v) => v as u8,
                                None => {
                                    cce_truncated = true;
                                    break;
                                }
                            };
                            let target_tag = match reader.read_bits(4) {
                                Some(v) => v as u8,
                                None => {
                                    cce_truncated = true;
                                    break;
                                }
                            };
                            let gain_element_index = match reader.read_bits(4) {
                                Some(v) => v as u8,
                                None => {
                                    cce_truncated = true;
                                    break;
                                }
                            };
                            gain_elements.push(GainElement {
                                cce_gain,
                                cce_scale,
                                target_tag,
                                gain_element_index,
                            });
                        }
                        gain_element_lists.push(GainElementList {
                            gain_element_scale,
                            num_gain_elements,
                            gain_elements,
                        });
                        if cce_truncated {
                            break;
                        }
                    }
                    reader.byte_align();
                    elements.push(Element::Cce(CouplingChannelElement {
                        instance_tag: tag,
                        common_window,
                        ics: shared,
                        num_gain_element_lists,
                        gain_element_lists,
                    }));
                }
                4 | 5 => {
                    let _tag = reader.read_bits(4).ok_or(AacParseError::UnexpectedEof)? as u8;
                    if id == 4 {
                        skip_data_stream_element(&mut reader)?;
                    } else if id == 5 {
                        skip_program_config_element(&mut reader)?;
                    }
                }
                _ => return Err(AacParseError::BadElementId),
            }
        }
        Ok(RawDataBlock { elements })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pack a slice of 0/1 bits (MSB-first) into bytes, for hand-built fixtures.
    fn bits_to_bytes(bits: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut cur = 0u8;
        let mut n = 0u32;
        for &b in bits {
            cur = (cur << 1) | (b & 1);
            n += 1;
            if n == 8 {
                out.push(cur);
                cur = 0;
                n = 0;
            }
        }
        if n > 0 {
            out.push(cur << (8 - n));
        }
        out
    }

    const ZERO: u8 = 0;
    const ONE: u8 = 1;

    fn b(values: &[u8]) -> Vec<u8> {
        values.to_vec()
    }

    #[test]
    fn parse_ics_only_long_window() {
        // ics_reserved_bit=0, window_sequence=OnlyLong(00), window_shape=1,
        // max_sfb=10(001010), predictor_data_present=0.
        let bits = b(&[
            ZERO, ZERO, ZERO, ONE, ZERO, ZERO, ONE, ZERO, ONE, ZERO, ZERO,
        ]);
        let bytes = bits_to_bytes(&bits);
        let mut r = BitReader::new(&bytes);
        let ics = IcsInfo::parse(&mut r).unwrap();
        assert_eq!(ics.window_sequence, WindowSequence::OnlyLong);
        assert!(ics.window_shape);
        assert_eq!(ics.max_sfb, 10);
        assert!(!ics.predictor_data_present);
        assert_eq!(ics.num_window_groups(), 1);
    }

    #[test]
    fn parse_section_data_all_zero() {
        // ics: reserved=0, OnlyLong, window_shape=1, max_sfb=10, pred=0.
        // section (one group): sect_cb=0 (4 bits), sect_len=10 (5 bits: "01010").
        let mut bits = b(&[
            ZERO, ZERO, ZERO, ONE, ZERO, ZERO, ONE, ZERO, ONE, ZERO, ZERO,
        ]);
        bits.extend_from_slice(&[ZERO, ZERO, ZERO, ZERO]); // sect_cb = 0 (4 bits)
        bits.extend_from_slice(&[ZERO, ONE, ZERO, ONE, ZERO]); // sect_len = 10 (5 bits, long window)
        let bytes = bits_to_bytes(&bits);
        let mut r = BitReader::new(&bytes);
        let ics = IcsInfo::parse(&mut r).unwrap();
        let sections = SectionData::parse(&mut r, &ics).unwrap();
        assert_eq!(sections.groups.len(), 1);
        assert_eq!(sections.groups[0].len(), 1);
        assert_eq!(sections.groups[0][0].sect_cb, 0);
        assert_eq!(sections.groups[0][0].sect_len, 10);
    }

    #[test]
    fn parse_full_sce_block() {
        // Raw data block: SCE then END, with an all-zero-section channel stream.
        let mut bits: Vec<u8> = Vec::new();
        bits.extend_from_slice(&[ZERO, ZERO, ZERO]); // id_syn_ele = SCE (0)
        bits.extend_from_slice(&[ZERO, ZERO, ZERO, ZERO]); // instance_tag = 0
        bits.extend_from_slice(&[ZERO; 8]); // global_gain = 0
        bits.push(ZERO); // ics_reserved_bit = 0
        bits.extend_from_slice(&[ZERO, ZERO]); // window_sequence = OnlyLong
        bits.push(ONE); // window_shape = 1
        bits.extend_from_slice(&[ZERO, ZERO, ONE, ZERO, ONE, ZERO]); // max_sfb = 10
        bits.push(ZERO); // predictor_data_present = 0
        bits.extend_from_slice(&[ZERO, ZERO, ZERO, ZERO]); // sect_cb = 0 (4 bits)
        bits.extend_from_slice(&[ZERO, ONE, ZERO, ONE, ZERO]); // sect_len = 10 (5 bits, long window)
        bits.extend_from_slice(&[ZERO, ZERO, ZERO]); // pulse/tns/gain_control_data_present = 0
        bits.extend_from_slice(&[ONE, ONE, ONE]); // id_syn_ele = END (7)
        let bytes = bits_to_bytes(&bits);
        let block = RawDataBlock::parse(&bytes, 4).unwrap();
        assert_eq!(block.elements.len(), 2);
        match &block.elements[0] {
            Element::Sce(sce) => {
                assert_eq!(sce.instance_tag, 0);
                assert_eq!(sce.stream.global_gain, 0);
                assert_eq!(sce.stream.ics.max_sfb, 10);
                assert_eq!(sce.stream.sections.groups[0][0].sect_len, 10);
            }
            other => panic!("expected SCE, got {other:?}"),
        }
        assert!(matches!(block.elements[1], Element::End));
    }

    #[test]
    fn parse_full_cpe_block() {
        // CPE with common_window=1, shared ICS, no ms mask, two all-zero channels.
        let mut bits: Vec<u8> = Vec::new();
        bits.extend_from_slice(&[ZERO, ZERO, ONE]); // id_syn_ele = CPE (1)
        bits.extend_from_slice(&[ZERO, ZERO, ZERO, ZERO]); // instance_tag = 0
        bits.push(ONE); // common_window = 1
        bits.push(ZERO); // ics_reserved_bit = 0
        bits.extend_from_slice(&[ZERO, ZERO]); // shared window_sequence = OnlyLong
        bits.push(ONE); // shared window_shape = 1
        bits.extend_from_slice(&[ZERO, ZERO, ONE, ZERO, ZERO, ZERO]); // shared max_sfb = 8
        bits.push(ZERO); // shared predictor_data_present = 0
        bits.extend_from_slice(&[ZERO, ZERO]); // ms_mask_present = 0
                                               // left channel: global_gain=0 (8 bits), sect_cb=0 (4 bits), sect_len=8 (5 bits: "01000"), flags=000
        bits.extend_from_slice(&[ZERO; 8]);
        bits.extend_from_slice(&[ZERO, ZERO, ZERO, ZERO]);
        bits.extend_from_slice(&[ZERO, ONE, ZERO, ZERO, ZERO]);
        bits.extend_from_slice(&[ZERO, ZERO, ZERO]); // left pulse/tns/gain_control_data_present = 0
                                                     // right channel: global_gain=0xff (8 bits), sect_cb=0 (4 bits), sect_len=8, flags=00
        bits.extend_from_slice(&[ONE; 8]);
        bits.extend_from_slice(&[ZERO, ZERO, ZERO, ZERO]);
        bits.extend_from_slice(&[ZERO, ONE, ZERO, ZERO, ZERO]);
        bits.extend_from_slice(&[ZERO, ZERO, ZERO]); // right pulse/tns/gain_control_data_present = 0
        bits.extend_from_slice(&[ONE, ONE, ONE]); // END
        let bytes = bits_to_bytes(&bits);
        let block = RawDataBlock::parse(&bytes, 4).unwrap();
        match &block.elements[0] {
            Element::Cpe(cpe) => {
                assert!(cpe.common_window);
                assert_eq!(cpe.ics.unwrap().max_sfb, 8);
                assert_eq!(cpe.ms_mask_present, 0);
                assert_eq!(cpe.left.global_gain, 0);
                assert_eq!(cpe.right.global_gain, 0xFF);
                assert_eq!(cpe.left.sections.groups[0][0].sect_len, 8);
            }
            other => panic!("expected CPE, got {other:?}"),
        }
    }

    #[test]
    fn parse_fill_element_with_payload() {
        // ffmpeg-style fill (the layout the parser follows to avoid desync):
        // TYPE_FIL(3) + len(4) + ext_type(4) + (len*8 - 4) fill bits, then END.
        // len = 3 => 20 fill bits (ext_type already consumed, 16 data bits + 4).
        let mut bits: Vec<u8> = Vec::new();
        bits.extend_from_slice(&[ONE, ONE, ZERO]); // id_syn_ele = FIL (6)
        bits.extend_from_slice(&[ZERO, ZERO, ONE, ONE]); // len = 3
        bits.extend_from_slice(&[ZERO, ZERO, ZERO, ZERO]); // ext_type = 0
        bits.extend_from_slice(&[ONE, ZERO, ONE, ZERO, ONE, ZERO, ONE, ONE]); // 0xAB
        bits.extend_from_slice(&[ONE, ONE, ZERO, ZERO, ONE, ONE, ZERO, ONE]); // 0xCD
        bits.extend_from_slice(&[ZERO, ZERO, ZERO, ZERO]); // 4 trailing fill bits
        bits.extend_from_slice(&[ONE, ONE, ONE]); // END
        let bytes = bits_to_bytes(&bits);
        let block = RawDataBlock::parse(&bytes, 4).unwrap();
        assert!(matches!(block.elements.first(), Some(Element::Fil(_))));
        assert!(matches!(block.elements.last(), Some(Element::End)));
    }

    #[test]
    fn nonzero_section_now_decodes_after_scalefactors() {
        // SCE whose only section uses sect_cb=1 (4-bit field). The structural
        // parser now proceeds past sections; with no spectral data present it
        // errors out (truncated), which is the honest non-panic behaviour.
        let mut bits: Vec<u8> = Vec::new();
        bits.extend_from_slice(&[ZERO, ZERO, ZERO]); // SCE
        bits.extend_from_slice(&[ZERO, ZERO, ZERO, ZERO]); // instance_tag
        bits.extend_from_slice(&[ZERO; 8]); // global_gain = 0
        bits.push(ZERO); // ics_reserved_bit = 0
        bits.extend_from_slice(&[ZERO, ZERO]); // OnlyLong
        bits.push(ONE); // window_shape
        bits.extend_from_slice(&[ZERO, ZERO, ONE, ZERO, ONE, ZERO]); // max_sfb=10
        bits.push(ZERO); // pred=0
        bits.extend_from_slice(&[ZERO, ZERO, ZERO, ONE]); // sect_cb = 1 (4 bits)
        bits.extend_from_slice(&[ZERO, ONE, ZERO, ONE, ZERO]); // sect_len=10 (5 bits, long window)
        bits.extend_from_slice(&[ZERO, ZERO, ZERO]); // pulse/tns/gain_control_data_present = 0
        let bytes = bits_to_bytes(&bits);
        // No spectral data present -> an error (EOF), not a panic.
        assert!(RawDataBlock::parse(&bytes, 4).is_err());
    }

    #[test]
    fn truncated_input_errors_not_panics() {
        // Just an SCE id and instance tag, then EOF.
        let bytes = bits_to_bytes(&[ZERO, ZERO, ZERO, ZERO, ZERO, ZERO, ZERO]);
        assert!(matches!(
            RawDataBlock::parse(&bytes, 4),
            Err(AacParseError::UnexpectedEof)
        ));
    }

    #[test]
    fn skips_data_stream_element_before_end() {
        // id=4 (DSE): tag=0, count=0 (no payload) -> 12 bits, byte-aligned to 16.
        // Then END. The DSE must be consumed without desyncing the END marker.
        let mut bits: Vec<u8> = Vec::new();
        bits.extend_from_slice(&[ONE, ZERO, ZERO]); // id_syn_ele = DSE (4)
        bits.extend_from_slice(&[ZERO; 4]); // instance_tag = 0
        bits.extend_from_slice(&[ZERO; 8]); // count = 0
        bits.push(ZERO); // byte-align padding (skipped by the parser)
        bits.extend_from_slice(&[ONE, ONE, ONE]); // id_syn_ele = END (7)
        let bytes = bits_to_bytes(&bits);
        let block = RawDataBlock::parse(&bytes, 4).unwrap();
        assert!(matches!(block.elements.first(), Some(Element::End)));
    }

    #[test]
    fn skips_data_stream_element_with_payload() {
        // id=4 (DSE): tag=0, count=2, two payload bytes, then byte-aligned END.
        let mut bits: Vec<u8> = Vec::new();
        bits.extend_from_slice(&[ONE, ZERO, ZERO]); // id_syn_ele = DSE (4)
        bits.extend_from_slice(&[ZERO; 4]); // instance_tag = 0
        bits.push(ZERO); // data_stream_byte_align_flag = 0
        bits.extend_from_slice(&[ZERO, ZERO, ZERO, ZERO, ZERO, ZERO, ONE, ZERO]); // count = 2
        bits.extend_from_slice(&[ONE, ZERO, ONE, ZERO, ZERO, ONE, ZERO, ONE]); // payload byte 0xA5
        bits.extend_from_slice(&[ZERO, ZERO, ONE, ONE, ONE, ONE, ZERO, ZERO]); // payload byte 0x3C
        bits.extend_from_slice(&[ONE, ONE, ONE]); // id_syn_ele = END (7)
        let bytes = bits_to_bytes(&bits);
        let block = RawDataBlock::parse(&bytes, 4).unwrap();
        assert!(matches!(block.elements.first(), Some(Element::End)));
    }

    #[test]
    fn skips_program_config_element_before_end() {
        // id=5 (PCE) with every channel count zero and an empty comment field:
        // 46 bits of fixed structure, byte-aligned to 48, then END.
        let mut bits: Vec<u8> = Vec::new();
        bits.extend_from_slice(&[ONE, ZERO, ONE]); // id_syn_ele = PCE (5)
        bits.extend_from_slice(&[ZERO; 4]); // instance_tag = 0
        bits.extend_from_slice(&[ZERO; 2]); // object_type
        bits.extend_from_slice(&[ZERO; 4]); // sampling_frequency_index
        bits.extend_from_slice(&[ZERO; 4]); // num_front
        bits.extend_from_slice(&[ZERO; 4]); // num_side
        bits.extend_from_slice(&[ZERO; 4]); // num_back
        bits.extend_from_slice(&[ZERO; 2]); // num_lfe
        bits.extend_from_slice(&[ZERO; 3]); // num_assoc
        bits.extend_from_slice(&[ZERO; 4]); // num_valid_cc
        bits.push(ZERO); // mono_mixdown_present
        bits.push(ZERO); // stereo_mixdown_present
        bits.push(ZERO); // matrix_mixdown_idx_present
        bits.extend_from_slice(&[ZERO; 8]); // comment_field_bytes = 0
        bits.extend_from_slice(&[ZERO; 3]); // byte-align padding (skipped)
        bits.extend_from_slice(&[ONE, ONE, ONE]); // id_syn_ele = END (7)
        let bytes = bits_to_bytes(&bits);
        let block = RawDataBlock::parse(&bytes, 4).unwrap();
        assert!(matches!(block.elements.first(), Some(Element::End)));
    }
}
