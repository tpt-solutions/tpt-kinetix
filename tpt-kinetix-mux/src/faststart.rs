//! Move an MP4's `moov` box in front of its `mdat` ("faststart").
//!
//! A progressive file whose index is at the end cannot start playing over HTTP
//! until it is fully downloaded. [`faststart`] streams the file once, copying
//! the bytes in 1 MiB pieces (nothing is held in memory but the `moov`), and
//! rewrites the chunk-offset tables (`stco` / `co64`) for the bytes that moved.
//! It works on any MP4, not only ones written by [`crate::Mp4Writer`].

use std::io::{self, Read, Seek, SeekFrom, Write};

use crate::MuxError;

/// Largest `moov` accepted (matches the demuxer's limit).
const MAX_MOOV: u64 = 256 << 20;
const MAX_TOP_LEVEL_BOXES: usize = 1 << 20;

/// What [`faststart`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaststartReport {
    /// `true` when `moov` was already ahead of `mdat` (the file was copied verbatim).
    pub already_faststart: bool,
    /// Size of the relocated `moov` box in bytes.
    pub moov_bytes: u64,
    /// Total bytes written.
    pub bytes_written: u64,
}

#[derive(Clone, Copy)]
struct TopBox {
    start: u64,
    size: u64,
    kind: [u8; 4],
}

fn scan<R: Read + Seek>(src: &mut R, len: u64) -> Result<Vec<TopBox>, MuxError> {
    let mut boxes = Vec::new();
    let mut pos = 0u64;
    while pos + 8 <= len {
        if boxes.len() >= MAX_TOP_LEVEL_BOXES {
            return Err(MuxError::InvalidConfig("too many top-level boxes".into()));
        }
        let mut hdr = [0u8; 16];
        let want = (len - pos).min(16) as usize;
        src.seek(SeekFrom::Start(pos))?;
        src.read_exact(&mut hdr[..want])?;
        let size32 = u32::from_be_bytes(hdr[0..4].try_into().unwrap());
        let kind: [u8; 4] = hdr[4..8].try_into().unwrap();
        let (hlen, size) = match size32 {
            1 => {
                if want < 16 {
                    return Err(MuxError::InvalidConfig(
                        "truncated 64-bit box header".into(),
                    ));
                }
                (16u64, u64::from_be_bytes(hdr[8..16].try_into().unwrap()))
            }
            0 => (8, len - pos),
            n => (8, u64::from(n)),
        };
        if size < hlen || size > len - pos {
            return Err(MuxError::InvalidConfig(format!(
                "invalid box size {size} at offset {pos}"
            )));
        }
        boxes.push(TopBox {
            start: pos,
            size,
            kind,
        });
        pos += size;
    }
    Ok(boxes)
}

fn copy_range<R: Read + Seek, W: Write>(
    src: &mut R,
    dst: &mut W,
    start: u64,
    len: u64,
) -> io::Result<()> {
    src.seek(SeekFrom::Start(start))?;
    let mut buf = vec![0u8; 1 << 20];
    let mut left = len;
    while left > 0 {
        let n = left.min(buf.len() as u64) as usize;
        src.read_exact(&mut buf[..n])?;
        dst.write_all(&buf[..n])?;
        left -= n as u64;
    }
    Ok(())
}

/// Adds `delta` to every chunk offset in `moov` that points into `[lo, hi)`.
fn patch_offsets(moov: &mut [u8], lo: u64, hi: u64, delta: u64) -> Result<(), MuxError> {
    fn walk(data: &mut [u8], lo: u64, hi: u64, delta: u64, depth: u32) -> Result<(), MuxError> {
        if depth > 8 {
            return Err(MuxError::InvalidConfig("moov nested too deeply".into()));
        }
        let mut pos = 0usize;
        while pos + 8 <= data.len() {
            let size32 = u32::from_be_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
            let kind: [u8; 4] = data[pos + 4..pos + 8].try_into().unwrap();
            let (hlen, size) = match size32 {
                1 => {
                    let big = data
                        .get(pos + 8..pos + 16)
                        .ok_or_else(|| MuxError::InvalidConfig("truncated box".into()))?;
                    (
                        16usize,
                        u64::from_be_bytes(big.try_into().unwrap()) as usize,
                    )
                }
                0 => (8, data.len() - pos),
                n => (8, n),
            };
            if size < hlen || pos + size > data.len() {
                return Err(MuxError::InvalidConfig("malformed box inside moov".into()));
            }
            let body = &mut data[pos + hlen..pos + size];
            match &kind {
                b"trak" | b"mdia" | b"minf" | b"stbl" => walk(body, lo, hi, delta, depth + 1)?,
                b"stco" | b"co64" => {
                    let wide = &kind == b"co64";
                    let width = if wide { 8 } else { 4 };
                    let count = u32::from_be_bytes(
                        body.get(4..8)
                            .ok_or_else(|| {
                                MuxError::InvalidConfig("truncated chunk offset table".into())
                            })?
                            .try_into()
                            .unwrap(),
                    ) as usize;
                    if body.len() < 8 + count * width {
                        return Err(MuxError::InvalidConfig(
                            "truncated chunk offset table".into(),
                        ));
                    }
                    for i in 0..count {
                        let at = 8 + i * width;
                        let off = if wide {
                            u64::from_be_bytes(body[at..at + 8].try_into().unwrap())
                        } else {
                            u64::from(u32::from_be_bytes(body[at..at + 4].try_into().unwrap()))
                        };
                        if off >= lo && off < hi {
                            let new = off + delta;
                            if wide {
                                body[at..at + 8].copy_from_slice(&new.to_be_bytes());
                            } else {
                                let n = u32::try_from(new).map_err(|_| {
                                    MuxError::Unsupported(
                                        "chunk offset exceeds 32 bits after moving moov; the file needs co64".into(),
                                    )
                                })?;
                                body[at..at + 4].copy_from_slice(&n.to_be_bytes());
                            }
                        }
                    }
                }
                _ => {}
            }
            pos += size;
        }
        Ok(())
    }
    walk(moov, lo, hi, delta, 0)
}

/// Copies `src` to `dst` with `moov` moved ahead of the first `mdat`.
pub fn faststart<R: Read + Seek, W: Write>(
    src: &mut R,
    dst: &mut W,
) -> Result<FaststartReport, MuxError> {
    let len = src.seek(SeekFrom::End(0))?;
    let boxes = scan(src, len)?;
    let moov_idx = boxes
        .iter()
        .position(|b| &b.kind == b"moov")
        .ok_or_else(|| MuxError::InvalidConfig("no moov box found".into()))?;
    let moov = boxes[moov_idx];
    let first_mdat = boxes.iter().find(|b| &b.kind == b"mdat").copied();

    // Nothing to do when moov already precedes the media.
    if first_mdat.is_none_or(|m| moov.start < m.start) {
        copy_range(src, dst, 0, len)?;
        return Ok(FaststartReport {
            already_faststart: true,
            moov_bytes: moov.size,
            bytes_written: len,
        });
    }
    let first_mdat = first_mdat.unwrap();
    if moov.size > MAX_MOOV {
        return Err(MuxError::InvalidConfig("moov box too large".into()));
    }

    let mut moov_bytes = vec![0u8; moov.size as usize];
    src.seek(SeekFrom::Start(moov.start))?;
    src.read_exact(&mut moov_bytes)?;
    // Everything in [first mdat, old moov) slides back by the size of moov;
    // data after the old moov keeps its position (moov left, and moved in front).
    let hlen = if u32::from_be_bytes(moov_bytes[0..4].try_into().unwrap()) == 1 {
        16
    } else {
        8
    };
    patch_offsets(
        &mut moov_bytes[hlen..],
        first_mdat.start,
        moov.start,
        moov.size,
    )?;

    let mut written = 0u64;
    // Boxes before the first mdat (ftyp, free, …).
    copy_range(src, dst, 0, first_mdat.start)?;
    written += first_mdat.start;
    dst.write_all(&moov_bytes)?;
    written += moov.size;
    // Everything from the first mdat on, minus the old moov.
    copy_range(src, dst, first_mdat.start, moov.start - first_mdat.start)?;
    written += moov.start - first_mdat.start;
    let after = moov.start + moov.size;
    copy_range(src, dst, after, len - after)?;
    written += len - after;
    dst.flush()?;
    Ok(FaststartReport {
        already_faststart: false,
        moov_bytes: moov.size,
        bytes_written: written,
    })
}
