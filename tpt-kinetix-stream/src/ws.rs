//! A minimal server-side WebSocket (RFC 6455) for browser publishing.
//!
//! A browser cannot stream a request body over HTTP/1.1 (`fetch` upload
//! streaming needs HTTP/2 over TLS), but it can open a WebSocket and send
//! `MediaRecorder` chunks as binary messages. This module is only what the
//! ingest needs: the opening handshake, a frame reader that yields the payload
//! bytes of data frames (answering pings, noticing close), and a close frame.
//!
//! No dependencies: SHA-1 and base64 are only used for the handshake accept key.

use anyhow::{bail, Result};
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Largest single frame payload accepted (a `MediaRecorder` chunk is far smaller).
const MAX_FRAME: u64 = 16 * 1024 * 1024;

const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// SHA-1 (FIPS 180-4). Only used for the handshake, never for security.
fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for block in msg.chunks(64) {
        let mut w = [0u32; 80];
        for (i, c) in block.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([c[0], c[1], c[2], c[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 20];
    for (i, v) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

/// The `Sec-WebSocket-Accept` value for a client's `Sec-WebSocket-Key`.
pub(crate) fn accept_key(client_key: &str) -> String {
    base64(&sha1(format!("{}{GUID}", client_key.trim()).as_bytes()))
}

/// Whether the request headers ask for a WebSocket upgrade; returns the key.
pub(crate) fn upgrade_key(headers: &std::collections::HashMap<String, String>) -> Option<&str> {
    let has = |h: &str, v: &str| {
        headers
            .get(h)
            .is_some_and(|x| x.to_ascii_lowercase().contains(v))
    };
    if has("upgrade", "websocket") && has("connection", "upgrade") {
        headers.get("sec-websocket-key").map(String::as_str)
    } else {
        None
    }
}

/// The `101 Switching Protocols` response.
pub(crate) fn handshake_response(client_key: &str) -> String {
    format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
        accept_key(client_key)
    )
}

/// Writes a close frame with `code` and a short `reason`.
pub(crate) async fn send_close<W: AsyncWrite + Unpin>(w: &mut W, code: u16, reason: &str) -> Result<()> {
    let reason = &reason.as_bytes()[..reason.len().min(100)];
    let mut f = vec![0x88, (2 + reason.len()) as u8];
    f.extend_from_slice(&code.to_be_bytes());
    f.extend_from_slice(reason);
    w.write_all(&f).await?;
    w.flush().await?;
    Ok(())
}

/// Reads frames until one carries data; returns its payload, or `None` once the
/// peer closes. Pings are answered; a text frame is rejected (the stream is
/// binary WebM).
///
/// `io` is both halves of the connection.
pub(crate) async fn next_chunk<S>(io: &mut S) -> Result<Option<Vec<u8>>>
where
    S: AsyncReadExt + AsyncWrite + Unpin,
{
    loop {
        let mut head = [0u8; 2];
        if io.read_exact(&mut head).await.is_err() {
            return Ok(None); // dropped without a close frame
        }
        let opcode = head[0] & 0x0F;
        let masked = head[1] & 0x80 != 0;
        let mut len = u64::from(head[1] & 0x7F);
        if len == 126 {
            let mut b = [0u8; 2];
            io.read_exact(&mut b).await?;
            len = u64::from(u16::from_be_bytes(b));
        } else if len == 127 {
            let mut b = [0u8; 8];
            io.read_exact(&mut b).await?;
            len = u64::from_be_bytes(b);
        }
        if len > MAX_FRAME {
            bail!("websocket frame of {len} bytes is too large");
        }
        if !masked {
            bail!("client frames must be masked");
        }
        let mut mask = [0u8; 4];
        io.read_exact(&mut mask).await?;
        let mut payload = vec![0u8; len as usize];
        io.read_exact(&mut payload).await?;
        for (i, b) in payload.iter_mut().enumerate() {
            *b ^= mask[i % 4];
        }
        match opcode {
            0x0 | 0x2 => return Ok(Some(payload)), // continuation / binary
            0x1 => bail!("text frames are not accepted; send binary WebM"),
            0x8 => return Ok(None),
            0x9 => {
                // Ping -> pong with the same payload (control frames are <= 125 bytes).
                let mut f = vec![0x8A, payload.len().min(125) as u8];
                f.extend_from_slice(&payload[..payload.len().min(125)]);
                io.write_all(&f).await?;
                io.flush().await?;
            }
            0xA => {} // pong
            _ => bail!("unknown websocket opcode {opcode:#x}"),
        }
    }
}

/// Builds a masked client frame (tests and tools).
pub fn client_frame(opcode: u8, payload: &[u8], mask: [u8; 4]) -> Vec<u8> {
    let mut f = vec![0x80 | opcode];
    match payload.len() {
        n if n < 126 => f.push(0x80 | n as u8),
        n if n <= 0xFFFF => {
            f.push(0x80 | 126);
            f.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            f.push(0x80 | 127);
            f.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    f.extend_from_slice(&mask);
    f.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_known_vectors() {
        let hex = |d: [u8; 20]| d.iter().map(|b| format!("{b:02x}")).collect::<String>();
        assert_eq!(hex(sha1(b"abc")), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(hex(sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(
            hex(sha1(&[b'a'; 1000])),
            "291e9a6c66994949b57ba5e650361e98fc36b1ba"
        );
    }

    #[test]
    fn accept_key_matches_rfc6455_example() {
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn base64_padding() {
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
    }

    #[tokio::test]
    async fn frames_round_trip_and_pings_are_answered() {
        let (mut client, mut server) = tokio::io::duplex(1 << 20);
        let big = vec![7u8; 70_000]; // uses the 8-byte length form
        client
            .write_all(&client_frame(0x2, b"hello", [1, 2, 3, 4]))
            .await
            .unwrap();
        client
            .write_all(&client_frame(0x9, b"p", [9, 9, 9, 9]))
            .await
            .unwrap();
        client
            .write_all(&client_frame(0x2, &big, [5, 6, 7, 8]))
            .await
            .unwrap();
        client
            .write_all(&client_frame(0x8, &[], [0, 0, 0, 0]))
            .await
            .unwrap();
        assert_eq!(next_chunk(&mut server).await.unwrap().unwrap(), b"hello");
        assert_eq!(next_chunk(&mut server).await.unwrap().unwrap(), big);
        assert!(next_chunk(&mut server).await.unwrap().is_none());
        let mut pong = [0u8; 3];
        client.read_exact(&mut pong).await.unwrap();
        assert_eq!(pong, [0x8A, 1, b'p']);
    }

    #[tokio::test]
    async fn unmasked_and_text_frames_are_rejected() {
        let (mut client, mut server) = tokio::io::duplex(1024);
        client.write_all(&[0x82, 0x01, 0xFF]).await.unwrap();
        assert!(next_chunk(&mut server).await.is_err());
        let (mut client, mut server) = tokio::io::duplex(1024);
        client
            .write_all(&client_frame(0x1, b"hi", [1, 1, 1, 1]))
            .await
            .unwrap();
        assert!(next_chunk(&mut server).await.is_err());
    }
}
