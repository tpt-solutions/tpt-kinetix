//! WebAssembly bindings (cargo feature `wasm`): run the packager in a browser,
//! a Cloudflare/Deno/Fastly worker or Node, reading the MP4 through a JavaScript
//! range-read callback.
//!
//! ```js
//! import init, { WasmPackager } from "./pkg/tpt_kinetix_package.js";
//! await init();
//! // `read(offset, length)` returns a Promise<Uint8Array> (e.g. fetch with a Range header).
//! const pk = await WasmPackager.open(fileLength, (off, len) => readRange(off, len), 6.0);
//! const master = pk.hlsMaster();                  // string
//! const init0  = pk.initSegment(0);               // Uint8Array
//! const seg    = await pk.mediaSegment(0, 3);     // Uint8Array, built from ranged reads
//! ```
//!
//! Only the MP4 index and the requested segment's samples are ever fetched.

use std::cell::Cell;
use std::io;

use js_sys::{Function, Uint8Array};
use tpt_kinetix_demux::AsyncReadAt;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use crate::{Packager, PackagerOptions};

/// An [`AsyncReadAt`] backed by a JS `(offset, length) => Promise<Uint8Array>`.
struct JsSource {
    len: u64,
    read: Function,
    requests: Cell<u32>,
}

fn io_err(msg: impl Into<String>) -> io::Error {
    io::Error::other(msg.into())
}

impl AsyncReadAt for JsSource {
    async fn len(&self) -> io::Result<u64> {
        Ok(self.len)
    }

    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        let end = offset
            .checked_add(buf.len() as u64)
            .filter(|&e| e <= self.len);
        if end.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "read past end of source",
            ));
        }
        self.requests.set(self.requests.get() + 1);
        let promise = self
            .read
            .call2(
                &JsValue::NULL,
                &JsValue::from_f64(offset as f64),
                &JsValue::from_f64(buf.len() as f64),
            )
            .map_err(|e| io_err(format!("read callback threw: {e:?}")))?;
        let value = JsFuture::from(js_sys::Promise::resolve(&promise))
            .await
            .map_err(|e| io_err(format!("read callback rejected: {e:?}")))?;
        let bytes = Uint8Array::new(&value);
        if bytes.length() as usize != buf.len() {
            return Err(io_err(format!(
                "read callback returned {} bytes, expected {}",
                bytes.length(),
                buf.len()
            )));
        }
        bytes.copy_to(buf);
        Ok(())
    }
}

fn js_err(e: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&e.to_string())
}

/// A packager over a JS-backed source.
#[wasm_bindgen]
pub struct WasmPackager {
    inner: Packager,
    source: JsSource,
}

#[wasm_bindgen]
impl WasmPackager {
    /// Loads the MP4 index. `read(offset, length)` must return a
    /// `Promise<Uint8Array>` of exactly `length` bytes.
    pub async fn open(
        len: f64,
        read: Function,
        segment_seconds: f64,
    ) -> Result<WasmPackager, JsValue> {
        let source = JsSource {
            len: len as u64,
            read,
            requests: Cell::new(0),
        };
        let inner = Packager::load(
            &source,
            PackagerOptions {
                segment_seconds,
                ..Default::default()
            },
        )
        .await
        .map_err(js_err)?;
        Ok(WasmPackager { inner, source })
    }

    /// Number of packaged tracks.
    #[wasm_bindgen(getter, js_name = trackCount)]
    pub fn track_count(&self) -> usize {
        self.inner.streams().len()
    }

    /// Number of segments per track.
    #[wasm_bindgen(getter, js_name = segmentCount)]
    pub fn segment_count(&self) -> usize {
        self.inner.segment_count()
    }

    /// Number of range reads issued so far (for diagnostics).
    #[wasm_bindgen(getter)]
    pub fn requests(&self) -> u32 {
        self.source.requests.get()
    }

    /// RFC 6381 codec string of `track`.
    pub fn codec(&self, track: usize) -> Option<String> {
        self.inner.codec(track).map(str::to_string)
    }

    /// The HLS master playlist.
    #[wasm_bindgen(js_name = hlsMaster)]
    pub fn hls_master(&self) -> String {
        self.inner.hls_master()
    }

    /// The HLS media playlist of `track`.
    #[wasm_bindgen(js_name = hlsMedia)]
    pub fn hls_media(&self, track: usize) -> Result<String, JsValue> {
        self.inner.hls_media(track).map_err(js_err)
    }

    /// The DASH MPD.
    #[wasm_bindgen(js_name = dashMpd)]
    pub fn dash_mpd(&self) -> String {
        self.inner.dash_mpd()
    }

    /// The initialization segment of `track`.
    #[wasm_bindgen(js_name = initSegment)]
    pub fn init_segment(&self, track: usize) -> Result<Vec<u8>, JsValue> {
        self.inner.init_segment(track).map_err(js_err)
    }

    /// Segment `n` (1-based) of `track`, built from ranged reads.
    #[wasm_bindgen(js_name = mediaSegment)]
    pub async fn media_segment(&self, track: usize, n: usize) -> Result<Vec<u8>, JsValue> {
        self.inner
            .media_segment(&self.source, track, n)
            .await
            .map_err(js_err)
    }
}
