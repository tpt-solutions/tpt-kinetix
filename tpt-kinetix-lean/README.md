# tpt-kinetix-lean

An original, embedded-first video codec for the
[TPT Kinetix](https://github.com/tpt-solutions/tpt-kinetix) media engine.

> **Status: working, original format.** Block reconstruction (intra and
> unidirectional-P prediction, integer transforms, in-loop deblock) runs
> end-to-end. Lean has no external reference decoder, so
> `LeanDecoder::capabilities()` reports `pixel_exact = false` and
> `deterministic = true`; a golden-vector test pins the bitstream and output.
> See [Limitations](#limitations).

Unlike `tpt-kinetix-av1`, which is a from-scratch
*conformant* implementation of an existing standard, Lean is an original
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
- rANS entropy coding via the shared `tpt-kinetix-bitstream` crate
- Reconstruction: 14-mode intra + unidirectional-P inter prediction
  (`src/prediction.rs`), integer Walsh–Hadamard/DCT transform bank and dequant
  (`src/transform.rs`), single-stage deblock (`src/deblock.rs`),
  wired together in `src/reconstruct.rs`
- `LeanDecoder::capabilities()` reports `pixel_exact = false`, `deterministic = true`

## Limitations

- No independent reference decoder exists, so output cannot be called
  pixel-exact against anything; strict mode returns `KinetixError::NotPixelExact`.
- v1 envelope: up to 1920×1080, 4 reference frames; `no_std`/MCU support is future work.

## License

MIT OR Apache-2.0
