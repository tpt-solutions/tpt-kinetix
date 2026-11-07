//! WebVTT cue payloads in the two container forms.
//!
//! - **ISO-BMFF (MP4)** samples are `vttc` cue boxes (ISO/IEC 14496-30) with
//!   optional `iden` (identifier) and `sttg` (settings) children and a `payl`
//!   (payload) child, or a `vtte` box for an empty cue.
//! - **Matroska (WebM)** blocks of `D_WEBVTT/SUBTITLES` are text: the cue
//!   identifier on the first line, the settings on the second, and the payload
//!   after that. An empty block is an empty cue.
//!
//! The demuxer turns Matroska blocks into the box form, so everything downstream
//! sees one form. The WebM writer turns it back.

const VTTC: &[u8; 4] = b"vttc";
const VTTE: &[u8; 4] = b"vtte";

fn boxed(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    out
}

/// The child boxes of `data` as (type, payload), stopping at the first
/// malformed header.
fn child_boxes(data: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at + 8 <= data.len() {
        let size =
            u32::from_be_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]) as usize;
        if size < 8 || at.checked_add(size).is_none_or(|end| end > data.len()) {
            break;
        }
        let kind = [data[at + 4], data[at + 5], data[at + 6], data[at + 7]];
        out.push((kind, &data[at + 8..at + size]));
        at += size;
    }
    out
}

/// Converts one Matroska `D_WEBVTT/SUBTITLES` block to the `vttc` (or `vtte`)
/// sample form.
pub fn cue_box_from_matroska(block: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(block);
    let mut lines = text.splitn(3, '\n');
    let (id, settings, payload) = match (lines.next(), lines.next(), lines.next()) {
        (Some(id), Some(settings), Some(payload)) => (id, settings, payload),
        // A block with no line breaks is all payload.
        _ => ("", "", text.as_ref()),
    };
    if id.is_empty() && settings.is_empty() && payload.is_empty() {
        return boxed(VTTE, &[]);
    }
    let mut children = Vec::new();
    if !id.is_empty() {
        children.extend(boxed(b"iden", id.as_bytes()));
    }
    if !settings.is_empty() {
        children.extend(boxed(b"sttg", settings.as_bytes()));
    }
    children.extend(boxed(b"payl", payload.as_bytes()));
    boxed(VTTC, &children)
}

/// The `vttc`/`vtte` form of a cue, whichever form `data` is in: a cue box is
/// returned unchanged, anything else is read as a Matroska block.
pub fn to_cue_box(data: &[u8]) -> Vec<u8> {
    match child_boxes(data).first().map(|(kind, _)| *kind) {
        Some(kind) if &kind == VTTC || &kind == VTTE => data.to_vec(),
        _ => cue_box_from_matroska(data),
    }
}

/// Converts a `vttc` (or `vtte`) sample to a Matroska block: identifier,
/// settings and payload on their own lines. An empty cue is an empty block.
/// A sample that is not a cue box is returned unchanged.
pub fn matroska_from_cue_box(sample: &[u8]) -> Vec<u8> {
    let Some((kind, body)) = child_boxes(sample).into_iter().next() else {
        return sample.to_vec();
    };
    if &kind == VTTE {
        return Vec::new();
    }
    if &kind != VTTC {
        return sample.to_vec();
    }
    let (mut id, mut settings, mut payload) = (String::new(), String::new(), String::new());
    for (child, data) in child_boxes(body) {
        let s = String::from_utf8_lossy(data);
        match &child {
            b"iden" => id = s.into_owned(),
            b"sttg" => settings = s.into_owned(),
            b"payl" => payload = s.into_owned(),
            _ => {}
        }
    }
    format!("{id}\n{settings}\n{payload}").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_only_block_becomes_a_payl_cue() {
        let sample = cue_box_from_matroska(b"\n\nHello subtitles");
        assert_eq!(sample, boxed(VTTC, &boxed(b"payl", b"Hello subtitles")));
    }

    #[test]
    fn identifier_and_settings_are_kept() {
        let sample = cue_box_from_matroska(b"c1\nalign:start\nHi");
        let expected = boxed(
            VTTC,
            &[
                boxed(b"iden", b"c1"),
                boxed(b"sttg", b"align:start"),
                boxed(b"payl", b"Hi"),
            ]
            .concat(),
        );
        assert_eq!(sample, expected);
    }

    #[test]
    fn empty_block_is_an_empty_cue_and_back() {
        assert_eq!(cue_box_from_matroska(b""), boxed(VTTE, &[]));
        assert_eq!(matroska_from_cue_box(&boxed(VTTE, &[])), Vec::<u8>::new());
    }

    #[test]
    fn block_and_sample_forms_round_trip() {
        for block in [
            b"\n\nHello subtitles".as_slice(),
            b"c1\nalign:start\nHi\nthere",
        ] {
            let sample = cue_box_from_matroska(block);
            assert_eq!(matroska_from_cue_box(&sample), block);
        }
    }

    #[test]
    fn a_non_cue_sample_passes_through() {
        assert_eq!(matroska_from_cue_box(b"plain"), b"plain");
    }
}
