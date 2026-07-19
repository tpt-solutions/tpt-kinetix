//! `wasm32` bindings for browser-based MP4 probing.
//!
//! Only built with `--features wasm`. Exposes a single function that mirrors
//! the output of `tpt-kinetix probe <file>` / the `probe_mp4` example, so a
//! client-side demo page and the CLI report the same fields.

use wasm_bindgen::prelude::wasm_bindgen;

use crate::mp4::Mp4Demuxer;

/// Probe an MP4/ISO-BMFF byte buffer and return a JSON string describing its
/// tracks.
///
/// On parse failure, returns a JSON object with an `"error"` field instead of
/// throwing, so callers only need to handle one return type.
#[wasm_bindgen]
pub fn probe_mp4(data: &[u8]) -> String {
    match Mp4Demuxer::new(data.to_vec()) {
        Ok(demuxer) => {
            let tracks: Vec<serde_json::Value> = demuxer
                .tracks()
                .iter()
                .map(|track| {
                    let duration_secs = if track.timescale != 0 {
                        track.duration as f64 / track.timescale as f64
                    } else {
                        0.0
                    };
                    serde_json::json!({
                        "track_id": track.track_id,
                        "media_type": format!("{:?}", track.media_type),
                        "codec": track.codec.map(|c| format!("{c:?}")),
                        "sample_count": track.sample_count(),
                        "duration_secs": duration_secs,
                        "width": track.width,
                        "height": track.height,
                    })
                })
                .collect();
            serde_json::json!({ "tracks": tracks }).to_string()
        }
        Err(e) => serde_json::json!({ "error": e.to_string() }).to_string(),
    }
}
