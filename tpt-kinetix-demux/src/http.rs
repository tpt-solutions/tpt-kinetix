//! HTTP range-request backend for [`ReadAt`] (cargo feature `http`).
//!
//! Probing a remote file with ffmpeg downloads `probesize` (5 MB by default)
//! and analyses it. An MP4's index is one box, so [`Mp4Reader`] over an
//! [`HttpRangeSource`] needs only a handful of requests, independent of the
//! file's size: typically one for the first block (which also reveals the total
//! length via `Content-Range`), one for the block holding the `moov` header and
//! one for the `moov` payload.
//!
//! The transport is abstracted by [`RangeFetch`] so tests (and callers with
//! their own HTTP stack, signed S3 requests, etc.) can plug in anything;
//! [`UreqFetch`] is the built-in HTTP/HTTPS implementation.
//!
//! [`Mp4Reader`]: crate::Mp4Reader

use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use crate::source::ReadAt;

/// One fetched range.
pub struct RangeResponse {
    /// The bytes served (may be shorter than requested at the end of the file).
    pub data: Vec<u8>,
    /// Total length of the resource when the server reported it.
    pub total: Option<u64>,
}

/// Fetches byte ranges of one remote resource.
pub trait RangeFetch {
    /// Fetches `len` bytes starting at `start` (the server may return fewer at
    /// the end of the resource). Must fail if the server ignores range requests
    /// and would send a larger body.
    fn fetch(&self, start: u64, len: u64) -> io::Result<RangeResponse>;
}

/// Default read-ahead block: large enough that the 16-byte box-header hops of a
/// top-level scan cost one request, small enough not to waste bandwidth.
pub const DEFAULT_BLOCK: usize = 64 * 1024;
const MAX_CACHED_BLOCKS: usize = 8;

struct Cache {
    /// `(block index, bytes)`, most recently used last.
    blocks: VecDeque<(u64, Vec<u8>)>,
}

/// A [`ReadAt`] over a [`RangeFetch`], with a small block cache and request
/// accounting.
pub struct HttpRangeSource<F: RangeFetch> {
    fetch: F,
    block: u64,
    total: Mutex<Option<u64>>,
    cache: Mutex<Cache>,
    requests: AtomicU64,
    bytes: AtomicU64,
}

impl<F: RangeFetch> HttpRangeSource<F> {
    /// Wraps `fetch` with the [`DEFAULT_BLOCK`] size.
    pub fn new(fetch: F) -> Self {
        Self::with_block_size(fetch, DEFAULT_BLOCK)
    }

    /// Wraps `fetch`, reading ahead in `block`-byte units (at least 4 KiB).
    pub fn with_block_size(fetch: F, block: usize) -> Self {
        Self {
            fetch,
            block: block.max(4096) as u64,
            total: Mutex::new(None),
            cache: Mutex::new(Cache {
                blocks: VecDeque::new(),
            }),
            requests: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
        }
    }

    /// Number of HTTP requests made so far.
    pub fn requests(&self) -> u64 {
        self.requests.load(Ordering::Relaxed)
    }

    /// Response bytes transferred so far.
    pub fn bytes_transferred(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }

    fn do_fetch(&self, start: u64, len: u64) -> io::Result<Vec<u8>> {
        self.requests.fetch_add(1, Ordering::Relaxed);
        let r = self.fetch.fetch(start, len)?;
        self.bytes.fetch_add(r.data.len() as u64, Ordering::Relaxed);
        if let Some(t) = r.total {
            let mut slot = self
                .total
                .lock()
                .map_err(|_| io::Error::other("poisoned"))?;
            slot.get_or_insert(t);
        }
        Ok(r.data)
    }

    /// Total length, fetching the first block to learn it if necessary.
    fn total_len(&self) -> io::Result<u64> {
        if let Some(t) = *self
            .total
            .lock()
            .map_err(|_| io::Error::other("poisoned"))?
        {
            return Ok(t);
        }
        // The first block doubles as the length probe: `Content-Range` carries it.
        self.block_bytes(0)?;
        self.total
            .lock()
            .map_err(|_| io::Error::other("poisoned"))?
            .ok_or_else(|| io::Error::other("server did not report the resource length"))
    }

    /// Returns block `idx`, from the cache or the network.
    fn block_bytes(&self, idx: u64) -> io::Result<Vec<u8>> {
        {
            let mut c = self
                .cache
                .lock()
                .map_err(|_| io::Error::other("poisoned"))?;
            if let Some(pos) = c.blocks.iter().position(|(i, _)| *i == idx) {
                let entry = c.blocks.remove(pos).unwrap();
                let bytes = entry.1.clone();
                c.blocks.push_back(entry);
                return Ok(bytes);
            }
        }
        let start = idx * self.block;
        let known_total = *self
            .total
            .lock()
            .map_err(|_| io::Error::other("poisoned"))?;
        let len = match known_total {
            Some(t) if start >= t => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "block past end",
                ));
            }
            Some(t) => self.block.min(t - start),
            None => self.block,
        };
        let data = self.do_fetch(start, len)?;
        let mut c = self
            .cache
            .lock()
            .map_err(|_| io::Error::other("poisoned"))?;
        c.blocks.push_back((idx, data.clone()));
        while c.blocks.len() > MAX_CACHED_BLOCKS {
            c.blocks.pop_front();
        }
        Ok(data)
    }
}

impl<F: RangeFetch> ReadAt for HttpRangeSource<F> {
    fn len(&self) -> io::Result<u64> {
        self.total_len()
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        let total = self.total_len()?;
        let end = offset
            .checked_add(buf.len() as u64)
            .filter(|&e| e <= total)
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "read past end"))?;
        // Large reads (a moov payload, a big sample) go straight to the server
        // as one exact range rather than through the block cache.
        if buf.len() as u64 >= self.block {
            let data = self.do_fetch(offset, buf.len() as u64)?;
            if data.len() != buf.len() {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "server returned a short range",
                ));
            }
            buf.copy_from_slice(&data);
            return Ok(());
        }
        let mut pos = offset;
        let mut out = 0usize;
        while pos < end {
            let idx = pos / self.block;
            let block = self.block_bytes(idx)?;
            let within = (pos - idx * self.block) as usize;
            let take = (block.len().saturating_sub(within)).min((end - pos) as usize);
            if take == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "server returned a short block",
                ));
            }
            buf[out..out + take].copy_from_slice(&block[within..within + take]);
            out += take;
            pos += take as u64;
        }
        Ok(())
    }
}

/// Parses the total from a `Content-Range: bytes a-b/total` value.
pub fn parse_content_range_total(v: &str) -> Option<u64> {
    v.rsplit('/').next()?.trim().parse().ok()
}

/// HTTP/HTTPS [`RangeFetch`] built on `ureq`.
pub struct UreqFetch {
    url: String,
    agent: ureq::Agent,
}

impl UreqFetch {
    /// A fetcher for `url` (`http://` or `https://`).
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            agent: ureq::Agent::new_with_defaults(),
        }
    }
}

impl RangeFetch for UreqFetch {
    fn fetch(&self, start: u64, len: u64) -> io::Result<RangeResponse> {
        let last = start + len.max(1) - 1;
        let mut resp = self
            .agent
            .get(&self.url)
            .header("Range", format!("bytes={start}-{last}"))
            .call()
            .map_err(|e| io::Error::other(format!("GET {}: {e}", self.url)))?;
        let status = resp.status().as_u16();
        let total = resp
            .headers()
            .get("content-range")
            .and_then(|v| v.to_str().ok())
            .and_then(parse_content_range_total);
        if status != 206 && !(status == 200 && start == 0) {
            return Err(io::Error::other(format!(
                "GET {}: unexpected status {status} (does the server support range requests?)",
                self.url
            )));
        }
        // A 200 means the server ignored `Range` and is sending the whole
        // resource; only accept it when it is no larger than what we asked for.
        let data = resp
            .body_mut()
            .with_config()
            .limit(len.saturating_add(1))
            .read_to_vec()
            .map_err(|e| {
                io::Error::other(format!(
                    "reading range of {}: {e} (server may not honour Range requests)",
                    self.url
                ))
            })?;
        let total = if status == 200 {
            Some(data.len() as u64)
        } else {
            total
        };
        Ok(RangeResponse { data, total })
    }
}

/// Convenience: an [`HttpRangeSource`] over [`UreqFetch`].
pub fn open_url(url: &str) -> HttpRangeSource<UreqFetch> {
    HttpRangeSource::new(UreqFetch::new(url))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;

    /// An in-memory "server" counting requests.
    struct Mock {
        data: Vec<u8>,
        hits: Arc<AtomicUsize>,
    }

    impl RangeFetch for Mock {
        fn fetch(&self, start: u64, len: u64) -> io::Result<RangeResponse> {
            self.hits.fetch_add(1, Ordering::Relaxed);
            let s = start as usize;
            let e = (s + len as usize).min(self.data.len());
            Ok(RangeResponse {
                data: self.data.get(s..e).unwrap_or(&[]).to_vec(),
                total: Some(self.data.len() as u64),
            })
        }
    }

    fn mock(n: usize) -> (HttpRangeSource<Mock>, Arc<AtomicUsize>, Vec<u8>) {
        let data: Vec<u8> = (0..n).map(|i| (i * 7 % 251) as u8).collect();
        let hits = Arc::new(AtomicUsize::new(0));
        let src = HttpRangeSource::with_block_size(
            Mock {
                data: data.clone(),
                hits: hits.clone(),
            },
            4096,
        );
        (src, hits, data)
    }

    #[test]
    fn length_probe_reuses_the_first_block() {
        let (src, hits, data) = mock(100_000);
        assert_eq!(src.len().unwrap(), 100_000);
        assert_eq!(hits.load(Ordering::Relaxed), 1);
        // Reading inside block 0 costs nothing more.
        let mut b = [0u8; 16];
        src.read_at(100, &mut b).unwrap();
        assert_eq!(&b[..], &data[100..116]);
        assert_eq!(hits.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn reads_span_blocks_and_hit_the_cache() {
        let (src, hits, data) = mock(100_000);
        let mut b = vec![0u8; 3000];
        src.read_at(3000, &mut b).unwrap(); // spans blocks 0 and 1
        assert_eq!(b, data[3000..6000]);
        let after_first = hits.load(Ordering::Relaxed);
        src.read_at(3500, &mut b).unwrap();
        assert_eq!(b, data[3500..6500]);
        assert!(hits.load(Ordering::Relaxed) <= after_first + 1);
    }

    #[test]
    fn large_reads_are_one_exact_range_and_tail_is_exact() {
        let (src, hits, data) = mock(100_000);
        src.len().unwrap();
        let before = hits.load(Ordering::Relaxed);
        let mut big = vec![0u8; 20_000];
        src.read_at(40_000, &mut big).unwrap();
        assert_eq!(big, data[40_000..60_000]);
        assert_eq!(hits.load(Ordering::Relaxed), before + 1);
        // The final, short block.
        let mut tail = [0u8; 10];
        src.read_at(99_990, &mut tail).unwrap();
        assert_eq!(&tail[..], &data[99_990..]);
        // Past the end.
        assert!(src.read_at(99_995, &mut tail).is_err());
    }

    #[test]
    fn content_range_total() {
        assert_eq!(parse_content_range_total("bytes 0-99/12345"), Some(12345));
        assert_eq!(parse_content_range_total("bytes 0-99/*"), None);
    }
}
