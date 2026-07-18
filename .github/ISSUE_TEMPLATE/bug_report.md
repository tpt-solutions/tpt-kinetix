---
name: Bug report
about: Report a defect in TPT Kinetix
title: "[bug] "
labels: ["bug"]
---

## Summary

A clear, concise description of the bug.

## Which crate(s)?

- [ ] tpt-kinetix-core
- [ ] tpt-kinetix-demux
- [ ] tpt-kinetix-mux
- [ ] tpt-kinetix-h264
- [ ] tpt-kinetix-av1
- [ ] tpt-kinetix-kg
- [ ] tpt-kinetix-pipeline
- [ ] tpt-kinetix-stream
- [ ] tpt-kinetix-cli

## Steps to reproduce

1. …
2. …

```rust
// Minimal reproducer, if possible.
```

## Expected behavior

What you expected to happen.

## Actual behavior

What actually happened. Include panic messages / backtraces
(`RUST_BACKTRACE=1`) and any sample media (attach small files where possible).

## Environment

- OS:
- Rust version (`rustc --version`):
- Crate version / git commit:

## Notes

> Reminder: several decoders are **not yet pixel-exact** (see each crate's
> README LIMITATIONS and `DecoderCapabilities`). "Wrong pixels" from an
> incomplete decode path is expected, not a bug — check `capabilities()` first.
