//! Knowledge-graph ingestion, analysis, and Rust codegen tooling.
//!
//! The KG tool ingests C source code for a codec (e.g. FFmpeg's H.264 decoder),
//! builds a knowledge graph of the bitstream parsing states and macroblock state
//! machine, performs dependency analysis to identify parallelism opportunities,
//! and emits Rust scaffolding with `rayon` parallel iterators pre-injected.

pub mod analysis;
pub mod codegen;
pub mod extraction;
pub mod graph;
pub mod ingestion;
