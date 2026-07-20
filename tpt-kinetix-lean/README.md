# tpt-kinetix-lean

An original, embedded-first video codec for the
[TPT Kinetix](https://github.com/tpt-solutions/tpt-kinetix) media engine.

> **Status: scaffold.** Header parsing and the core rANS entropy-coding
> primitives are implemented and tested; block reconstruction (prediction,
> transform, in-loop filter) is not. `LeanDecoder::capabilities()` reports
> `pixel_exact = false` accordingly — see [LIMITATIONS](#limitations).

Unlike `tpt-kinetix-h264` and `tpt-kinetix-av1`, which are from-scratch
*conformant* implementations of existing standards, Lean is an original
bitstream format designed by this project. It trades roughly 10-15% worse
compression than AV1 for a decoder that stays small, auditable, and
genuinely parallel at the entropy-decode stage — see the crate-level docs in
[`src/lib.rs`](src/lib.rs) for the full design rationale.

## Design at a glance

- **Entropy coding**: rANS (`src/rans.rs`), not CABAC — independently
  decodable interleaved sub-streams instead of a bit-serial dependency chain.
- **Bounded memory**: the sequence header (`src/headers.rs`) declares max
  frame dimensions and reference count up front, so a decoder sizes its
  arena once and never grows it.
- **Fixed, shallow block partitioning** — no recursive quad/multi-type tree.
- **Integer-only transforms** — no floating point.
- **v1 target**: embedded Linux (Raspberry Pi–class), std-compatible with
  an alloc-free decode hot path; full `no_std`/MCU support is future work.

## Current capabilities

- Sequence/frame header parsing and validation (`src/headers.rs`)
- rANS encode/decode primitives + multi-stream framing (`src/rans.rs`)
- `LeanDecoder::capabilities()` reports `pixel_exact = false`

## Limitations

Block reconstruction (intra/inter prediction, transform, in-loop filter) is
not implemented. `LeanDecoder::decode()` returns `Ok(None)` once a frame
header parses successfully (or `KinetixError::NotPixelExact` in strict
mode) — it never returns placeholder pixel data.

## License

MIT OR Apache-2.0
