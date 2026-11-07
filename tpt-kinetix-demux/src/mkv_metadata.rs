//! The tags and chapters of a Matroska file: the `Tags` and `Chapters` elements at
//! the top of the Segment. They are read by walking the Segment's children by their
//! headers, skipping the clusters, so a large file costs a few small reads.
//!
//! Only the text of the simple tags and the chapter titles and start times is kept.
//! Targets are not checked: a tag or chapter is taken from the file's first edition,
//! and tags apply to the whole file, which is how the writers in this repository and
//! ffmpeg use them.

use crate::source::ReadAt;
use tpt_kinetix_core::error::KinetixError;

const ID_EBML: u32 = 0x1A45_DFA3;
const ID_SEGMENT: u32 = 0x1853_8067;
const ID_INFO: u32 = 0x1549_A966;
const ID_TITLE: u32 = 0x7BA9;
const ID_TAGS: u32 = 0x1254_C367;
const ID_TAG: u32 = 0x7373;
const ID_SIMPLE_TAG: u32 = 0x67C8;
const ID_TAG_NAME: u32 = 0x45A3;
const ID_TAG_STRING: u32 = 0x4487;
const ID_CHAPTERS: u32 = 0x1043_A770;
const ID_EDITION_ENTRY: u32 = 0x45B9;
const ID_CHAPTER_ATOM: u32 = 0xB6;
const ID_CHAPTER_TIME_START: u32 = 0x91;
const ID_CHAPTER_DISPLAY: u32 = 0x80;
const ID_CHAP_STRING: u32 = 0x85;

/// Largest `Tags` or `Chapters` element read; metadata beyond this is skipped.
const MAX_METADATA_BYTES: u64 = 4 << 20;

/// The file-level metadata of a Matroska file.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MkvMetadata {
    /// `(tag name, value)` pairs, in file order (`TITLE`, `ARTIST`, ...).
    pub tags: Vec<(String, String)>,
    /// `(start in nanoseconds, title)` of each chapter of the first edition.
    pub chapters: Vec<(u64, String)>,
}

/// A parsed element header: its id, its size (`None` when unknown), and the length of
/// the header itself.
struct Header {
    id: u32,
    size: Option<u64>,
    len: usize,
}

/// Parses an element header at the start of `buf`.
fn header(buf: &[u8]) -> Option<Header> {
    let first = *buf.first()?;
    let id_len = first.leading_zeros() as usize + 1;
    if id_len > 4 || buf.len() < id_len {
        return None;
    }
    let id = buf[..id_len]
        .iter()
        .fold(0u32, |acc, b| (acc << 8) | u32::from(*b));
    let size_first = *buf.get(id_len)?;
    let size_len = size_first.leading_zeros() as usize + 1;
    if size_len > 8 || buf.len() < id_len + size_len {
        return None;
    }
    let mut value = u64::from(size_first) & ((1u64 << (8 - size_len)) - 1);
    for b in &buf[id_len + 1..id_len + size_len] {
        value = (value << 8) | u64::from(*b);
    }
    // All value bits set means "unknown size".
    let all_ones = value == (1u64 << (7 * size_len)) - 1;
    Some(Header {
        id,
        size: (!all_ones).then_some(value),
        len: id_len + size_len,
    })
}

/// The `(id, body)` of each complete element in `buf`.
fn children(mut buf: &[u8]) -> Vec<(u32, &[u8])> {
    let mut out = Vec::new();
    while let Some(h) = header(buf) {
        let Some(size) = h.size else { break };
        let Ok(size) = usize::try_from(size) else {
            break;
        };
        let Some(end) = h.len.checked_add(size).filter(|e| *e <= buf.len()) else {
            break;
        };
        out.push((h.id, &buf[h.len..end]));
        buf = &buf[end..];
    }
    out
}

/// A big-endian unsigned integer element body.
fn uint(body: &[u8]) -> u64 {
    body.iter()
        .take(8)
        .fold(0u64, |acc, b| (acc << 8) | u64::from(*b))
}

/// A string element body, without its trailing NUL padding.
fn text(body: &[u8]) -> String {
    String::from_utf8_lossy(body)
        .trim_end_matches('\0')
        .to_string()
}

/// The simple tags of one `SimpleTag`, nested ones included, as name and value pairs.
fn simple_tags(body: &[u8], out: &mut Vec<(String, String)>) {
    let mut name = None;
    let mut value = None;
    for (id, b) in children(body) {
        match id {
            ID_TAG_NAME => name = Some(text(b)),
            ID_TAG_STRING => value = Some(text(b)),
            ID_SIMPLE_TAG => simple_tags(b, out),
            _ => {}
        }
    }
    if let (Some(name), Some(value)) = (name, value) {
        if !name.is_empty() && !value.is_empty() {
            out.push((name, value));
        }
    }
}

/// Reads the tags of a `Tags` element body.
fn parse_tags(body: &[u8], out: &mut Vec<(String, String)>) {
    for (id, tag) in children(body) {
        if id != ID_TAG {
            continue;
        }
        for (id, simple) in children(tag) {
            if id == ID_SIMPLE_TAG {
                simple_tags(simple, out);
            }
        }
    }
}

/// Reads the first edition of a `Chapters` element body.
fn parse_chapters(body: &[u8], out: &mut Vec<(u64, String)>) {
    let Some((_, edition)) = children(body)
        .into_iter()
        .find(|(id, _)| *id == ID_EDITION_ENTRY)
    else {
        return;
    };
    for (id, atom) in children(edition) {
        if id != ID_CHAPTER_ATOM {
            continue;
        }
        let mut start = None;
        let mut title = None;
        for (id, b) in children(atom) {
            match id {
                ID_CHAPTER_TIME_START => start = Some(uint(b)),
                ID_CHAPTER_DISPLAY => {
                    if let Some((_, s)) =
                        children(b).into_iter().find(|(i, _)| *i == ID_CHAP_STRING)
                    {
                        title = Some(text(s));
                    }
                }
                _ => {}
            }
        }
        if let (Some(start), Some(title)) = (start, title) {
            out.push((start, title));
        }
    }
}

/// The header of the element at `pos`, read from `source`; `None` at the end.
fn header_at<S: ReadAt + ?Sized>(
    source: &S,
    pos: u64,
    len: u64,
) -> Result<Option<Header>, KinetixError> {
    if pos + 2 > len {
        return Ok(None);
    }
    let want = usize::try_from((len - pos).min(24)).unwrap_or(24);
    let mut buf = vec![0u8; want];
    source.read_at(pos, &mut buf)?;
    Ok(header(&buf))
}

/// Reads the tags and chapters of the Matroska file behind `source`.
///
/// A file that is not Matroska is an error; a Matroska file with no tags or chapters
/// gives empty lists.
pub fn read_metadata<S: ReadAt + ?Sized>(source: &S) -> Result<MkvMetadata, KinetixError> {
    let len = source.len()?;
    let mut meta = MkvMetadata::default();
    let Some(ebml) = header_at(source, 0, len)? else {
        return Err(KinetixError::Parse("not a Matroska file".into()));
    };
    if ebml.id != ID_EBML {
        return Err(KinetixError::Parse("not a Matroska file".into()));
    }
    let mut pos = ebml.len as u64 + ebml.size.unwrap_or(0);
    let Some(segment) = header_at(source, pos, len)? else {
        return Ok(meta);
    };
    if segment.id != ID_SEGMENT {
        return Err(KinetixError::Parse(
            "no Segment after the EBML header".into(),
        ));
    }
    let Some(seg_size) = segment.size else {
        return Ok(meta);
    };
    let end = (pos + segment.len as u64 + seg_size).min(len);
    pos += segment.len as u64;
    while pos < end {
        let Some(h) = header_at(source, pos, end)? else {
            break;
        };
        let Some(size) = h.size else {
            break; // an unknown-size element: nothing after it can be found
        };
        let body_start = pos + h.len as u64;
        if h.id == ID_INFO && size <= MAX_METADATA_BYTES {
            // The file's title is in `Info`, not in a tag. It is reported as the
            // TITLE tag, unless the file has one of its own.
            let mut body = vec![0u8; size as usize];
            source.read_at(body_start, &mut body)?;
            if let Some((_, title)) = children(&body).into_iter().find(|(id, _)| *id == ID_TITLE) {
                let title = text(title);
                let has_title = meta
                    .tags
                    .iter()
                    .any(|(n, _)| n.eq_ignore_ascii_case("TITLE"));
                if !title.is_empty() && !has_title {
                    meta.tags.insert(0, ("TITLE".to_string(), title));
                }
            }
        }
        if (h.id == ID_TAGS || h.id == ID_CHAPTERS) && size <= MAX_METADATA_BYTES {
            let mut body = vec![0u8; size as usize];
            source.read_at(body_start, &mut body)?;
            if h.id == ID_TAGS {
                parse_tags(&body, &mut meta.tags);
            } else {
                parse_chapters(&body, &mut meta.chapters);
            }
        }
        pos = body_start.saturating_add(size);
    }
    Ok(meta)
}

#[cfg(test)]
mod tests {
    use super::{header, read_metadata};

    /// An element with a one-byte size field (bodies under 127 bytes).
    fn element(id: &[u8], body: &[u8]) -> Vec<u8> {
        let mut v = id.to_vec();
        v.push(0x80 | body.len() as u8);
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn element_headers_parse_one_to_four_byte_ids_and_unknown_sizes() {
        let h = header(&[0x1A, 0x45, 0xDF, 0xA3, 0x84]).unwrap();
        assert_eq!((h.id, h.size, h.len), (0x1A45_DFA3, Some(4), 5));
        let unknown = header(&[0xA3, 0xFF]).unwrap();
        assert_eq!((unknown.id, unknown.size), (0xA3, None));
    }

    #[test]
    fn reads_tags_and_chapters_from_a_minimal_file() {
        let name = element(&[0x45, 0xA3], b"TITLE");
        let value = element(&[0x44, 0x87], b"Kinetix");
        let simple = element(&[0x67, 0xC8], &[name, value].concat());
        let tag = element(&[0x73, 0x73], &simple);
        let tags = element(&[0x12, 0x54, 0xC3, 0x67], &tag);

        let start = element(&[0x91], &[0x00, 0x00, 0x00, 0x00, 0x00, 0x4C, 0x4B, 0x40]);
        let chap = element(&[0x85], b"Intro");
        let display = element(&[0x80], &chap);
        let atom = element(&[0xB6], &[start, display].concat());
        let edition = element(&[0x45, 0xB9], &atom);
        let chapters = element(&[0x10, 0x43, 0xA7, 0x70], &edition);

        let segment_body = [tags, chapters].concat();
        let mut file = element(&[0x1A, 0x45, 0xDF, 0xA3], &[]);
        let segment = element(&[0x18, 0x53, 0x80, 0x67], &segment_body);
        file.extend_from_slice(&segment);

        let meta = read_metadata(&file).unwrap();
        assert_eq!(
            meta.tags,
            vec![("TITLE".to_string(), "Kinetix".to_string())]
        );
        assert_eq!(meta.chapters, vec![(5_000_000, "Intro".to_string())]);
    }

    #[test]
    fn a_file_that_is_not_matroska_is_an_error() {
        assert!(read_metadata(&b"RIFF....".to_vec()).is_err());
    }
}
