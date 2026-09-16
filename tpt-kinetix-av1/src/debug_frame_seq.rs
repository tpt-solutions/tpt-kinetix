//! Scratch debug-only global counter used to correlate cross-module
//! `eprintln!` traces (e.g. "which real coded frame is this?") when
//! bisecting an ordering bug. Not part of the public API.
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);
static ACTIVE: AtomicU64 = AtomicU64::new(0);

/// Call once at the top of processing for a new real coded frame: bumps the
/// running counter and records it as the "currently active" frame label that
/// `current()` reports for the rest of that frame's processing (until the
/// next `next()` call).
pub fn next() -> u64 {
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    ACTIVE.store(n, Ordering::SeqCst);
    n
}

/// The label of whichever frame most recently called `next()` — stable for
/// the whole duration of that frame's processing (unlike reading `SEQ`
/// directly, which is already bumped for the *next* frame by the time a
/// later call site in the *current* frame's processing reads it).
pub fn current() -> u64 {
    ACTIVE.load(Ordering::SeqCst)
}
