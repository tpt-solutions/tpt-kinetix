# Publishing `tpt-kinetix-kg` as a public tool

> Evaluation note (2026-07-18). Source: Phase 10 Innovation review item
> "Evaluate publishing/positioning `kinetix-kg` as a public 'bring your own
> codec' tool rather than internal-only tooling."

## Summary

`tpt-kinetix-kg` is the knowledge-graph ingestion / codegen tooling that powers
the H.264 and AV1 decoders in this workspace. Today it is used internally to
turn FFmpeg C source into a Rust decoding scaffold (AST → bitstream parsing
tree → macroblock/state machine → `rayon`-parallel codec scaffolding).

This document records the evaluation of whether to ship it publicly as a
standalone "bring your own codec" tool.

## What it can already do (public-ready surface)

- Parse FFmpeg H.264 C source into an AST (`tree-sitter-c` based ingestion).
- Build a knowledge graph (parsing states, syntax elements, macroblock states,
  transitions, data dependencies) and serialize it for inspection.
- Identify independent decode units (e.g. slice-level independence) and emit
  `rayon` parallel-iterator injection points.
- Emit decoder scaffolding (structs, state enums, parse-function stubs) from
  the graph, run end-to-end via a CLI entry point.

## Why it is worth publishing publicly

- **Adoption lever.** A reusable "ingest C codec → scaffold Rust decoder" tool
  lowers the barrier for the long tail of codecs FFmpeg supports but Rust does
  not. This is the single biggest multiplier for the project's codec-expansion
  story (see `docs/codec-backlog.md`).
- **Community codec contributions.** External contributors could bring their
  own codec and open a PR with a generated/completed decoder, rather than the
  core team hand-porting everything.
- **Distinct positioning.** Most Rust multimedia tooling stops at bindings
  (`rav1e`, `dav1d-sys`). A codegen-from-reference-C tool is differentiated and
  demonstrably useful as a teaching/acceleration aid.

## Risks / what must be true before a `v0.1.0` publish

- **Output is a scaffold, not a finished decoder.** The generated code still
  requires hand-completion (entropy decoding, prediction, deblocking). The tool
  must be documented and labeled as an *accelerator*, not a drop-in decoder,
  to avoid users shipping silently-wrong output. The `DecoderCapabilities`
  signal already exists in `tpt-kinetix-core` and should be surfaced by any
  generated decoder.
- **Re-producibility / provenance.** Ingesting third-party C source raises
  licensing questions (FFmpeg is LGPL/GPL). Generated scaffolding that copies
  structure (not verbatim code) is fine, but the tool should document that the
  *input* C is the user's responsibility and that copied snippets inherit the
  input's license.
- **MSRV / dependency hygiene.** The CLI pins `tree-sitter` and `rayon`; a
  public crate needs the same `cargo-deny` / MSRV guarantees the rest of the
  workspace enforces.

## Recommendation

Publish `tpt-kinetix-kg` as a **separate, clearly-labeled `0.1.0`** crate (its
own `Cargo.toml`, README with a "scaffold, not a decoder" warning, and a
runnable `ingest → graph → codegen` example). Keep it out of the main
`cargo publish` workspace release sequence (it has no runtime dependency on the
other `tpt-kinetix-*` crates), so it can iterate on its own cadence.

Follow-up actions (good first issues):
- Add a `tpt-kinetix-kg/examples/ingest_ffmpeg_h264.rs` end-to-end example.
- Add a README "Limitations" section mirroring the decoder capability caveats.
- Document the license/provenance expectations for ingested C source.
