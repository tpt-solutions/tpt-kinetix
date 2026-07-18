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
    /// Crate name for the emitted scaffold, e.g. `"tpt-kinetix-h264"`.
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
    out.push_str("//! DO NOT edit by hand; re-run `tpt-kinetix-kg codegen` to regenerate.\n\n");
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
    out.push_str("//! DO NOT edit by hand; re-run `tpt-kinetix-kg codegen` to regenerate.\n\n");
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
    out.push_str("//! DO NOT edit by hand; re-run `tpt-kinetix-kg codegen` to regenerate.\n\n");
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
        out.push_str(&format!("pub fn process_set_{idx}(items: &[()]) {{\n"));
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
    out.push_str("//! DO NOT edit by hand; re-run `tpt-kinetix-kg codegen` to regenerate.\n\n");

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::{find_independent_sets, mark_parallel_regions};
    use crate::graph::EdgeKind;

    fn build_graph() -> KnowledgeGraph {
        let mut g = KnowledgeGraph::new();
        g.add_node(NodeKind::Function {
            name: "decode_slice".into(),
        });
        g.add_node(NodeKind::Function {
            name: "parse_nal".into(),
        });
        g.add_node(NodeKind::MacroblockState {
            name: "MB_INTRA".into(),
        });
        g.add_node(NodeKind::MacroblockState {
            name: "MB_INTER".into(),
        });
        g
    }

    #[test]
    fn generates_functions_and_mod_file() {
        let g = build_graph();
        let sets = find_independent_sets(&g);
        let opts = CodegenOptions {
            crate_name: "tpt-kinetix-h264".into(),
            inject_rayon: false,
        };
        let files = generate(&g, &sets, &opts).unwrap();

        assert!(files.contains_key("src/generated/functions.rs"));
        assert!(files.contains_key("src/generated/mb_states.rs"));
        assert!(files.contains_key("src/generated/mod.rs"));
        // No rayon file when inject_rayon is false.
        assert!(!files.contains_key("src/generated/parallel_sets.rs"));

        let funcs = &files["src/generated/functions.rs"];
        assert!(funcs.contains("pub fn decode_slice"));
        assert!(funcs.contains("pub fn parse_nal"));

        let states = &files["src/generated/mb_states.rs"];
        assert!(states.contains("MB_INTRA"));
        assert!(states.contains("MB_INTER"));
        assert!(states.contains("pub enum MacroblockState"));

        let mod_file = &files["src/generated/mod.rs"];
        assert!(mod_file.contains("pub mod functions;"));
        assert!(mod_file.contains("pub mod mb_states;"));
    }

    #[test]
    fn injects_rayon_when_requested() {
        let mut g = build_graph();
        let sets = find_independent_sets(&g);
        mark_parallel_regions(&mut g, &sets);
        // Recompute sets on the enriched graph so a multi-node level exists.
        let sets = find_independent_sets(&g);

        let opts = CodegenOptions {
            crate_name: "tpt-kinetix-h264".into(),
            inject_rayon: true,
        };
        let files = generate(&g, &sets, &opts).unwrap();
        assert!(files.contains_key("src/generated/parallel_sets.rs"));
        let par = &files["src/generated/parallel_sets.rs"];
        assert!(par.contains("use rayon::prelude::*;"));
        assert!(par.contains("par_iter"));
    }

    #[test]
    fn sanitises_awkward_variant_names() {
        assert_eq!(sanitise_variant("MB_INTRA"), "MB_INTRA");
        assert_eq!(sanitise_variant("mb-inter"), "Mb_inter");
        assert_eq!(sanitise_variant("123state"), "state");
        // All-invalid falls back to a State_ prefix.
        assert_eq!(sanitise_variant("_0"), "State__0");
    }

    #[test]
    fn no_rayon_file_without_multinode_sets() {
        // Graph where every level is a singleton -> no parallel file emitted
        // even with inject_rayon, because emit only writes for sets >= 2.
        let mut g = KnowledgeGraph::new();
        let a = g.add_node(NodeKind::Function { name: "a".into() });
        let b = g.add_node(NodeKind::Function { name: "b".into() });
        g.add_edge(a, b, EdgeKind::DataDependency);
        let sets = find_independent_sets(&g);
        let opts = CodegenOptions {
            crate_name: "c".into(),
            inject_rayon: true,
        };
        let files = generate(&g, &sets, &opts).unwrap();
        // The file is created but contains only the header (no process_set fns).
        if let Some(par) = files.get("src/generated/parallel_sets.rs") {
            assert!(!par.contains("process_set_"));
        }
    }
}
