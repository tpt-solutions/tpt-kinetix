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

/// Whether a `KINETIX_*` key is set, without building a `String` for it.
#[inline]
pub fn is_set(key: &str) -> bool {
    var(key).is_ok()
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

/// Env-gated phase timing (`KINETIX_AV1_PHASE=1`), the admin-free stand-in for
/// a sampling profiler (todo-perf.md Phase 2). AV1 decode is single-threaded,
/// so thread-local accumulators are enough; a per-frame summary is printed
/// every [`PHASE_REPORT_EVERY`] frames.
#[derive(Default)]
pub struct Av1PhaseTimers {
    pub tile_ns: std::cell::Cell<u64>,
    pub deblock_ns: std::cell::Cell<u64>,
    pub cdef_ns: std::cell::Cell<u64>,
    pub superres_ns: std::cell::Cell<u64>,
    pub lr_ns: std::cell::Cell<u64>,
    pub grain_ns: std::cell::Cell<u64>,
    pub frames: std::cell::Cell<u64>,
}

const PHASE_REPORT_EVERY: u64 = 100;

thread_local! {
    static AV1_PHASES: Av1PhaseTimers = Av1PhaseTimers::default();
}

/// Run `f`, accumulating its wall time into `phase` when `KINETIX_AV1_PHASE`
/// is set; passes through untouched (and free) otherwise.
#[inline]
pub fn av1_timed<R>(
    phase: &dyn Fn(&Av1PhaseTimers) -> &std::cell::Cell<u64>,
    f: impl FnOnce() -> R,
) -> R {
    if !phase_enabled() {
        return f();
    }
    let t = std::time::Instant::now();
    let out = f();
    let dt = t.elapsed().as_nanos() as u64;
    AV1_PHASES.with(|p| {
        let c = phase(p);
        c.set(c.get().wrapping_add(dt));
    });
    out
}

/// Account one frame and print the accumulated averages when the report
/// interval elapses.
pub fn av1_phase_frame_tick() {
    if !phase_enabled() {
        return;
    }
    AV1_PHASES.with(|p| {
        let f = p.frames.get() + 1;
        p.frames.set(f);
        if f % PHASE_REPORT_EVERY == 0 {
            let n = f.max(1) as f64;
            let us = |cell: &std::cell::Cell<u64>| cell.get() as f64 / 1000.0 / n;
            let (t, d, c, s, l, g) = (
                &p.tile_ns,
                &p.deblock_ns,
                &p.cdef_ns,
                &p.superres_ns,
                &p.lr_ns,
                &p.grain_ns,
            );
            eprintln!(
                "av1 phases (per-frame avg over {f}): tiles {:.0}us deblock {:.0}us cdef {:.0}us superres {:.0}us loop-restoration {:.0}us film-grain {:.0}us",
                us(t),
                us(d),
                us(c),
                us(s),
                us(l),
                us(g),
            );
            p.tile_ns.set(0);
            p.deblock_ns.set(0);
            p.cdef_ns.set(0);
            p.superres_ns.set(0);
            p.lr_ns.set(0);
            p.grain_ns.set(0);
            p.frames.set(0);
        }
    });
}

/// Whether the phase timers are on. This is itself a hot-path predicate —
/// `av1_timed` calls it around every timed phase of every frame — so it goes
/// through the same guarded fast path as every other debug knob rather than
/// doing a real environment lookup each time.
fn phase_enabled() -> bool {
    is_set("KINETIX_AV1_PHASE")
}
