# tpt-kinetix-ffi (C ABI)

A C interface to the container tools: probe, faststart, remux and package. Build the
shared library with `cargo build --release -p tpt-kinetix-ffi`; it produces
`tpt_kinetix_ffi.dll` / `libtpt_kinetix_ffi.so` / `libtpt_kinetix_ffi.dylib`, a static
library, and the header `include/tpt_kinetix.h` (regenerate with `just ffi-header`).

```c
#include "tpt_kinetix.h"

char *json = tk_probe_json("clip.webm");          /* {"format":...,"streams":[...]} */
if (json == NULL) { fprintf(stderr, "%s\n", tk_last_error()); return 1; }
tk_free_string(json);

tk_faststart("in.mp4", "fast.mp4");               /* 0 or -1 */
tk_remux("in.mp4", "out.webm", 0, 0);             /* container from the extension */

struct TkPackageSummary s;
tk_package("in.mp4", "out/", 6.0, NULL, NULL, NULL, &s);   /* clear HLS + DASH */
```

## Conventions

- Strings returned by the library are owned by the caller; free them with
  `tk_free_string`. `tk_version` returns a static string that must not be freed.
- A call that fails returns null (or -1). `tk_last_error` gives the reason on the same
  thread, until the next failing call on that thread.
- No Rust panic crosses the boundary.
- `tk_package` takes an optional CENC key: pass `key_id` and `key` (32 hex digits each)
  together, or both null. CENC covers VP9, AV1 and Opus samples only.

## Testing

`tools/ffi-smoke-test.sh` checks that the header matches the Rust source, compiles
`tests/c/smoke.c` against the library, and runs it on ffmpeg-made media.
