//! AV1 symbol (arithmetic) decoder (AV1 spec §8.2, "Parsing process for
//! symbol decoder").
//!
//! This is the multi-symbol adaptive range decoder that all CDF-coded (`S()`)
//! syntax elements in a real AV1 tile go through. It is a distinct entropy
//! coding engine from H.264 CABAC (`tpt-kinetix-h264`) and from the ad hoc
//! `BitReader`-based exp-golomb-like scheme `reconstruct::decode_tile_group`
//! used to read coefficients before AV1 Phase B — that scheme could not
//! decode a real AV1 bitstream, since real encoders never produce it.
//! [`crate::coeff`] now drives this decoder with the spec `coeffs()` syntax,
//! and `decode_tile_group` calls into that.
//!
//! Implements, verbatim per spec:
//! * §8.2.2 `init_symbol` — decoder initialization
//! * §8.2.3 `read_bool` — the fixed-probability (p=1/2) boolean special case
//! * §8.2.5 `read_literal` — `n` raw-ish bits built from `read_bool`
//! * §8.2.6 `read_symbol` — the general N-ary decode + CDF adaptation/update
//!
//! **Not implemented here**: §8.2.4 `exit_symbol` (trailing-bit validation
//! and the Tile*→Saved* CDF snapshot copy), since that is tied to per-tile
//! bookkeeping (`context_update_tile_id`, `disable_frame_end_update_cdf`,
//! the full set of named CDF arrays) that arrives with the real tile/
//! partition syntax in AV1 Phase C.
//!
//! CDF arrays use the spec's representation directly: an array of length
//! `N + 1` for an `N`-symbol alphabet, where `cdf[N - 1] == 1 << 15` (32768,
//! the fixed top of the probability space) and `cdf[N]` is an adaptation
//! counter (saturating at 32) rather than a probability.

/// Number of bits to reduce CDF precision during arithmetic coding (spec
/// `EC_PROB_SHIFT`).
const EC_PROB_SHIFT: u32 = 6;

/// Minimum probability assigned to each symbol during arithmetic coding
/// (spec `EC_MIN_PROB`).
const EC_MIN_PROB: u32 = 4;

// ─────────────────────────────────────────────────────────────────────────
// Symbol-level trace (AV1 Phase G.0 tooling, 2026-08-20).
//
// A zero-cost-when-unused, structured (not `eprintln!`-text) record of every
// `read_symbol` call this decoder makes, gated by the `KINETIX_AV1_SYMBOL_TRACE`
// env var. This replaces one-off `eprintln!("DBG ...")` sprinkles with data a
// harness can actually diff programmatically: source location (via
// `#[track_caller]`, propagated transparently through `read_bool`/
// `read_literal` since those are *also* `#[track_caller]`, so a
// `dec.read_literal(4)` call in `intra_block.rs` shows *that* call site, not
// `read_bool`'s internal one), bit position before/after, alphabet size, and
// the decoded value. Threaded via a `thread_local` rather than a parameter on
// every call site in `coeff.rs`/`reconstruct/*.rs`, since piping an explicit
// sink through the whole `decode_tile_group` → `TileDecodeState` →
// `decode_superblock` → `decode_intra_block`/`reconstruct_tx_block` call
// chain would touch dozens of signatures for a debug-only feature.
use std::cell::RefCell;
use std::panic::Location;

/// One symbol-decoder read, captured when tracing is enabled.
#[derive(Debug, Clone, Copy)]
pub struct SymbolTraceEntry {
    /// Monotonic index of this read within the current trace session.
    pub seq: usize,
    /// Alphabet size (`cdf.len() - 1`) of the symbol read. `read_bool`/
    /// `read_literal` bit-reads show `n_symbols == 2`.
    pub n_symbols: usize,
    /// The decoded symbol index (`0..n_symbols`).
    pub value: usize,
    /// Absolute bit position in the tile's data buffer before this read.
    pub bit_pos_before: usize,
    /// Absolute bit position after this read (renormalization included).
    pub bit_pos_after: usize,
    /// Source location of the *originating* call (propagated through
    /// `read_bool`/`read_literal` when they're the direct caller).
    pub location: &'static Location<'static>,
}

thread_local! {
    static SYMBOL_TRACE: RefCell<Option<Vec<SymbolTraceEntry>>> = const { RefCell::new(None) };
}

/// Start (or reset) a symbol trace for the current thread. Call before
/// decoding; drain with [`take_symbol_trace`] after.
pub fn enable_symbol_trace() {
    SYMBOL_TRACE.with(|t| *t.borrow_mut() = Some(Vec::new()));
    BLOCK_MARKERS.with(|t| *t.borrow_mut() = Some(Vec::new()));
}

/// Whether a trace session is currently active on this thread (checked once
/// per `read_symbol` call; cheap relative to the arithmetic decode itself).
pub fn symbol_trace_enabled() -> bool {
    SYMBOL_TRACE.with(|t| t.borrow().is_some())
}

/// Take (and clear) the accumulated trace for the current thread.
pub fn take_symbol_trace() -> Vec<SymbolTraceEntry> {
    SYMBOL_TRACE.with(|t| t.borrow_mut().take().unwrap_or_default())
}

/// Non-consuming copy of the current trace values (decoded symbol indices),
/// for embedding into a block capture so the oracle can diff its own
/// independent re-decode against them.
pub fn symbol_trace_values() -> Vec<usize> {
    SYMBOL_TRACE.with(|t| {
        t.borrow()
            .as_ref()
            .map_or_else(Vec::new, |v| v.iter().map(|e| e.value).collect())
    })
}

/// Length of the active trace (used to slice the per-block symbol range).
pub fn symbol_trace_len_now() -> usize {
    symbol_trace_len()
}

fn push_symbol_trace(entry_fn: impl FnOnce(usize) -> SymbolTraceEntry) {
    SYMBOL_TRACE.with(|t| {
        if let Some(v) = t.borrow_mut().as_mut() {
            let seq = v.len();
            v.push(entry_fn(seq));
        }
    });
}

/// The current length of the active trace, i.e. the `seq` the *next*
/// `read_symbol` call will get. Used by [`mark_block`] to stamp block-level
/// markers with the trace index they precede, without needing every
/// `reconstruct/` call site to thread a sink through.
pub fn symbol_trace_len() -> usize {
    SYMBOL_TRACE.with(|t| t.borrow().as_ref().map_or(0, Vec::len))
}

/// A human-readable label for "what block/stage starts at trace index
/// `trace_seq`", pushed from `reconstruct/` at natural block/tx boundaries
/// (`decode_intra_block`, `reconstruct_tx_block`) when tracing is enabled.
/// This is what turns "trace entry #47 diverged" into "that's mi=(0,8)
/// BLOCK_16X16, plane 0, tx px=(32,8)" for a human without re-deriving it.
#[derive(Debug, Clone)]
pub struct BlockMarker {
    pub trace_seq: usize,
    pub label: String,
}

thread_local! {
    static BLOCK_MARKERS: RefCell<Option<Vec<BlockMarker>>> = const { RefCell::new(None) };
}

/// Record a block/stage boundary marker at the current trace position, if a
/// trace session is active (no-op and no allocation otherwise).
pub fn mark_block(label: impl FnOnce() -> String) {
    if !symbol_trace_enabled() {
        return;
    }
    let trace_seq = symbol_trace_len();
    BLOCK_MARKERS.with(|t| {
        let mut slot = t.borrow_mut();
        if slot.is_none() {
            *slot = Some(Vec::new());
        }
        slot.as_mut().unwrap().push(BlockMarker {
            trace_seq,
            label: label(),
        });
    });
}

/// One block's byte-level capture, for the independent Python oracle
/// (`tools/av1_oracle/diff_block.py`). When `KINETIX_AV1_CAPTURE` names a
/// `(plane, px_x, px_y)` block, the decoder writes a JSON file to
/// `av1_capture.json` capturing that block's raw tile bytes (starting at the
/// exact bit offset where its `coeffs()` begins), its `TxBlockCtx`, and — most
/// importantly — the slice of Kinetix's own symbol-trace values for *this
/// block's* `coeffs()` read, so the oracle can independently re-decode the same
/// bytes and diff symbol-by-symbol.
///
/// Call this **after** `read_coeffs` for the block, passing `pre_bit_pos`
/// (the decoder's `bit_pos` immediately before the call) and
/// `pre_trace_len` (the trace length before the call); this lets the function
/// capture both the byte range and the per-block symbol slice.
///
/// This is the bridge spelled out as todo-av1.md Phase G.0's next step (1):
/// "extract real tile bytes + the exact `TxBlockCtx`/CDF state at a specific
/// block via the new symbol-trace/block-marker infrastructure".
///
/// We hand-roll JSON (rather than pull in `serde`) because this is a
/// debug-only capture path and the crate doesn't otherwise depend on `serde`.
pub fn maybe_capture_block(
    dec: &SymbolDecoder,
    blk: &crate::coeff::TxBlockCtx,
    ctxs: &crate::coeff::CoeffContexts,
    cdfs: &crate::coeff::TileCdfs,
    base_q_idx: u8,
    pre_bit_pos: usize,
    pre_trace_len: usize,
) -> bool {
    let (plane, px_x, px_y) = match capture_target() {
        Some(t) => t,
        None => return false,
    };
    // Match on `blk.plane` plus the transform block's pixel origin derived
    // from `x4`/`y4` (units of 4 samples).
    let origin_x = blk.x4 * 4;
    let origin_y = blk.y4 * 4;
    if plane != blk.plane || px_x != origin_x || px_y != origin_y {
        return false;
    }
    let byte_off = pre_bit_pos / 8;
    let data_hex = if byte_off < dec.data.len() {
        hex_encode(&dec.data[byte_off..])
    } else {
        String::new()
    };
    // Slice Kinetix's own trace to this block's coeffs() symbol range.
    let all_vals = symbol_trace_values();
    let ref_vals: Vec<usize> = all_vals
        .into_iter()
        .skip(pre_trace_len)
        .collect();
    let ref_json = ref_vals
        .iter()
        .map(|v| v.to_string())
        .collect::<Vec<_>>()
        .join(",");
    // Neighbour level/dc context (needed for the oracle to start from the
    // same state Kinetix had at this block — otherwise contexts derived from
    // above_level/left_level diverge immediately).
    let snap = ctxs.ctx_snapshot(blk.plane);
    let fmt_vec = |v: &[u8]| {
        v.iter()
            .map(|x| x.to_string())
            .collect::<Vec<_>>()
            .join(",")
    };
    let above_level = fmt_vec(&snap.above_level);
    let above_dc = fmt_vec(&snap.above_dc);
    let left_level = fmt_vec(&snap.left_level);
    let left_dc = fmt_vec(&snap.left_dc);
    // Adapted (mid-tile) CDF tables, so the oracle replays Kinetix's exact
    // state at this block — removing the last confound (CDF adaptation) from
    // the symbol diff.
    let cdf = cdfs.cdf_snapshot();
    let cdfs_json = format!(
        "{{\n    \
         \"txb_skip\": {},\n    \"eob_pt_16\": {},\n    \"eob_pt_32\": {},\n    \
         \"eob_pt_64\": {},\n    \"eob_pt_128\": {},\n    \"eob_pt_256\": {},\n    \
         \"eob_pt_512\": {},\n    \"eob_pt_1024\": {},\n    \"eob_extra\": {},\n    \
         \"coeff_base_eob\": {},\n    \"coeff_base\": {},\n    \"coeff_br\": {},\n    \
         \"dc_sign\": {},\n    \"intra_tx_type_set1\": {},\n    \"intra_tx_type_set2\": {}\n  }}",
        json_nest3_u16(&cdf.txb_skip),
        json_nest3_u16(&cdf.eob_pt_16),
        json_nest3_u16(&cdf.eob_pt_32),
        json_nest3_u16(&cdf.eob_pt_64),
        json_nest3_u16(&cdf.eob_pt_128),
        json_nest3_u16(&cdf.eob_pt_256),
        json_nest2_u16(&cdf.eob_pt_512),
        json_nest2_u16(&cdf.eob_pt_1024),
        json_nest4_u16(&cdf.eob_extra),
        json_nest4_u16(&cdf.coeff_base_eob),
        json_nest4_u16(&cdf.coeff_base),
        json_nest4_u16(&cdf.coeff_br),
        json_nest3_u16(&cdf.dc_sign),
        json_nest3_u16(&cdf.intra_tx_type_set1),
        json_nest3_u16(&cdf.intra_tx_type_set2),
    );
    let json = format!(
        "{{\n  \"data_hex\": \"{data_hex}\",\n  \"bit_offset\": 0,\n  \
         \"base_q_idx\": {base_q_idx},\n  \"reference_values\": [{ref_json}],\n  \
         \"ctx\": {{\n    \"above_level\": [{above_level}],\n    \
         \"above_dc\": [{above_dc}],\n    \"left_level\": [{left_level}],\n    \
         \"left_dc\": [{left_dc}]\n  }},\n  \
         \"cdfs\": {cdfs_json},\n  \
         \"blk\": {{\n    \"plane\": {},\n    \
         \"tx_size\": {},\n    \"x4\": {},\n    \"y4\": {},\n    \"max_x4\": {},\n    \
         \"max_y4\": {},\n    \"block_w\": {},\n    \"block_h\": {},\n    \
         \"intra_dir\": {},\n    \"uv_mode\": {},\n    \"qindex_positive\": {},\n    \
         \"reduced_tx_set\": {},\n    \"lossless\": {}\n  }}\n}}\n",
        blk.plane,
        blk.tx_size,
        blk.x4,
        blk.y4,
        blk.max_x4,
        blk.max_y4,
        blk.block_w,
        blk.block_h,
        blk.intra_dir,
        blk.uv_mode,
        blk.qindex_positive,
        blk.reduced_tx_set,
        blk.lossless,
    );
    let _ = std::fs::write("av1_capture.json", json);
    true
}

fn capture_target() -> Option<(usize, usize, usize)> {
    let spec = std::env::var("KINETIX_AV1_CAPTURE").ok()?;
    // Capturing a block implies we want a symbol trace so the per-block symbol
    // slice can be embedded for the oracle diff.
    if !symbol_trace_enabled() {
        enable_symbol_trace();
    }
    let mut parts = spec.split(':');
    let plane = parts.next()?.parse().ok()?;
    let px_x = parts.next()?.parse().ok()?;
    let px_y = parts.next()?.parse().ok()?;
    Some((plane, px_x, px_y))
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Serialize a nested `Vec<Vec<u16>>` to compact JSON for the block capture
/// (used for the adapted-CDF snapshot so the oracle can replay Kinetix's
/// exact mid-tile CDF tables).
fn json_nest_u16(v: &[Vec<u16>]) -> String {
    let rows: Vec<String> = v
        .iter()
        .map(|r| {
            let inner: Vec<String> = r.iter().map(|x| x.to_string()).collect();
            format!("[{}]", inner.join(","))
        })
        .collect();
    format!("[{}]", rows.join(","))
}

/// Serialize a 2-deep `Vec<Vec<u16>>` (eob_pt_512/eob_pt_1024) to compact JSON.
fn json_nest2_u16(v: &[Vec<u16>]) -> String {
    json_nest_u16(v)
}

/// Serialize a 3-deep `Vec<Vec<Vec<u16>>>` to compact JSON.
fn json_nest3_u16(v: &[Vec<Vec<u16>>]) -> String {
    let l: Vec<String> = v.iter().map(json_nest_u16).collect();
    format!("[{}]", l.join(","))
}

/// Serialize a 4-deep `Vec<Vec<Vec<Vec<u16>>>>` (coeff_base/coeff_br/eob_extra)
/// to compact JSON.
fn json_nest4_u16(v: &[Vec<Vec<Vec<u16>>]) -> String {
    let l: Vec<String> = v.iter().map(json_nest3_u16).collect();
    format!("[{}]", l.join(","))
}

/// Take (and clear) the accumulated block markers for the current thread.
pub fn take_block_markers() -> Vec<BlockMarker> {
    BLOCK_MARKERS.with(|t| t.borrow_mut().take().unwrap_or_default())
}

/// `FloorLog2(x)`: floor of the base-2 logarithm of `x` (spec §4.7).
///
/// `x` must be >= 1 (guaranteed by all call sites below: `SymbolRange` is
/// always >= 1, and `N` — the symbol count — is always >= 2).
#[inline]
fn floor_log2(mut x: u32) -> u32 {
    debug_assert!(x >= 1);
    let mut s = 0u32;
    while x != 0 {
        x >>= 1;
        s += 1;
    }
    s - 1
}

/// The AV1 symbol (arithmetic) decoder.
///
/// Reads MSB-first from a byte-aligned buffer, per §8.2.2. Bits read past
/// the end of `data` are treated as zero padding, matching the spec's
/// `SymbolMaxBits`-going-negative behavior.
pub struct SymbolDecoder<'a> {
    data: &'a [u8],
    bit_pos: usize,
    symbol_value: u32,
    symbol_range: u32,
    symbol_max_bits: i64,
}

impl<'a> SymbolDecoder<'a> {
    /// `init_symbol(sz)` (§8.2.2), with `sz = data.len()`.
    pub fn new(data: &'a [u8]) -> Self {
        Self::new_with_bit_offset(data, 0)
    }

    /// Construct a decoder over `data` that begins reading at `bit_offset`
    /// bits into the buffer. Used when a tile group's entropy-coded payload
    /// immediately follows a tile-group header that didn't start the buffer
    /// at byte 0 (e.g. a multi-tile `tile_start_and_end_present_flag` +
    /// `tg_start`/`tg_end` read before `byte_alignment()`).
    ///
    /// `init_symbol`'s initial 15-bit window and `SymbolMaxBits` must be
    /// read/computed starting at `bit_offset`, not at bit 0 — a previous
    /// version here called the bit-0 constructor first and only overwrote
    /// `bit_pos` afterwards, which left `symbol_value` initialized from the
    /// wrong bytes (and `symbol_max_bits` too large by `bit_offset`) for any
    /// nonzero offset, desyncing the entire tile from the very first symbol.
    pub fn new_with_bit_offset(data: &'a [u8], bit_offset: usize) -> Self {
        let mut dec = SymbolDecoder {
            data,
            bit_pos: bit_offset,
            symbol_value: 0,
            symbol_range: 1 << 15,
            symbol_max_bits: 0,
        };
        let remaining_bits = (data.len() * 8).saturating_sub(bit_offset);
        let num_bits = remaining_bits.min(15) as u32;
        let buf = dec.f(num_bits);
        let padded_buf = buf << (15 - num_bits);
        dec.symbol_value = ((1u32 << 15) - 1) ^ padded_buf;
        dec.symbol_range = 1 << 15;
        dec.symbol_max_bits = remaining_bits as i64 - 15;
        dec
    }

    /// Scratch debug accessor, session 2026-08-19: how far into `data` (in
    /// bits) the decoder has read, and how many real (non-padding) bits
    /// remain per `SymbolMaxBits`. Used to rule out an upstream desync that
    /// already ran the decoder past real tile data by the time it reaches a
    /// specific block. Not spec-normative; delete once root-caused.
    pub fn dbg_bit_pos(&self) -> (usize, i64, usize) {
        (self.bit_pos, self.symbol_max_bits, self.data.len() * 8)
    }

    /// Current read position in bits into the tile data buffer. Used by the
    /// Phase G.0 block-capture bridge to record the exact byte/bit offset a
    /// block's `coeffs()` begins at, so the independent oracle can re-seek
    /// there. (Not spec-normative.)
    pub fn bit_position(&self) -> usize {
        self.bit_pos
    }

    /// Raw bitstream bit read, MSB-first, byte-aligned start. Returns 0 for
    /// positions past the end of `data` (spec §8.2.2 padding behavior).
    fn read_bit(&mut self) -> u32 {
        let byte_idx = self.bit_pos / 8;
        let bit = if byte_idx < self.data.len() {
            let shift = 7 - (self.bit_pos % 8);
            (self.data[byte_idx] >> shift) & 1
        } else {
            0
        };
        self.bit_pos += 1;
        bit as u32
    }

    /// `f(n)` (§9 "Parsing process for f(n)"): read `n` bits MSB-first.
    fn f(&mut self, n: u32) -> u32 {
        let mut x = 0u32;
        for _ in 0..n {
            x = 2 * x + self.read_bit();
        }
        x
    }

    /// `read_symbol(cdf)` (§8.2.6).
    ///
    /// `cdf` has length `N + 1` for an `N`-symbol alphabet: `cdf[N - 1]`
    /// must equal `1 << 15`, and `cdf[N]` is the adaptation counter. Always
    /// performs the CDF update (equivalent to `disable_cdf_update == 0`);
    /// callers that need the non-adapting variant should pass a scratch
    /// copy of the table.
    ///
    /// Returns the decoded symbol index (`0..N`).
    #[track_caller]
    pub fn read_symbol(&mut self, cdf: &mut [u16]) -> usize {
        let n = cdf.len() - 1;
        debug_assert!(n >= 2, "cdf must describe at least 2 symbols");
        debug_assert_eq!(cdf[n - 1], 1 << 15, "cdf[N-1] must be 32768");
        let location = Location::caller();
        let bit_pos_before = self.bit_pos;

        let mut cur = self.symbol_range;
        let mut prev;
        let mut symbol: usize = 0;
        loop {
            prev = cur;
            let f = (1u32 << 15) - cdf[symbol] as u32;
            cur = ((self.symbol_range >> 8) * (f >> EC_PROB_SHIFT)) >> (7 - EC_PROB_SHIFT);
            cur += EC_MIN_PROB * (n as u32 - symbol as u32 - 1);
            if self.symbol_value >= cur {
                break;
            }
            symbol += 1;
        }

        self.symbol_range = prev - cur;
        self.symbol_value -= cur;

        // Renormalization (§8.2.6, ordered steps 1-7).
        let bits = 15 - floor_log2(self.symbol_range);
        self.symbol_range <<= bits;
        let num_bits = bits.min(self.symbol_max_bits.max(0) as u32);
        let new_data = self.f(num_bits);
        let padded_data = new_data << (bits - num_bits);
        self.symbol_value = padded_data ^ (((self.symbol_value + 1) << bits) - 1);
        self.symbol_max_bits -= bits as i64;

        // CDF adaptation/update.
        let count = cdf[n] as u32;
        let rate = 3 + (count > 15) as u32 + (count > 31) as u32 + floor_log2(n as u32).min(2);
        let mut tmp: u32 = 0;
        for (i, slot) in cdf[..n - 1].iter_mut().enumerate() {
            if i == symbol {
                tmp = 1 << 15;
            }
            let c = *slot as u32;
            *slot = if tmp < c {
                (c - ((c - tmp) >> rate)) as u16
            } else {
                (c + ((tmp - c) >> rate)) as u16
            };
        }
        if cdf[n] < 32 {
            cdf[n] += 1;
        }

        if symbol_trace_enabled() {
            let bit_pos_after = self.bit_pos;
            push_symbol_trace(|seq| SymbolTraceEntry {
                seq,
                n_symbols: n,
                value: symbol,
                bit_pos_before,
                bit_pos_after,
                location,
            });
        }

        symbol
    }

    /// `read_bool()` (§8.2.3): fixed p=1/2 boolean special case. The cdf is
    /// constructed fresh each call, so its post-decode adaptation is
    /// discarded, matching the spec note that implementations may skip it.
    ///
    /// `#[track_caller]` so a trace entry pushed by the inner
    /// [`Self::read_symbol`] call attributes to *this* function's caller
    /// (e.g. a specific `read_skip`/`read_delta_qindex` call site in
    /// `reconstruct/`), not to this line in `entropy.rs` — `#[track_caller]`
    /// is transparent through a chain of `#[track_caller]` functions.
    #[track_caller]
    pub fn read_bool(&mut self) -> bool {
        let mut cdf = [1u16 << 14, 1u16 << 15, 0u16];
        self.read_symbol(&mut cdf) == 1
    }

    /// `read_literal(n)` (§8.2.5): build an `n`-bit value from `read_bool`.
    /// Also `#[track_caller]` for the same reason as `read_bool` — a
    /// `read_literal(4)` call in e.g. `read_delta_qindex` shows up in the
    /// trace as 4 bit-reads attributed to that call site, not to this loop.
    #[track_caller]
    pub fn read_literal(&mut self, n: u32) -> u32 {
        let mut x = 0u32;
        for _ in 0..n {
            x = 2 * x + self.read_bool() as u32;
        }
        x
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // All golden vectors below were cross-checked against an independent
    // Python transcription of AV1 spec §8.2 (init_symbol/read_symbol/
    // read_bool/read_literal), run on synthetic byte buffers — the AV1
    // spec itself does not ship a worked numeric example for the symbol
    // decoder, so this differential check (two independent implementations
    // of the same spec text) stands in for it.

    #[test]
    fn floor_log2_matches_bit_length_minus_one() {
        assert_eq!(floor_log2(1), 0);
        assert_eq!(floor_log2(2), 1);
        assert_eq!(floor_log2(3), 1);
        assert_eq!(floor_log2(4), 2);
        assert_eq!(floor_log2(32768), 15);
        assert_eq!(floor_log2(65535), 15);
    }

    #[test]
    fn bool_all_zero_bytes_decodes_all_zero_symbols() {
        let mut dec = SymbolDecoder::new(&[0x00, 0x00, 0x00]);
        let syms: Vec<bool> = (0..8).map(|_| dec.read_bool()).collect();
        assert_eq!(syms, vec![false; 8]);
    }

    #[test]
    fn bool_all_one_bytes_decodes_all_one_symbols() {
        let mut dec = SymbolDecoder::new(&[0xFF, 0xFF, 0xFF]);
        let syms: Vec<bool> = (0..8).map(|_| dec.read_bool()).collect();
        assert_eq!(syms, vec![true; 8]);
    }

    #[test]
    fn read_literal_matches_reference_trace() {
        let mut dec = SymbolDecoder::new(&[0xA5, 0x3C, 0x81, 0xF0]);
        let lits: Vec<u32> = (0..4).map(|_| dec.read_literal(4)).collect();
        assert_eq!(lits, vec![10, 5, 4, 2]);
    }

    #[test]
    fn multi_symbol_uniform4_matches_reference_trace() {
        let mut cdf = [8192u16, 16384, 24576, 32768, 0];
        let mut dec = SymbolDecoder::new(&[0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC]);

        let expected_symbols = [0usize, 1, 0, 0, 1, 3];
        let expected_cdf_snapshots: [[u16; 5]; 6] = [
            [8960, 16896, 24832, 32768, 1],
            [8680, 17392, 25080, 32768, 2],
            [9432, 17872, 25320, 32768, 3],
            [10161, 18337, 25552, 32768, 4],
            [9844, 18787, 25777, 32768, 5],
            [9537, 18200, 24972, 32768, 6],
        ];

        for i in 0..6 {
            let symbol = dec.read_symbol(&mut cdf);
            assert_eq!(symbol, expected_symbols[i], "symbol mismatch at step {i}");
            assert_eq!(cdf, expected_cdf_snapshots[i], "cdf mismatch at step {i}");
        }
    }

    #[test]
    fn txb_skip_context0_matches_reference_trace() {
        // Default_Txb_Skip_Cdf[3][3][0] from the AV1 spec's default CDF
        // table (10.additional.tables.md).
        let mut cdf = [31671u16, 32768, 0];
        let mut dec = SymbolDecoder::new(&[0x7E, 0x91, 0x2D, 0x44, 0xC3, 0x0F, 0xAA, 0x55]);

        let expected_symbols = [0usize; 10];
        let expected_cdf_snapshots: [[u16; 3]; 10] = [
            [31739, 32768, 1],
            [31803, 32768, 2],
            [31863, 32768, 3],
            [31919, 32768, 4],
            [31972, 32768, 5],
            [32021, 32768, 6],
            [32067, 32768, 7],
            [32110, 32768, 8],
            [32151, 32768, 9],
            [32189, 32768, 10],
        ];

        for i in 0..10 {
            let symbol = dec.read_symbol(&mut cdf);
            assert_eq!(symbol, expected_symbols[i], "symbol mismatch at step {i}");
            assert_eq!(cdf, expected_cdf_snapshots[i], "cdf mismatch at step {i}");
        }
    }

    #[test]
    fn read_literal32_forces_multiple_renormalization_refills() {
        let mut dec = SymbolDecoder::new(&[0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF]);
        assert_eq!(dec.read_literal(32), 16_325_324);
    }

    #[test]
    fn short_buffer_reads_zero_padding_past_end() {
        let mut dec = SymbolDecoder::new(&[0x80]);
        let syms: Vec<bool> = (0..12).map(|_| dec.read_bool()).collect();
        let expected = [
            true, false, false, false, false, false, false, false, false, false, false, false,
        ];
        assert_eq!(syms, expected);
    }
}
