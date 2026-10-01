# oracle/

Reference-only excerpts of FFmpeg's H.264 C sources (LGPL), kept so the `dbg_*` tests in
`../tests/` can run Rust code in lockstep against the original engine (for example
`tests/dbg_engine_diff.rs` parses its tables out of `cabac_ref.c`).

- These files are **not** compiled into the crate and **not** part of any published package.
  `out-kinetix-h264` is `publish = false` (patent-encumbered; see `PATENTS.md`).
- `ff_*.c` / `ff_*.h` are git-ignored: they are fetched or dropped in locally during bring-up.
- Do not copy anything from here into a published crate.
