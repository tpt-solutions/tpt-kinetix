//! RTMP handshake (C0/C1/C2, S0/S1/S2).

use tokio::io::{AsyncReadExt, AsyncWriteExt};

const RTMP_VERSION: u8 = 3;
const HANDSHAKE_SIZE: usize = 1536;

/// Opaque handshake state — held between steps if the caller drives the state
/// machine manually. Currently unused by `perform_server_handshake`, which
/// drives all steps internally, but kept here for future incremental parsing.
pub struct HandshakeState {
    pub c1: [u8; HANDSHAKE_SIZE],
}

/// Perform the server side of the RTMP handshake on `stream`.
///
/// Steps:
/// 1. Read C0 (1 byte) and verify the RTMP version is 3.
/// 2. Read C1 (1536 bytes).
/// 3. Write S0 (`0x03`) + S1 (1536 pseudo-random bytes) + S2 (echo of C1).
/// 4. Read C2 (1536 bytes) — echo not validated in this implementation.
pub async fn perform_server_handshake(stream: &mut tokio::net::TcpStream) -> anyhow::Result<()> {
    // ── C0 ──────────────────────────────────────────────────────────────────
    let mut c0 = [0u8; 1];
    stream.read_exact(&mut c0).await?;
    anyhow::ensure!(
        c0[0] == RTMP_VERSION,
        "unsupported RTMP version {}, expected {RTMP_VERSION}",
        c0[0]
    );

    // ── C1 ──────────────────────────────────────────────────────────────────
    let mut c1 = [0u8; HANDSHAKE_SIZE];
    stream.read_exact(&mut c1).await?;

    // ── S0 + S1 + S2 ────────────────────────────────────────────────────────
    // S0: version byte
    stream.write_all(&[RTMP_VERSION]).await?;

    // S1: 1536 bytes — timestamp (4 bytes, 0) + zeros (4 bytes) + random body
    // For simplicity we use a deterministic pseudo-random fill.
    let mut s1 = [0u8; HANDSHAKE_SIZE];
    // bytes 0-3: timestamp (all zeros)
    // bytes 4-7: zeros (server version field)
    // bytes 8+: pseudo-random data
    for (i, byte) in s1[8..].iter_mut().enumerate() {
        *byte = ((i * 7 + 131) & 0xFF) as u8;
    }
    stream.write_all(&s1).await?;

    // S2: echo of C1
    stream.write_all(&c1).await?;
    stream.flush().await?;

    // ── C2 ──────────────────────────────────────────────────────────────────
    let mut c2 = [0u8; HANDSHAKE_SIZE];
    stream.read_exact(&mut c2).await?;
    // We intentionally do not validate that C2 is an echo of S1.

    Ok(())
}
