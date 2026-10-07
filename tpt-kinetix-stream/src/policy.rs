//! Ingest hardening for the live server: publish authentication, per-publish
//! limits, and the counters behind `GET /metrics`.
//!
//! The default [`IngestPolicy`] is open (no token, no limits) so existing
//! deployments behave as before; every field tightens one thing.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// What a publisher must present and how much it may send.
#[derive(Clone, Debug, Default)]
pub struct IngestPolicy {
    /// Token every publisher must present (`Authorization: Bearer <t>` or
    /// `?token=<t>`). `None` leaves publishing open.
    pub token: Option<String>,
    /// Per-key tokens; a key listed here needs *its* token instead of `token`.
    pub key_tokens: HashMap<String, String>,
    /// Abort a publish that sends nothing for this long.
    pub idle_timeout: Option<Duration>,
    /// Abort a publish that has run this long.
    pub max_duration: Option<Duration>,
    /// Abort a publish whose sustained rate (measured after a 2 s grace)
    /// exceeds this many bits per second.
    pub max_bitrate_bps: Option<u64>,
    /// Abort a publish that has sent this many bytes.
    pub max_bytes: Option<u64>,
    /// Refuse a publish when this many streams are already live.
    pub max_streams: Option<usize>,
    /// Refuse (409) a second publisher on a key that is live, instead of
    /// replacing the presentation.
    pub reject_concurrent: bool,
}

/// Why a publish was refused or cut off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    Unauthorized,
    Conflict,
    TooManyStreams,
    BitrateExceeded,
    ByteLimit,
    DurationLimit,
    Idle,
}

impl Refusal {
    pub(crate) fn status(self) -> u16 {
        match self {
            Refusal::Unauthorized => 401,
            Refusal::Conflict => 409,
            Refusal::TooManyStreams => 503,
            Refusal::BitrateExceeded => 429,
            Refusal::ByteLimit => 413,
            Refusal::DurationLimit => 408,
            Refusal::Idle => 408,
        }
    }

    pub(crate) fn message(self) -> &'static str {
        match self {
            Refusal::Unauthorized => "missing or wrong publish token",
            Refusal::Conflict => "this key already has a publisher",
            Refusal::TooManyStreams => "too many live streams",
            Refusal::BitrateExceeded => "publish bitrate over the limit",
            Refusal::ByteLimit => "publish size over the limit",
            Refusal::DurationLimit => "publish duration over the limit",
            Refusal::Idle => "publisher idle for too long",
        }
    }
}

/// Constant-time byte comparison, so a token cannot be guessed by timing.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Extracts a presented token from the request headers / query string.
fn presented_token<'a>(headers: &'a HashMap<String, String>, query: &'a str) -> Option<&'a str> {
    if let Some(v) = headers.get("authorization") {
        let v = v.trim();
        if v.len() > 7 && v[..7].eq_ignore_ascii_case("bearer ") {
            return Some(v[7..].trim());
        }
    }
    query
        .split('&')
        .filter_map(|p| p.split_once('='))
        .find(|(k, _)| *k == "token")
        .map(|(_, v)| v)
}

impl IngestPolicy {
    /// The publish token a request presents (bearer header or `?token=`), if any.
    pub(crate) fn presented(headers: &HashMap<String, String>, query: &str) -> Option<String> {
        presented_token(headers, query).map(str::to_string)
    }

    /// Checks the publish credentials for `key`.
    pub(crate) fn authorize(
        &self,
        key: &str,
        headers: &HashMap<String, String>,
        query: &str,
    ) -> Result<(), Refusal> {
        let required = self.key_tokens.get(key).or(self.token.as_ref());
        let Some(required) = required else {
            return Ok(());
        };
        match presented_token(headers, query) {
            Some(t) if ct_eq(t.as_bytes(), required.as_bytes()) => Ok(()),
            _ => Err(Refusal::Unauthorized),
        }
    }

    /// Checks a running publish against the limits.
    pub(crate) fn check_progress(&self, elapsed: Duration, bytes: u64) -> Result<(), Refusal> {
        if self.max_bytes.is_some_and(|m| bytes > m) {
            return Err(Refusal::ByteLimit);
        }
        if self.max_duration.is_some_and(|m| elapsed > m) {
            return Err(Refusal::DurationLimit);
        }
        if let Some(max) = self.max_bitrate_bps {
            let secs = elapsed.as_secs_f64();
            if secs >= 2.0 && (bytes as f64 * 8.0 / secs) > max as f64 {
                return Err(Refusal::BitrateExceeded);
            }
        }
        Ok(())
    }
}

/// Counters exposed at `GET /metrics` in the Prometheus text format.
#[derive(Default)]
pub(crate) struct Metrics {
    pub publishes_started: AtomicU64,
    pub publishes_refused_auth: AtomicU64,
    pub publishes_refused_limit: AtomicU64,
    pub publishes_cut_off: AtomicU64,
    pub publish_bytes: AtomicU64,
    pub playback_requests: AtomicU64,
}

impl Metrics {
    pub(crate) fn inc(c: &AtomicU64) {
        c.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn count_refusal(&self, r: Refusal) {
        match r {
            Refusal::Unauthorized => Self::inc(&self.publishes_refused_auth),
            Refusal::Conflict | Refusal::TooManyStreams => Self::inc(&self.publishes_refused_limit),
            _ => Self::inc(&self.publishes_cut_off),
        }
    }

    /// Prometheus exposition text; `active` is the live publisher count.
    pub(crate) fn render(&self, active: usize) -> String {
        let g = |c: &AtomicU64| c.load(Ordering::Relaxed);
        let mut s = String::new();
        let mut line = |name: &str, kind: &str, help: &str, v: u64| {
            s.push_str(&format!(
                "# HELP {name} {help}\n# TYPE {name} {kind}\n{name} {v}\n"
            ));
        };
        line(
            "kinetix_publishers_active",
            "gauge",
            "Publishers currently sending.",
            active as u64,
        );
        line(
            "kinetix_publishes_started_total",
            "counter",
            "Publishes accepted.",
            g(&self.publishes_started),
        );
        line(
            "kinetix_publishes_refused_auth_total",
            "counter",
            "Publishes refused for a missing or wrong token.",
            g(&self.publishes_refused_auth),
        );
        line(
            "kinetix_publishes_refused_limit_total",
            "counter",
            "Publishes refused by the stream-count or concurrency limit.",
            g(&self.publishes_refused_limit),
        );
        line(
            "kinetix_publishes_cut_off_total",
            "counter",
            "Publishes aborted for exceeding a bitrate, size, duration or idle limit.",
            g(&self.publishes_cut_off),
        );
        line(
            "kinetix_publish_bytes_total",
            "counter",
            "Bytes received from publishers.",
            g(&self.publish_bytes),
        );
        line(
            "kinetix_playback_requests_total",
            "counter",
            "Playback (GET/HEAD) requests served.",
            g(&self.playback_requests),
        );
        s
    }
}

/// The set of keys with a live publisher.
#[derive(Default, Clone)]
pub(crate) struct ActiveSet(Arc<Mutex<HashSet<String>>>);

/// Releases a key from the [`ActiveSet`] when dropped.
pub(crate) struct ActiveGuard {
    set: ActiveSet,
    key: String,
}

impl ActiveSet {
    pub(crate) fn len(&self) -> usize {
        self.0.lock().unwrap().len()
    }

    /// Marks `key` live, applying the stream-count and concurrency limits.
    pub(crate) fn enter(&self, key: &str, policy: &IngestPolicy) -> Result<ActiveGuard, Refusal> {
        let mut set = self.0.lock().unwrap();
        if policy.reject_concurrent && set.contains(key) {
            return Err(Refusal::Conflict);
        }
        if !set.contains(key) && policy.max_streams.is_some_and(|m| set.len() >= m) {
            return Err(Refusal::TooManyStreams);
        }
        set.insert(key.to_string());
        Ok(ActiveGuard {
            set: self.clone(),
            key: key.to_string(),
        })
    }
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.set.0.lock().unwrap().remove(&self.key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(k: &str, v: &str) -> HashMap<String, String> {
        HashMap::from([(k.to_string(), v.to_string())])
    }

    #[test]
    fn open_policy_accepts_everyone() {
        let p = IngestPolicy::default();
        assert!(p.authorize("k", &HashMap::new(), "").is_ok());
    }

    #[test]
    fn token_via_bearer_or_query() {
        let p = IngestPolicy {
            token: Some("s3cret".into()),
            ..Default::default()
        };
        assert!(p
            .authorize("k", &h("authorization", "Bearer s3cret"), "")
            .is_ok());
        assert!(p
            .authorize("k", &HashMap::new(), "a=1&token=s3cret")
            .is_ok());
        assert_eq!(
            p.authorize("k", &h("authorization", "Bearer nope"), ""),
            Err(Refusal::Unauthorized)
        );
        assert_eq!(
            p.authorize("k", &HashMap::new(), ""),
            Err(Refusal::Unauthorized)
        );
    }

    #[test]
    fn key_token_overrides_global() {
        let p = IngestPolicy {
            token: Some("global".into()),
            key_tokens: HashMap::from([("cam".into(), "own".into())]),
            ..Default::default()
        };
        assert!(p.authorize("cam", &HashMap::new(), "token=own").is_ok());
        assert!(p.authorize("cam", &HashMap::new(), "token=global").is_err());
        assert!(p
            .authorize("other", &HashMap::new(), "token=global")
            .is_ok());
    }

    #[test]
    fn progress_limits() {
        let p = IngestPolicy {
            max_bitrate_bps: Some(8_000),
            max_bytes: Some(10_000),
            max_duration: Some(Duration::from_secs(60)),
            ..Default::default()
        };
        // 1 s in: inside the grace period, bitrate is not judged yet.
        assert!(p.check_progress(Duration::from_secs(1), 5_000).is_ok());
        // 4 s, 8000 B = 16 kbit/s > 8 kbit/s.
        assert_eq!(
            p.check_progress(Duration::from_secs(4), 8_000),
            Err(Refusal::BitrateExceeded)
        );
        assert_eq!(
            p.check_progress(Duration::from_secs(4), 20_000),
            Err(Refusal::ByteLimit)
        );
        assert_eq!(
            p.check_progress(Duration::from_secs(61), 1),
            Err(Refusal::DurationLimit)
        );
    }

    #[test]
    fn active_set_enforces_limits_and_releases() {
        let set = ActiveSet::default();
        let p = IngestPolicy {
            max_streams: Some(1),
            reject_concurrent: true,
            ..Default::default()
        };
        let g = set.enter("a", &p).unwrap();
        assert_eq!(set.len(), 1);
        assert!(matches!(set.enter("a", &p), Err(Refusal::Conflict)));
        assert!(matches!(set.enter("b", &p), Err(Refusal::TooManyStreams)));
        drop(g);
        assert_eq!(set.len(), 0);
        assert!(set.enter("b", &p).is_ok());
    }
}
