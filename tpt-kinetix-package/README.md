# tpt-kinetix-package

Just-in-time HLS (fMP4) and DASH packaging of MP4 files.

Given an MP4 (a local file, an HTTP object, or any `AsyncReadAt`), `Packager` reads only the index,
plans key-frame-aligned segments, and builds playlists, init segments and media segments **on demand**
from ranged reads: nothing is pre-processed or stored. Pure Rust, no C, and it compiles to WebAssembly,
so the same code can run on a server, in a Cloudflare Worker, or in the browser.

See `todo-io.md` (M4) in the repository root.
