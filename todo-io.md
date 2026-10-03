# todo-io.md — memory-safe media I/O: demux, package, stream

**Direction (decided 2026-10-03).** Kinetix is not going to out-run ffmpeg/dav1d at
codec decode: AV1 decode is at 0.03-0.17x of libdav1d single-threaded and the realistic
ceiling for pure Rust without hand assembly is roughly a third to a half of that. Rust
does not buy speed over C; it buys memory safety and ergonomic concurrency. So the
product is the **I/O layer**, where those are the point and where speed is I/O-bound
rather than assembly-bound: containers, packaging, ingest, streaming. Codec work
(AV1/VP9 decode, the original codecs) stays as-is and is not the priority.

## What the I/O layer is today (measured/read 2026-10-03)

| Piece | State | Why it is not useful yet |
|:---|:---|:---|
| `tpt-kinetix-demux` (2.7k lines) | MP4 works, MPEG-TS works, MKV/WebM basic | **Reads the whole file into memory** (`std::fs::read` in the CLI and every example); no `Read + Seek`/ranged source |
| `tpt-kinetix-mux` (0.6k lines) | progressive MP4, **one H.264 track** | builds the file in a `Vec<u8>` (`finish() -> Vec<u8>`), no audio, no multi-track, no fMP4/CMAF |
| `tpt-kinetix-stream` (2.4k lines) | RTMP ingest, HLS (MPEG-TS segments) | TS-only HLS; no fMP4/CMAF/DASH; no audio |
| CLI | `probe` works; `transcode` partial; `stream` stub | no `remux`, no `package`, no JSON probe |

Audio is the biggest functional hole: the AAC crate was removed (it lives in tpt-cadence),
so every container path currently carries video only. A packager that cannot carry audio
is not usable. The fix is **codec-agnostic passthrough** (packets with a codec id +
extradata), not decoders.

## Where ffmpeg is actually weak (and Kinetix can win)

Measured here (Windows, 1080p/120 s/85 MB H.264 MP4, avg of 20 runs):

| Operation | ffmpeg | Kinetix today |
|:---|---:|---:|
| fixed process cost: `ffmpeg -i clip.mp4 -f null -t 0.001 -` | 114 ms | n/a (library: 0 ms) |
| `ffprobe -show_streams -show_format` | 152 ms | 61 ms (reads all 85 MB; will not scale) |
| `ffprobe -show_entries stream=...` (minimal) | 44 ms | |

Known/qualitative, to be measured before claiming:
1. **Per-request process spawn.** ffmpeg/ffprobe are CLIs; a service that probes or
   repackages per request pays ~50-150 ms of process + DLL start and a fork/exec each time.
   An in-process library pays nothing.
2. **Remote probing.** ffmpeg's probe downloads `probesize` (5 MB default) and applies
   `analyzeduration`. An MP4 `moov` can be fetched with two HTTP range requests (tail + head).
   Target: probe an S3/HTTP object in ~2 round trips and a few hundred KB.
3. **Concurrency density.** RTMP->HLS in ffmpeg is one process per stream (tens of MB RSS
   each). One async Rust process can carry thousands of streams. Measure RSS/stream and
   streams/core against ffmpeg.
4. **Robustness on hostile input.** ffmpeg demuxers are a steady CVE source. Here every
   parser already has `*_never_panics` proptests + fuzz targets; extend to the whole path.
5. **Throughput is NOT a differentiator**: `-c copy` remux is disk/network-bound for both.
   Do not promise speed; promise latency, memory, density, safety, embeddability (WASM).

## Milestones

- [~] **M1 — Streaming demux.** *(MP4 done 2026-10-04: `ReadAt` source trait + `Mp4Reader`; CLI `probe` no longer loads the file; TS and MKV still take a whole buffer; HTTP-range source + fragmented MP4 still open.)* `Demuxer` over `Read + Seek` (and an async/ranged source
  trait); MP4 reads `moov` without loading `mdat`; samples are read on demand. Replace every
  `fs::read` in CLI/examples. Test with a >RAM synthetic sparse file and an HTTP-range mock.
- [ ] **M2 — Codec-agnostic tracks.** Packets carry `codec id + extradata` for H.264, AV1,
  VP9, **AAC/Opus/MP3 as opaque passthrough** (no decoder). Demux MP4/MKV/TS multi-track.
- [ ] **M3 — Streaming muxer.** `Write`-based MP4 muxer: multi-track, faststart, edit lists,
  and **fragmented MP4 / CMAF**. Remux (`tpt-kinetix remux in out`) round-trips vs
  `ffprobe`/`ffmpeg -c copy` byte-for-semantics.
- [ ] **M4 — Packaging.** HLS with fMP4 segments + DASH (on top of M3), live sliding window,
  low-latency parts; RTMP/SRT ingest -> package, audio included.
- [ ] **M5 — Probe service.** `probe --json` matching ffprobe's field names for the common
  cases; HTTP-range remote probing; WASM build kept working.
- [ ] **M6 — Evidence.** `just bench-io`: startup, remote-probe round trips, RSS/stream and
  streams/core vs ffmpeg, hostile-input corpus (ffmpeg crash/hang count vs Kinetix).

## Out of scope

Beating ffmpeg/dav1d at codec decode speed; new codec work; transcoding as a headline
feature. (Existing decoders stay and keep their conformance tests.)

### M1 progress (2026-10-04)

* `tpt-kinetix-demux`: `source::ReadAt` (positional reads; impls for memory, `File`, any `Read+Seek`, plus a
  `CountingSource` for I/O assertions) and `mp4::Mp4Reader` — hops top-level box headers, reads only `moov`,
  builds the sample index once (O(samples)), serves each packet with one positional read.
* Fixed on the way: the old demuxer was O(n^2) per file (stsc/stss rescans per sample), ignored `ctts`
  (so `pts == dts` on B-frame streams), read tracks sequentially instead of by decode time, and
  `parse_stsd` allocated from an attacker-controlled `entry_count`.
* Hostile-input limits: moov <= 256 MiB, <= 16 Mi samples/track, bounded top-level scan, sample bytes
  allocated only after an in-file bounds check. Proptest + new `fuzz_mp4_reader` target.
* Measured (1080p, 85 MB / 2.2 GB MP4, this machine): `tpt-kinetix probe` 27 ms / 32 ms (was 61 ms and
  would have read all 2.2 GB); `ffprobe -show_streams` 90 ms / 59 ms. Open of the 21 MB test file reads < 4 KiB.
* Remaining for M1: streaming TS (`TsDemuxer` still takes a `Vec`), MKV, an HTTP-range `ReadAt`
  (probe an S3 object in ~2 requests), fragmented MP4 (`moof`) input, `Mp4Demuxer` callers in
  pipeline/CLI transcode that still `fs::read` (`stage.rs`, `main.rs`).
