//! `faststart` must never panic on hostile input: a file with corrupt box sizes,
//! truncated chunk-offset tables or random bytes returns `Err` (or `Ok` with
//! consistent output), never a panic or an out-of-bounds slice.

use std::io::Cursor;

use proptest::prelude::*;
use tpt_kinetix_mux::faststart;

fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    v.extend_from_slice(kind);
    v.extend_from_slice(body);
    v
}

/// `ftyp`, `mdat`, `moov` with one `stco` entry pointing into the media. The
/// `moov` sits behind the `mdat`, so `faststart` has to move it.
fn fixture() -> Vec<u8> {
    let ftyp = boxed(b"ftyp", b"isom\0\0\0\0isom");
    let mdat = boxed(b"mdat", &[0xAA; 64]);
    let mdat_payload = (ftyp.len() + 8) as u32;
    let mut stco_body = vec![0, 0, 0, 0, 0, 0, 0, 1]; // version/flags, entry count
    stco_body.extend_from_slice(&mdat_payload.to_be_bytes());
    let stco = boxed(b"stco", &stco_body);
    let moov = boxed(
        b"moov",
        &boxed(
            b"trak",
            &boxed(b"mdia", &boxed(b"minf", &boxed(b"stbl", &stco))),
        ),
    );
    [ftyp, mdat, moov].concat()
}

#[test]
fn fixture_is_moved_to_faststart() {
    let file = fixture();
    let mut out = Vec::new();
    let report = faststart(&mut Cursor::new(&file), &mut out).unwrap();
    assert!(!report.already_faststart);
    assert_eq!(report.bytes_written as usize, out.len());
    assert_eq!(out.len(), file.len());
    let moov = out.windows(4).position(|w| w == b"moov").unwrap();
    let mdat = out.windows(4).position(|w| w == b"mdat").unwrap();
    assert!(moov < mdat, "moov must precede mdat after faststart");
}

#[test]
fn chunk_offset_table_claiming_too_many_entries_is_an_error() {
    // A `stco` whose entry count (u32::MAX) cannot fit in its box.
    let mut file = fixture();
    let stco = file.windows(4).position(|w| w == b"stco").unwrap();
    let count_at = stco + 4 + 4; // type, then version/flags
    file[count_at..count_at + 4].copy_from_slice(&u32::MAX.to_be_bytes());
    let mut out = Vec::new();
    assert!(faststart(&mut Cursor::new(&file), &mut out).is_err());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn faststart_never_panics_on_mutated_or_truncated_files(
        edits in proptest::collection::vec((any::<usize>(), any::<u8>()), 0..8),
        cut in proptest::option::of(any::<usize>()),
    ) {
        let mut file = fixture();
        for (at, value) in edits {
            let n = file.len();
            file[at % n] = value;
        }
        if let Some(cut) = cut {
            file.truncate(cut % (file.len() + 1));
        }
        let mut out = Vec::new();
        if let Ok(report) = faststart(&mut Cursor::new(&file), &mut out) {
            prop_assert_eq!(report.bytes_written as usize, out.len());
        }
    }

    #[test]
    fn faststart_never_panics_on_random_bytes(
        bytes in proptest::collection::vec(any::<u8>(), 0..512),
    ) {
        let mut out = Vec::new();
        let _ = faststart(&mut Cursor::new(&bytes), &mut out);
    }
}
