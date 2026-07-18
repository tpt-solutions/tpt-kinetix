# Codec Evaluation: AAC Audio Decode/Encode

**Status**: Under evaluation — Post-MVP candidate  
**Last updated**: Phase 9 evaluation

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

The `symphonia` crate (`symphonia-codec-aac`) provides a pure-Rust AAC-LC decoder
with coverage of the main streaming profiles. It is actively maintained, has reasonable
test coverage, and is licensed MIT/Apache-2.0.

Relevant crates:
- `symphonia-codec-aac` — AAC-LC decode (part of the `symphonia` workspace)
- `symphonia-bundle-mp3` — MP3 decode (for comparison)
- No mature pure-Rust AAC encoder exists; `fdk-aac` (via FFI) is the most common
  production encoder but is GPL-encumbered.

---

## Recommendation

**For the initial release, wrap `symphonia::codec::aac` rather than generating a new
implementation from the KG scaffold.**

Rationale:

1. **Time-to-correct**: a correct AAC implementation takes 3–4 weeks; wrapping
   `symphonia` takes 1–2 days and immediately delivers correct output.
2. **KG tool value**: the KG pipeline adds most value for *video* codecs where spatial
   parallelism (slice/tile/macroblock-row independence) maps directly onto `rayon`
   work-stealing. Audio codecs are largely serial per channel and the parallelism
   surface is smaller.
3. **License compatibility**: `symphonia-codec-aac` is Apache-2.0/MIT, matching the
   Kinetix workspace license.
4. **Maintenance**: offloads codec correctness maintenance to the `symphonia` team for
   an initial release; the KG-generated implementation can be a v2 replacement once
   video codecs are stable.

If a custom AAC implementation is required later (e.g. to support AAC-HE v2 / PS,
which `symphonia` does not yet cover completely), revisit the KG codegen approach at
that point.

---

## Estimated Effort

| Approach | Effort | Risk |
|----------|--------|------|
| Wrap `symphonia-codec-aac` | 1–2 days | Low |
| KG scaffold + hand-complete AAC-LC | 3–4 weeks | Medium |
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
