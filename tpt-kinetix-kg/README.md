# tpt-kinetix-kg

Knowledge-graph ingestion, dependency analysis, and Rust codegen tooling for deriving safe decoders from C source.

`tpt-kinetix-kg` parses a codec's C source (e.g. FFmpeg's `libavcodec/h264dec.c`) with
`tree-sitter-c`, builds a graph of parsing states and macroblock/state-machine transitions,
runs dependency analysis to find data-independent decode units, and emits Rust scaffolding
with `rayon` parallel iterators pre-injected at those independence points. It is the tool
this workspace uses to bootstrap new codec crates (see
[`docs/adding-a-codec.md`](../docs/adding-a-codec.md) for the full workflow) — it is not a
graph-visualization or general-purpose knowledge-graph tool.

## Quick usage

```sh
# Parse a C source file and print graph statistics
cargo run -p tpt-kinetix-kg -- ingest path/to/codec.c

# Build the full graph and write it as JSON
cargo run -p tpt-kinetix-kg -- graph path/to/codec.c -o codec.kg.json

# Find independent (parallelizable) decode units
cargo run -p tpt-kinetix-kg -- analyze codec.kg.json

# Generate Rust scaffolding with rayon injected at independence points
cargo run -p tpt-kinetix-kg -- codegen codec.kg.json --crate-name tpt-kinetix-mycodec --inject-rayon

# All of the above in one step
cargo run -p tpt-kinetix-kg -- run path/to/codec.c --crate-name tpt-kinetix-mycodec --inject-rayon
```

See [`examples/ingest_ffmpeg_h264.rs`](examples/ingest_ffmpeg_h264.rs) for the equivalent
ingest → graph → analyze flow driven from library code instead of the CLI.

## Limitations

- The C→graph extraction passes (`extract_bitstream_parsing_tree`,
  `extract_macroblock_state_machine`) are heuristic, syntax-driven walks over the
  `tree-sitter-c` AST — they find functions, switch/loop control flow, and enum-based state,
  but do not perform full semantic/type analysis. Generated scaffolding is a starting point
  for hand-completion, not a finished decoder.
- Only C source is supported as input (via `tree-sitter-c`); C++ codec sources are not parsed.
- Provenance/licensing of any C source you ingest is your responsibility — `tpt-kinetix-kg`
  does not track or check the license of the input file. If you ingest FFmpeg source, the
  generated Rust scaffold's relationship to FFmpeg's LGPL/GPL licensing should be reviewed
  before the resulting crate is published.

See the [workspace README](../README.md) for the full project overview, architecture diagram, and quickstart guide.
