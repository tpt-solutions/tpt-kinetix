# Refactor: split `tpt-kinetix-h264/src/slice_data.rs` (4529 lines)

> Scope locked to the single file `tpt-kinetix-h264/src/slice_data.rs`. This is the
> first of several planned splits (see "Later" section). Goal: break the monolith into
> focused submodules with **zero behavioral change** and **unchanged public API path**
> (`crate::slice_data::<item>`), verified by the existing test + conformance suite.

## Current state

- Module declared in `tpt-kinetix-h264/src/lib.rs:?` as `pub mod slice_data;` (single file).
- Public surface consumed by the crate (verified via grep):
  - `decoder.rs`: `ParsedSlice`, `SliceDataError`, `parse_i_slice`, `parse_i_slice_cabac`,
    `parse_p_slice`, `parse_p_slice_cabac`, `parse_b_slice`, `parse_b_slice_cabac`.
  - `reconstruct.rs`: `raster_of_8x8_sub`, `ParsedSlice`.
  - (doc comments in `entropy.rs`/`slice.rs`/`reconstruct.rs` reference `slice_data::…`.)
- Top of file (`slice_data.rs:1-86`) holds module docs + `#[rustfmt::skip]` constant
  tables (`I16X16_TABLE`, `GOLOMB_TO_INTRA4X4_CBP`, `GOLOMB_TO_INTER_CBP`, `P_SUB_MB_PARTS`,
  `B_2PART_TABLE`) — must travel with their usage.

## Target layout

Convert `src/slice_data.rs` → directory `src/slice_data/`:

```
src/slice_data/
├── mod.rs          # module docs, constants/tables (1-86), shared helpers, re-exports, #[cfg(test)] mod tests
├── ctx.rs          # error + context structs (87-612, 1191-1362)
├── cavlc.rs        # CAVLC slice parsers (613-1190, 3340-3955) + parse_cavlc_block/parse_cavlc_chroma_dc
├── cabac_i.rs      # parse_i_slice_cabac (1363-1467)
├── cabac_p.rs      # CabacSliceContexts helpers + parse_p_slice_cabac + parse_p_macroblock_cabac (1468-1770-ish, 1770-1880)
└── cabac_b.rs      # parse_b_slice_cabac + parse_b_macroblock_cabac + pb helpers (1881-3340)
```

> Note: `cabac_p.rs`/`cabac_b.rs` also contain the private `parse_p/b_macroblock_cabac`
> and the shared `PbCabacSliceContexts`/`cabac_decode_mvd_component`/`parse_intra_mb_cabac_pb`
> /`decode_inter_cbp_cabac`/`decode_inter_residual_cabac` helpers (lines 1191-3340). These
> are placed in `cabac_p.rs` (P/B share the PB path) — see execution step 4.

## Precise section boundaries (from item map)

| Item | Lines |
|------|-------|
| module docs + `#[rustfmt::skip]` const tables | 1–86 |
| `SliceDataError` | 87–129 |
| `MbNz`, `ParsedSlice`, `MbPredCtx`, `MbCabacCtx` (structs) | 135–208 |
| `MbInterCabacCtx` + intra helpers (`partition_blocks`, `partition_dims`, `amvd_sum`, `ref_idx_gt0_neighbors`) | 209–449 |
| `NeighbourCtx` + cbp-neighbor helpers (`cabac_cbp_neighbors`, `dc_cbf_neighbor`, `luma_cbf_neighbors`, `chroma_cbf_neighbors`) | 450–612 |
| `parse_i_slice` (CAVLC) + `NeighbourSide`, `mpm_pred_mode`, `mpm_pred_mode_8x8`, `luma_nc`, `chroma_nc`, `combine_nc`, `parse_intra_macroblock` | 613–1190 |
| `CabacSliceContexts`, `PbCabacSliceContexts`, `cabac_decode_mvd_component` | 1191–1362 |
| `parse_i_slice_cabac` | 1363–1467 |
| `parse_intra_macroblock_cabac` | 1468–1769 |
| `parse_p_slice_cabac`, `parse_p_macroblock_cabac` | 1770–1880 |
| `parse_b_slice_cabac`, `parse_b_macroblock_cabac`, `parse_intra_mb_cabac_pb`, `decode_inter_cbp_cabac`, `decode_inter_residual_cabac` | 1881–3340 |
| `parse_p_slice` (CAVLC), `read_ref_idx`, `parse_p_macroblock` | 3340–3597 |
| `parse_b_slice` (CAVLC), `parse_b_macroblock` | 3598–3955 |
| `parse_intra_residuals` | 3956–4189 |
| `raster_of_8x8_sub`, `parse_cavlc_block`, `parse_cavlc_chroma_dc` | 4190–4401 |
| `#[cfg(test)] mod tests` | 4402–end |

## Execution steps (mechanical move, no logic change)

1. **Create `src/slice_data/mod.rs`.** Move lines 1–86 (docs + const tables). Add the
   submodule declarations:
   ```rust
   mod ctx;
   mod cavlc;
   mod cabac_i;
   mod cabac_p;
   mod cabac_b;
   ```
2. **`ctx.rs`** — move lines 87–612 and 1191–1362 (error + all `*Ctx`/`*Contexts` structs
   and their intra/neighbor helper fns). Add `use crate::…;` / `use super::*` as needed.
3. **`cavlc.rs`** — move lines 613–1190 and 3340–4401 (CAVLC `parse_i/p/b_slice`,
   `parse_intra_macroblock`, `parse_intra_residuals`, `raster_of_8x8_sub`,
   `parse_cavlc_block`, `parse_cavlc_chroma_dc`, `read_ref_idx`, `parse_p/b_macroblock`).
4. **`cabac_i.rs`** — move lines 1363–1467 (`parse_i_slice_cabac`).
5. **`cabac_p.rs`** — move lines 1468–1770 (`parse_intra_macroblock_cabac`), 1770–1880
   (`parse_p_slice_cabac` + `parse_p_macroblock_cabac`), and the shared PB scaffolding
   1191–1362 already in `ctx.rs` is referenced — instead keep `CabacSliceContexts` in
   `ctx.rs` and have `cabac_p.rs`/`cabac_b.rs` `use super::ctx::*`. Add the remaining
   P/B helpers (1881–3340: `parse_b_slice_cabac`, `parse_b_macroblock_cabac`,
   `parse_intra_mb_cabac_pb`, `decode_inter_cbp_cabac`, `decode_inter_residual_cabac`)
   — these live in **`cabac_b.rs`** (they are the B/intra-PB decode path).
6. **`cabac_b.rs`** — move 1881–3340 as above.
7. **Re-exports in `mod.rs`** so `crate::slice_data::X` keeps resolving:
   ```rust
   pub use ctx::{SliceDataError, MbNz, ParsedSlice, MbPredCtx, MbCabacCtx, MbInterCabacCtx, NeighbourCtx, CabacSliceContexts};
   pub use cavlc::{parse_i_slice, parse_p_slice, parse_b_slice, raster_of_8x8_sub, parse_cavlc_block};
   pub use cabac_i::parse_i_slice_cabac;
   pub use cabac_p::{parse_p_slice_cabac};
   pub use cabac_b::{parse_b_slice_cabac};
   ```
   Keep `#[cfg(test)] mod tests` (4402–end) in `mod.rs`; it does `use super::*;` and will
   see all re-exported + `pub(crate)` items. Ensure any private helper the tests touch
   (`combine_nc`, `parse_cavlc_block`, etc.) is reachable (`pub(crate)` or re-exported).
8. **Imports:** every moved fn uses `T: crate::trace::DecodeTracer`, `BitReader`,
   `R<…>` (from `crate::…`), `Macroblock`, etc. Preserve the exact `use` lines at the top
   of each new file; do not let `cargo fmt`/`clippy -D warnings` introduce unused-import
   errors. Keep all `#[allow(clippy::too_many_arguments)]` attributes intact.
9. **`lib.rs`:** change `pub mod slice_data;` — no change needed (directory mode is
   auto-detected by Rust). Confirm no other file imports `slice_data` items via a path
   that assumed a single file (it doesn't; only `crate::slice_data::Name`).

## Risks / guardrails

- **`#[rustfmt::skip]` tables (lines 23/34/46)** must move verbatim with their constants.
- **No signature changes** — only `mod` boundaries move. Do not rename, inline, or
  reorder logic.
- **`pub(crate)` vs `pub`** — items currently `pub` stay `pub`; private helpers that cross
  file boundaries must become `pub(crate)` (or be re-exported) so the split compiles under
  `-D warnings` (no dead-code warnings either).
- **Tests** reference private helpers (`combine_nc`, `parse_cavlc_block`,
  `raster_of_8x8_sub`); keeping `mod tests` in `mod.rs` with `use super::*` keeps them
  working without widening visibility.
- **Clippy `-D warnings`** is enforced in CI (`just clippy`); a clean `just check` is the
  bar.

## Validation

1. `just fmt` (honor `rustfmt.toml`: max_width=100, group_imports=StdExternalCrate).
2. `just clippy` → must be `-D warnings` clean for `-p tpt-kinetix-h264`.
3. `cargo nextest run -p tpt-kinetix-h264` (or `cargo test -p tpt-kinetix-h264`) — all unit
   tests incl. `slice_data` tests + any proptest regressions pass.
4. `cargo build -p tpt-kinetix-h264` (and `cargo build --workspace` to catch cross-crate
   path breakage, e.g. `decoder.rs`/`reconstruct.rs`).
5. `just conformance` (ffmpeg-gated) — H.264 decoder output must be **identical** before/after
   (non-blocking pixel-exact per AGENTS.md, but splitting must not regress it).
6. `RUSTDOCFLAGS="-D warnings" cargo doc -p tpt-kinetix-h264 --no-deps` — re-exports must
   not produce broken doc links.

## Later (out of scope for this task)

Same pattern for: `av1/src/reconstruct.rs` (dequant/transform/intra/tile_group/partition),
`av1/src/entropy_cdf.rs` (per-category CDF tables), and `h264` `ref_pic.rs` / `decoder.rs`
/ `reconstruct.rs` / `entropy.rs`, `av1/frame.rs`.
