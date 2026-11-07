//! The compact track summary behind `tk_probe_json` and the bindings' `probe()`.
//!
//! Unlike the CLI's `probe --json` (ffprobe-shaped, with durations and frame
//! counts), this lists only the format and each track's codec, type and size or
//! layout, so every binding returns the same small document.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use tpt_kinetix_core::codec::MediaType;
use tpt_kinetix_core::stream::StreamInfo;
use tpt_kinetix_demux::{IvfDemuxer, MkvReader, Mp4Reader};

/// The tracks of `path` (WebM, Matroska, IVF or MP4) as the compact JSON that the C
/// ABI and the language bindings return: `{"format":...,"streams":[...]}`.
pub fn summary_json(path: &Path) -> Result<String, String> {
    let mut head = [0u8; 4];
    {
        let mut f = File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
        let n = f.read(&mut head).map_err(|e| e.to_string())?;
        if n < 4 {
            return Err("file is too short to be a media file".into());
        }
    }
    let (format, streams) = if head == [0x1A, 0x45, 0xDF, 0xA3] {
        let file = File::open(path).map_err(|e| e.to_string())?;
        let reader = MkvReader::open(file).map_err(|e| e.to_string())?;
        ("matroska,webm", reader.streams().to_vec())
    } else if &head == b"DKIF" {
        let data = std::fs::read(path).map_err(|e| e.to_string())?;
        let demuxer = IvfDemuxer::new(data).map_err(|e| e.to_string())?;
        ("ivf", vec![demuxer.stream_info().clone()])
    } else {
        let file = File::open(path).map_err(|e| e.to_string())?;
        let reader = Mp4Reader::open(file).map_err(|e| e.to_string())?;
        ("mov,mp4,m4a,3gp,3g2,mj2", reader.streams())
    };
    Ok(document(format, &streams))
}

fn document(format: &str, streams: &[StreamInfo]) -> String {
    let items: Vec<String> = streams
        .iter()
        .enumerate()
        .map(|(i, s)| stream_json(i, s))
        .collect();
    format!(
        "{{\"format\":\"{}\",\"streams\":[{}]}}",
        format,
        items.join(",")
    )
}

fn stream_json(index: usize, s: &StreamInfo) -> String {
    let kind = match s.media_type {
        MediaType::Video => "video",
        MediaType::Audio => "audio",
        _ => "other",
    };
    let mut out = format!(
        "{{\"index\":{index},\"codec\":\"{}\",\"type\":\"{kind}\"",
        s.codec.name()
    );
    match s.media_type {
        MediaType::Video => {
            out.push_str(&format!(",\"width\":{},\"height\":{}", s.width, s.height));
        }
        MediaType::Audio => {
            out.push_str(&format!(
                ",\"channels\":{},\"sample_rate\":{}",
                s.channels, s.sample_rate
            ));
        }
        _ => {}
    }
    out.push('}');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpt_kinetix_core::codec::CodecId;

    #[test]
    fn stream_json_has_the_fields_per_kind() {
        let mut v = StreamInfo::new(0, CodecId::Vp9, 90_000);
        v.media_type = MediaType::Video;
        v.width = 320;
        v.height = 240;
        assert_eq!(
            stream_json(0, &v),
            r#"{"index":0,"codec":"vp9","type":"video","width":320,"height":240}"#
        );
        let mut a = StreamInfo::new(1, CodecId::Opus, 48_000);
        a.media_type = MediaType::Audio;
        a.channels = 2;
        a.sample_rate = 48_000;
        assert_eq!(
            stream_json(1, &a),
            r#"{"index":1,"codec":"opus","type":"audio","channels":2,"sample_rate":48000}"#
        );
    }

    #[test]
    fn a_missing_file_is_an_error_not_a_panic() {
        assert!(summary_json(Path::new("/definitely/not/here.mp4")).is_err());
    }
}
