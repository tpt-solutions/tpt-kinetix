//! Enhanced RTMP ingest for the royalty-free codecs, feeding live fMP4 HLS.
//!
//! An RTMP publisher (an Enhanced-RTMP-capable encoder such as OBS 30.2+, or a
//! test client) sends **AV1** (`av01`) or **VP9** (`vp09`) video and **Opus**
//! audio. Each connection gets an [`RtmpLiveSession`] that turns its events into
//! tracks and frames for a [`LivePackager`](tpt_kinetix_package::LivePackager)
//! registered under the stream key, so the presentation is served by the same
//! HTTP endpoints as WebM ingest (`/<key>/master.m3u8`, ...).
//!
//! Tracks are announced to the packager once the video configuration is known
//! and either the audio configuration has arrived or a second of media has gone
//! by without any (a video-only publisher). Frames that arrive earlier wait in a
//! bounded queue.

use tpt_kinetix_core::codec::CodecId;
use tpt_kinetix_core::stream::StreamInfo;
use tpt_kinetix_demux::rfconfig::{
    av1_dimensions, opus_head_to_dops, vp9_config_from_frame, vpcc_record,
};

use crate::live::{LiveServer, Shared};
use crate::rtmp::flv::{AacPacketType, AvcPacketType, FlvAudioCodec, FlvVideoCodec};
use crate::rtmp::server::{RtmpMediaEvent, SessionSink};

/// Media waiting for the tracks to be announced.
const MAX_QUEUED_FRAMES: usize = 2000;
/// How long to wait for an audio configuration before assuming video only.
const AUDIO_WAIT_MS: u32 = 1000;

struct Queued {
    audio: bool,
    pts_ms: i64,
    key: bool,
    data: Vec<u8>,
}

/// Activity bookkeeping shared with the idle watchdog of one publish.
struct Watch {
    began: std::time::Instant,
    /// Milliseconds since `began` of the last event from the publisher.
    last_ms: std::sync::atomic::AtomicU64,
    /// The watchdog cut the publish off; later frames are ignored.
    cut: std::sync::atomic::AtomicBool,
    /// The publish ended normally; the watchdog stops.
    done: std::sync::atomic::AtomicBool,
}

impl Watch {
    fn new() -> Self {
        Self {
            began: std::time::Instant::now(),
            last_ms: std::sync::atomic::AtomicU64::new(0),
            cut: std::sync::atomic::AtomicBool::new(false),
            done: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn touch(&self) {
        self.last_ms.store(
            self.began.elapsed().as_millis() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    fn idle_for(&self) -> std::time::Duration {
        let last = self.last_ms.load(std::sync::atomic::Ordering::Relaxed);
        self.began
            .elapsed()
            .saturating_sub(std::time::Duration::from_millis(last))
    }
}

/// The state of one RTMP publisher.
pub struct RtmpLiveSession {
    watch: std::sync::Arc<Watch>,
    server: LiveServer,
    live: Option<Shared>,
    /// The stream key being published, for recording.
    key: String,
    /// Holds this publisher's slot in the server's live-stream set.
    guard: std::sync::Arc<std::sync::Mutex<Option<crate::policy::ActiveGuard>>>,
    started: std::time::Instant,
    received: u64,
    video: Option<StreamInfo>,
    audio: Option<StreamInfo>,
    /// VP9 needs a key frame to learn the picture size (and, without a
    /// sequence-start record, its configuration).
    vp9_needs_frame: bool,
    announced: bool,
    has_audio_track: bool,
    first_ts: Option<u32>,
    queue: Vec<Queued>,
    warned_codec: bool,
}

impl RtmpLiveSession {
    /// A session that publishes into `server`.
    pub fn new(server: LiveServer) -> Self {
        Self {
            watch: std::sync::Arc::new(Watch::new()),
            server,
            live: None,
            key: String::new(),
            guard: Default::default(),
            started: std::time::Instant::now(),
            received: 0,
            video: None,
            audio: None,
            vp9_needs_frame: false,
            announced: false,
            has_audio_track: false,
            first_ts: None,
            queue: Vec::new(),
            warned_codec: false,
        }
    }

    /// Wraps this session as an event sink for [`crate::RtmpServer::with_session_factory`].
    pub fn into_sink(mut self) -> SessionSink {
        Box::new(move |e| self.on_event(e))
    }

    /// Handles one event from the connection.
    pub fn on_event(&mut self, event: &RtmpMediaEvent) {
        self.watch.touch();
        if self.watch.cut.load(std::sync::atomic::Ordering::Relaxed) {
            return; // the idle watchdog already ended this publish
        }
        let ts = match event {
            RtmpMediaEvent::PublishStart {
                stream_key,
                capabilities,
            } => {
                tracing::info!(
                    four_ccs = ?capabilities.video_four_ccs
                        .iter().map(|c| String::from_utf8_lossy(c).into_owned())
                        .collect::<Vec<_>>(),
                    multitrack = capabilities.multitrack,
                    "RTMP capabilities"
                );
                self.start(stream_key);
                return;
            }
            RtmpMediaEvent::PublishStop => {
                self.stop();
                return;
            }
            // HDR metadata is informational for ingest: log it once per
            // publish so an HDR OBS feed is visible, but do not feed it to
            // the packager as coded frames.
            RtmpMediaEvent::Hdr { hdr, .. } => {
                if !self.warned_codec {
                    tracing::info!(bytes = hdr.raw.len(), "RTMP HDR metadata");
                }
                return;
            }
            // Multitrack selection is accepted and logged; the live packager
            // carries a single video track, so frames keep flowing there.
            RtmpMediaEvent::Multitrack { track_number } => {
                tracing::info!(track_number, "RTMP multitrack select");
                return;
            }
            RtmpMediaEvent::Video { timestamp, tag } => {
                if self.live.is_none() {
                    return;
                }
                let codec = match tag.codec {
                    FlvVideoCodec::Av1 => CodecId::Av1,
                    FlvVideoCodec::Vp9 => CodecId::Vp9,
                    _ => return self.unsupported("video codec (only AV1 and VP9 are ingested)"),
                };
                self.first_ts.get_or_insert(*timestamp);
                if tag.is_sequence_header() {
                    self.video_config(codec, &tag.data);
                } else if tag.avc_packet_type == AvcPacketType::Nalu && !tag.data.is_empty() {
                    self.video_frame(codec, *timestamp, tag.frame_type.is_keyframe(), &tag.data);
                }
                *timestamp
            }
            RtmpMediaEvent::Audio { timestamp, tag } => {
                if self.live.is_none() {
                    return;
                }
                if tag.codec != FlvAudioCodec::Opus {
                    return self.unsupported("audio codec (only Opus is ingested)");
                }
                self.first_ts.get_or_insert(*timestamp);
                match tag.aac_packet_type {
                    AacPacketType::SequenceHeader => self.audio_config(&tag.data),
                    AacPacketType::Raw if !tag.data.is_empty() => {
                        self.enqueue(true, *timestamp, true, tag.data.clone());
                    }
                    _ => {}
                }
                *timestamp
            }
        };
        self.maybe_announce(ts);
    }

    fn start(&mut self, key: &str) {
        self.stop();
        // The key may carry a publish token (`name?token=...`); only the name is kept.
        let (live, key, guard) = match self.server.begin(key) {
            Some((key, live, guard)) => (Some(live), key, Some(guard)),
            None => (None, key.split('?').next().unwrap_or_default().to_string(), None),
        };
        *self = Self {
            live,
            key,
            guard: std::sync::Arc::new(std::sync::Mutex::new(guard)),
            ..Self::new(self.server.clone())
        };
        self.spawn_idle_watchdog();
    }

    /// Ends a publisher that stops sending (a dead encoder or a cut cable keeps
    /// the TCP connection open), when the policy sets an idle timeout: the
    /// presentation is completed and the publisher's slot released. The
    /// connection itself is left to the RTMP server.
    fn spawn_idle_watchdog(&self) {
        use std::sync::atomic::Ordering::Relaxed;
        let (Some(idle), Some(live), Ok(rt)) = (
            self.server.idle_timeout(),
            self.live.clone(),
            tokio::runtime::Handle::try_current(),
        ) else {
            return;
        };
        let (watch, guard, server, key) = (
            self.watch.clone(),
            self.guard.clone(),
            self.server.clone(),
            self.key.clone(),
        );
        rt.spawn(async move {
            let tick = (idle / 4).clamp(
                std::time::Duration::from_millis(50),
                std::time::Duration::from_secs(1),
            );
            loop {
                tokio::time::sleep(tick).await;
                if watch.done.load(Relaxed) {
                    return;
                }
                if watch.idle_for() >= idle {
                    watch.cut.store(true, Relaxed);
                    tracing::warn!(%key, "RTMP publisher idle for too long; ending the publish");
                    let _ = live.lock().unwrap().finish();
                    server.record(&key, &live, true);
                    server.count_idle_cut();
                    *guard.lock().unwrap() = None;
                    return;
                }
            }
        });
    }

    fn stop(&mut self) {
        if self.live.is_some() {
            // A publish that ended before the wait for audio elapsed still has media.
            self.maybe_announce(u32::MAX);
        }
        if let Some(live) = self.live.take() {
            let _ = live.lock().unwrap().finish();
            self.server.record(&self.key, &live, true);
            tracing::info!("RTMP publish finished");
        }
        self.watch
            .done
            .store(true, std::sync::atomic::Ordering::Relaxed);
        *self.guard.lock().unwrap() = None;
    }

    fn unsupported(&mut self, what: &str) {
        if !self.warned_codec {
            self.warned_codec = true;
            tracing::warn!("ignoring an unsupported {what}");
        }
    }

    fn video_config(&mut self, codec: CodecId, record: &[u8]) {
        let mut info = StreamInfo::new(0, codec, 90_000);
        match codec {
            CodecId::Av1 => {
                info.extradata = record.to_vec();
                if let Some((w, h)) = av1_dimensions(record) {
                    (info.width, info.height) = (w, h);
                }
            }
            _ => {
                // The record is a `vpcC` payload, with or without its version/flags word.
                info.extradata = if record.len() >= 4 && record[..4] == [1, 0, 0, 0] {
                    record.to_vec()
                } else {
                    [&[1u8, 0, 0, 0][..], record].concat()
                };
                self.vp9_needs_frame = true;
            }
        }
        self.video = Some(info);
    }

    fn audio_config(&mut self, head: &[u8]) {
        let channels = head.get(9).copied().unwrap_or(2);
        let Some(dops) = opus_head_to_dops(head, channels) else {
            return tracing::warn!("unusable Opus identification header");
        };
        let mut info = StreamInfo::new(1, CodecId::Opus, 48_000);
        info.channels = u16::from(channels);
        info.sample_rate = 48_000;
        info.extradata = dops;
        self.audio = Some(info);
    }

    fn video_frame(&mut self, codec: CodecId, ts: u32, key: bool, data: &[u8]) {
        if self.vp9_needs_frame && key {
            if let Some(cfg) = vp9_config_from_frame(data, 0, 0) {
                let info = self
                    .video
                    .get_or_insert_with(|| StreamInfo::new(0, codec, 90_000));
                (info.width, info.height) = (cfg.width, cfg.height);
                if info.extradata.is_empty() {
                    info.extradata = vpcc_record(&cfg);
                }
                self.vp9_needs_frame = false;
            }
        }
        self.enqueue(false, ts, key, data.to_vec());
    }

    fn enqueue(&mut self, audio: bool, ts: u32, key: bool, data: Vec<u8>) {
        let q = Queued {
            audio,
            pts_ms: i64::from(ts),
            key,
            data,
        };
        if self.announced {
            if !audio || self.has_audio_track {
                self.deliver(q);
            }
        } else if self.queue.len() < MAX_QUEUED_FRAMES {
            self.queue.push(q);
        }
    }

    fn maybe_announce(&mut self, ts: u32) {
        if self.announced || self.live.is_none() {
            return;
        }
        let video_ready = self
            .video
            .as_ref()
            .is_some_and(|v| !self.vp9_needs_frame || v.codec != CodecId::Vp9);
        if !video_ready {
            return;
        }
        let waited = ts.saturating_sub(self.first_ts.unwrap_or(ts)) >= AUDIO_WAIT_MS;
        if self.audio.is_none() && !waited {
            return;
        }
        let mut tracks = vec![self.video.clone().unwrap()];
        if let Some(a) = self.audio.clone() {
            tracks.push(a);
            self.has_audio_track = true;
        }
        let live = self.live.clone().unwrap();
        if let Err(e) = live.lock().unwrap().set_tracks(tracks) {
            tracing::warn!(error = %e, "RTMP publish rejected");
            self.live = None;
            return;
        }
        self.announced = true;
        for q in std::mem::take(&mut self.queue) {
            if !q.audio || self.has_audio_track {
                self.deliver(q);
            }
        }
    }

    fn deliver(&mut self, q: Queued) {
        if self.live.is_none() {
            return;
        }
        self.received += q.data.len() as u64;
        self.server.count_publish_bytes(q.data.len() as u64);
        if let Err(why) = self
            .server
            .check_rtmp_progress(self.started.elapsed(), self.received)
        {
            tracing::warn!(key = %self.key, "RTMP publish cut off: {why}");
            // What was already published stays playable; the slot is released.
            self.stop();
            return;
        }
        let Some(live) = &self.live else { return };
        let track = usize::from(q.audio);
        let result = live
            .lock()
            .unwrap()
            .push(track, q.pts_ms, q.key, q.data, None);
        self.server.record(&self.key, live, false);
        if let Err(e) = result {
            tracing::warn!(error = %e, "RTMP frame rejected; ending the publish");
            self.live = None;
        }
    }
}

impl LiveServer {
    /// An RTMP server whose publishers appear as live presentations of this
    /// server, under the stream key they publish to.
    ///
    /// Enhanced RTMP: AV1 / VP9 video and Opus audio. Pass `tls` to terminate
    /// RTMPS (`rtmps://`, requires the `rtmps` feature); `None` is plain RTMP.
    pub fn rtmp_server(
        &self,
        bind_addr: &str,
        tls: Option<crate::rtmp::RtmpsIdentity>,
    ) -> crate::RtmpServer {
        let server = self.clone();
        crate::RtmpServer::new(crate::RtmpConfig {
            bind_addr: bind_addr.to_string(),
            tls,
        })
        .with_session_factory(move || RtmpLiveSession::new(server.clone()).into_sink())
    }
}
