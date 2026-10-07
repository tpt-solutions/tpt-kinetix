//! WHIP (WebRTC-HTTP ingest, RFC 9725): browsers and encoders publish **VP9 + Opus**
//! over WebRTC with one HTTP request.
//!
//! `POST /whip/<key>` with `Content-Type: application/sdp` and an SDP offer is
//! answered `201 Created` with the SDP answer and a `Location` for the session;
//! `DELETE` on that location ends it. Authentication is the usual publish policy
//! (`Authorization: Bearer <token>` or `?token=`). The media then flows over ICE /
//! DTLS / SRTP on a UDP port of this server and ends up in the same live HLS / DASH
//! presentation as every other ingest.
//!
//! The WebRTC stack is [`str0m`], sans-I/O and with its pure-Rust crypto (no
//! OpenSSL, no C), driven here by a tokio UDP socket per session. It is behind the
//! `whip` cargo feature because of its dependency tree; without it the endpoint
//! answers `501`.
//!
//! Frames are handed to the same [`RtmpLiveSession`](crate::rtmp_live::RtmpLiveSession)
//! the RTMP ingest uses (as synthetic events), so authentication, limits, the idle
//! timeout, recording, reconnect handling and multi-rendition packaging all apply
//! unchanged.
//!
//! Limits of this first version: VP9 and Opus only (AV1 over WebRTC needs the
//! sequence header turned into an `av1C`), non-trickle ICE (the answer carries the
//! candidates), no `PATCH`, and audio/video are aligned by arrival time of each
//! track's first packet rather than RTCP sender reports, so A/V sync can be off by
//! the start-up jitter (tens of milliseconds).

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use tokio::sync::oneshot;

/// Where WebRTC media is received.
#[derive(Clone, Debug, Default)]
pub struct WhipConfig {
    /// Addresses advertised to the publisher as ICE candidates. Empty means
    /// loopback plus the machine's primary address; set the public address when the
    /// server sits behind NAT.
    pub candidate_ips: Vec<IpAddr>,
    /// UDP port media is received on; `0` picks a free one per session.
    pub udp_port: u16,
}

/// Live WHIP sessions, so a `DELETE` can end one.
#[derive(Clone, Default)]
pub(crate) struct WhipSessions(Arc<Mutex<HashMap<String, oneshot::Sender<()>>>>);

impl WhipSessions {
    pub(crate) fn insert(&self, id: String, stop: oneshot::Sender<()>) {
        self.0.lock().unwrap().insert(id, stop);
    }

    /// Ends the session; `false` when there is no such session.
    pub(crate) fn stop(&self, id: &str) -> bool {
        match self.0.lock().unwrap().remove(id) {
            Some(tx) => {
                let _ = tx.send(());
                true
            }
            None => false,
        }
    }

    pub(crate) fn forget(&self, id: &str) {
        self.0.lock().unwrap().remove(id);
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.0.lock().unwrap().len()
    }
}

/// Whether a VP9 frame is a key frame (uncompressed header: `frame_type == 0`).
pub(crate) fn vp9_is_key(frame: &[u8]) -> bool {
    let Some(&b0) = frame.first() else {
        return false;
    };
    // frame_marker(2) must be 2; profile = low | high << 1; profile 3 has a reserved bit.
    if b0 >> 6 != 2 {
        return false;
    }
    let profile = ((b0 >> 5) & 1) | (((b0 >> 4) & 1) << 1);
    let mut bit = 4; // bits consumed so far (marker + profile low/high)
    if profile == 3 {
        bit += 1; // reserved_zero
    }
    let at = |bit: usize| (frame.get(bit / 8).copied().unwrap_or(0) >> (7 - bit % 8)) & 1;
    if at(bit) == 1 {
        return false; // show_existing_frame
    }
    bit += 1;
    at(bit) == 0 // frame_type: 0 = KEY_FRAME
}

/// A minimal `OpusHead` identification header for a WebRTC Opus stream (always
/// 48 kHz; WebRTC carries no pre-skip, so the usual 312 is used).
pub(crate) fn opus_head(channels: u8) -> Vec<u8> {
    let mut h = b"OpusHead".to_vec();
    h.push(1); // version
    h.push(channels);
    h.extend_from_slice(&312u16.to_le_bytes()); // pre-skip
    h.extend_from_slice(&48_000u32.to_le_bytes());
    h.extend_from_slice(&0i16.to_le_bytes()); // output gain
    h.push(0); // mapping family
    h
}

#[cfg(feature = "whip")]
pub(crate) use engine::start_session;

#[cfg(feature = "whip")]
mod engine {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::time::{Duration, Instant};

    use anyhow::{anyhow, Context, Result};
    use str0m::change::SdpOffer;
    use str0m::format::Codec;
    use str0m::media::{KeyframeRequestKind, MediaKind, Mid};
    use str0m::net::{Protocol, Receive};
    use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc, RtcConfig};
    use tokio::net::UdpSocket;
    use tokio::sync::oneshot;

    use super::{opus_head, vp9_is_key, WhipConfig, WhipSessions};
    use crate::live::LiveServer;
    use crate::rtmp::flv::{
        AacPacketType, AvcPacketType, ExVideoPacketType, FlvAudioCodec, FlvAudioTag, FlvFrameType,
        FlvVideoCodec, FlvVideoTag,
    };
    use crate::rtmp::server::{RtmpCapabilities, RtmpMediaEvent};
    use crate::rtmp_live::RtmpLiveSession;

    /// The machine's primary outbound address (no packet is sent).
    fn primary_ip() -> Option<IpAddr> {
        let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
        s.connect("192.0.2.1:9").ok()?;
        s.local_addr().ok().map(|a| a.ip())
    }

    /// Accepts the offer and starts the media task; returns the SDP answer.
    ///
    /// `publish_key` is the stream key as the publish policy wants it (a token, when
    /// one was presented, rides along as `name?token=...`).
    pub(crate) async fn start_session(
        server: LiveServer,
        cfg: &WhipConfig,
        sessions: WhipSessions,
        session_id: String,
        publish_key: String,
        offer_sdp: &str,
    ) -> Result<String> {
        static CRYPTO: std::sync::Once = std::sync::Once::new();
        CRYPTO.call_once(|| str0m::crypto::from_feature_flags().install_process_default());

        let offer =
            SdpOffer::from_sdp_string(offer_sdp).map_err(|e| anyhow!("bad SDP offer: {e}"))?;
        let mut rtc = RtcConfig::new()
            .clear_codecs()
            .enable_opus(true, false)
            .enable_vp9(true)
            .build(Instant::now());

        // One socket per advertised address, all on the same port. A single socket
        // bound to 0.0.0.0 would not say which local address a datagram arrived on,
        // and the ICE agent needs that to match it to a candidate.
        let mut ips = cfg.candidate_ips.clone();
        if ips.is_empty() {
            ips.push(IpAddr::V4(Ipv4Addr::LOCALHOST));
            ips.extend(primary_ip());
        }
        let mut sockets: Vec<std::sync::Arc<UdpSocket>> = Vec::new();
        let mut port = cfg.udp_port;
        for ip in &ips {
            let socket = UdpSocket::bind(SocketAddr::new(*ip, port))
                .await
                .with_context(|| format!("bind the WebRTC UDP socket on {ip}:{port}"))?;
            port = socket.local_addr()?.port();
            let c = Candidate::host(socket.local_addr()?, "udp")
                .map_err(|e| anyhow!("candidate: {e}"))?;
            rtc.add_local_candidate(c);
            sockets.push(std::sync::Arc::new(socket));
        }
        let answer = rtc
            .sdp_api()
            .accept_offer(offer)
            .map_err(|e| anyhow!("offer rejected: {e}"))?;
        let answer_sdp = answer.to_sdp_string();

        let (stop_tx, stop_rx) = oneshot::channel();
        sessions.insert(session_id.clone(), stop_tx);
        tokio::spawn(async move {
            if let Err(e) = run(server, publish_key, rtc, sockets, stop_rx).await {
                tracing::warn!(error = %e, "WHIP session ended with an error");
            }
            sessions.forget(&session_id);
        });
        Ok(answer_sdp)
    }

    /// Per-track mapping of RTP time onto the session timeline (milliseconds).
    struct Origin {
        /// Timeline offset (ms) of the track's first packet: its arrival time.
        base_ms: f64,
        /// RTP media time (ms) of that first packet.
        rtp_ms: f64,
    }

    impl Origin {
        fn map(&self, rtp_ms: f64) -> u32 {
            (self.base_ms + (rtp_ms - self.rtp_ms)).max(0.0) as u32
        }
    }

    async fn run(
        server: LiveServer,
        publish_key: String,
        mut rtc: Rtc,
        sockets: Vec<std::sync::Arc<UdpSocket>>,
        mut stop: oneshot::Receiver<()>,
    ) -> Result<()> {
        // Every socket feeds one channel with (payload, source, the local address it
        // arrived on); the tasks end when this function returns and drops the receiver.
        let (dgram_tx, mut dgram_rx) =
            tokio::sync::mpsc::channel::<(Vec<u8>, SocketAddr, SocketAddr)>(256);
        let mut readers = Vec::new();
        for socket in &sockets {
            let (socket, tx) = (socket.clone(), dgram_tx.clone());
            let local = socket.local_addr()?;
            readers.push(tokio::spawn(async move {
                let mut buf = vec![0u8; 2048];
                loop {
                    match socket.recv_from(&mut buf).await {
                        Ok((n, source)) => {
                            if tx.send((buf[..n].to_vec(), source, local)).await.is_err() {
                                return;
                            }
                        }
                        // Windows reports an ICMP "port unreachable" for an earlier send (an
                        // ICE check to a candidate nobody listens on) as an error on the
                        // *next* receive. It says nothing about the session: keep going.
                        Err(e)
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::ConnectionReset
                                    | std::io::ErrorKind::ConnectionAborted
                            ) => {}
                        Err(_) => return,
                    }
                }
            }));
        }
        drop(dgram_tx);
        let mut session = RtmpLiveSession::new(server.clone());
        session.on_event(&RtmpMediaEvent::PublishStart {
            stream_key: publish_key,
            capabilities: RtmpCapabilities::default(),
        });
        let began = Instant::now();
        let mut kinds: std::collections::HashMap<Mid, MediaKind> = Default::default();
        let (mut video_origin, mut audio_origin): (Option<Origin>, Option<Origin>) = (None, None);
        // WebRTC encoders send a key frame at the start and then only on request, but
        // segments are cut at key frames: ask for one every segment duration (PLI), as
        // any WebRTC-to-HLS gateway does.
        let key_every = Duration::from_secs_f64(server.segment_seconds().max(0.5));
        let mut video_mid: Option<(Mid, Option<str0m::media::Rid>)> = None;
        let mut last_key = Instant::now();
        let mut last_pli = Instant::now() - Duration::from_secs(10);

        let result: Result<()> = loop {
            if !rtc.is_alive() {
                break Ok(());
            }
            let timeout = match rtc.poll_output().map_err(|e| anyhow!("{e}"))? {
                Output::Timeout(t) => t,
                Output::Transmit(t) => {
                    // Out through the socket the ICE agent chose (its source address).
                    let out = sockets
                        .iter()
                        .find(|s| s.local_addr().is_ok_and(|a| a == t.source))
                        .or_else(|| {
                            sockets
                                .iter()
                                .find(|s| s.local_addr().is_ok_and(|a| a.ip() == t.source.ip()))
                        })
                        .or(sockets.first());
                    if let Some(out) = out {
                        // A send error (unreachable candidate) is not fatal to the session.
                        let _ = out.send_to(&t.contents, t.destination).await;
                    }
                    continue;
                }
                Output::Event(e) => {
                    if !matches!(e, Event::MediaData(_)) {
                        tracing::trace!("WHIP event: {e:?}");
                    }
                    match e {
                        Event::IceConnectionStateChange(IceConnectionState::Disconnected) => {
                            break Ok(())
                        }
                        Event::MediaAdded(m) => {
                            kinds.insert(m.mid, m.kind);
                        }
                        Event::MediaData(d) => {
                            let kind = kinds.get(&d.mid).copied();
                            let codec = d.params.spec().codec;
                            let rtp_ms = d.time.as_seconds() * 1000.0;
                            let arrival_ms = d
                                .network_time
                                .saturating_duration_since(began)
                                .as_secs_f64()
                                * 1000.0;
                            match (kind, codec) {
                                (Some(MediaKind::Video), Codec::Vp9) => {
                                    let origin = video_origin.get_or_insert_with(|| {
                                        // First video packet: declare the track. The record is
                                        // empty on purpose, so the configuration is derived
                                        // from the first key frame.
                                        session.on_event(&RtmpMediaEvent::Video {
                                            timestamp: arrival_ms as u32,
                                            tag: video_tag(true, true, Vec::new()),
                                        });
                                        Origin {
                                            base_ms: arrival_ms,
                                            rtp_ms,
                                        }
                                    });
                                    let key = vp9_is_key(&d.data);
                                    video_mid = Some((d.mid, d.rid));
                                    if key {
                                        last_key = Instant::now();
                                    }
                                    session.on_event(&RtmpMediaEvent::Video {
                                        timestamp: origin.map(rtp_ms),
                                        tag: video_tag(key, false, d.data.to_vec()),
                                    });
                                    if !d.contiguous && last_pli.elapsed() > Duration::from_secs(1)
                                    {
                                        // A gap in the RTP stream: ask for a key frame.
                                        last_pli = Instant::now();
                                        if let Some(rx) =
                                            rtc.direct_api().stream_rx_by_mid(d.mid, d.rid)
                                        {
                                            rx.request_keyframe(KeyframeRequestKind::Pli);
                                        }
                                    }
                                }
                                (Some(MediaKind::Audio), Codec::Opus) => {
                                    let origin = audio_origin.get_or_insert_with(|| {
                                        session.on_event(&RtmpMediaEvent::Audio {
                                            timestamp: arrival_ms as u32,
                                            tag: FlvAudioTag {
                                                codec: FlvAudioCodec::Opus,
                                                aac_packet_type: AacPacketType::SequenceHeader,
                                                track_id: 0,
                                                data: opus_head(2),
                                            },
                                        });
                                        Origin {
                                            base_ms: arrival_ms,
                                            rtp_ms,
                                        }
                                    });
                                    session.on_event(&RtmpMediaEvent::Audio {
                                        timestamp: origin.map(rtp_ms),
                                        tag: FlvAudioTag {
                                            codec: FlvAudioCodec::Opus,
                                            aac_packet_type: AacPacketType::Raw,
                                            track_id: 0,
                                            data: d.data.to_vec(),
                                        },
                                    });
                                }
                                _ => {} // another codec was negotiated away; nothing to do
                            }
                        }
                        _ => {}
                    }
                    continue;
                }
            };
            if let Some((mid, rid)) = video_mid {
                if last_key.elapsed() >= key_every && last_pli.elapsed() >= key_every / 2 {
                    last_pli = Instant::now();
                    if let Some(rx) = rtc.direct_api().stream_rx_by_mid(mid, rid) {
                        rx.request_keyframe(KeyframeRequestKind::Pli);
                    }
                    continue; // the request is output to be polled
                }
            }
            let wait = timeout
                .saturating_duration_since(Instant::now())
                // Wake in time for the next key frame request.
                .min(key_every / 4);
            tokio::select! {
                _ = &mut stop => break Ok(()),
                d = dgram_rx.recv() => {
                    let Some((data, source, local)) = d else { break Ok(()) };
                    let contents = data.as_slice().try_into().map_err(|e| anyhow!("{e}"))?;
                    rtc.handle_input(Input::Receive(
                        Instant::now(),
                        Receive {
                            proto: Protocol::Udp,
                            source,
                            destination: local,
                            contents,
                        },
                    ))
                    .map_err(|e| anyhow!("{e}"))?;
                }
                _ = tokio::time::sleep(wait) => {
                    rtc.handle_input(Input::Timeout(Instant::now())).map_err(|e| anyhow!("{e}"))?;
                }
            }
        };
        for r in readers {
            r.abort();
        }
        session.on_event(&RtmpMediaEvent::PublishStop);
        result
    }

    fn video_tag(key: bool, sequence_start: bool, data: Vec<u8>) -> FlvVideoTag {
        FlvVideoTag {
            frame_type: if key {
                FlvFrameType::KeyFrame
            } else {
                FlvFrameType::InterFrame
            },
            codec: FlvVideoCodec::Vp9,
            avc_packet_type: if sequence_start {
                AvcPacketType::SequenceHeader
            } else {
                AvcPacketType::Nalu
            },
            composition_time: 0,
            packet_kind: if sequence_start {
                ExVideoPacketType::SequenceStart
            } else {
                ExVideoPacketType::CodedFramesX
            },
            hdr: None,
            track_id: 0,
            data,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vp9_key_frame_detection() {
        // frame_marker 2, profile 0, show_existing 0, frame_type 0 (key): 10 0 0 0 0 .. = 0x80
        assert!(vp9_is_key(&[0x80, 0x00]));
        // frame_type 1 (non-key): 10 0 0 0 1 -> 0x84
        assert!(!vp9_is_key(&[0x84, 0x00]));
        // show_existing_frame = 1: 10 0 0 1 -> 0x88
        assert!(!vp9_is_key(&[0x88]));
        // wrong frame marker
        assert!(!vp9_is_key(&[0x00]));
        assert!(!vp9_is_key(&[]));
        // profile 1 (low bit set): 10 1 0 0 0 .. key
        assert!(vp9_is_key(&[0xA0]));
        // profile 3 carries a reserved bit before show_existing_frame: 10 1 1 0 0 0 = 0xB0
        assert!(vp9_is_key(&[0xB0]));
    }

    #[test]
    fn opus_head_layout() {
        let h = opus_head(2);
        assert_eq!(&h[..8], b"OpusHead");
        assert_eq!((h[8], h[9]), (1, 2));
        assert_eq!(u16::from_le_bytes([h[10], h[11]]), 312);
        assert_eq!(u32::from_le_bytes([h[12], h[13], h[14], h[15]]), 48_000);
        assert_eq!(h.len(), 19);
    }

    #[test]
    fn sessions_can_be_stopped_once() {
        let s = WhipSessions::default();
        let (tx, mut rx) = oneshot::channel();
        s.insert("k/1".into(), tx);
        assert_eq!(s.len(), 1);
        assert!(s.stop("k/1"));
        assert!(rx.try_recv().is_ok());
        assert!(!s.stop("k/1"));
        assert_eq!(s.len(), 0);
    }
}
