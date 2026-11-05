# Security policy

## Supported versions

TPT Kinetix is pre-1.0. Security fixes go to the latest published release
(0.1.x). Older releases are not patched.

## Reporting a vulnerability

Please do not open a public issue for a security problem. Report it privately
with GitHub's private vulnerability reporting: open the repository's
**Security** tab and choose **Report a vulnerability**.

Include what you can:

- the crate or command affected and the version;
- an input file or byte sequence that reproduces it, if you have one;
- what you observed (panic, hang, excessive memory, wrong output) and what you
  expected.

Parsers take untrusted input (containers, RTMP and WebRTC ingest, HTTP ranges),
so a panic, an unbounded allocation or a hang on hostile input counts as a
security issue. Crash reproducers are welcome; they become regression tests
under `fuzz/corpus/` or `tests/`.

We aim to acknowledge a report within a week and to agree a disclosure date
before a fix is published.
