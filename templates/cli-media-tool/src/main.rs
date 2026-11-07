//! A small media inspection tool built on `tpt-kinetix-demux`.
//!
//! Usage: `cargo run -- <file.webm|file.mkv|file.mp4|file.ivf>`
//!
//! It prints one line per track: index, media type, codec and size. Extend
//! `probe` with more fields, or add subcommands (remux, package) from the
//! `tpt-kinetix` CLI's source as a starting point.

use std::fs::File;
use std::io::Read;
use std::process::ExitCode;

use tpt_kinetix_core::stream::StreamInfo;
use tpt_kinetix_demux::{IvfDemuxer, MkvReader, Mp4Reader};

fn main() -> ExitCode {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: {} <media file>", env!("CARGO_PKG_NAME"));
        return ExitCode::from(2);
    };
    match probe(&path) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Prints every track of `path`, picking the demuxer from the file's magic bytes.
fn probe(path: &str) -> Result<(), String> {
    let mut head = [0u8; 4];
    let n = File::open(path)
        .and_then(|mut f| f.read(&mut head))
        .map_err(|e| format!("open {path}: {e}"))?;
    if n < 4 {
        return Err("file is too short to be a media file".into());
    }

    let streams: Vec<StreamInfo> = if head == [0x1A, 0x45, 0xDF, 0xA3] {
        let file = File::open(path).map_err(|e| e.to_string())?;
        MkvReader::open(file).map_err(|e| e.to_string())?.streams().to_vec()
    } else if &head == b"DKIF" {
        let data = std::fs::read(path).map_err(|e| e.to_string())?;
        let demuxer = IvfDemuxer::new(data).map_err(|e| e.to_string())?;
        vec![demuxer.stream_info().clone()]
    } else {
        let file = File::open(path).map_err(|e| e.to_string())?;
        Mp4Reader::open(file).map_err(|e| e.to_string())?.streams()
    };

    for s in &streams {
        println!(
            "track {} {:?} {:?} {}x{} ({} ch, {} Hz)",
            s.index, s.media_type, s.codec, s.width, s.height, s.channels, s.sample_rate
        );
    }
    Ok(())
}
