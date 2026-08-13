# tpt-kinetix-realtime

An original low-latency, loss-resilient video codec for the TPT Kinetix
engine.

Realtime's design center is **sub-frame latency** and **graceful degradation
under packet loss**, not maximum compression ratio. The format is
**profile-agnostic**: cloud gaming, video conferencing, and AR/smart-glasses
overlay are three preset parameter sets over one shared core — no B-frame
lookahead, hybrid FEC + intra-refresh + concealment loss recovery, and an
enforced per-frame latency budget.

See `docs/realtime-codec-design.md` for the full design (DECISION blocks for
loss recovery, GOP structure, latency budget, validation metric, v1 budget,
and the shared-primitive relationship to `tpt-kinetix-lean`).

## Status

Scaffold: the profile-aware sequence/frame headers (`src/headers.rs`) and the
shared `BitReader`/rANS primitives (`tpt-kinetix-bitstream`) exist and
round-trip, but block reconstruction, slice framing, intra-refresh masking,
FEC, and concealment are not implemented yet. `RealtimeDecoder::capabilities`
reports `pixel_exact: false` accordingly.
