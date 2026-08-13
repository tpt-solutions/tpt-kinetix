# web-demo

A small, dependency-free static page that probes an MP4 file entirely client-side using a
`wasm32` build of `tpt-kinetix-demux`. Nothing is uploaded — parsing happens in your browser.

This is a demo of the demux/probe path, not a product UI — TPT Kinetix has no server-side
frontend (see the root README's "Current status" table for what's actually implemented).

## Build and run

Requires [`wasm-pack`](https://rustwasm.github.io/wasm-pack/) (`cargo install wasm-pack`) and
the `wasm32-unknown-unknown` target (`rustup target add wasm32-unknown-unknown` — already added
by `scripts/setup.sh` / `scripts/setup.ps1`).

```sh
just wasm-demo
```

This runs `wasm-pack build --target web --out-dir web-demo/pkg -- --features wasm` from
`tpt-kinetix-demux/`, then serves this directory on `http://127.0.0.1:8787`.

## Manual build

```sh
cd tpt-kinetix-demux
wasm-pack build --target web --out-dir ../web-demo/pkg -- --features wasm
cd ../web-demo
python -m http.server 8787   # or any static file server
```

`pkg/` is generated output (`.gitignore`d) — regenerate it with the command above whenever
`tpt-kinetix-demux/src/wasm.rs` changes.
