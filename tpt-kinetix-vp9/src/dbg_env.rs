//! Fast-path wrapper around `std::env::var_os` for the decoder's debug knobs.
//!
//! The decoder consults a large number of `TPT_VP9_*` debug switches, and some
//! of them sit in the *innermost* loops: `booldec::read_bool` checks one on
//! **every bool decoded** (millions of times per frame — every coefficient
//! token, every mode), and `loop_filter::loop_filter_edge` checks one on
//! **every deblocking edge of every superblock**. A real `std::env::var_os`
//! call locks the environment, scans it and allocates an `OsString`, which
//! dominated decode time (see `todo-perf.md` Phase 3).
//!
//! [`refresh`] is called once per [`crate::Vp9Decoder::decode`] and records
//! whether *any* `TPT_VP9_*` switch is set. While none is, [`var_os`] is a
//! single relaxed atomic load. When one is, it is exactly `std::env::var_os`,
//! so debugging tools that toggle variables between `decode` calls keep
//! behaving as before.
//!
//! This mirrors `tpt-kinetix-av1`'s `dbg_env`, which was written for the same
//! reason. Not part of the public API.
use std::sync::atomic::{AtomicBool, Ordering};

/// Starts `true` (slow but always correct) until the first [`refresh`].
static ANY_SET: AtomicBool = AtomicBool::new(true);

/// Re-scan the process environment for any `TPT_VP9_*` variable.
pub fn refresh() {
    let any = std::env::vars_os().any(|(k, _)| k.to_string_lossy().starts_with("TPT_VP9_"));
    ANY_SET.store(any, Ordering::Relaxed);
}

/// Drop-in replacement for `std::env::var_os` for `TPT_VP9_*` keys.
#[inline]
pub fn var_os(key: &str) -> Option<std::ffi::OsString> {
    if ANY_SET.load(Ordering::Relaxed) {
        std::env::var_os(key)
    } else {
        None
    }
}

/// Whether a `TPT_VP9_*` key is set, without building an `OsString` for it.
#[inline]
pub fn is_set(key: &str) -> bool {
    var_os(key).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fast path must never make a *set* variable invisible, or the
    /// `TPT_VP9_*` debugging tools would silently stop printing. `refresh()`
    /// is per-packet, so a switch set mid-run still has to be picked up.
    #[test]
    fn refresh_tracks_set_and_unset_keys() {
        // SAFETY: single-threaded test; no other thread reads the env here.
        unsafe { std::env::set_var("TPT_VP9_DBG_ENVGUARD_TEST", "1") };
        refresh();
        assert!(
            ANY_SET.load(Ordering::Relaxed),
            "a set key must arm the fast path"
        );
        assert!(is_set("TPT_VP9_DBG_ENVGUARD_TEST"));
        assert!(var_os("TPT_VP9_DBG_ENVGUARD_TEST").is_some());

        // A key with no `TPT_VP9_` prefix must not arm it on its own. (The
        // switch above has to be cleared first, or it is still legitimately
        // arming the fast path.)
        unsafe { std::env::remove_var("TPT_VP9_DBG_ENVGUARD_TEST") };
        unsafe { std::env::set_var("TPT_KINETIX_NOT_A_SWITCH", "1") };
        refresh();
        assert!(
            !ANY_SET.load(Ordering::Relaxed),
            "unrelated variables must not arm the slow path"
        );
        assert!(!is_set("TPT_VP9_DBG_ENVGUARD_TEST"));
        // The switch is still genuinely gone, not just hidden.
        assert!(std::env::var_os("TPT_VP9_DBG_ENVGUARD_TEST").is_none());

        unsafe { std::env::remove_var("TPT_KINETIX_NOT_A_SWITCH") };
        refresh();
        assert!(!ANY_SET.load(Ordering::Relaxed));
    }
}
