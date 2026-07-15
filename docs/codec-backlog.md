# Codec Backlog

The full ~400-codec FFmpeg surface is explicitly out of scope for the current phases.
Tracked candidates in priority order:

| Codec | Phase | Notes |
|-------|-------|-------|
| AAC (decode) | Post-MVP | See codec-evaluations/aac.md |
| HEVC/H.265 | Post-MVP | See codec-evaluations/hevc.md |
| VP9 | Future | Similar to AV1; share KG tooling |
| Opus | Future | Audio; consider wrapping `opus` crate |
| MPEG-2 Video | Low | Legacy; low priority |
| MPEG-2 Audio / MP3 | Low | Legacy; low priority |

Add new candidates here as they are prioritized.
