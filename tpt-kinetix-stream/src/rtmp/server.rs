//! RTMP ingest server — accepts TCP connections, performs the handshake,
//! completes the AMF0 `connect`/`createStream`/`publish` negotiation, reassembles
//! the chunk stream into messages, depacketizes FLV audio/video, and forwards
//! high-level media events to a caller-supplied handler.

use std::sync::Arc;

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

use super::{
    amf::{self, Amf0Value},
    chunk::{ChunkAssembler, MessageTypeId, RtmpMessage},
    flv::{self, FlvAudioTag, FlvVideoTag},
    handshake::perform_server_handshake,
};

/// Configuration for the RTMP ingest server.
#[derive(Debug, Clone)]
pub struct RtmpConfig {
    /// Address to bind on, e.g. `"0.0.0.0:1935"`.
    pub bind_addr: String,
    /// Optional TLS identity for RTMPS (`rtmps://`): a PEM-encoded
    /// certificate chain plus a PEM-encoded private key. When `None` the
    /// server speaks plain RTMP. Requires the crate's `rtmps` feature
    /// (`tokio-rustls`); enabling the feature without setting this keeps
    /// plain RTMP.
    pub tls: Option<RtmpsIdentity>,
}

/// PEM-encoded certificate chain + private key for RTMPS.
#[derive(Debug, Clone)]
pub struct RtmpsIdentity {
    /// PEM certificate chain (leaf first).
    pub cert_chain_pem: Vec<u8>,
    /// PEM PKCS#8 / RSA / SEC1 private key.
    pub key_pem: Vec<u8>,
}

impl Default for RtmpConfig {
    fn default() -> Self {
        Self {
            bind_addr: "0.0.0.0:1935".into(),
            tls: None,
        }
    }
}

/// `capsEx` bit: the sender supports reconnection.
pub const CAPS_EX_RECONNECT: u32 = 0x01;
/// `capsEx` bit: the sender supports multitrack.
pub const CAPS_EX_MULTITRACK: u32 = 0x02;
/// `capsEx` bit: the sender can parse `ModEx` signals.
pub const CAPS_EX_MODEX: u32 = 0x04;
/// `capsEx` bit: the sender supports the timestamp nanosecond offset.
pub const CAPS_EX_TIMESTAMP_NANO_OFFSET: u32 = 0x08;
/// `FourCcInfoMask`: can decode the codec.
pub const FOUR_CC_CAN_DECODE: u8 = 0x01;
/// `FourCcInfoMask`: can encode the codec.
pub const FOUR_CC_CAN_ENCODE: u8 = 0x02;
/// `FourCcInfoMask`: can forward the codec.
pub const FOUR_CC_CAN_FORWARD: u8 = 0x04;

/// What the client announced during the capability exchange (`connect` +
/// `releaseStream`/`FCPublish` hints and the audio/video codec fields of the
/// `connect` command object). OBS 30+ sends `fourCcList` / `audioFourCcList`
/// when Enhanced RTMP is enabled; older encoders send nothing, which is also
/// recorded (all `None`/empty) so downstream code can fall back safely.
#[derive(Debug, Clone, Default)]
pub struct RtmpCapabilities {
    /// Video FourCCs the client claims it may send (`av01`, `vp09`, `hvc1`, ...).
    pub video_four_ccs: Vec<[u8; 4]>,
    /// Audio FourCCs the client claims it may send (`Opus`, ...).
    pub audio_four_ccs: Vec<[u8; 4]>,
    /// Whether the client asked for multitrack mode (`multitrack: true`).
    pub multitrack: bool,
    /// Enhanced RTMP v2 `capsEx` flags the client declared ([`CAPS_EX_RECONNECT`] etc.).
    pub caps_ex: u32,
    /// Enhanced RTMP v2 `videoFourCcInfoMap`: FourCC (or `"*"`) -> [`FOUR_CC_CAN_DECODE`] /
    /// `ENCODE` / `FORWARD` flags. Empty when the client sent none.
    pub video_four_cc_info: Vec<(String, u8)>,
    /// Enhanced RTMP v2 `audioFourCcInfoMap`, as above.
    pub audio_four_cc_info: Vec<(String, u8)>,
    /// The `app` the client connected to.
    pub app: Option<String>,
}

/// A high-level media event emitted after AMF negotiation and FLV
/// depacketization.
///
/// This is the bridge point into downstream processing (e.g. feeding audio /
/// video payloads into `tpt-kinetix-pipeline`).
#[derive(Debug, Clone)]
pub enum RtmpMediaEvent {
    /// The client issued `publish` for the given stream key.
    PublishStart {
        /// The stream key / name requested by the publisher.
        stream_key: String,
        /// What the client announced during the capability exchange.
        capabilities: RtmpCapabilities,
    },
    /// A depacketized video tag (SPS/PPS sequence header or coded NALUs).
    Video {
        /// Message timestamp in milliseconds.
        timestamp: u32,
        /// The parsed FLV video tag.
        tag: FlvVideoTag,
    },
    /// A depacketized audio tag (AudioSpecificConfig or coded frames).
    Audio {
        /// Message timestamp in milliseconds.
        timestamp: u32,
        /// The parsed FLV audio tag.
        tag: FlvAudioTag,
    },
    /// HDR/color metadata from an Enhanced-RTMP `Metadata` video packet.
    /// Emitted instead of `Video` for packet kind `Metadata` so ingest layers
    /// can forward it (SEI / `colr` / `mdcv` / `clli`) rather than treat it as
    /// coded frames.
    Hdr {
        /// Message timestamp in milliseconds.
        timestamp: u32,
        /// The HDR metadata payload.
        hdr: flv::HdrMetadata,
    },
    /// The publisher stopped or disconnected.
    PublishStop,
}

/// A handler invoked for every high-level media event on a connection.
///
/// Handlers must be cheap and `Send + Sync` because a clone is shared across all
/// connection tasks.
pub type MediaHandler = Arc<dyn Fn(&RtmpMediaEvent) + Send + Sync>;

/// A per-connection event sink, so state (for example a live packager) can be
/// kept for each publisher separately.
pub type SessionSink = Box<dyn FnMut(&RtmpMediaEvent) + Send>;

/// Creates a [`SessionSink`] for every accepted connection.
pub type SessionFactory = Arc<dyn Fn() -> SessionSink + Send + Sync>;

/// An RTMP ingest server.
pub struct RtmpServer {
    config: RtmpConfig,
    handler: Option<MediaHandler>,
    session: Option<SessionFactory>,
}

impl RtmpServer {
    /// Create a new server with the given configuration.
    pub fn new(config: RtmpConfig) -> Self {
        Self {
            config,
            handler: None,
            session: None,
        }
    }

    /// Register a factory that makes one event sink per connection. Unlike
    /// [`Self::with_handler`] the sink is `FnMut` and sees only its own
    /// connection's events, so it can track a single publisher's state.
    pub fn with_session_factory<F>(mut self, factory: F) -> Self
    where
        F: Fn() -> SessionSink + Send + Sync + 'static,
    {
        self.session = Some(Arc::new(factory));
        self
    }

    /// Register a handler that receives every high-level media event.
    pub fn with_handler<F>(mut self, handler: F) -> Self
    where
        F: Fn(&RtmpMediaEvent) + Send + Sync + 'static,
    {
        self.handler = Some(Arc::new(handler));
        self
    }

    /// Bind and start accepting RTMP connections.
    ///
    /// Each accepted connection is spawned into its own Tokio task. The future
    /// returned by this method runs forever (or until an accept error occurs).
    ///
    /// When [`RtmpConfig::tls`] is set (and the `rtmps` feature is enabled) the
    /// listener terminates TLS first, so OBS in "RTMPS" mode can publish to
    /// the same ingest path.
    pub async fn run(&self) -> anyhow::Result<()> {
        let listener = TcpListener::bind(&self.config.bind_addr).await?;
        self.serve(listener).await
    }

    /// Accept RTMP connections on an already-bound listener.
    pub async fn serve(&self, listener: TcpListener) -> anyhow::Result<()> {
        #[cfg(feature = "rtmps")]
        let tls = self.tls_acceptor()?;
        tracing::info!(addr = ?listener.local_addr().ok(), "RTMP server listening");
        loop {
            let (stream, peer_addr) = listener.accept().await?;
            tracing::info!(%peer_addr, "RTMP client connected");
            let handler = self.handler.clone();
            let session = self.session.as_ref().map(|f| f());
            #[cfg(feature = "rtmps")]
            let tls = tls.clone();
            #[cfg(feature = "rtmps")]
            let use_tls = self.config.tls.is_some();
            tokio::spawn(async move {
                #[cfg(feature = "rtmps")]
                if use_tls {
                    let tls = tls.expect("RTMPS identity checked at serve() entry");
                    match tls.accept(stream).await {
                        Ok(tls_stream) => {
                            let mut tls_stream = tls_stream;
                            if let Err(e) =
                                handle_connection(&mut tls_stream, handler, session).await
                            {
                                tracing::warn!(%peer_addr, error = %e, "RTMPS connection ended");
                            }
                        }
                        Err(e) => tracing::warn!(%peer_addr, error = %e, "RTMPS handshake failed"),
                    }
                    return;
                }
                let mut stream = stream;
                if let Err(e) = handle_connection(&mut stream, handler, session).await {
                    // A dropped/reset connection is expected and recovered by
                    // simply ending this task; the listener keeps accepting.
                    tracing::warn!(%peer_addr, error = %e, "RTMP connection ended");
                }
            });
        }
    }

    /// Build the TLS acceptor for RTMPS from [`RtmpConfig::tls`].
    #[cfg(feature = "rtmps")]
    fn tls_acceptor(&self) -> anyhow::Result<Option<std::sync::Arc<tokio_rustls::TlsAcceptor>>> {
        self.config.tls.as_ref().map(build_tls_acceptor).transpose()
    }
}

/// A TLS acceptor for `id` (shared by RTMPS and the live server's HTTPS / `wss://`).
#[cfg(feature = "rtmps")]
pub(crate) fn build_tls_acceptor(
    id: &RtmpsIdentity,
) -> anyhow::Result<std::sync::Arc<tokio_rustls::TlsAcceptor>> {
    use tokio_rustls::rustls;
    use tokio_rustls::rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer};
    let certs: Vec<CertificateDer<'static>> =
        CertificateDer::pem_slice_iter(&id.cert_chain_pem[..]).collect::<Result<Vec<_>, _>>()?;
    anyhow::ensure!(!certs.is_empty(), "TLS identity has no certificates");
    let key = PrivateKeyDer::from_pem_slice(&id.key_pem[..])
        .map_err(|e| anyhow::anyhow!("TLS identity has no usable private key: {e}"))?;
    // An explicit provider: when another crate in the build enables a second one,
    // rustls cannot choose a process default and `builder()` would panic.
    let cfg = rustls::ServerConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_no_client_auth()
    .with_single_cert(certs, key)?;
    Ok(std::sync::Arc::new(tokio_rustls::TlsAcceptor::from(
        std::sync::Arc::new(cfg),
    )))
}

/// Chunk stream ids we use when writing responses.
const CSID_PROTOCOL: u32 = 2;
const CSID_COMMAND: u32 = 3;

/// Serialize a single Type-0 RTMP chunk carrying a whole message.
///
/// `payload` must be `<= chunk_size`; for the small control/command messages we
/// emit here this always holds against the default 128-byte (or larger) size.
fn write_message(
    csid: u32,
    type_id: u8,
    stream_id: u32,
    timestamp: u32,
    payload: &[u8],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(12 + payload.len());
    // Basic header: fmt=0, csid (assume csid < 64).
    out.push((csid & 0x3F) as u8);
    // Message header (11 bytes).
    let ts = timestamp & 0x00FF_FFFF;
    out.extend_from_slice(&ts.to_be_bytes()[1..]); // 3-byte timestamp
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes()[1..]); // 3-byte length
    out.push(type_id);
    out.extend_from_slice(&stream_id.to_le_bytes()); // 4-byte LE stream id
    out.extend_from_slice(payload);
    out
}

fn window_ack_size(size: u32) -> Vec<u8> {
    write_message(
        CSID_PROTOCOL,
        MessageTypeId::WindowAckSize as u8,
        0,
        0,
        &size.to_be_bytes(),
    )
}

fn set_peer_bandwidth(size: u32, limit_type: u8) -> Vec<u8> {
    let mut body = size.to_be_bytes().to_vec();
    body.push(limit_type);
    write_message(
        CSID_PROTOCOL,
        MessageTypeId::SetPeerBandwidth as u8,
        0,
        0,
        &body,
    )
}

fn set_chunk_size(size: u32) -> Vec<u8> {
    write_message(
        CSID_PROTOCOL,
        MessageTypeId::SetChunkSize as u8,
        0,
        0,
        &size.to_be_bytes(),
    )
}

fn command(values: &[Amf0Value]) -> Vec<u8> {
    let body = amf::encode_all(values);
    write_message(CSID_COMMAND, MessageTypeId::CommandAmf0 as u8, 0, 0, &body)
}

/// The `_result` reply to `connect`.
fn connect_result(transaction_id: f64) -> Vec<u8> {
    command(&connect_result_values(transaction_id))
}

/// The AMF values of the `connect` `_result`: the server's side of the Enhanced
/// RTMP capability exchange (`fourCcList` for v1 clients, `capsEx` and the
/// FourCC info maps for v2). The server repackages what it receives, so it
/// states `CanForward` for the codecs it ingests (AV1, VP9, Opus), and
/// multitrack plus `ModEx` parsing (nanosecond offsets are parsed and ignored).
fn connect_result_values(transaction_id: f64) -> Vec<Amf0Value> {
    let info_map = |codecs: &[&str]| {
        Amf0Value::Object(
            codecs
                .iter()
                .map(|c| {
                    (
                        (*c).to_string(),
                        Amf0Value::Number(f64::from(FOUR_CC_CAN_FORWARD)),
                    )
                })
                .collect(),
        )
    };
    vec![
        Amf0Value::String("_result".into()),
        Amf0Value::Number(transaction_id),
        Amf0Value::Object(vec![
            ("fmsVer".into(), Amf0Value::String("FMS/3,0,1,123".into())),
            ("capabilities".into(), Amf0Value::Number(31.0)),
            // Enhanced RTMP: the codecs this server can ingest.
            (
                "fourCcList".into(),
                Amf0Value::StrictArray(vec![
                    Amf0Value::String("av01".into()),
                    Amf0Value::String("vp09".into()),
                    Amf0Value::String("Opus".into()),
                ]),
            ),
            (
                "capsEx".into(),
                Amf0Value::Number(f64::from(CAPS_EX_MULTITRACK | CAPS_EX_MODEX)),
            ),
            ("videoFourCcInfoMap".into(), info_map(&["av01", "vp09"])),
            ("audioFourCcInfoMap".into(), info_map(&["Opus"])),
        ]),
        Amf0Value::Object(vec![
            ("level".into(), Amf0Value::String("status".into())),
            (
                "code".into(),
                Amf0Value::String("NetConnection.Connect.Success".into()),
            ),
            (
                "description".into(),
                Amf0Value::String("Connection succeeded.".into()),
            ),
        ]),
    ]
}

/// The `_result` reply to `createStream`, returning a stream id.
fn create_stream_result(transaction_id: f64, stream_id: f64) -> Vec<u8> {
    command(&[
        Amf0Value::String("_result".into()),
        Amf0Value::Number(transaction_id),
        Amf0Value::Null,
        Amf0Value::Number(stream_id),
    ])
}

/// The `onStatus` reply confirming `publish`.
fn publish_start_status() -> Vec<u8> {
    command(&[
        Amf0Value::String("onStatus".into()),
        Amf0Value::Number(0.0),
        Amf0Value::Null,
        Amf0Value::Object(vec![
            ("level".into(), Amf0Value::String("status".into())),
            (
                "code".into(),
                Amf0Value::String("NetStream.Publish.Start".into()),
            ),
            (
                "description".into(),
                Amf0Value::String("Publishing started.".into()),
            ),
        ]),
    ])
}

/// Handle a single RTMP client connection.
///
/// Generic over the transport so the same negotiation + chunk reassembly runs
/// over plain TCP and over a TLS-terminated (RTMPS) stream.
async fn handle_connection<S>(
    stream: &mut S,
    handler: Option<MediaHandler>,
    mut session: Option<SessionSink>,
) -> anyhow::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    // 1. RTMP handshake.
    perform_server_handshake(stream).await?;
    tracing::info!("RTMP handshake complete");

    let mut emit = |event: RtmpMediaEvent| {
        if let Some(h) = handler.as_ref() {
            h(&event);
        }
        if let Some(s) = session.as_mut() {
            s(&event);
        }
    };

    // 2. Reassemble the chunk stream into messages and negotiate.
    let mut assembler = ChunkAssembler::new();
    let mut created_stream = false;
    let mut caps = RtmpCapabilities::default();
    let mut buf = [0u8; 8192];

    loop {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            tracing::info!("RTMP client disconnected");
            emit(RtmpMediaEvent::PublishStop);
            break;
        }

        for msg in assembler.push(&buf[..n]) {
            match MessageTypeId::from_u8(msg.message_type_id) {
                Some(MessageTypeId::SetChunkSize) => {
                    if msg.payload.len() >= 4 {
                        let size = u32::from_be_bytes([
                            msg.payload[0],
                            msg.payload[1],
                            msg.payload[2],
                            msg.payload[3],
                        ]);
                        assembler.set_chunk_size(size);
                        tracing::debug!(size, "RTMP chunk size updated");
                    }
                }
                Some(MessageTypeId::CommandAmf0) => {
                    let mut events = Vec::new();
                    handle_command(stream, &msg, &mut created_stream, &mut caps, &mut events)
                        .await?;
                    events.into_iter().for_each(&mut emit);
                }
                // One message can carry several tracks (Enhanced RTMP Multitrack):
                // each becomes its own event, tagged with its `track_id`.
                Some(MessageTypeId::Video) => match flv::parse_video_tags(&msg.payload) {
                    Ok(tags) => {
                        for tag in tags {
                            if let Some(hdr) = tag.hdr.clone() {
                                emit(RtmpMediaEvent::Hdr {
                                    timestamp: msg.timestamp,
                                    hdr,
                                });
                            } else {
                                emit(RtmpMediaEvent::Video {
                                    timestamp: msg.timestamp,
                                    tag,
                                });
                            }
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "bad FLV video tag"),
                },
                Some(MessageTypeId::Audio) => match flv::parse_audio_tags(&msg.payload) {
                    Ok(tags) => {
                        for tag in tags {
                            emit(RtmpMediaEvent::Audio {
                                timestamp: msg.timestamp,
                                tag,
                            });
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "bad FLV audio tag"),
                },
                _ => {
                    tracing::trace!(type_id = msg.message_type_id, "ignoring RTMP message");
                }
            }
        }
    }

    Ok(())
}

/// Read an Enhanced-RTMP v2 `[audio|video]FourCcInfoMap` from a `connect` command
/// object: an object of FourCC (or `"*"`) -> capability flags.
fn four_cc_info_map(obj: &Amf0Value, key: &str) -> Vec<(String, u8)> {
    let Some(Amf0Value::Object(props) | Amf0Value::EcmaArray(props)) = obj.get(key) else {
        return Vec::new();
    };
    props
        .iter()
        .filter_map(|(k, v)| Some((k.clone(), v.as_f64()? as u8)))
        .collect()
}

/// Read an Enhanced-RTMP FourCC list (`fourCcList` / `audioFourCcList`) from a
/// `connect` command object: a strict array of 4-char strings.
fn four_cc_list(obj: &Amf0Value, key: &str) -> Vec<[u8; 4]> {
    let Some(list) = obj.get(key) else {
        return Vec::new();
    };
    let items = match list {
        Amf0Value::StrictArray(items) => items.as_slice(),
        _ => return Vec::new(),
    };
    items
        .iter()
        .filter_map(|v| v.as_str())
        .filter_map(|s| {
            let b = s.as_bytes();
            (b.len() == 4).then(|| [b[0], b[1], b[2], b[3]])
        })
        .collect()
}

/// Process a single AMF0 command message and send the appropriate response.
async fn handle_command<S>(
    stream: &mut S,
    msg: &RtmpMessage,
    created_stream: &mut bool,
    caps: &mut RtmpCapabilities,
    emit: &mut Vec<RtmpMediaEvent>,
) -> anyhow::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let values = match amf::decode_all(&msg.payload) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "failed to decode AMF0 command");
            return Ok(());
        }
    };

    let command_name = values.first().and_then(|v| v.as_str()).unwrap_or("");
    let transaction_id = values.get(1).and_then(|v| v.as_f64()).unwrap_or(0.0);

    match command_name {
        "connect" => {
            tracing::info!("RTMP connect");
            // Record the v2 capability exchange: the `connect` command object
            // (values[2]) carries the app name plus optional Enhanced-RTMP
            // codec lists (`fourCcList`, `audioFourCcList`, `multitrack`).
            if let Some(obj) = values.get(2) {
                caps.app = obj.get("app").and_then(|v| v.as_str()).map(str::to_string);
                caps.video_four_ccs = four_cc_list(obj, "fourCcList");
                caps.audio_four_ccs = four_cc_list(obj, "audioFourCcList");
                caps.multitrack = obj
                    .get("multitrack")
                    .and_then(|v| match v {
                        Amf0Value::Boolean(b) => Some(*b),
                        Amf0Value::Number(n) => Some(*n != 0.0),
                        _ => None,
                    })
                    .unwrap_or(false);
                caps.caps_ex = obj
                    .get("capsEx")
                    .and_then(Amf0Value::as_f64)
                    .map_or(0, |n| n as u32);
                caps.video_four_cc_info = four_cc_info_map(obj, "videoFourCcInfoMap");
                caps.audio_four_cc_info = four_cc_info_map(obj, "audioFourCcInfoMap");
                // A v2 client may send only the info maps: what it can encode is
                // what it may send, so fold those into the plain lists.
                for (list, info) in [
                    (&mut caps.video_four_ccs, &caps.video_four_cc_info),
                    (&mut caps.audio_four_ccs, &caps.audio_four_cc_info),
                ] {
                    for (key, flags) in info {
                        let b = key.as_bytes();
                        if b.len() == 4 && flags & FOUR_CC_CAN_ENCODE != 0 {
                            let cc = [b[0], b[1], b[2], b[3]];
                            if !list.contains(&cc) {
                                list.push(cc);
                            }
                        }
                    }
                }
            }
            // Standard control message sequence, then _result.
            stream.write_all(&window_ack_size(2_500_000)).await?;
            stream.write_all(&set_peer_bandwidth(2_500_000, 2)).await?;
            stream.write_all(&set_chunk_size(4096)).await?;
            stream.write_all(&connect_result(transaction_id)).await?;
            stream.flush().await?;
        }
        "createStream" => {
            tracing::info!("RTMP createStream");
            *created_stream = true;
            stream
                .write_all(&create_stream_result(transaction_id, 1.0))
                .await?;
            stream.flush().await?;
        }
        "publish" => {
            // publish(transaction, null, streamKey, publishType)
            let stream_key = values
                .get(3)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            tracing::info!(%stream_key, "RTMP publish");
            stream.write_all(&publish_start_status()).await?;
            stream.flush().await?;
            emit.push(RtmpMediaEvent::PublishStart {
                stream_key,
                capabilities: caps.clone(),
            });
        }
        "deleteStream" | "FCUnpublish" | "closeStream" => {
            tracing::info!(command_name, "RTMP publish teardown");
            emit.push(RtmpMediaEvent::PublishStop);
        }
        "releaseStream" | "FCPublish" | "_checkbw" => {
            // Acknowledge with an empty _result so common encoders proceed.
            stream
                .write_all(&command(&[
                    Amf0Value::String("_result".into()),
                    Amf0Value::Number(transaction_id),
                    Amf0Value::Null,
                    Amf0Value::Null,
                ]))
                .await?;
            stream.flush().await?;
        }
        other => {
            tracing::debug!(command = other, "unhandled RTMP command");
        }
    }

    Ok(())
}
