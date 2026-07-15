//! C source ingestion: parse codec source into an AST representation.
//!
//! TODO (Phase 1): Evaluate `tree-sitter-c` vs `libclang` bindings and
//! implement the chosen approach.

/// A placeholder for the ingested C AST.
pub struct CAst {
    pub source_path: String,
}

impl CAst {
    /// Ingest the C source file at `path` and return an AST handle.
    ///
    /// TODO (Phase 1): Implement via tree-sitter or libclang.
    pub fn from_file(path: impl Into<String>) -> anyhow::Result<Self> {
        Ok(Self { source_path: path.into() })
    }
}
