# kinetix-kg developer guide

## What this tool does

`kinetix-kg` ingests C source code for a codec, builds a typed knowledge graph
of its bitstream-parsing states and macroblock state machine, performs
dependency analysis to identify parallelism opportunities, and emits Rust
decoder scaffolding with `rayon` parallel iterators pre-injected at the
independence points.

---

## Running the KG tool against a new codec

### 1. Ingest and inspect

```sh
kinetix-kg ingest path/to/codec/h264dec.c
```

Prints graph statistics: how many functions, switch-case states, loop bodies,
macroblock states, and parallel regions were extracted.

### 2. Build the full graph JSON

```sh
kinetix-kg graph path/to/codec/h264dec.c -o h264.kg.json
```

Writes a JSON representation of the knowledge graph that can be committed
alongside the codec source or inspected with `jq`.

### 3. Dependency analysis

```sh
kinetix-kg analyze h264.kg.json
```

Runs Kahn's topological-sort algorithm over `DataDependency` edges and prints
the independent sets — groups of nodes that share no data dependencies and may
be processed concurrently.

### 4. Code generation

```sh
kinetix-kg codegen h264.kg.json \
  --crate-name kinetix-h264 \
  --inject-rayon \
  --output-dir kinetix-h264/
```

Writes generated Rust files under `--output-dir`:

| File | Contents |
|------|----------|
| `src/generated/functions.rs` | `pub fn` stubs for every extracted C function |
| `src/generated/mb_states.rs` | `MacroblockState` enum derived from C enums |
| `src/generated/parallel_sets.rs` | `rayon::par_iter` scaffolding for each independent set |
| `src/generated/mod.rs` | Module re-exports |

### 5. End-to-end in one command

```sh
kinetix-kg run path/to/codec/h264dec.c \
  --crate-name kinetix-h264 \
  --inject-rayon \
  --output-dir kinetix-h264/
```

Runs all four phases and writes generated files.

---

## tree-sitter-c vs libclang: why we chose tree-sitter

| Criterion | tree-sitter-c | libclang |
|-----------|--------------|----------|
| Rust integration | Pure Rust crate (`tree-sitter-c`) | Requires LLVM/libclang C++ system library |
| CI requirements | No system libraries | LLVM must be installed |
| Crate shipping | Works anywhere `cc` works | Depends on `LIBCLANG_PATH` env var |
| Full C semantics | Syntactic only — no type resolution | Full preprocessed type information |
| Speed | Very fast (incremental re-parsing) | Slower initial parse |
| Suitability | Structural analysis: function shapes, switch/loop patterns | Precise type-aware analysis |

For Phase 1 (structural scaffolding) tree-sitter is the right tool: we only
need to find function boundaries, switch-case states, loop bodies, and enum
declarations.  Type resolution is deferred to Phase 3 where a human completes
the scaffold.

---

## Graph schema

### NodeKind

| Variant | Meaning |
|---------|---------|
| `Function { name }` | A C function definition |
| `SwitchCase { value }` | One `case` arm in a `switch` — models a bitstream state |
| `LoopBody { label }` | A `for`/`while`/`do` loop body — candidate for data parallelism |
| `MacroblockState { name }` | One variant of a C `enum` used as a state machine |
| `SyntaxElement { name, bits }` | A named bitstream syntax element; `bits` is optional width |
| `ParallelRegion { description }` | Meta-node wrapping a group of nodes safe to parallelise |

### EdgeKind

| Variant | Meaning |
|---------|---------|
| `Calls` | Function A calls function B |
| `Transition` | Control flows from state A to state B |
| `DataDependency` | B reads data produced by A; constrains ordering |
| `Independent` | A and B have no data dependencies (may run in parallel) |

---

## Interpreting the codegen output

### `functions.rs`

Contains a `pub fn` stub for every C function found in the source.  Each stub
calls `todo!()` with a Phase-3 reminder.  Replace each `todo!()` with the real
Rust implementation, using the knowledge graph JSON as a reference for what
data the function depends on.

### `mb_states.rs`

Contains a `MacroblockState` enum derived from C `enum` declarations.  Add
`impl` blocks for state transitions as Phase 3 proceeds.

### `parallel_sets.rs`

Contains `rayon::par_iter().for_each(...)` scaffolding for each independent
set of nodes.  The `//` comments list the graph node ids and kinds that belong
to the set.  Fill in the closures with the actual per-item work.

The `process_set_N` functions are generated stubs; they will not compile into
a final binary until the `KinetixError` type and item types are filled in
(Phase 3).

---

## Extending the tool

* **Adding a new extraction pass** — implement a new function in
  `src/extraction.rs` that takes `&CAst` and returns a `KnowledgeGraph`, then
  wire it up in `src/main.rs::build_combined_graph`.
* **Adding new edge kinds** — add a variant to `EdgeKind` in `src/graph.rs`
  and update `src/analysis.rs` if the new edges should influence parallelism
  detection.
* **Custom codegen templates** — extend `src/codegen.rs`; each emitter is a
  plain `fn emit_*(…) -> String`.
