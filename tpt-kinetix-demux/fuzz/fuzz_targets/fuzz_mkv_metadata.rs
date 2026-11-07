#![no_main]

//! Fuzz target for the metadata parsers: the Matroska tags, title and chapters
//! (`mkv_metadata`), and the MP4 `udta` text items and chapter list. Malformed input
//! must give empty results or an error, never a panic.

use libfuzzer_sys::fuzz_target;
use tpt_kinetix_demux::{mkv_metadata, mp4::metadata};

fuzz_target!(|data: &[u8]| {
    let _ = mkv_metadata::read_metadata(&data.to_vec());
    let _ = metadata::text_tags(data);
    let _ = metadata::chapters(data);
});
