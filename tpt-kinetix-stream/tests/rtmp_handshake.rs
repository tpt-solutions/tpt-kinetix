use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tpt_kinetix_stream::rtmp::handshake::perform_server_handshake;

const HANDSHAKE_SIZE: usize = 1536;

/// Spin up a local TCP listener, connect a raw client stream, send a valid
/// C0+C1 and then C2, and assert that the server sends back S0+S1+S2
/// (1 + 1536 + 1536 = 3073 bytes).
#[tokio::test]
async fn test_handshake_smoke() {
    // Bind on an OS-assigned port.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server_addr = listener.local_addr().unwrap();

    // Server task: performs the handshake and then signals completion.
    let server_task = tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.unwrap();
        perform_server_handshake(&mut conn).await.unwrap();
    });

    // Client: send C0 + C1, read S0+S1+S2, then send C2.
    let mut client = TcpStream::connect(server_addr).await.unwrap();

    // C0: RTMP version 3
    client.write_all(&[3u8]).await.unwrap();

    // C1: 1536 bytes (timestamp + zeros + body)
    let c1 = vec![0u8; HANDSHAKE_SIZE];
    client.write_all(&c1).await.unwrap();
    client.flush().await.unwrap();

    // Read S0 (1) + S1 (1536) + S2 (1536) = 3073 bytes
    let expected_len = 1 + HANDSHAKE_SIZE + HANDSHAKE_SIZE;
    let mut server_response = vec![0u8; expected_len];
    client.read_exact(&mut server_response).await.unwrap();

    // S0 must be version 3
    assert_eq!(server_response[0], 3, "S0 version byte should be 3");

    // C2: echo of S1 (bytes 1..1537)
    let c2 = server_response[1..1 + HANDSHAKE_SIZE].to_vec();
    client.write_all(&c2).await.unwrap();
    client.flush().await.unwrap();

    // Wait for the server to finish.
    server_task.await.unwrap();
}
