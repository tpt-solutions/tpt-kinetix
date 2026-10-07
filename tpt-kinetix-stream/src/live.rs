//! Live ingest and playback server for the royalty-free codecs.
//!
//! A publisher streams a **WebM** (AV1 or VP9 video, Opus audio) to
//! `POST|PUT /ingest/<key>` over HTTP (chunked or length-delimited), e.g.
//!
//! ```text
//! ffmpeg -re -i input -c:v libsvtav1 -c:a libopus -f webm -method POST \
//!        http://host:8080/ingest/cam1
//! ```
//!
//! and viewers play it as live fMP4 HLS from the same port:
//! `GET /<key>/master.m3u8`, `/<key>/track-N.m3u8`, `/<key>/init-N.mp4`,
//! `/<key>/seg-N-M.m4s`. Browsers' `MediaRecorder` produces the same WebM, so a
//! web page can publish through a streaming `fetch` upload.
//!
//! The ingest side is the incremental Matroska parser from `tpt-kinetix-demux`
//! feeding `tpt-kinetix-package`'s [`LivePackager`]; this module only does the
//! HTTP and bookkeeping. One publisher per key; a new publish replaces the old
//! presentation.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tpt_kinetix_demux::mkv_stream::{MkvEvent, MkvStream};
use tpt_kinetix_package::{LiveOptions, LivePackager, PlaylistRequest};

use crate::policy::{ActiveGuard, ActiveSet, IngestPolicy, Metrics, Refusal};
use crate::record::Recorder;
use crate::whip::{WhipConfig, WhipSessions};
use crate::ws;

/// The browser publishing demo served at `/publish`.
const PUBLISH_PAGE: &str = include_str!("../web/publish.html");

/// Largest header block accepted from a client.
const MAX_HEADER_BYTES: usize = 16 * 1024;
/// How long a blocking playlist reload holds a connection open before giving up.
const MAX_BLOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(6);
/// How often a blocked playlist request re-checks the packager.
const BLOCK_POLL: std::time::Duration = std::time::Duration::from_millis(20);

/// Parses the low-latency reload query (`_HLS_msn`, `_HLS_part`, `_HLS_skip`).
fn playlist_request(query: &str) -> PlaylistRequest {
    let mut req = PlaylistRequest::default();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        match k {
            "_HLS_msn" => req.msn = v.parse().ok(),
            "_HLS_part" => req.part = v.parse().ok(),
            // A delta update is asked for with `YES` (or `v2`), not a number.
            "_HLS_skip" if v == "YES" || v == "v2" => req.skip = Some(1),
            _ => {}
        }
    }
    req
}

pub(crate) type Shared = Arc<Mutex<LivePackager>>;

/// What a connection needs to be: plain TCP or TLS over it.
trait Io: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin> Io for T {}
/// One accepted connection, TLS or not.
type Conn = Box<dyn Io>;

/// Live ingest + HLS server state.
#[derive(Clone)]
pub struct LiveServer {
    opts: LiveOptions,
    streams: Arc<Mutex<HashMap<String, Shared>>>,
    policy: Arc<IngestPolicy>,
    metrics: Arc<Metrics>,
    active: ActiveSet,
    recorder: Option<Arc<Recorder>>,
    whip_cfg: Arc<WhipConfig>,
    whip_sessions: WhipSessions,
    #[cfg(feature = "rtmps")]
    tls: Option<Arc<tokio_rustls::TlsAcceptor>>,
}

impl LiveServer {
    /// A server whose presentations use `opts`.
    pub fn new(opts: LiveOptions) -> Self {
        Self {
            opts,
            streams: Arc::new(Mutex::new(HashMap::new())),
            policy: Arc::new(IngestPolicy::default()),
            metrics: Arc::new(Metrics::default()),
            active: ActiveSet::default(),
            recorder: None,
            whip_cfg: Arc::new(WhipConfig::default()),
            whip_sessions: WhipSessions::default(),
            #[cfg(feature = "rtmps")]
            tls: None,
        }
    }

    /// Serves HTTPS and `wss://` instead of plain HTTP and `ws://`: connections are
    /// TLS-terminated with `identity` (PEM chain + key) before any HTTP is read.
    /// Needed for `wss://` ingest and for a browser publish page served from a
    /// secure context. Requires the `rtmps` feature (it carries `tokio-rustls`).
    #[cfg(feature = "rtmps")]
    pub fn with_tls(mut self, identity: &crate::rtmp::RtmpsIdentity) -> Result<Self> {
        self.tls = Some(crate::rtmp::server::build_tls_acceptor(identity)?);
        Ok(self)
    }

    /// Where WHIP (WebRTC) publishers send media; see [`crate::whip`]. The default
    /// advertises loopback and the machine's primary address on an ephemeral port.
    pub fn with_whip(mut self, cfg: WhipConfig) -> Self {
        self.whip_cfg = Arc::new(cfg);
        self
    }

    /// Records every publish under `dir` (see [`Recorder`]): segments are
    /// persisted as they complete and served at `/<key>/dvr/...` during and
    /// after the publish.
    pub fn with_recording(mut self, dir: impl Into<std::path::PathBuf>) -> Self {
        self.recorder = Some(Arc::new(Recorder::new(dir)));
        self
    }

    /// Like [`Self::with_recording`], with retention limits.
    pub fn with_recording_limits(
        mut self,
        dir: impl Into<std::path::PathBuf>,
        limits: crate::record::RecordingLimits,
    ) -> Self {
        self.recorder = Some(Arc::new(Recorder::new(dir).with_limits(limits)));
        self
    }

    /// A fresh packager, collecting finished segments when recording.
    fn new_packager(&self) -> Shared {
        let mut p = LivePackager::new(self.opts.clone());
        p.set_recording(self.recorder.is_some());
        Arc::new(Mutex::new(p))
    }

    /// Persists what `live` has finished since the last call.
    pub(crate) fn record(&self, key: &str, live: &Shared, ended: bool) {
        if let Some(rec) = &self.recorder {
            if let Err(e) = rec.record(key, &mut live.lock().unwrap(), ended) {
                tracing::warn!(key, error = %e, "recording failed");
            }
        }
    }

    /// Applies publish authentication and limits (see [`IngestPolicy`]); the
    /// default is open.
    pub fn with_policy(mut self, policy: IngestPolicy) -> Self {
        self.policy = Arc::new(policy);
        self
    }

    /// Registers a new presentation for an RTMP publisher, replacing any earlier
    /// one. `raw_key` is the stream key as the encoder sent it, optionally
    /// carrying the publish token the way OBS and ffmpeg users commonly append it
    /// (`cam1?token=s3cret`). `None` when the key is invalid or the
    /// [`IngestPolicy`] refuses the publish.
    pub(crate) fn begin(&self, raw_key: &str) -> Option<(String, Shared, ActiveGuard)> {
        let (key, query) = raw_key.split_once('?').unwrap_or((raw_key, ""));
        if !valid_key(key) {
            tracing::warn!(key, "rejecting an invalid stream key");
            return None;
        }
        if let Err(refusal) = self.policy.authorize(key, &HashMap::new(), query) {
            self.metrics.count_refusal(refusal);
            tracing::warn!(key, "RTMP publish refused: {}", refusal.message());
            return None;
        }
        let guard = match self.active.enter(key, &self.policy) {
            Ok(g) => g,
            Err(refusal) => {
                self.metrics.count_refusal(refusal);
                tracing::warn!(key, "RTMP publish refused: {}", refusal.message());
                return None;
            }
        };
        // A new presentation restarts segment numbering: keep the old recording.
        if let Some(rec) = &self.recorder {
            rec.next_generation(key);
        }
        let live = self.new_packager();
        self.streams
            .lock()
            .unwrap()
            .insert(key.to_string(), live.clone());
        tracing::info!(key, "publish started");
        Metrics::inc(&self.metrics.publishes_started);
        Some((key.to_string(), live, guard))
    }

    /// Whether an RTMP publisher that has run `elapsed` and sent `bytes` is still
    /// within the policy; counts the cut-off in the metrics when not.
    pub(crate) fn check_rtmp_progress(
        &self,
        key: &str,
        elapsed: std::time::Duration,
        bytes: u64,
    ) -> Result<(), &'static str> {
        match self.policy.check_progress(key, elapsed, bytes) {
            Ok(()) => Ok(()),
            Err(refusal) => {
                self.metrics.count_refusal(refusal);
                Err(refusal.message())
            }
        }
    }

    /// The target segment length, in seconds.
    #[cfg(feature = "whip")]
    pub(crate) fn segment_seconds(&self) -> f64 {
        self.opts.segment_seconds
    }

    /// The idle timeout that applies to `key`, if any.
    pub(crate) fn idle_timeout(&self, key: &str) -> Option<std::time::Duration> {
        self.policy.limits_for(key).idle_timeout
    }

    /// Counts an idle cut-off in `/metrics`.
    pub(crate) fn count_idle_cut(&self) {
        self.metrics.count_refusal(Refusal::Idle);
    }

    /// Counts RTMP payload bytes in `/metrics`.
    pub(crate) fn count_publish_bytes(&self, n: u64) {
        self.metrics
            .publish_bytes
            .fetch_add(n, std::sync::atomic::Ordering::Relaxed);
    }

    /// Accepts connections on `listener` forever.
    pub async fn serve(self, listener: TcpListener) -> Result<()> {
        tracing::info!(addr = ?listener.local_addr().ok(), "live server listening");
        loop {
            let (stream, peer) = listener.accept().await?;
            let this = self.clone();
            tokio::spawn(async move {
                #[cfg(feature = "rtmps")]
                let conn: Conn = match &this.tls {
                    Some(tls) => match tls.accept(stream).await {
                        Ok(t) => Box::new(t),
                        Err(e) => {
                            tracing::warn!(%peer, error = %e, "TLS handshake failed");
                            return;
                        }
                    },
                    None => Box::new(stream),
                };
                #[cfg(not(feature = "rtmps"))]
                let conn: Conn = Box::new(stream);
                if let Err(e) = this.handle(conn).await {
                    tracing::warn!(%peer, error = %e, "live connection ended with an error");
                }
            });
        }
    }

    /// Binds `addr` and serves.
    pub async fn bind_and_serve(self, addr: &str) -> Result<()> {
        let listener = TcpListener::bind(addr)
            .await
            .with_context(|| format!("bind {addr}"))?;
        self.serve(listener).await
    }

    async fn handle(&self, stream: Conn) -> Result<()> {
        let mut r = BufReader::new(stream);
        let mut head = String::new();
        let mut total = 0usize;
        let mut lines = Vec::new();
        loop {
            head.clear();
            let n = r.read_line(&mut head).await?;
            if n == 0 {
                return Ok(());
            }
            total += n;
            if total > MAX_HEADER_BYTES {
                bail!("header block too large");
            }
            let line = head.trim_end_matches(['\r', '\n']).to_string();
            if line.is_empty() && !lines.is_empty() {
                break;
            }
            if !line.is_empty() {
                lines.push(line);
            }
        }
        let mut parts = lines[0].split_whitespace();
        let method = parts.next().unwrap_or("").to_string();
        let target = parts.next().unwrap_or("/").to_string();
        let headers: HashMap<String, String> = lines[1..]
            .iter()
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
            .collect();
        let (path, query) = match target.split_once('?') {
            Some((p, q)) => (p.to_string(), q.to_string()),
            None => (target.clone(), String::new()),
        };

        if path.starts_with("/whip/") {
            return self.whip(r, &method, &path, &query, &headers).await;
        }
        match method.as_str() {
            "GET" if path.starts_with("/ingest/") && ws::upgrade_key(&headers).is_some() => {
                // Browser publishing: MediaRecorder chunks as binary WebSocket messages.
                let Some(key) = path.strip_prefix("/ingest/").filter(|k| valid_key(k)) else {
                    return respond(
                        r.get_mut(),
                        Reply::text(404, "publish to /ingest/<key>"),
                        false,
                    )
                    .await;
                };
                if let Err(refusal) = self.policy.authorize(key, &headers, &query) {
                    self.metrics.count_refusal(refusal);
                    tracing::warn!(key, "publish refused: {}", refusal.message());
                    return respond(
                        r.get_mut(),
                        Reply::text(refusal.status(), refusal.message()),
                        false,
                    )
                    .await;
                }
                self.ingest(r, key.to_string(), &headers, true).await
            }
            "GET" | "HEAD" => {
                if path == "/metrics" {
                    let body = self.metrics.render(self.active.len()).into_bytes();
                    let mut reply = Reply::text(200, "");
                    reply.body = body;
                    reply.content_type = "text/plain; version=0.0.4";
                    return respond(r.get_mut(), reply, method == "HEAD").await;
                }
                if path == "/publish" {
                    // A ready-made browser publisher (camera -> MediaRecorder -> WebSocket).
                    let mut reply = Reply::text(200, "");
                    reply.body = PUBLISH_PAGE.as_bytes().to_vec();
                    reply.content_type = "text/html; charset=utf-8";
                    return respond(r.get_mut(), reply, method == "HEAD").await;
                }
                Metrics::inc(&self.metrics.playback_requests);
                let mut reply = self.playback(&path, &query).await;
                if let Some((live, t, n)) = reply.stream.take() {
                    if method == "GET" {
                        return self.stream_segment(r.get_mut(), live, t, n).await;
                    }
                }
                respond(r.get_mut(), reply, method == "HEAD").await
            }
            "POST" | "PUT" => {
                let Some(key) = path.strip_prefix("/ingest/").filter(|k| valid_key(k)) else {
                    return respond(
                        r.get_mut(),
                        Reply::text(404, "publish to /ingest/<key>"),
                        false,
                    )
                    .await;
                };
                if let Err(refusal) = self.policy.authorize(key, &headers, &query) {
                    self.metrics.count_refusal(refusal);
                    tracing::warn!(key, "publish refused: {}", refusal.message());
                    return respond(
                        r.get_mut(),
                        Reply::text(refusal.status(), refusal.message()),
                        false,
                    )
                    .await;
                }
                self.ingest(r, key.to_string(), &headers, false).await
            }
            "OPTIONS" => respond(r.get_mut(), Reply::text(204, ""), false).await,
            _ => respond(r.get_mut(), Reply::text(405, "method not allowed"), false).await,
        }
    }

    async fn playback(&self, path: &str, query: &str) -> Reply {
        let Some((key, name)) = path.trim_start_matches('/').split_once('/') else {
            return Reply::text(404, "expected /<key>/<resource>");
        };
        // Per-stream latency stats for the hls.js latency probe and operators:
        // `GET /<key>/_stats` reports the packager's live-edge estimate plus
        // the configured part/segment targets, so a measured player latency
        // can be compared against the packaging floor.
        if name == "_stats" {
            let Some(live) = self.streams.lock().unwrap().get(key).cloned() else {
                return Reply::text(404, "no such stream");
            };
            let live = live.lock().unwrap();
            let body = format!(
                "{{\"ready\":{},\"live_latency_secs\":{},\"part_seconds\":{},\"segment_seconds\":{},\"latest_segment\":{},\"latest_part\":{}}}",
                live.is_ready(),
                opt_f64(live.live_latency_secs()),
                opt_f64(live.part_seconds()),
                live.segment_seconds(),
                live.latest_segment(),
                live.latest_part(),
            );
            return Reply::json(body.into_bytes());
        }
        if let Some(file) = name.strip_prefix("dvr/") {
            let Some(rec) = &self.recorder else {
                return Reply::text(404, "recording is not enabled");
            };
            return match rec.read(key, file) {
                Some(b) if file.ends_with(".m3u8") => Reply::playlist(b),
                Some(b) => Reply::media("video/mp4", b),
                None => Reply::text(404, "no such recording"),
            };
        }
        let Some(live) = self.streams.lock().unwrap().get(key).cloned() else {
            return Reply::text(404, "no such stream");
        };
        let num = |s: &str| s.parse::<usize>().ok();
        let not_ready = || Reply::text(404, "the stream is not ready yet; retry shortly");
        if name == "master.m3u8" {
            return live
                .lock()
                .unwrap()
                .master_playlist()
                .map_or_else(not_ready, |p| Reply::playlist(p.into_bytes()));
        }
        // The same live presentation as a dynamic DASH manifest. The segment and
        // init URLs are identical to HLS', so a player can switch between them.
        if name == "manifest.mpd" {
            return live
                .lock()
                .unwrap()
                .dash_mpd()
                .map_or_else(not_ready, |m| Reply::xml(m.into_bytes()));
        }
        if let Some(t) = name
            .strip_prefix("track-")
            .and_then(|s| s.strip_suffix(".m3u8"))
            .and_then(num)
        {
            let req = playlist_request(query);
            if req.msn.is_some() && !self.await_request(&live, t, &req).await {
                return Reply::text(404, "no such segment yet; retry shortly");
            }
            return live
                .lock()
                .unwrap()
                .media_playlist_for(t, &req)
                .map_or_else(not_ready, |p| Reply::playlist(p.into_bytes()));
        }
        if let Some(t) = name
            .strip_prefix("init-")
            .and_then(|s| s.strip_suffix(".mp4"))
            .and_then(num)
        {
            return live
                .lock()
                .unwrap()
                .init_segment(t)
                .map_or_else(not_ready, |b| Reply::media("video/mp4", b));
        }
        if let Some(rest) = name
            .strip_prefix("seg-")
            .and_then(|s| s.strip_suffix(".m4s"))
        {
            if let Some((t, n)) = rest.split_once('-') {
                if let (Some(t), Ok(n)) = (num(t), n.parse::<u64>()) {
                    let shared = live.clone();
                    let live = live.lock().unwrap();
                    if let Some(b) = live.segment(t, n) {
                        return Reply::media("video/iso.segment", b.to_vec());
                    }
                    // Low-latency DASH: a player may request the segment in
                    // progress early (`availabilityTimeOffset`). It is streamed:
                    // each CMAF chunk goes out as it is published and the
                    // response ends when the segment completes.
                    if live.part_seconds().is_some()
                        && !live.is_finished()
                        && n == live.latest_segment() + 1
                    {
                        let mut reply = Reply::text(200, "");
                        reply.content_type = "video/iso.segment";
                        reply.cache = "no-cache";
                        reply.stream = Some((shared, t, n));
                        return reply;
                    }
                    return Reply::text(404, "no such segment");
                }
            }
        }
        if let Some(rest) = name
            .strip_prefix("part-")
            .and_then(|s| s.strip_suffix(".mp4"))
        {
            if let Some((t, rest)) = rest.split_once('-') {
                if let Some((n, i)) = rest.split_once('-') {
                    if let (Some(t), Ok(n), Ok(i)) = (num(t), n.parse::<u64>(), i.parse::<u64>()) {
                        // The playlist's EXT-X-PRELOAD-HINT names the next part before
                        // it exists, and the player requests it at once: hold the
                        // request open until the part is published (RFC 8216bis 6.2.5.2).
                        let part = self.await_part(&live, t, n, i).await;
                        return part.map_or_else(
                            || Reply::text(404, "no such part"),
                            |b| Reply::media("video/iso.segment", b.to_vec()),
                        );
                    }
                }
            }
        }
        Reply::text(404, "no such resource")
    }

    /// WHIP: `POST /whip/<key>` (SDP offer -> `201` + SDP answer) and
    /// `DELETE /whip/<key>/<session>`.
    async fn whip(
        &self,
        mut r: BufReader<Conn>,
        method: &str,
        path: &str,
        query: &str,
        headers: &HashMap<String, String>,
    ) -> Result<()> {
        let rest = path.trim_start_matches("/whip/");
        let (key, session) = match rest.split_once('/') {
            Some((k, s)) => (k, Some(s)),
            None => (rest, None),
        };
        if !valid_key(key) {
            return respond(
                r.get_mut(),
                Reply::text(404, "whip endpoint is /whip/<key>"),
                false,
            )
            .await;
        }
        if method == "OPTIONS" {
            return respond(r.get_mut(), Reply::text(204, ""), false).await;
        }
        if let Err(refusal) = self.policy.authorize(key, headers, query) {
            self.metrics.count_refusal(refusal);
            tracing::warn!(key, "WHIP publish refused: {}", refusal.message());
            return respond(
                r.get_mut(),
                Reply::text(refusal.status(), refusal.message()),
                false,
            )
            .await;
        }
        let reply = match (method, session) {
            ("POST", None) => self.whip_offer(&mut r, key, query, headers).await,
            ("DELETE", Some(id)) => {
                if self.whip_sessions.stop(&format!("{key}/{id}")) {
                    Reply::text(200, "")
                } else {
                    Reply::text(404, "no such session")
                }
            }
            _ => Reply::text(405, "WHIP: POST an SDP offer to /whip/<key>"),
        };
        respond(r.get_mut(), reply, false).await
    }

    #[cfg(feature = "whip")]
    async fn whip_offer(
        &self,
        r: &mut BufReader<Conn>,
        key: &str,
        query: &str,
        headers: &HashMap<String, String>,
    ) -> Reply {
        if !headers
            .get("content-type")
            .is_some_and(|v| v.to_ascii_lowercase().starts_with("application/sdp"))
        {
            return Reply::text(415, "send the SDP offer as application/sdp");
        }
        // The publish policy's slot / concurrency limits, checked up front so the
        // publisher gets an HTTP error instead of a silent drop. (Probed, not held:
        // the session takes its own slot when media starts.)
        if let Err(refusal) = self.active.enter(key, &self.policy) {
            self.metrics.count_refusal(refusal);
            return Reply::text(refusal.status(), refusal.message());
        }
        let mut body = Body::new(headers);
        let mut offer = Vec::new();
        loop {
            match body.next(r).await {
                Ok(Some(chunk)) => {
                    offer.extend_from_slice(&chunk);
                    if offer.len() > 64 * 1024 {
                        return Reply::text(413, "SDP offer too large");
                    }
                }
                Ok(None) => break,
                Err(e) => return Reply::text(400, &e.to_string()),
            }
        }
        let Ok(offer) = String::from_utf8(offer) else {
            return Reply::text(400, "the SDP offer is not UTF-8");
        };
        let id = format!("{:016x}", rand_id());
        // A presented token rides along as `name?token=...`, which is how the
        // publish policy reads it.
        let publish_key = match IngestPolicy::presented(headers, query) {
            Some(t) => format!("{key}?token={t}"),
            None => key.to_string(),
        };
        match crate::whip::start_session(
            self.clone(),
            &self.whip_cfg,
            self.whip_sessions.clone(),
            format!("{key}/{id}"),
            publish_key,
            &offer,
        )
        .await
        {
            Ok(answer) => {
                let mut reply = Reply::text(201, "");
                reply.content_type = "application/sdp";
                reply.body = answer.into_bytes();
                reply.extra_headers = format!("Location: /whip/{key}/{id}{}", "\r\n");
                reply
            }
            Err(e) => {
                tracing::warn!(key, error = %e, "WHIP offer rejected");
                Reply::text(400, &e.to_string())
            }
        }
    }

    #[cfg(not(feature = "whip"))]
    async fn whip_offer(
        &self,
        _r: &mut BufReader<Conn>,
        _key: &str,
        _query: &str,
        _headers: &HashMap<String, String>,
    ) -> Reply {
        Reply::text(
            501,
            "WHIP needs the `whip` cargo feature (cargo build --features whip)",
        )
    }

    /// Streams segment `number` of `track` while it is still being published
    /// (low-latency DASH): headers first, then every CMAF chunk as it appears with
    /// HTTP chunked framing, then whatever the finished segment adds after its last
    /// part, so the bytes received equal the completed segment exactly.
    async fn stream_segment(
        &self,
        s: &mut Conn,
        live: Shared,
        track: usize,
        number: u64,
    ) -> Result<()> {
        s.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: video/iso.segment\r\nCache-Control: no-cache\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Headers: *\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
        )
        .await?;
        let budget =
            std::time::Duration::from_secs_f64(live.lock().unwrap().segment_seconds() * 3.0 + 5.0);
        let deadline = tokio::time::Instant::now() + budget;
        let (mut next_part, mut sent) = (0u64, 0usize);
        loop {
            let (parts, whole, gone) = {
                let l = live.lock().unwrap();
                let mut parts = Vec::new();
                while let Some(p) = l.part(track, number, next_part + parts.len() as u64) {
                    parts.push(p);
                }
                let whole = l.segment(track, number);
                (
                    parts,
                    whole,
                    number <= l.latest_segment() || l.is_finished(),
                )
            };
            for p in &parts {
                write_http_chunk(s, p).await?;
                next_part += 1;
                sent += p.len();
            }
            if let Some(seg) = whole {
                // Samples that missed the last part form the segment's tail.
                if seg.len() > sent {
                    write_http_chunk(s, &seg[sent..]).await?;
                }
                break;
            }
            if gone || tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(BLOCK_POLL).await;
        }
        s.write_all(b"0\r\n\r\n").await?;
        s.flush().await?;
        let _ = s.shutdown().await;
        Ok(())
    }

    /// The part `track`/`number`/`index`, waiting up to `MAX_BLOCK_WAIT` for a part
    /// that is about to be published (a preload hint); `None` for one that never
    /// will be (a finished stream, a completed segment without it, or one too far
    /// ahead to be a hint).
    async fn await_part(
        &self,
        live: &Shared,
        track: usize,
        number: u64,
        index: u64,
    ) -> Option<Vec<u8>> {
        let deadline = tokio::time::Instant::now() + MAX_BLOCK_WAIT;
        loop {
            {
                let live = live.lock().unwrap();
                if let Some(p) = live.part(track, number, index) {
                    return Some(p.to_vec());
                }
                // Only the in-progress segment (or the one a hint will open next)
                // can still gain parts.
                if live.is_finished()
                    || number <= live.latest_segment()
                    || number > live.latest_segment() + 2
                {
                    return None;
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(BLOCK_POLL).await;
        }
    }

    /// Blocks until the media `req` asks for exists, the stream ends, or
    /// `MAX_BLOCK_WAIT` elapses. Returns whether the request can be answered.
    ///
    /// This is the low-latency HLS blocking playlist reload: a player reloads
    /// with `_HLS_msn`/`_HLS_part` while it is still behind and the connection is
    /// held open, so a new part costs one request instead of a poll cycle.
    async fn await_request(&self, live: &Shared, track: usize, req: &PlaylistRequest) -> bool {
        let deadline = tokio::time::Instant::now() + MAX_BLOCK_WAIT;
        loop {
            if live.lock().unwrap().satisfies(track, req) {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(BLOCK_POLL).await;
        }
    }

    async fn ingest(
        &self,
        mut r: BufReader<Conn>,
        key: String,
        headers: &HashMap<String, String>,
        ws: bool,
    ) -> Result<()> {
        let _guard = match self.active.enter(&key, &self.policy) {
            Ok(g) => g,
            Err(refusal) => {
                self.metrics.count_refusal(refusal);
                tracing::warn!(%key, "publish refused: {}", refusal.message());
                return respond(
                    r.get_mut(),
                    Reply::text(refusal.status(), refusal.message()),
                    false,
                )
                .await;
            }
        };
        if ws {
            // The refusals above are plain HTTP errors, which a WebSocket client
            // sees as a failed handshake; from here the connection is a WebSocket.
            let client_key = ws::upgrade_key(headers).unwrap_or_default();
            r.get_mut()
                .write_all(ws::handshake_response(client_key).as_bytes())
                .await?;
        } else if headers
            .get("expect")
            .is_some_and(|v| v.eq_ignore_ascii_case("100-continue"))
        {
            r.get_mut()
                .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
                .await?;
        }
        // A finished presentation under this key is the previous publish of the same
        // stream: keep it, so a reconnect continues the playlists (with a
        // discontinuity) instead of starting over. It is replaced below if the new
        // publisher's codec configuration differs.
        let mut live: Shared = {
            let mut map = self.streams.lock().unwrap();
            match map.get(&key) {
                Some(old) if old.lock().unwrap().is_finished() => old.clone(),
                _ => {
                    if let Some(rec) = &self.recorder {
                        rec.next_generation(&key);
                    }
                    let fresh = self.new_packager();
                    map.insert(key.clone(), fresh.clone());
                    fresh
                }
            }
        };
        tracing::info!(%key, "publish started");
        Metrics::inc(&self.metrics.publishes_started);

        let mut body = if ws {
            Body::WebSocket(
                self.policy
                    .ws_ping_interval
                    .or(Some(std::time::Duration::from_secs(15))),
            )
        } else {
            Body::new(headers)
        };
        let mut parser = MkvStream::new();
        let mut result: Result<()> = Ok(());
        let mut cut_off: Option<Refusal> = None;
        let started = std::time::Instant::now();
        let mut received = 0u64;
        let limits = self.policy.limits_for(&key);
        'read: loop {
            let next = match limits.idle_timeout {
                Some(idle) => match tokio::time::timeout(idle, body.next(&mut r)).await {
                    Ok(n) => n,
                    Err(_) => {
                        cut_off = Some(Refusal::Idle);
                        break 'read;
                    }
                },
                None => body.next(&mut r).await,
            };
            // A broken body ends the publish like any other error: what was
            // received stays playable and the playlists are completed below.
            let next = match next {
                Ok(n) => n,
                Err(e) => {
                    result = Err(e);
                    break 'read;
                }
            };
            let Some(chunk) = next else { break 'read };
            received += chunk.len() as u64;
            self.metrics
                .publish_bytes
                .fetch_add(chunk.len() as u64, std::sync::atomic::Ordering::Relaxed);
            if let Err(refusal) = self
                .policy
                .check_progress(&key, started.elapsed(), received)
            {
                cut_off = Some(refusal);
                break 'read;
            }
            let events = match parser.push(&chunk) {
                Ok(e) => e,
                Err(e) => {
                    result = Err(anyhow::anyhow!("{e}"));
                    break 'read;
                }
            };
            if let Err(e) = self.apply(&key, &mut live, events) {
                result = Err(e);
                break 'read;
            }
        }
        if let Some(refusal) = cut_off {
            self.metrics.count_refusal(refusal);
            tracing::warn!(%key, "publish cut off: {}", refusal.message());
            // Keep what was already published playable, then say why it stopped.
            let _ = live.lock().unwrap().finish();
            self.record(&key, &live, true);
            return end_publish(&mut r, ws, Reply::text(refusal.status(), refusal.message())).await;
        }
        if result.is_ok() {
            result = parser
                .finish()
                .map_err(|e| anyhow::anyhow!("{e}"))
                .and_then(|events| self.apply(&key, &mut live, events));
        }
        // End of the publish (cleanly or not): complete the playlists.
        let _ = live.lock().unwrap().finish();
        self.record(&key, &live, true);
        match result {
            Ok(()) => {
                tracing::info!(%key, "publish finished");
                end_publish(&mut r, ws, Reply::text(200, "ok")).await
            }
            Err(e) => {
                tracing::warn!(%key, error = %e, "publish rejected");
                end_publish(&mut r, ws, Reply::text(400, &e.to_string())).await
            }
        }
    }
}

/// Formats an `Option<f64>` for the `_stats` JSON body.
fn opt_f64(v: Option<f64>) -> String {
    match v {
        Some(x) => format!("{x:.6}"),
        None => "null".to_string(),
    }
}

fn valid_key(k: &str) -> bool {
    !k.is_empty()
        && k.len() <= 128
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

impl LiveServer {
    /// Applies parsed events to the packager of `key`.
    ///
    /// When the publisher's tracks do not match the presentation it reconnected
    /// to (a codec or resolution change), a fresh presentation replaces it:
    /// viewers must reload the playlists, as they would for any new publish.
    fn apply(&self, key: &str, live: &mut Shared, events: Vec<MkvEvent>) -> Result<()> {
        for e in &events {
            if let MkvEvent::Tracks(t) = e {
                if !live.lock().unwrap().accepts_tracks(t) {
                    tracing::info!(
                        key,
                        "track configuration changed; starting a new presentation"
                    );
                    let fresh = self.new_packager();
                    // The old presentation ends cleanly for anyone still watching it.
                    let _ = live.lock().unwrap().finish();
                    self.record(key, live, true);
                    if let Some(rec) = &self.recorder {
                        rec.next_generation(key);
                    }
                    self.streams
                        .lock()
                        .unwrap()
                        .insert(key.to_string(), fresh.clone());
                    *live = fresh;
                }
            }
        }
        apply(live, events)?;
        self.record(key, live, false);
        Ok(())
    }
}

/// Applies parsed events to the packager.
fn apply(live: &Shared, events: Vec<MkvEvent>) -> Result<()> {
    let mut live = live.lock().unwrap();
    for e in events {
        match e {
            MkvEvent::Tracks(t) => live.set_tracks(t).map_err(|e| anyhow::anyhow!("{e}"))?,
            // A live stream has no Cues; the index arrives after the body.
            MkvEvent::Cue(_) => {}
            MkvEvent::Frame(f) => live
                .push(f.stream, f.pts_ms, f.key, f.data, f.duration_ms)
                .map_err(|e| anyhow::anyhow!("{e}"))?,
        }
    }
    Ok(())
}

/// A request body: chunked, length-delimited, or until the peer closes.
enum Body {
    Chunked,
    /// Binary frames; pinged after this much silence.
    WebSocket(Option<std::time::Duration>),
    Length(u64),
    UntilEof,
    Done,
}

impl Body {
    fn new(headers: &HashMap<String, String>) -> Self {
        if headers
            .get("transfer-encoding")
            .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"))
        {
            Body::Chunked
        } else if let Some(n) = headers.get("content-length").and_then(|v| v.parse().ok()) {
            Body::Length(n)
        } else {
            Body::UntilEof
        }
    }

    /// The next piece of the body, or `None` at its end.
    async fn next(&mut self, r: &mut BufReader<Conn>) -> Result<Option<Vec<u8>>> {
        match self {
            Body::Done => Ok(None),
            Body::WebSocket(keepalive) => {
                let chunk = ws::next_chunk(r, *keepalive).await?;
                if chunk.is_none() {
                    *self = Body::Done;
                }
                Ok(chunk)
            }
            Body::UntilEof => {
                let mut buf = vec![0u8; 64 * 1024];
                let n = r.read(&mut buf).await?;
                if n == 0 {
                    *self = Body::Done;
                    return Ok(None);
                }
                buf.truncate(n);
                Ok(Some(buf))
            }
            Body::Length(left) => {
                if *left == 0 {
                    *self = Body::Done;
                    return Ok(None);
                }
                let mut buf = vec![0u8; (*left).min(64 * 1024) as usize];
                let n = r.read(&mut buf).await?;
                if n == 0 {
                    *self = Body::Done;
                    return Ok(None);
                }
                *left -= n as u64;
                buf.truncate(n);
                Ok(Some(buf))
            }
            Body::Chunked => {
                let mut line = String::new();
                if r.read_line(&mut line).await? == 0 {
                    *self = Body::Done;
                    return Ok(None);
                }
                let size = usize::from_str_radix(line.trim().split(';').next().unwrap_or("0"), 16)
                    .context("bad chunk size")?;
                if size == 0 {
                    // Trailer headers until the blank line.
                    loop {
                        line.clear();
                        if r.read_line(&mut line).await? == 0 || line.trim().is_empty() {
                            break;
                        }
                    }
                    *self = Body::Done;
                    return Ok(None);
                }
                if size > 64 * 1024 * 1024 {
                    bail!("chunk of {size} bytes is too large");
                }
                let mut buf = vec![0u8; size];
                r.read_exact(&mut buf).await?;
                let mut crlf = [0u8; 2];
                r.read_exact(&mut crlf).await?;
                Ok(Some(buf))
            }
        }
    }
}

/// An HTTP response.
struct Reply {
    status: u16,
    content_type: &'static str,
    cache: &'static str,
    body: Vec<u8>,
    /// Set for a segment still being published: the body is streamed as the
    /// parts appear (low-latency DASH) instead of being sent whole.
    stream: Option<(Shared, usize, u64)>,
    /// Extra raw header lines, each ending in CRLF.
    extra_headers: String,
}

impl Reply {
    fn text(status: u16, msg: &str) -> Self {
        Self {
            status,
            content_type: "text/plain",
            cache: "no-store",
            body: msg.as_bytes().to_vec(),
            stream: None,
            extra_headers: String::new(),
        }
    }

    fn playlist(body: Vec<u8>) -> Self {
        Self {
            status: 200,
            content_type: "application/vnd.apple.mpegurl",
            cache: "no-cache",
            body,
            stream: None,
            extra_headers: String::new(),
        }
    }

    fn xml(body: Vec<u8>) -> Self {
        Self {
            status: 200,
            content_type: "application/dash+xml",
            // A live manifest must never be cached: it describes a sliding window.
            cache: "no-cache",
            body,
            stream: None,
            extra_headers: String::new(),
        }
    }

    fn json(body: Vec<u8>) -> Self {
        Self {
            status: 200,
            content_type: "application/json",
            cache: "no-cache",
            body,
            stream: None,
            extra_headers: String::new(),
        }
    }

    fn media(content_type: &'static str, body: Vec<u8>) -> Self {
        Self {
            status: 200,
            content_type,
            cache: "public, max-age=60",
            body,
            stream: None,
            extra_headers: String::new(),
        }
    }
}

async fn end_publish(r: &mut BufReader<Conn>, ws: bool, reply: Reply) -> Result<()> {
    if !ws {
        return respond(r.get_mut(), reply, false).await;
    }
    let code = match reply.status {
        200 => 1000,
        400 => 1007,
        413 => 1009,
        _ => 1008,
    };
    let reason = String::from_utf8_lossy(&reply.body).to_string();
    ws::send_close(r.get_mut(), code, &reason).await?;
    let _ = r.get_mut().shutdown().await;
    Ok(())
}

/// A hard-to-guess session id (the clock plus a process-wide counter, hashed).
fn rand_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static N: AtomicU64 = AtomicU64::new(0x9E37_79B9_7F4A_7C15);
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    let mut x = t ^ N.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed);
    x ^= x >> 30;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// One HTTP/1.1 chunk of a chunked response.
async fn write_http_chunk(s: &mut Conn, data: &[u8]) -> Result<()> {
    s.write_all(format!("{:X}\r\n", data.len()).as_bytes())
        .await?;
    s.write_all(data).await?;
    s.write_all(b"\r\n").await?;
    s.flush().await?;
    Ok(())
}

async fn respond(s: &mut Conn, r: Reply, head_only: bool) -> Result<()> {
    let reason = match r.status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        415 => "Unsupported Media Type",
        501 => "Not Implemented",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        409 => "Conflict",
        413 => "Payload Too Large",
        429 => "Too Many Requests",
        503 => "Service Unavailable",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: {}\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Headers: *\r\nAccess-Control-Allow-Methods: GET, POST, PUT, DELETE, OPTIONS\r\nAccess-Control-Expose-Headers: Location\r\nTiming-Allow-Origin: *\r\n{}Connection: close\r\n\r\n",
        r.status,
        r.content_type,
        r.body.len(),
        r.cache,
        r.extra_headers
    );
    s.write_all(head.as_bytes()).await?;
    if !head_only {
        s.write_all(&r.body).await?;
    }
    s.flush().await?;
    let _ = s.shutdown().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_keys_are_validated() {
        assert!(valid_key("cam-1_a.b"));
        assert!(!valid_key(""));
        assert!(!valid_key("../x"));
        assert!(!valid_key("a/b"));
        assert!(!valid_key(&"k".repeat(200)));
    }

    #[test]
    fn body_mode_from_headers() {
        let h = |k: &str, v: &str| HashMap::from([(k.to_string(), v.to_string())]);
        assert!(matches!(
            Body::new(&h("transfer-encoding", "chunked")),
            Body::Chunked
        ));
        assert!(matches!(
            Body::new(&h("content-length", "10")),
            Body::Length(10)
        ));
        assert!(matches!(Body::new(&HashMap::new()), Body::UntilEof));
    }
}
