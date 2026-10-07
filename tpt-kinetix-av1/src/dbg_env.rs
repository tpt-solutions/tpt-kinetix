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
/// a sampling profiler (todo-perf.md Phase 2). Tile decode runs per tile on
/// rayon workers and the post-filter planes run concurrently, so the
/// accumulators are process-global atomics — thread-locals would hide the
/// share of the work a worker thread did from the main thread's report. A
/// per-frame summary is printed every `PHASE_REPORT_EVERY` frames.
#[derive(Default)]
pub struct Av1PhaseTimers {
    pub tile_ns: std::sync::atomic::AtomicU64,
    pub deblock_ns: std::sync::atomic::AtomicU64,
    pub cdef_ns: std::sync::atomic::AtomicU64,
    pub superres_ns: std::sync::atomic::AtomicU64,
    pub lr_ns: std::sync::atomic::AtomicU64,
    pub grain_ns: std::sync::atomic::AtomicU64,
    /// Tile-phase sub-splits (todo-perf.md Phase 3b item 2). `tile_ns` lumps
    /// entropy decode, coefficient read, dequant + inverse transform,
    /// prediction and motion compensation together, and every measurement so
    /// far says the entropy slice dominates — so the ranking *inside* the tile
    /// phase is what decides whether the next move is a SIMD kernel (item 3)
    /// or entropy work (item 5). These four are all subsets of `tile_ns`; their
    /// sum is less than it because the partition/mode syntax between blocks is
    /// untimed.
    /// Coefficient read through the symbol decoder (`read_coeffs`).
    pub coeff_ns: std::sync::atomic::AtomicU64,
    /// Dequantize + 2-D inverse transform.
    pub itx_ns: std::sync::atomic::AtomicU64,
    /// Intra prediction + the residual add back into the plane.
    pub pred_ns: std::sync::atomic::AtomicU64,
    /// Inter prediction: motion-vector prediction + motion compensation.
    pub mc_ns: std::sync::atomic::AtomicU64,
    pub frames: std::sync::atomic::AtomicU64,
}

impl Av1PhaseTimers {
    const fn new() -> Self {
        Self {
            tile_ns: std::sync::atomic::AtomicU64::new(0),
            deblock_ns: std::sync::atomic::AtomicU64::new(0),
            cdef_ns: std::sync::atomic::AtomicU64::new(0),
            superres_ns: std::sync::atomic::AtomicU64::new(0),
            lr_ns: std::sync::atomic::AtomicU64::new(0),
            grain_ns: std::sync::atomic::AtomicU64::new(0),
            coeff_ns: std::sync::atomic::AtomicU64::new(0),
            itx_ns: std::sync::atomic::AtomicU64::new(0),
            pred_ns: std::sync::atomic::AtomicU64::new(0),
            mc_ns: std::sync::atomic::AtomicU64::new(0),
            frames: std::sync::atomic::AtomicU64::new(0),
        }
    }
}

const PHASE_REPORT_EVERY: u64 = 100;

static AV1_PHASES: Av1PhaseTimers = Av1PhaseTimers::new();

/// Run `f`, accumulating its wall time into `phase` when `KINETIX_AV1_PHASE`
/// is set; passes through untouched (and free) otherwise.
#[inline]
pub fn av1_timed<R>(
    phase: &dyn Fn(&Av1PhaseTimers) -> &std::sync::atomic::AtomicU64,
    f: impl FnOnce() -> R,
) -> R {
    use std::sync::atomic::Ordering;
    if !phase_enabled() {
        return f();
    }
    let t = std::time::Instant::now();
    let out = f();
    let dt = t.elapsed().as_nanos() as u64;
    phase(&AV1_PHASES).fetch_add(dt, Ordering::Relaxed);
    out
}

/// Account one frame and print the accumulated averages when the report
/// interval elapses.
pub fn av1_phase_frame_tick() {
    use std::sync::atomic::Ordering;
    if !phase_enabled() {
        return;
    }
    let p = &AV1_PHASES;
    let f = p.frames.fetch_add(1, Ordering::Relaxed) + 1;
    if f % PHASE_REPORT_EVERY == 0 {
        let n = f.max(1) as f64;
        let us =
            |cell: &std::sync::atomic::AtomicU64| cell.load(Ordering::Relaxed) as f64 / 1000.0 / n;
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
        // Second line rather than an extension of the first: anything
        // scraping the phase line (docs, ad-hoc greps) keeps working.
        eprintln!(
            "av1 tile sub-phases (per-frame avg over {f}, all subsets of tiles): coeffs {:.0}us itx {:.0}us intra-pred {:.0}us mc {:.0}us",
            us(&p.coeff_ns),
            us(&p.itx_ns),
            us(&p.pred_ns),
            us(&p.mc_ns),
        );
        // Reset after printing (swap), so the next window starts at zero.
        // `fetch_sub(f * PHASE_REPORT_EVERY)` would race with concurrent
        // additions; a plain swap is the documented idiom.
        let reset = |cell: &std::sync::atomic::AtomicU64| cell.swap(0, Ordering::Relaxed);
        reset(&p.tile_ns);
        reset(&p.deblock_ns);
        reset(&p.cdef_ns);
        reset(&p.superres_ns);
        reset(&p.lr_ns);
        reset(&p.grain_ns);
        reset(&p.coeff_ns);
        reset(&p.itx_ns);
        reset(&p.pred_ns);
        reset(&p.mc_ns);
        p.frames.swap(0, Ordering::Relaxed);
    }
}

/// Whether the phase timers are on. This is itself a hot-path predicate —
/// `av1_timed` calls it around every timed phase of every frame — so it goes
/// through the same guarded fast path as every other debug knob rather than
/// doing a real environment lookup each time.
fn phase_enabled() -> bool {
    is_set("KINETIX_AV1_PHASE")
}
