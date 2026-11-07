# Media tool

A starter command-line tool that lists the tracks of a WebM, Matroska, MP4 or IVF
file, using [`tpt-kinetix-demux`](https://crates.io/crates/tpt-kinetix-demux).

```sh
cargo run -- clip.webm
```

Next steps: read `src/main.rs`, which is about 60 lines, then add the operations you need.
The `tpt-kinetix-mux` crate writes MP4, WebM, IVF, Ogg and fMP4; `tpt-kinetix-pipeline` runs decode
and encode stages.
