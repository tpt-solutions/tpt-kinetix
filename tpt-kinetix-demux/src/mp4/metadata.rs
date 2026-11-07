//! Text metadata from a movie's `udta` box (the iTunes-style `meta`/`ilst` items:
//! title, artist, album, and so on), as name and value pairs. Other items, cover
//! art included, are left alone; the raw `udta` is what a copy carries over.

/// The payload of a `udta` box, given either whole (header included, as the demuxer
/// keeps it) or as its payload.
fn udta_payload(udta: &[u8]) -> &[u8] {
    match udta.get(4..8) {
        Some(b"udta") => &udta[8..],
        _ => udta,
    }
}

/// The Matroska tag name for each supported `ilst` item, by its four-byte type
/// (`0xA9` is the © of the iTunes keys).
fn tag_name(kind: &[u8]) -> Option<&'static str> {
    match kind {
        [0xA9, b'n', b'a', b'm'] => Some("TITLE"),
        [0xA9, b'A', b'R', b'T'] => Some("ARTIST"),
        [0xA9, b'a', b'l', b'b'] => Some("ALBUM"),
        [0xA9, b'd', b'a', b'y'] => Some("DATE_RELEASED"),
        [0xA9, b'c', b'm', b't'] => Some("COMMENT"),
        [0xA9, b'g', b'e', b'n'] => Some("GENRE"),
        _ => None,
    }
}

/// The `(kind, payload)` of each complete box in `data`, stopping at the first
/// one that does not fit.
fn boxes(data: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
    let mut rest = data;
    std::iter::from_fn(move || {
        if rest.len() < 8 {
            return None;
        }
        let size = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
        if size < 8 || size > rest.len() {
            return None;
        }
        let kind = [rest[4], rest[5], rest[6], rest[7]];
        let payload = &rest[8..size];
        rest = &rest[size..];
        Some((kind, payload))
    })
}

/// The payload of the first child box of type `kind`.
fn child<'a>(data: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(data).find(|(k, _)| k == kind).map(|(_, p)| p)
}

/// The chapters of a `udta` box (the QuickTime `chpl` list) as `(start in
/// nanoseconds, title)`, in file order. Empty when there is no list or it is cut short.
pub fn chapters(udta: &[u8]) -> Vec<(u64, String)> {
    let mut out = Vec::new();
    let udta = udta_payload(udta);
    let Some(chpl) = child(udta, b"chpl") else {
        return out;
    };
    // Version 1 has four reserved bytes after the version and flags.
    let (version, mut p) = match chpl.first() {
        Some(v) => (*v, if chpl.len() >= 8 { 8 } else { chpl.len() }),
        None => return out,
    };
    if version == 0 {
        p = 4;
    }
    let Some(&count) = chpl.get(p) else {
        return out;
    };
    p += 1;
    for _ in 0..count {
        // Start time: 8 bytes in units of 100 ns; then a length byte and the title.
        let (Some(start), Some(&len)) = (chpl.get(p..p + 8), chpl.get(p + 8)) else {
            break;
        };
        let start = u64::from_be_bytes(start.try_into().unwrap_or([0; 8])).saturating_mul(100);
        let Some(title) = chpl.get(p + 9..p + 9 + usize::from(len)) else {
            break;
        };
        out.push((start, String::from_utf8_lossy(title).to_string()));
        p += 9 + usize::from(len);
    }
    out
}

/// The text items of a `udta` box as `(Matroska tag name, value)` pairs, in file
/// order. Empty values are dropped. `None` when there is no `meta`/`ilst`.
pub fn text_tags(udta: &[u8]) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    let udta = udta_payload(udta);
    let Some(meta) = child(udta, b"meta") else {
        return out;
    };
    // iTunes writes `meta` as a full box: a version and flags come before its
    // children. QuickTime's `meta` has its children first. The first child's type is
    // at byte 4 in the QuickTime layout and at byte 8 in the iTunes one.
    let children = if meta.len() >= 8 && matches!(&meta[4..8], b"hdlr" | b"ilst" | b"keys") {
        meta
    } else {
        meta.get(4..).unwrap_or(&[])
    };
    let Some(ilst) = child(children, b"ilst") else {
        return out;
    };
    for (kind, item) in boxes(ilst) {
        let Some(name) = tag_name(&kind) else {
            continue;
        };
        let Some(data) = child(item, b"data") else {
            continue;
        };
        // The `data` box holds a type indicator (1 = UTF-8 text), a locale, then the value.
        if data.len() < 8 || u32::from_be_bytes([data[0], data[1], data[2], data[3]]) != 1 {
            continue;
        }
        let value = String::from_utf8_lossy(&data[8..])
            .trim_end_matches('\0')
            .to_string();
        if !value.is_empty() {
            out.push((name, value));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{chapters, text_tags};

    fn boxed(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut v = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(payload);
        v
    }

    fn text_item(kind: &[u8; 4], value: &str) -> Vec<u8> {
        let mut data = 1u32.to_be_bytes().to_vec(); // UTF-8
        data.extend_from_slice(&[0; 4]); // locale
        data.extend_from_slice(value.as_bytes());
        boxed(kind, &boxed(b"data", &data))
    }

    #[test]
    fn reads_itunes_style_text_items_in_both_meta_layouts() {
        let ilst = [
            text_item(b"\xA9nam", "Kinetix"),
            text_item(b"\xA9ART", "TPT"),
            text_item(b"\xA9cmt", ""),
        ]
        .concat();
        let ilst = boxed(b"ilst", &ilst);
        let hdlr = boxed(b"hdlr", &[0; 24]);

        // iTunes: a full box, with version and flags before the children.
        let mut full = vec![0u8; 4];
        full.extend_from_slice(&hdlr);
        full.extend_from_slice(&ilst);
        let udta = boxed(b"udta", &boxed(b"meta", &full));
        let want = vec![
            ("TITLE", "Kinetix".to_string()),
            ("ARTIST", "TPT".to_string()),
        ];
        assert_eq!(text_tags(&udta), want);

        // QuickTime: no version and flags.
        let mut plain = hdlr.clone();
        plain.extend_from_slice(&ilst);
        let udta = boxed(b"udta", &boxed(b"meta", &plain));
        assert_eq!(text_tags(&udta), want);
    }

    #[test]
    fn a_movie_without_metadata_has_no_tags() {
        assert!(text_tags(&[]).is_empty());
        assert!(text_tags(&boxed(b"udta", &boxed(b"free", &[0; 4]))).is_empty());
    }

    #[test]
    fn reads_the_chpl_chapter_list() {
        // Version 1: version and flags, four reserved bytes, a count, then each
        // chapter's start (in 100 ns units), a title length and the title.
        let mut chpl = vec![1u8, 0, 0, 0, 0, 0, 0, 0, 2];
        chpl.extend_from_slice(&0u64.to_be_bytes());
        chpl.push(5);
        chpl.extend_from_slice(b"Intro");
        chpl.extend_from_slice(&5_000_000u64.to_be_bytes());
        chpl.push(9);
        chpl.extend_from_slice(b"Main part");
        let udta = boxed(b"udta", &boxed(b"chpl", &chpl));
        assert_eq!(
            chapters(&udta),
            vec![
                (0, "Intro".to_string()),
                (500_000_000, "Main part".to_string())
            ]
        );
        assert!(chapters(&boxed(b"udta", &[])).is_empty());
    }
}
