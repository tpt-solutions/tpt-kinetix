//! ISO-BMFF box parsers using `nom`.
//!
//! Each parser takes a slice of the *payload* bytes (after the header has been
//! stripped) unless noted otherwise.

use nom::{
    bytes::complete::take,
    multi::count,
    number::complete::{be_u32, be_u64, be_u8},
    IResult,
};

// ---------------------------------------------------------------------------
// Box header
// ---------------------------------------------------------------------------

/// Parsed ISO-BMFF box header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoxHeader {
    /// Total box size in bytes (including header).  When the on-wire size
    /// field is `1` the actual size is read from the following 8-byte
    /// `largesize` field.
    pub size: u64,
    /// Four-character box type code.
    pub box_type: [u8; 4],
}

/// Parses a box header from `input`.
///
/// Returns `(remaining_input, header)`.  The `remaining_input` points at the
/// first byte *after* the header (i.e. the first byte of the box payload).
///
/// # Examples
///
/// ```
/// use kinetix_demux::mp4::boxes::parse_box_header;
///
/// // A minimal 8-byte ftyp header with size=16
/// let bytes: &[u8] = &[0,0,0,16, b'f',b't',b'y',b'p',  b'i',b's',b'o',b'm', 0,0,0,0];
/// let (rest, hdr) = parse_box_header(bytes).unwrap();
/// assert_eq!(hdr.box_type, *b"ftyp");
/// assert_eq!(hdr.size, 16);
/// assert_eq!(rest.len(), 8); // 16 total - 8 header = 8 remaining
/// ```
pub fn parse_box_header(input: &[u8]) -> IResult<&[u8], BoxHeader> {
    let (input, size32) = be_u32(input)?;
    let (input, type_bytes) = take(4usize)(input)?;
    let mut box_type = [0u8; 4];
    box_type.copy_from_slice(type_bytes);

    let (input, size) = if size32 == 1 {
        let (input, large) = be_u64(input)?;
        (input, large)
    } else {
        (input, size32 as u64)
    };

    Ok((input, BoxHeader { size, box_type }))
}

// ---------------------------------------------------------------------------
// ftyp
// ---------------------------------------------------------------------------

/// `ftyp` (File Type) box.
#[derive(Debug, Clone)]
pub struct FtypBox {
    pub major_brand: [u8; 4],
    pub minor_version: u32,
    pub compatible_brands: Vec<[u8; 4]>,
}

/// Parses the *payload* of an `ftyp` box (header already consumed).
pub fn parse_ftyp(input: &[u8]) -> IResult<&[u8], FtypBox> {
    let (input, brand_bytes) = take(4usize)(input)?;
    let mut major_brand = [0u8; 4];
    major_brand.copy_from_slice(brand_bytes);

    let (input, minor_version) = be_u32(input)?;

    // Remaining bytes are compatible brands, 4 bytes each.
    let brand_count = input.len() / 4;
    let mut compatible_brands = Vec::with_capacity(brand_count);
    let mut remaining = input;
    for _ in 0..brand_count {
        let (rest, b) = take(4usize)(remaining)?;
        let mut brand = [0u8; 4];
        brand.copy_from_slice(b);
        compatible_brands.push(brand);
        remaining = rest;
    }

    Ok((
        remaining,
        FtypBox {
            major_brand,
            minor_version,
            compatible_brands,
        },
    ))
}

// ---------------------------------------------------------------------------
// mvhd
// ---------------------------------------------------------------------------

/// `mvhd` (Movie Header) box.
#[derive(Debug, Clone)]
pub struct MvhdBox {
    pub version: u8,
    pub timescale: u32,
    pub duration: u64,
}

/// Parses the payload of an `mvhd` box.
pub fn parse_mvhd(input: &[u8]) -> IResult<&[u8], MvhdBox> {
    let (input, version) = be_u8(input)?;
    // Skip flags (3 bytes)
    let (input, _flags) = take(3usize)(input)?;

    let (input, timescale, duration) = if version == 1 {
        // v1: creation_time u64, modification_time u64, timescale u32, duration u64
        let (i, _ctime) = be_u64(input)?;
        let (i, _mtime) = be_u64(i)?;
        let (i, ts) = be_u32(i)?;
        let (i, dur) = be_u64(i)?;
        (i, ts, dur)
    } else {
        // v0: creation_time u32, modification_time u32, timescale u32, duration u32
        let (i, _ctime) = be_u32(input)?;
        let (i, _mtime) = be_u32(i)?;
        let (i, ts) = be_u32(i)?;
        let (i, dur) = be_u32(i)?;
        (i, ts, dur as u64)
    };

    Ok((
        input,
        MvhdBox {
            version,
            timescale,
            duration,
        },
    ))
}

// ---------------------------------------------------------------------------
// tkhd
// ---------------------------------------------------------------------------

/// `tkhd` (Track Header) box.
#[derive(Debug, Clone)]
pub struct TkhdBox {
    pub version: u8,
    pub track_id: u32,
    pub duration: u64,
    pub width: u32,
    pub height: u32,
}

/// Parses the payload of a `tkhd` box.
pub fn parse_tkhd(input: &[u8]) -> IResult<&[u8], TkhdBox> {
    let (input, version) = be_u8(input)?;
    let (input, _flags) = take(3usize)(input)?;

    let (input, track_id, duration) = if version == 1 {
        // creation_time u64, modification_time u64, track_id u32, reserved u32, duration u64
        let (i, _ctime) = be_u64(input)?;
        let (i, _mtime) = be_u64(i)?;
        let (i, tid) = be_u32(i)?;
        let (i, _reserved) = be_u32(i)?;
        let (i, dur) = be_u64(i)?;
        (i, tid, dur)
    } else {
        // creation_time u32, modification_time u32, track_id u32, reserved u32, duration u32
        let (i, _ctime) = be_u32(input)?;
        let (i, _mtime) = be_u32(i)?;
        let (i, tid) = be_u32(i)?;
        let (i, _reserved) = be_u32(i)?;
        let (i, dur) = be_u32(i)?;
        (i, tid, dur as u64)
    };

    // reserved[2] u32, layer i16, alternate_group i16, volume i16, reserved i16, matrix[9] i32
    // That's 8 + 2 + 2 + 2 + 2 + 36 = 52 bytes
    let (input, _skip) = take(52usize)(input)?;

    // width and height are 16.16 fixed-point; we take the integer part (top 16 bits)
    let (input, width_fp) = be_u32(input)?;
    let (input, height_fp) = be_u32(input)?;

    Ok((
        input,
        TkhdBox {
            version,
            track_id,
            duration,
            width: width_fp >> 16,
            height: height_fp >> 16,
        },
    ))
}

// ---------------------------------------------------------------------------
// mdhd
// ---------------------------------------------------------------------------

/// `mdhd` (Media Header) box.
#[derive(Debug, Clone)]
pub struct MdhdBox {
    pub version: u8,
    pub timescale: u32,
    pub duration: u64,
}

/// Parses the payload of an `mdhd` box.
pub fn parse_mdhd(input: &[u8]) -> IResult<&[u8], MdhdBox> {
    let (input, version) = be_u8(input)?;
    let (input, _flags) = take(3usize)(input)?;

    let (input, timescale, duration) = if version == 1 {
        let (i, _ctime) = be_u64(input)?;
        let (i, _mtime) = be_u64(i)?;
        let (i, ts) = be_u32(i)?;
        let (i, dur) = be_u64(i)?;
        (i, ts, dur)
    } else {
        let (i, _ctime) = be_u32(input)?;
        let (i, _mtime) = be_u32(i)?;
        let (i, ts) = be_u32(i)?;
        let (i, dur) = be_u32(i)?;
        (i, ts, dur as u64)
    };

    Ok((
        input,
        MdhdBox {
            version,
            timescale,
            duration,
        },
    ))
}

// ---------------------------------------------------------------------------
// hdlr
// ---------------------------------------------------------------------------

/// `hdlr` (Handler Reference) box.
#[derive(Debug, Clone)]
pub struct HdlrBox {
    pub handler_type: [u8; 4],
    pub name: String,
}

/// Parses the payload of an `hdlr` box.
pub fn parse_hdlr(input: &[u8]) -> IResult<&[u8], HdlrBox> {
    let (input, _version) = be_u8(input)?;
    let (input, _flags) = take(3usize)(input)?;
    // pre_defined u32
    let (input, _pre) = be_u32(input)?;
    let (input, ht_bytes) = take(4usize)(input)?;
    let mut handler_type = [0u8; 4];
    handler_type.copy_from_slice(ht_bytes);
    // reserved[3] u32
    let (input, _reserved) = take(12usize)(input)?;
    // name: null-terminated UTF-8 string (may be empty)
    let name = String::from_utf8_lossy(input)
        .trim_end_matches('\0')
        .to_string();
    Ok((&[], HdlrBox { handler_type, name }))
}

// ---------------------------------------------------------------------------
// stts
// ---------------------------------------------------------------------------

/// One entry in the `stts` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SttsEntry {
    pub sample_count: u32,
    pub sample_delta: u32,
}

/// `stts` (Time-to-Sample) box.
#[derive(Debug, Clone)]
pub struct SttsBox {
    pub entries: Vec<SttsEntry>,
}

/// Parses the payload of an `stts` box.
pub fn parse_stts(input: &[u8]) -> IResult<&[u8], SttsBox> {
    let (input, _version) = be_u8(input)?;
    let (input, _flags) = take(3usize)(input)?;
    let (input, entry_count) = be_u32(input)?;
    let (input, raw) = count(
        |i| {
            let (i, sc) = be_u32(i)?;
            let (i, sd) = be_u32(i)?;
            Ok((
                i,
                SttsEntry {
                    sample_count: sc,
                    sample_delta: sd,
                },
            ))
        },
        entry_count as usize,
    )(input)?;
    Ok((input, SttsBox { entries: raw }))
}

// ---------------------------------------------------------------------------
// stss
// ---------------------------------------------------------------------------

/// `stss` (Sync Sample) box.
#[derive(Debug, Clone)]
pub struct StssBox {
    pub sample_numbers: Vec<u32>,
}

/// Parses the payload of an `stss` box.
pub fn parse_stss(input: &[u8]) -> IResult<&[u8], StssBox> {
    let (input, _version) = be_u8(input)?;
    let (input, _flags) = take(3usize)(input)?;
    let (input, entry_count) = be_u32(input)?;
    let (input, sample_numbers) = count(be_u32, entry_count as usize)(input)?;
    Ok((input, StssBox { sample_numbers }))
}

// ---------------------------------------------------------------------------
// stsz
// ---------------------------------------------------------------------------

/// `stsz` (Sample Size) box.
#[derive(Debug, Clone)]
pub struct StszBox {
    /// If non-zero, every sample has this fixed size and `sample_sizes` is empty.
    pub default_size: u32,
    pub sample_sizes: Vec<u32>,
}

/// Parses the payload of an `stsz` box.
pub fn parse_stsz(input: &[u8]) -> IResult<&[u8], StszBox> {
    let (input, _version) = be_u8(input)?;
    let (input, _flags) = take(3usize)(input)?;
    let (input, default_size) = be_u32(input)?;
    let (input, sample_count) = be_u32(input)?;
    let (input, sample_sizes) = if default_size == 0 {
        count(be_u32, sample_count as usize)(input)?
    } else {
        (input, Vec::new())
    };
    Ok((
        input,
        StszBox {
            default_size,
            sample_sizes,
        },
    ))
}

// ---------------------------------------------------------------------------
// stco
// ---------------------------------------------------------------------------

/// `stco` (Chunk Offset, 32-bit) box.
#[derive(Debug, Clone)]
pub struct StcoBox {
    pub offsets: Vec<u32>,
}

/// Parses the payload of an `stco` box.
pub fn parse_stco(input: &[u8]) -> IResult<&[u8], StcoBox> {
    let (input, _version) = be_u8(input)?;
    let (input, _flags) = take(3usize)(input)?;
    let (input, entry_count) = be_u32(input)?;
    let (input, offsets) = count(be_u32, entry_count as usize)(input)?;
    Ok((input, StcoBox { offsets }))
}

// ---------------------------------------------------------------------------
// co64
// ---------------------------------------------------------------------------

/// `co64` (Chunk Offset, 64-bit) box.
#[derive(Debug, Clone)]
pub struct Co64Box {
    pub offsets: Vec<u64>,
}

/// Parses the payload of a `co64` box.
pub fn parse_co64(input: &[u8]) -> IResult<&[u8], Co64Box> {
    let (input, _version) = be_u8(input)?;
    let (input, _flags) = take(3usize)(input)?;
    let (input, entry_count) = be_u32(input)?;
    let (input, offsets) = count(be_u64, entry_count as usize)(input)?;
    Ok((input, Co64Box { offsets }))
}

// ---------------------------------------------------------------------------
// stsc
// ---------------------------------------------------------------------------

/// One entry in the `stsc` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StscEntry {
    pub first_chunk: u32,
    pub samples_per_chunk: u32,
    pub sample_description_index: u32,
}

/// `stsc` (Sample-to-Chunk) box.
#[derive(Debug, Clone)]
pub struct StscBox {
    pub entries: Vec<StscEntry>,
}

/// Parses the payload of an `stsc` box.
pub fn parse_stsc(input: &[u8]) -> IResult<&[u8], StscBox> {
    let (input, _version) = be_u8(input)?;
    let (input, _flags) = take(3usize)(input)?;
    let (input, entry_count) = be_u32(input)?;
    let (input, entries) = count(
        |i| {
            let (i, fc) = be_u32(i)?;
            let (i, spc) = be_u32(i)?;
            let (i, sdi) = be_u32(i)?;
            Ok((
                i,
                StscEntry {
                    first_chunk: fc,
                    samples_per_chunk: spc,
                    sample_description_index: sdi,
                },
            ))
        },
        entry_count as usize,
    )(input)?;
    Ok((input, StscBox { entries }))
}
