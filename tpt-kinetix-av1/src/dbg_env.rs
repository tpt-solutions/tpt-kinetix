//! Fast-path wrapper around `std::env::var` for the decoder's debug knobs.
//!
//! The decoder has a large number of `KINETIX_AV1_*` debug switches, many of
//! them consulted per block or per transform block. A real `std::env::var`
//! call costs an OS lookup plus a `String` allocation, which dominated decode
//! time. [`refresh`] (called once per [`crate::Av1Decoder::decode`]) records
//! whether *any* `KINETIX_*` debug switch is set; while none is, [`var`] returns
//! immediately. When one is set, [`var`] is exactly `std::env::var`, so
//! debugging tools that toggle variables between `decode` calls behave as
//! before. Not part of the public API.
use std::sync::atomic::{AtomicBool, Ordering};

/// Starts `true` (slow but always correct) until the first [`refresh`].
static ANY_SET: AtomicBool = AtomicBool::new(true);

/// Re-scan the process environment for any `KINETIX_*` variable.
pub fn refresh() {
    // `*_DIR` variables (e.g. `KINETIX_AV1_FATE_DIR`) and `KINETIX_BENCH_ITERS`
    // configure test tooling, never a decoder switch, so they must not
    // disable the fast path.
    let any = std::env::vars_os().any(|(k, _)| {
        let k = k.to_string_lossy();
        k.starts_with("KINETIX_") && !k.ends_with("_DIR") && k != "KINETIX_BENCH_ITERS"
    });
    ANY_SET.store(any, Ordering::Relaxed);
}

/// Drop-in replacement for `std::env::var` for `KINETIX_*` keys.
#[inline]
pub fn var(key: &str) -> Result<String, std::env::VarError> {
    if ANY_SET.load(Ordering::Relaxed) {
        std::env::var(key)
    } else {
        Err(std::env::VarError::NotPresent)
    }
}
