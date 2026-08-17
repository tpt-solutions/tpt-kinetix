# TPT Kinetix — Native AAC-LC Decoder Todo

> Active work. See [todo.md](todo.md) for the project index.


## Phase 18 — Native AAC-LC Decoder (remove symphonia MPL-2.0 dependency) (2026-08-13)

> `cargo deny check` fails CI ("Deny (licenses / advisories)" job, run
> 30184382291) because `tpt-kinetix-aac` depends on `symphonia-core` /
> `symphonia-codec-aac`, both MPL-2.0, which isn't in `deny.toml`'s
> `licenses.allow` list (plus an unrelated `bitflags` v1/v2 duplicate-version
> ban from the same dependency). Rather than allow-list MPL-2.0,
> `docs/codec-evaluations/aac.md`'s original build-vs-wrap tradeoff is being
> revisited: replace symphonia with a native from-scratch AAC-LC decoder,
> consistent with the H.264/AV1 native-reimplementation approach. This is a
> multi-week effort; per explicit decision, the "Deny" CI job is left **red**
> for the duration (no temporary `deny.toml` MPL-2.0 stopgap) until Phase 7
> below removes symphonia entirely. Constraint: new code must be written from
> the public ISO/IEC 13818-7 / 14496-3 spec, not transcribed from the local
> MPL-2.0 symphonia-codec-aac source (which may only be read for algorithm
> structure). The `tpt-kinetix-kg` FFmpeg-C-ingest pipeline is intentionally
> not used (FFmpeg's AAC decoder is LGPL/GPL — worse license posture than the
> problem being solved). Full phased plan:
> `C:\Users\phill\.claude\plans\https-github-com-tpt-solutions-tpt-kinet-squishy-lemur.md`.

- [ ] Phase 1 — `tpt-kinetix-aac/src/bitreader.rs` (MSB-first, AAC escape-value
      helpers, modeled on `tpt-kinetix-h264/src/bitreader.rs`'s shape) +
      parse-only syntax structs (`IcsInfo`, `SectionData`, SCE/CPE/LFE/FIL/END
      element dispatch). Exit: unit tests on hand-built fixtures +
      `*_never_panics` proptest.
- [x] Phase 2 — `src/codebooks.rs`: the 11 (+1 escape) Huffman spectral
      codebooks, independently transcribed from spec tables, tree-walk decode
      + escape handling. Exit: unit tests per codebook against hand-encoded
      sequences + bounded-consumption/no-panic proptest. **Done and wired**:
      `decode_codeword`/`decode_spectral_quad`/`decode_scalefactor` implement
      all 11 spectral codebooks + the book-11 escape sequence + the
      scalefactor codebook (unit-tested), and as of the 2026-08-15 native-decode
      rewire (see session note above) it's no longer dead code — it's the
      real spectral-decode path `decoder.rs` runs on every channel stream.
      Still missing: the bounded-consumption/no-panic proptest.
- [x] Phase 3 — `src/scalefactors.rs`, `src/dequant.rs`, `src/pns.rs`,
      `src/tns.rs`, `src/pulse.rs`: DPCM scalefactor decode, dequantization
      formula, perceptual noise substitution, temporal noise shaping, pulse
      data. Exit: unit tests against hand-computed values; TNS filter
      validated against an independently computed reference. **Done**: all
      five modules exist, are unit-tested, and are wired into
      `decoder.rs::decode_channel_stream`. `scalefactors.rs`/`dequant.rs`
      iterate the real `SectionData` (not a naive `0..max_sfb` loop). DSE/PCE
      elements are skipped rather than rejected so real ffmpeg streams parse.
      Still missing: an independently-computed-reference cross-check for the
      TNS filter (current `tns.rs` tests compare against an in-crate
      reference implementation, not an external one).
- [x] Phase 4 — `src/stereo.rs`: M/S and intensity-stereo reconstruction for
      channel_pair_element. Exit: unit tests reconstructing L/R from known
      coded spectra. **Done**: `apply_stereo` implements both M/S (per-band
      mask) and intensity stereo, 4 unit tests pass, wired into
      `decoder.rs`'s Pass 3.
- [x] Phase 5 — `src/mdct.rs` (1024/128-point IMDCT, written from scratch —
      new to the whole workspace, no existing MDCT code anywhere) +
      `src/window.rs` (KBD/sine windows, window-sequence transitions,
      overlap-add state). Exit: IMDCT(MDCT(x))≈x round-trip test,
      window-value tests at known points, proptest over window-sequence
      combinations. **Done**: both modules exist, unit-tested (basis
      orthonormality/roundtrip, window symmetry/power-sum), wired into
      `decoder.rs`'s Pass 4 (`imdct_long`/`imdct_short` + `long_synthesis`/
      `short_synthesis`). Still missing: the proptest over window-sequence
      combinations called for by the exit criteria.
- [~] Phase 6 — Wire phases 1-5 into `decode_raw_data_block`, swap
      `decoder.rs`'s internals onto the native path (public
      `AacDecoder::new/with_config/set_config/set_strict/capabilities/decode/config`
      API unchanged), new `tests/conformance_aac.rs` via
      `tpt-kinetix-test-utils`'s `decode_aac_with_ffmpeg` + `audio_diff`
      reference harness, `tests/proptest_decode_never_panics.rs`,
      `just fuzz tpt-kinetix-aac fuzz_aac_decode 60`. Exit: conformance test
      passes at a documented tolerance, bench recorded, fuzz run clean.
      **Wiring done, conformance not yet passing (uncommitted, 2026-08-15):**
      `decoder.rs::decode()` no longer touches `symphonia` at all — see the
      "AAC native decode rewired" session note above for the full 4-pass
      pipeline (spectral decode → CCE coupling → stereo → IMDCT/synthesis)
      and the new CCE parsing in `syntax.rs`. But `tests/conformance_aac.rs`'s
      `native_aac_matches_ffmpeg_reference` still doesn't validate anything —
      run with `--nocapture` and the native decoder fails every one of a real
      `ffmpeg`-encoded stream's first frames with
      `Err(Parse(UnexpectedEof))`/`Err(Parse(BadSectionCodebook))`, so the
      empty-native-output early-return still skips the real assertion. This is
      NOT the previously-suspected CCE-support gap (CCE is now parsed) — the
      failure happens earlier in raw_data_block parsing against a real stream.
      Root-causing that first-frame desync (hand-built unit fixtures all pass;
      only real ffmpeg-encoded bitstreams fail) is the actual remaining work.
      `tests/proptest_decode_never_panics.rs` and the `fuzz_aac_decode` target
      are not started. **Updated 2026-08-15 (further session, uncommitted,
      see session note further above):** `RawDataBlock::parse` gained real
      fixes (missing `fill_element()` `byte_alignment()`, tolerating a
      missing/implicit `END` at frame boundary, real CCE parsing instead of
      `Unsupported`) but `frame 3: Err(Parse(BadSectionCodebook))` still
      occurs, and the conformance test no longer skips silently — it now
      reaches the PCM-tolerance assertion and panics with an
      `f32`-overflow-magnitude diff (~3.4×10^38), a second, distinct bug
      (garbage/NaN sample synthesis) on top of the still-unresolved parse
      desync. Debug `eprintln!` tracing is still active in `syntax.rs` from
      this investigation. Phase 6 remains open.
- [~] Phase 7 — Remove `symphonia-codec-aac`/`symphonia-core` from
      `tpt-kinetix-aac/Cargo.toml` and root `Cargo.toml`; update
      `docs/codec-evaluations/aac.md`, both READMEs' status tables, and
      module doc comments (drop "delegated to symphonia-codec-aac" language).
      Exit: `grep -rn symphonia` empty; `just check` and `just deny` both
      green. **Dependency removal done (uncommitted, 2026-08-15):** the
      `symphonia-codec-aac`/`symphonia-core` lines are gone from root
      `Cargo.toml`, and `cargo deny check licenses` no longer flags MPL-2.0 —
      confirmed the license check now fails on an unrelated pre-existing issue
      (`webpki-roots` CDLA-Permissive-2.0 via `tpt-kinetix-kg → ureq`), not
      symphonia. Still open: `docs/codec-evaluations/aac.md`, both READMEs,
      and module doc comments still say "delegated to symphonia-codec-aac";
      `tpt-kinetix-stream/fuzz/Cargo.lock` still has a stale symphonia entry
      (will drop on next `cargo update`); `grep -rn symphonia` is not yet
      empty. Also blocked on Phase 6 actually passing before this can be
      called done in spirit (the dependency is gone, but the decoder it was
      replaced with doesn't yet decode real streams).

