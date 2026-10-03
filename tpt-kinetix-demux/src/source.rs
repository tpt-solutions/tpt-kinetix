//! Positional byte sources for demuxers.
//!
//! A demuxer should never need the whole file in memory: an MP4's `moov` index
//! is a few hundred KiB even for a multi-gigabyte file, and each packet is a
//! short read at a known offset. [`ReadAt`] is the smallest interface that
//! supports that, and is exactly what an HTTP range-request backend needs:
//! `len()` is a `HEAD`, `read_at(offset, buf)` is `Range: bytes=offset-…`.
//!
//! Implementations are provided for in-memory buffers, [`std::fs::File`] (on
//! Windows and Unix) and any [`Read`] + [`Seek`] value via [`SeekSource`].

use std::io::{self, Read, Seek, SeekFrom};
use std::sync::Mutex;

/// A random-access, read-only byte source.
pub trait ReadAt {
    /// Total length of the source in bytes.
    fn len(&self) -> io::Result<u64>;

    /// Fills `buf` with the bytes at `offset .. offset + buf.len()`.
    ///
    /// Fails with [`io::ErrorKind::UnexpectedEof`] if the range extends past
    /// the end of the source. Must not modify any shared cursor, so `&self`
    /// reads can interleave.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()>;

    /// Returns `true` when the source is empty.
    fn is_empty(&self) -> io::Result<bool> {
        Ok(self.len()? == 0)
    }
}

fn eof() -> io::Error {
    io::Error::new(io::ErrorKind::UnexpectedEof, "read past end of source")
}

impl ReadAt for [u8] {
    fn len(&self) -> io::Result<u64> {
        Ok(<[u8]>::len(self) as u64)
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let start = usize::try_from(offset).map_err(|_| eof())?;
        let end = start.checked_add(buf.len()).ok_or_else(eof)?;
        let src = self.get(start..end).ok_or_else(eof)?;
        buf.copy_from_slice(src);
        Ok(())
    }
}

impl ReadAt for Vec<u8> {
    fn len(&self) -> io::Result<u64> {
        Ok(Vec::len(self) as u64)
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.as_slice().read_at(offset, buf)
    }
}

impl<T: ReadAt + ?Sized> ReadAt for &T {
    fn len(&self) -> io::Result<u64> {
        (**self).len()
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        (**self).read_at(offset, buf)
    }
}

impl<T: ReadAt + ?Sized> ReadAt for Box<T> {
    fn len(&self) -> io::Result<u64> {
        (**self).len()
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        (**self).read_at(offset, buf)
    }
}

#[cfg(any(unix, windows))]
impl ReadAt for std::fs::File {
    fn len(&self) -> io::Result<u64> {
        Ok(self.metadata()?.len())
    }

    #[cfg(unix)]
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        use std::os::unix::fs::FileExt;
        self.read_exact_at(buf, offset)
    }

    #[cfg(windows)]
    fn read_at(&self, mut offset: u64, mut buf: &mut [u8]) -> io::Result<()> {
        use std::os::windows::fs::FileExt;
        while !buf.is_empty() {
            let n = self.seek_read(buf, offset)?;
            if n == 0 {
                return Err(eof());
            }
            offset += n as u64;
            buf = &mut buf[n..];
        }
        Ok(())
    }
}

/// Adapts any [`Read`] + [`Seek`] value to [`ReadAt`] behind a mutex.
pub struct SeekSource<R> {
    inner: Mutex<R>,
}

impl<R: Read + Seek> SeekSource<R> {
    /// Wraps `reader`.
    pub fn new(reader: R) -> Self {
        Self {
            inner: Mutex::new(reader),
        }
    }
}

impl<R: Read + Seek> ReadAt for SeekSource<R> {
    fn len(&self) -> io::Result<u64> {
        let mut r = self
            .inner
            .lock()
            .map_err(|_| io::Error::other("poisoned"))?;
        let here = r.stream_position()?;
        let end = r.seek(SeekFrom::End(0))?;
        r.seek(SeekFrom::Start(here))?;
        Ok(end)
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let mut r = self
            .inner
            .lock()
            .map_err(|_| io::Error::other("poisoned"))?;
        r.seek(SeekFrom::Start(offset))?;
        r.read_exact(buf)
    }
}

/// Asynchronous counterpart of [`ReadAt`], for runtimes where I/O cannot block:
/// a Cloudflare Worker or browser `fetch`, an async HTTP client, a tokio file.
///
/// Demuxing logic is written once against this trait; synchronous sources are
/// adapted with [`Blocking`] (their futures are always ready, so they complete
/// on the first poll), which is how [`ReadAt`] callers reuse the same code via
/// [`block_on`].
///
/// The futures are not required to be `Send`: WebAssembly is single-threaded,
/// and multi-threaded servers can run the sync API on a blocking pool.
#[allow(async_fn_in_trait)]
pub trait AsyncReadAt {
    /// Total length of the source in bytes.
    async fn len(&self) -> io::Result<u64>;

    /// Fills `buf` with the bytes at `offset .. offset + buf.len()`, or fails
    /// with [`io::ErrorKind::UnexpectedEof`].
    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()>;

    /// Returns `true` when the source is empty.
    async fn is_empty(&self) -> io::Result<bool> {
        Ok(self.len().await? == 0)
    }
}

impl<T: AsyncReadAt + ?Sized> AsyncReadAt for &T {
    async fn len(&self) -> io::Result<u64> {
        (**self).len().await
    }

    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        (**self).read_at(offset, buf).await
    }
}

/// Adapts a synchronous [`ReadAt`] to [`AsyncReadAt`] (futures complete immediately).
pub struct Blocking<S>(pub S);

impl<S: ReadAt> AsyncReadAt for Blocking<S> {
    async fn len(&self) -> io::Result<u64> {
        self.0.len()
    }

    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.0.read_at(offset, buf)
    }
}

/// Runs a future that never suspends (every await point is immediately ready,
/// as with [`Blocking`] sources) to completion on the current thread.
///
/// # Panics
/// If the future returns `Pending`: use a real executor for genuinely
/// asynchronous sources.
pub fn block_on<F: std::future::Future>(f: F) -> F::Output {
    use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
    fn noop_raw() -> RawWaker {
        fn clone(_: *const ()) -> RawWaker {
            noop_raw()
        }
        fn noop(_: *const ()) {}
        static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
        RawWaker::new(std::ptr::null(), &VTABLE)
    }
    // SAFETY: the vtable functions do nothing and the data pointer is never used.
    let waker = unsafe { Waker::from_raw(noop_raw()) };
    let mut cx = Context::from_waker(&waker);
    let mut f = std::pin::pin!(f);
    match f.as_mut().poll(&mut cx) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("block_on: the future suspended; use an async executor instead"),
    }
}

/// Wraps a source and counts the I/O it serves, so tests and benchmarks can
/// assert that a demuxer only touches the bytes it needs.
pub struct CountingSource<S> {
    inner: S,
    calls: std::sync::atomic::AtomicU64,
    bytes: std::sync::atomic::AtomicU64,
}

impl<S> CountingSource<S> {
    /// Wraps `inner` with zeroed counters.
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            calls: Default::default(),
            bytes: Default::default(),
        }
    }

    /// Number of `read_at` calls served so far.
    pub fn calls(&self) -> u64 {
        self.calls.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Total bytes requested through `read_at` so far.
    pub fn bytes(&self) -> u64 {
        self.bytes.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl<S: ReadAt> ReadAt for CountingSource<S> {
    fn len(&self) -> io::Result<u64> {
        self.inner.len()
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        use std::sync::atomic::Ordering::Relaxed;
        self.calls.fetch_add(1, Relaxed);
        self.bytes.fetch_add(buf.len() as u64, Relaxed);
        self.inner.read_at(offset, buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slice_reads_and_bounds() {
        let data: Vec<u8> = (0..10).collect();
        let mut b = [0u8; 3];
        data.read_at(2, &mut b).unwrap();
        assert_eq!(b, [2, 3, 4]);
        assert!(data.read_at(8, &mut b).is_err());
        assert!(data.read_at(u64::MAX, &mut b).is_err());
        assert_eq!(ReadAt::len(&data).unwrap(), 10);
    }

    #[test]
    fn seek_source_matches_slice() {
        let data: Vec<u8> = (0..100).collect();
        let s = SeekSource::new(std::io::Cursor::new(data.clone()));
        assert_eq!(s.len().unwrap(), 100);
        let mut a = [0u8; 7];
        let mut b = [0u8; 7];
        s.read_at(50, &mut a).unwrap();
        data.read_at(50, &mut b).unwrap();
        assert_eq!(a, b);
        assert!(s.read_at(95, &mut a).is_err());
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn file_source_reads_positionally() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("tpt_readat_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f.bin");
        let data: Vec<u8> = (0..=255).collect();
        std::fs::File::create(&path)
            .unwrap()
            .write_all(&data)
            .unwrap();
        let f = std::fs::File::open(&path).unwrap();
        assert_eq!(ReadAt::len(&f).unwrap(), 256);
        let mut b = [0u8; 4];
        f.read_at(250, &mut b).unwrap();
        assert_eq!(b, [250, 251, 252, 253]);
        f.read_at(0, &mut b).unwrap();
        assert_eq!(b, [0, 1, 2, 3]);
        assert!(f.read_at(254, &mut b).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn blocking_adapter_and_block_on() {
        let data: Vec<u8> = (0..32).collect();
        let src = Blocking(&data);
        let v = block_on(async {
            let len = AsyncReadAt::len(&src).await.unwrap();
            let mut b = [0u8; 4];
            AsyncReadAt::read_at(&src, 10, &mut b).await.unwrap();
            (len, b)
        });
        assert_eq!(v, (32, [10, 11, 12, 13]));
        let bad = block_on(async {
            let mut b = [0u8; 4];
            AsyncReadAt::read_at(&src, 30, &mut b).await
        });
        assert!(bad.is_err());
    }

    #[test]
    fn counting_source_counts() {
        let c = CountingSource::new(vec![0u8; 64]);
        let mut b = [0u8; 8];
        c.read_at(0, &mut b).unwrap();
        c.read_at(8, &mut b).unwrap();
        assert_eq!((c.calls(), c.bytes()), (2, 16));
    }
}
