# tpt-kinetix-ops

The container operations behind the `tpt-kinetix` CLI, as a library: probe JSON, remux
(including faststart and fragmented output), package (HLS and DASH with optional CENC) and the
run summaries they print. The C ABI (`tpt-kinetix-ffi`), the Python bindings (`tpt-kinetix-py`)
and the Node.js bindings (`tpt-kinetix-node`) call these same functions, so every front end
produces the same output.

Nothing here decodes media. The crate is `publish = false`; it is a workspace-internal library.

| Module | What it does |
| --- | --- |
| `probe_json` | The ffprobe-shaped `probe --json` document |
| `remux` | Stream-copy remux (`remux` and `sanitize`), with the container chosen from the output name |
| `package` | HLS and DASH packaging of a file into a directory, with optional CENC (`package`, `encryption_from_hex`) |
| `summary` | The compact track list the C ABI and the bindings return |

Changes are listed in [CHANGELOG.md](CHANGELOG.md) and the workspace [CHANGELOG.md](../CHANGELOG.md).
