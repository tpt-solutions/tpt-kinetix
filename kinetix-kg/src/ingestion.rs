//! C source ingestion: parse codec source into an AST representation using tree-sitter.

use tree_sitter::{Language, Parser, Tree};

/// A parsed C AST backed by tree-sitter.
pub struct CAst {
    source: String,
    tree: Tree,
    language: Language,
}

impl CAst {
    /// Parse `source` as C and return a [`CAst`].
    pub fn from_source(source: &str) -> anyhow::Result<Self> {
        let language: Language = tree_sitter_c::LANGUAGE.into();
        let mut parser = Parser::new();
        parser.set_language(&language)?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("tree-sitter failed to parse C source"))?;
        Ok(Self {
            source: source.to_string(),
            tree,
            language,
        })
    }

    /// Read the file at `path`, then call [`Self::from_source`].
    pub fn from_file(path: impl AsRef<std::path::Path>) -> anyhow::Result<Self> {
        let source = std::fs::read_to_string(path)?;
        Self::from_source(&source)
    }

    /// Return the root node of the parsed tree.
    pub fn root_node(&self) -> tree_sitter::Node<'_> {
        self.tree.root_node()
    }

    /// Return the original source text.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Return the tree-sitter [`Language`] used to parse this AST.
    pub fn language(&self) -> &Language {
        &self.language
    }
}
