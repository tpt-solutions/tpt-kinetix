//! Fast-path wrapper around `std::env::var` for the decoder's debug knobs.
//!
//! The decoder has a large number of `KINETIX_*` debug switches, several of
//! them consulted per macroblock or per slice (`KINETIX_BINTRACE`,
//! `KINETIX_FFLAG`, `KINETIX_SKIP_DEBLOCK`, ...). A real `std::env::var` call
//! locks the environment, scans it and allocates an `OsString` — at 1080p that
//! is tens of thousands of lookups per frame on the hottest decode paths.
//! [`refresh`] (called once per [`crate::H264Decoder::decode`]) records whether
//! *any* `KINETIX_*` switch is set; while none is, [`var`] / [`var_os`] return
//! immediately. When one is set, they are exactly `std::env::var` /
//! `std::env::var_os`, so tools that toggle variables between `decode` calls
//! behave as before. Not part of the public API.
//!
//! Same pattern as `tpt-kinetix-av1::dbg_env` and `tpt-kinetix-vp9::dbg_env`.
use std::sync::atomic::{AtomicBool, Ordering};

/// Starts `true` (slow but always correct) until the first [`refresh`].
static ANY_SET: AtomicBool = AtomicBool::new(true);

/// Re-scan the process environment for any `KINETIX_*` variable.
pub fn refresh() {
    let any = std::env::vars_os().any(|(k, _)| {
        let k = k.to_string_lossy();
        k.starts_with("KINETIX_")
    });
    ANY_SET.store(any, Ordering::Relaxed);
}

/// Drop-in replacement for `std::env::var` for `KINETIX_*` keys.
#[inline]
pub fn var(key: &str) -> Result<String, std::env::VarError> {
    if !ANY_SET.load(Ordering::Relaxed) {
        return Err(std::env::VarError::NotPresent);
    }
    std::env::var(key)
}

/// Drop-in replacement for `std::env::var_os` for `KINETIX_*` keys.
#[inline]
pub fn var_os(key: &str) -> Option<std::ffi::OsString> {
    if !ANY_SET.load(Ordering::Relaxed) {
        return None;
    }
    std::env::var_os(key)
}

/// Whether `key` is set, with the same fast path as [`var`].
#[inline]
pub fn is_set(key: &str) -> bool {
    var_os(key).is_some()
}
