# tpt-kinetix (Python)

Python bindings for the TPT Kinetix container tools: probe, remux and package
media files. Nothing is decoded; the work is done on the container, in-process.

```python
import tpt_kinetix

info = tpt_kinetix.probe("clip.webm")          # dict: format + streams
tpt_kinetix.faststart("in.mp4", "fast.mp4")    # index moved to the front
tpt_kinetix.remux("in.mp4", "out.webm")        # stream copy, container from extension
tpt_kinetix.remux("in.mp4", "frag.mp4", fragment_ms=2000)

counts = tpt_kinetix.package("in.mp4", "out/", segment_seconds=6.0)
# CENC (VP9, AV1 and Opus samples only): both key and key_id, 32 hex digits each
tpt_kinetix.package("in.webm", "out-cenc/", key_id="00112233445566778899aabbccddeeff",
                    key="ffeeddccbbaa99887766554433221100", scheme="cenc")
```

Failures raise `tpt_kinetix.KinetixError`, with the Rust error chain as the message.

## Building

Built with [maturin](https://www.maturin.rs). From this directory:

```sh
maturin develop        # into the active virtualenv
python -m unittest discover -s tests
```

`tools/py-binding-test.sh` at the repository root builds a wheel, installs it into a
fresh virtualenv, and runs the tests against ffmpeg-made media.

This crate is not published to PyPI (`publish = false` in the workspace); the
package name is reserved for the maintainer to decide.
