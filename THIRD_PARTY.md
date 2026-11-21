# Third-party notices

TPT Kinetix code is MIT OR Apache-2.0 (see `LICENSE-MIT` and `LICENSE-APACHE`), except for the
crate listed under "Copyleft exception" below.

This file lists third-party projects whose code was ported, transcribed or closely adapted into
this repository. Each such file keeps a comment naming its source. The listed licences require
that their copyright notice and licence text be kept with the code; the notices below satisfy that
for the ported portions. Permissively-licensed Rust dependencies are checked by `just deny`.

## Ported or adapted code

| Project | Licence | Where it is used |
|---|---|---|
| [dav1d](https://code.videolan.org/videolan/dav1d) | BSD-2-Clause | `tpt-kinetix-av1`: `src/reconstruct/comp_ctx.rs`, `src/reconstruct/inter_block.rs`, `src/reconstruct/intra_block.rs`, `src/frame.rs`, `src/film_grain_table.rs` (Gaussian sequence table), plus comments naming dav1d behaviour |
| [libaom](https://aomedia.googlesource.com/aom) | BSD-2-Clause (with AOM patent grant) | `tpt-kinetix-av1`: `src/reconstruct/predict.rs` (`sm_weight_arrays`) |
| [libgav1](https://chromium.googlesource.com/codecs/libgav1) | Apache-2.0 | `tpt-kinetix-av1`: `src/reconstruct/palette.rs` (`kDcLookup` 8-bit table) |
| [libvpx](https://chromium.googlesource.com/webm/libvpx) | BSD-3-Clause | `tpt-kinetix-vp9`: `src/transform.rs` (`iadst8_c`, `iadst16_c`) |

### BSD-2-Clause (dav1d, libaom)

Copyright the dav1d authors (VideoLAN) and the Alliance for Open Media (AOM) contributors.
Redistribution and use in source and binary forms, with or without modification, are permitted
provided that the conditions of the upstream licence are met. The full text is in the upstream
`COPYING` / `LICENSE` file.

### BSD-3-Clause (libvpx)

Copyright the WebM Project authors. Redistribution and use in source and binary forms,
with or without modification, are permitted under the conditions of the upstream `LICENSE` file.

### Apache-2.0 (libgav1)

Copyright Google LLC. Licensed under the Apache License, Version 2.0. The full text is at
<https://www.apache.org/licenses/LICENSE-2.0>.

## Copyleft exception: `out-kinetix-h264`

`out-kinetix-h264` contains code transcribed from FFmpeg's `libavcodec` (LGPL-2.1-or-later). It is
therefore licensed `LGPL-2.1-or-later`, not MIT/Apache-2.0, and is `publish = false`. See
[PATENTS.md](PATENTS.md) and [out-kinetix-h264/README.md](out-kinetix-h264/README.md).

## Reference data not committed

Numeric tables extracted from FFmpeg (for `tpt-kinetix-vp9` and `out-kinetix-h264`) are checked by
`tpt-kinetix-kg verify-tables` against a pinned FFmpeg commit. The FFmpeg source itself is fetched into
a gitignored cache (`**/.cache/`) and is never committed.
