//! Rust code generation from a [`KnowledgeGraph`].
//!
//! Emits:
//! * A function stub module (`src/generated/functions.rs`) for each
//!   `NodeKind::Function`.
//! * A macroblock state enum (`src/generated/mb_states.rs`) derived from
//!   `NodeKind::MacroblockState` nodes.
//! * Rayon parallel-iterator scaffolding
//!   (`src/generated/parallel_sets.rs`) for each [`IndependentSet`] when
//!   `opts.inject_rayon` is `true`.

use std::collections::HashMap;

use crate::analysis::IndependentSet;
use crate::graph::{KnowledgeGraph, NodeKind};

/// Options controlling the emitted Rust code.
#[derive(Debug, Default)]
pub struct CodegenOptions {
    /// Crate name for the emitted scaffold, e.g. `"kinetix-h264"`.
    pub crate_name: String,
    /// Whether to inject `rayon` parallel iterators at independent sets.
    pub inject_rayon: bool,
}

/// Generate Rust source files from a [`KnowledgeGraph`] and its pre-computed
/// independent sets.
///
/// Returns a map of `"src/generated/<file>.rs"` → Rust source text.
pub fn generate(
    graph: &KnowledgeGraph,
    independent_sets: &[IndependentSet],
    opts: &CodegenOptions,
) -> anyhow::Result<HashMap<String, String>> {
    let mut files: HashMap<String, String> = HashMap::new();

    files.insert(
        "src/generated/functions.rs".to_string(),
        emit_functions(graph, &opts.crate_name),
    );

    let mb_states = graph.nodes_by_kind(|k| matches!(k, NodeKind::MacroblockState { .. }));
    if !mb_states.is_empty() {
        files.insert(
            "src/generated/mb_states.rs".to_string(),
            emit_mb_states(&mb_states),
        );
    }

    if opts.inject_rayon && !independent_sets.is_empty() {
        files.insert(
            "src/generated/parallel_sets.rs".to_string(),
            emit_parallel_sets(independent_sets, graph),
        );
    }

    // Top-level mod file that re-exports everything.
    files.insert(
        "src/generated/mod.rs".to_string(),
        emit_mod_file(&files, opts),
    );

    Ok(files)
}

// ── emitters ─────────────────────────────────────────────────────────────────

fn emit_functions(graph: &KnowledgeGraph, _crate_name: &str) -> String {
    let mut out = String::new();
    out.push_str("//! Auto-generated function stubs — fill in from KG scaffold (Phase 3).\n");
    out.push_str("//!\n");
    out.push_str("//! DO NOT edit by hand; re-run `kinetix-kg codegen` to regenerate.\n\n");
    out.push_str("use crate::KinetixError;\n\n");

    for node in graph.nodes_by_kind(|k| matches!(k, NodeKind::Function { .. })) {
        let NodeKind::Function { name } = &node.kind else {
            continue;
        };
        out.push_str("/// Auto-generated from knowledge graph.\n");
        out.push_str(&format!(
            "pub fn {name}(/* TODO: fill params from graph */) -> Result<(), KinetixError> {{\n"
        ));
        out.push_str(&format!(
            "    todo!(\"{name} — Phase 3: hand-complete from KG scaffold\")\n"
        ));
        out.push_str("}\n\n");
    }

    out
}

fn emit_mb_states(nodes: &[&crate::graph::Node]) -> String {
    let mut out = String::new();
    out.push_str("//! Auto-generated macroblock state enum.\n");
    out.push_str("//!\n");
    out.push_str("//! DO NOT edit by hand; re-run `kinetix-kg codegen` to regenerate.\n\n");
    out.push_str("#[derive(Debug, Clone, Copy, PartialEq, Eq)]\n");
    out.push_str("pub enum MacroblockState {\n");

    for node in nodes {
        let NodeKind::MacroblockState { name } = &node.kind else {
            continue;
        };
        // Sanitise name: replace non-alphanumeric with underscore, then
        // capitalise the first letter so it's a valid Rust variant.
        let variant = sanitise_variant(name);
        out.push_str(&format!("    {variant},\n"));
    }

    out.push_str("}\n");
    out
}

fn emit_parallel_sets(sets: &[IndependentSet], graph: &KnowledgeGraph) -> String {
    let mut out = String::new();
    out.push_str("//! Auto-generated rayon parallel-iterator scaffolding.\n");
    out.push_str("//!\n");
    out.push_str("//! DO NOT edit by hand; re-run `kinetix-kg codegen` to regenerate.\n\n");
    out.push_str("use rayon::prelude::*;\n\n");

    for (idx, set) in sets.iter().enumerate() {
        if set.node_ids.len() < 2 {
            continue;
        }
        out.push_str(&format!(
            "// Items in independent set #{idx} can be processed in parallel:\n"
        ));
        out.push_str(&format!("// {}\n", set.description));
        out.push_str("//\n");
        out.push_str("// Nodes:\n");
        for &id in &set.node_ids {
            if let Some(node) = graph.nodes.iter().find(|n| n.id == id) {
                out.push_str(&format!("//   [{id}] {:?}\n", node.kind));
            }
        }
        out.push_str(&format!(
            "pub fn process_set_{idx}(items: &[()]) {{\n"
        ));
        out.push_str("    items.par_iter().for_each(|_item| {\n");
        out.push_str("        // TODO: fill body from KG scaffold (Phase 3)\n");
        out.push_str("    });\n");
        out.push_str("}\n\n");
    }

    out
}

fn emit_mod_file(files: &HashMap<String, String>, opts: &CodegenOptions) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "//! Generated modules for crate `{}`.\n",
        opts.crate_name
    ));
    out.push_str("//!\n");
    out.push_str("//! DO NOT edit by hand; re-run `kinetix-kg codegen` to regenerate.\n\n");

    for path in files.keys() {
        if path == "src/generated/mod.rs" {
            continue;
        }
        // Extract module name from "src/generated/<name>.rs".
        if let Some(module) = path
            .strip_prefix("src/generated/")
            .and_then(|s| s.strip_suffix(".rs"))
        {
            out.push_str(&format!("pub mod {module};\n"));
        }
    }

    out
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn sanitise_variant(name: &str) -> String {
    let mut result: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect();
    // Capitalise first char for Rust convention.
    if let Some(first) = result.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    // Remove leading underscores and digits to produce a valid identifier.
    let trimmed = result.trim_start_matches(|c: char| c == '_' || c.is_ascii_digit());
    if trimmed.is_empty() {
        format!("State_{name}")
    } else {
        trimmed.to_string()
    }
}
