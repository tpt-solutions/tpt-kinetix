# Codec Evaluation: AAC Audio Decode/Encode

**Status**: ✅ Implemented (AAC-LC) — fully native decoder in `tpt-kinetix-aac` (no third-party dependency)  
**Last updated**: Phase 18 (Phase 7 complete - symphonia removed)

---

## Overview

Advanced Audio Coding (AAC) is the dominant audio codec for streaming (HLS, DASH),
broadcast (DVB, ATSC), and device playback (iOS, Android). Supporting AAC decode is
essentially a requirement for any production-grade media pipeline that handles MP4/fMP4
containers.

---

## Technical Complexity

**Complexity rating: Medium**

AAC combines three main algorithmic components:

1. **MDCT (Modified Discrete Cosine Transform)** — the spectral analysis and synthesis
   core. The forward/inverse MDCT operates on windowed frames of 1024 or 128 samples
   (long and short block types). The window switching logic (long→start→short→stop) adds
   implementation complexity but is well-specified in ISO 13818-7 and ISO 14496-3.

2. **Huffman entropy coding** — spectral coefficients are coded with 11 code books
   (including one for unsigned pairs, one for signed quads, and several for unsigned
   quads with different maximum magnitudes). The Huffman tables are fixed (not
   adaptive), which simplifies implementation compared to CABAC.

3. **TNS (Temporal Noise Shaping)** — a per-channel LPC filter applied in the
   frequency domain before quantisation/dequantisation. TNS is present in most
   real-world AAC streams and must be decoded correctly for clean audio output.

Additional features present in AAC-LC streams encountered in the wild:
- **Stereo coding**: M/S stereo and intensity stereo require special joint-channel
  reconstruction.
- **PNS (Perceptual Noise Substitution)**: noise-filled spectral bands; must be
  detected and filled with shaped noise.
- **SBR (Spectral Band Replication)**: used in AAC-HE v1; extends bandwidth by
  replicating and patching the high-frequency region.
- **PS (Parametric Stereo)**: used in AAC-HE v2; upmixes mono to stereo using
  side-channel parameters.

For an initial release, targeting AAC-LC (Low Complexity profile) covers the majority
of streaming use cases. SBR/PS support can follow.

---

## KG Tool Applicability

**KG applicability rating: High**

FFmpeg's `aac.c` (and `aacdec.c` / `aacdectab.c`) has a clear state-machine structure
that the `tpt-kinetix-kg` ingestion pass handles well:

- The top-level `aac_decode_frame` function is a well-defined decode entry point.
- Spectral coefficient decoding loops (`decode_spectrum_and_dequant`) are LoopBody
  nodes over channel and window-group indices; these are candidates for parallelism
  across independent channels.
- The Huffman dispatch (`decode_band_types`) produces clean SwitchCase nodes for the
  11 code book selection logic.

Expected graph statistics for `aacdec.c` (~3 500 lines):

| Metric | Expected range |
|--------|----------------|
| Function nodes | 60–90 |
| SwitchCase nodes | 40–70 |
| LoopBody nodes | 30–50 |
| Total nodes | 1 200–1 800 |

The dependency analysis should correctly identify that the left/right channel
reconstruction passes are independent, enabling `rayon::join` parallelism for stereo
streams.

---

## Rust Ecosystem

Historically, the `symphonia` crate (`symphonia-codec-aac`) provided a pure-Rust AAC-LC decoder
with coverage of the main streaming profiles. However, `tpt-kinetix-aac` now implements a **fully native**
AAC-LC decoder (Huffman codebooks, IMDCT, TNS, PNS, M/S stereo, intensity stereo, pulse, windowing)
transcribed from the ISO/IEC 14496-3 / 13818-7 specifications, with no third-party codec dependencies.

Relevant crates:
- `tpt-kinetix-aac` — fully native AAC-LC decode (this workspace)
- No mature pure-Rust AAC encoder exists; `fdk-aac` (via FFI) is the most common
  production encoder but is GPL-encumbered.

---

## Implementation Summary

> **Implemented (Phase 18).** `tpt-kinetix-aac`'s `AacDecoder::decode()` returns real interleaved
> `f32` PCM using a fully native AAC-LC reconstruction pipeline (Huffman spectral decode,
> inverse quantization, IMDCT, TNS, PNS, pulse, M/S and intensity stereo, windowing/overlap-add).
> See `tpt-kinetix-aac/src/decoder.rs` and the `ffmpeg`-gated conformance test in
> `tpt-kinetix-aac/tests/conformance_aac.rs`.

Rationale for native implementation:
1. **License compliance**: avoids the MPL-2.0 dependency of `symphonia-codec-aac`/`symphonia-core`,
   keeping the workspace Apache-2.0/MIT clean for `cargo deny`.
2. **Zero third-party risk**: no external codec crate to audit, update, or rely on.
3. **KG pipeline validation**: demonstrates the knowledge-graph-assisted codec development workflow
   for audio (phases 1–7), complementing the video codec work.

If HE-AAC v1/v2 (SBR/PS) support is required later, the existing native pipeline can be extended
with the SBR/QMF toolchain (ISO/IEC 14496-3 §4.6.18).

---

## Estimated Effort (Historical)

| Approach | Effort | Risk |
|----------|--------|------|
| Wrap `symphonia-codec-aac` (not chosen) | 1–2 days | Low |
| KG scaffold + hand-complete AAC-LC (actual) | 3–4 weeks | Medium |
| KG scaffold + hand-complete AAC-HE v1/v2 | 6–8 weeks | High |

---

## Priority

**Medium — worth completing before v1.0**

Most HLS/DASH streaming pipelines carry AAC audio tracks. A release without AAC decode
forces users to fall back to `ffmpeg` for any stream that has audio, which undermines
the project's goal of being a self-contained pipeline. Wrapping `symphonia` is a
low-cost way to check this box early.

Suggested milestone: integrate the `symphonia` wrapper during Phase 6 (Streaming Engine)
so that RTMP ingest with AAC audio works end-to-end before the v0.1 release.
