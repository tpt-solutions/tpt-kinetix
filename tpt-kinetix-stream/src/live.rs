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
use tokio::net::{TcpListener, TcpStream};
use tpt_kinetix_demux::mkv_stream::{MkvEvent, MkvStream};
use tpt_kinetix_package::{LiveOptions, LivePackager, PlaylistRequest};

use crate::policy::{ActiveSet, IngestPolicy, Metrics, Refusal};
use crate::record::Recorder;

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
        let Ok(n) = v.parse::<u64>() else { continue };
        match k {
            "_HLS_msn" => req.msn = Some(n),
            "_HLS_part" => req.part = Some(n),
            "_HLS_skip" => req.skip = Some(n),
            _ => {}
        }
    }
    req
}

pub(crate) type Shared = Arc<Mutex<LivePackager>>;

/// Live ingest + HLS server state.
#[derive(Clone)]
pub struct LiveServer {
    opts: LiveOptions,
    streams: Arc<Mutex<HashMap<String, Shared>>>,
    policy: Arc<IngestPolicy>,
    metrics: Arc<Metrics>,
    active: ActiveSet,
    recorder: Option<Arc<Recorder>>,
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
        }
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

    /// Registers a new presentation under `key`, replacing any earlier one;
    /// `None` when `key` is not a valid stream key.
    pub(crate) fn begin(&self, key: &str) -> Option<Shared> {
        if !valid_key(key) {
            tracing::warn!(key, "rejecting an invalid stream key");
            return None;
        }
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
        Some(live)
    }

    /// Accepts connections on `listener` forever.
    pub async fn serve(self, listener: TcpListener) -> Result<()> {
        tracing::info!(addr = ?listener.local_addr().ok(), "live server listening");
        loop {
            let (stream, peer) = listener.accept().await?;
            let this = self.clone();
            tokio::spawn(async move {
                if let Err(e) = this.handle(stream).await {
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

    async fn handle(&self, stream: TcpStream) -> Result<()> {
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

        match method.as_str() {
            "GET" | "HEAD" => {
                if path == "/metrics" {
                    let body = self.metrics.render(self.active.len()).into_bytes();
                    let mut reply = Reply::text(200, "");
                    reply.body = body;
                    reply.content_type = "text/plain; version=0.0.4";
                    return respond(r.get_mut(), reply, method == "HEAD").await;
                }
                Metrics::inc(&self.metrics.playback_requests);
                let reply = self.playback(&path, &query).await;
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
                self.ingest(r, key.to_string(), &headers).await
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
                    let live = live.lock().unwrap();
                    // Low-latency DASH: a player may request the segment in
                    // progress early (`availabilityTimeOffset`); serve the CMAF
                    // chunks published so far with chunked framing. A completed
                    // segment is served whole as before.
                    if let Some(b) = live.segment(t, n) {
                        return Reply::media("video/iso.segment", b.to_vec());
                    }
                    if let Some(prefix) = live.segment_prefix(t, n) {
                        return Reply::chunk(prefix);
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
                        return live.lock().unwrap().part(t, n, i).map_or_else(
                            || Reply::text(404, "no such part"),
                            |b| Reply::media("video/iso.segment", b.to_vec()),
                        );
                    }
                }
            }
        }
        Reply::text(404, "no such resource")
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
        mut r: BufReader<TcpStream>,
        key: String,
        headers: &HashMap<String, String>,
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
        if headers
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

        let mut body = Body::new(headers);
        let mut parser = MkvStream::new();
        let mut result: Result<()> = Ok(());
        let mut cut_off: Option<Refusal> = None;
        let started = std::time::Instant::now();
        let mut received = 0u64;
        'read: loop {
            let next = match self.policy.idle_timeout {
                Some(idle) => match tokio::time::timeout(idle, body.next(&mut r)).await {
                    Ok(n) => n?,
                    Err(_) => {
                        cut_off = Some(Refusal::Idle);
                        break 'read;
                    }
                },
                None => body.next(&mut r).await?,
            };
            let Some(chunk) = next else { break 'read };
            received += chunk.len() as u64;
            self.metrics
                .publish_bytes
                .fetch_add(chunk.len() as u64, std::sync::atomic::Ordering::Relaxed);
            if let Err(refusal) = self.policy.check_progress(started.elapsed(), received) {
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
            return respond(
                r.get_mut(),
                Reply::text(refusal.status(), refusal.message()),
                false,
            )
            .await;
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
                respond(r.get_mut(), Reply::text(200, "ok"), false).await
            }
            Err(e) => {
                tracing::warn!(%key, error = %e, "publish rejected");
                respond(r.get_mut(), Reply::text(400, &e.to_string()), false).await
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
                    tracing::info!(key, "track configuration changed; starting a new presentation");
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
    async fn next(&mut self, r: &mut BufReader<TcpStream>) -> Result<Option<Vec<u8>>> {
        match self {
            Body::Done => Ok(None),
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
}

impl Reply {
    fn text(status: u16, msg: &str) -> Self {
        Self {
            status,
            content_type: "text/plain",
            cache: "no-store",
            body: msg.as_bytes().to_vec(),
        }
    }

    fn playlist(body: Vec<u8>) -> Self {
        Self {
            status: 200,
            content_type: "application/vnd.apple.mpegurl",
            cache: "no-cache",
            body,
        }
    }

    fn xml(body: Vec<u8>) -> Self {
        Self {
            status: 200,
            content_type: "application/dash+xml",
            // A live manifest must never be cached: it describes a sliding window.
            cache: "no-cache",
            body,
        }
    }

    fn json(body: Vec<u8>) -> Self {
        Self {
            status: 200,
            content_type: "application/json",
            cache: "no-cache",
            body,
        }
    }

    fn media(content_type: &'static str, body: Vec<u8>) -> Self {
        Self {
            status: 200,
            content_type,
            cache: "public, max-age=60",
            body,
        }
    }

    /// A low-latency DASH CMAF chunk response: the segment in progress is
    /// served with `Transfer-Encoding: chunked` semantics (signalled here via
    /// `chunked: true`; [`respond`] switches to chunked framing), so a player
    /// that requested with `availabilityTimeOffset` starts receiving chunks
    /// before the segment completes.
    fn chunk(body: Vec<u8>) -> Self {
        Self {
            status: 200,
            content_type: "video/iso.segment",
            cache: "no-cache",
            body,
        }
    }

    /// Whether this reply must use HTTP chunked framing (LL-DASH chunks).
    fn is_chunk(&self) -> bool {
        self.content_type == "video/iso.segment" && self.cache == "no-cache"
    }
}

async fn respond(s: &mut TcpStream, r: Reply, head_only: bool) -> Result<()> {
    let reason = match r.status {
        200 => "OK",
        204 => "No Content",
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
    // Low-latency DASH chunks stream with chunked framing so the player starts
    // receiving CMAF chunks before the segment completes.
    let head = if r.is_chunk() && !head_only {
        format!(
            "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nCache-Control: {}\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Headers: *\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
            r.status, r.content_type, r.cache
        )
    } else {
        format!(
            "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: {}\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Headers: *\r\nConnection: close\r\n\r\n",
            r.status,
            r.content_type,
            r.body.len(),
            r.cache
        )
    };
    s.write_all(head.as_bytes()).await?;
    if !head_only {
        if r.is_chunk() {
            // One chunk is enough: the packager hands over every CMAF chunk
            // published so far, and the terminating zero-chunk closes it.
            let len = format!("{:X}\r\n", r.body.len());
            s.write_all(len.as_bytes()).await?;
            s.write_all(&r.body).await?;
            s.write_all(b"\r\n0\r\n\r\n").await?;
        } else {
            s.write_all(&r.body).await?;
        }
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
