//! AST extraction passes: walk a [`CAst`] and populate a [`KnowledgeGraph`].
//!
//! Two passes are provided:
//! * [`extract_bitstream_parsing_tree`] — finds functions, switch cases, loops,
//!   and call edges.
//! * [`extract_macroblock_state_machine`] — finds enum declarations and models
//!   them as macroblock state nodes with transition edges.

use std::collections::HashMap;

use tree_sitter::Node;

use crate::graph::{EdgeKind, KnowledgeGraph, NodeKind};
use crate::ingestion::CAst;

// ── helpers ──────────────────────────────────────────────────────────────────

/// Call `visitor` on `node` and every descendant, depth-first.
fn visit_all<'a, F>(node: Node<'a>, visitor: &mut F)
where
    F: FnMut(Node<'a>),
{
    visitor(node);
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            visit_all(cursor.node(), visitor);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

/// Extract the identifier text of a function declarator node.
///
/// Handles direct identifiers and one level of pointer indirection.
fn function_name<'a>(func_def: Node<'a>, source: &'a [u8]) -> Option<&'a str> {
    let decl = func_def.child_by_field_name("declarator")?;
    let inner = match decl.kind() {
        "function_declarator" => decl.child_by_field_name("declarator")?,
        // int *func(...) → pointer_declarator → function_declarator → identifier
        "pointer_declarator" => {
            let fd = decl.child_by_field_name("declarator")?;
            if fd.kind() == "function_declarator" {
                fd.child_by_field_name("declarator")?
            } else {
                return None;
            }
        }
        _ => return None,
    };
    if inner.kind() == "identifier" {
        inner.utf8_text(source).ok()
    } else {
        None
    }
}

/// Return the text of the first named child of `node` that is an identifier.
fn first_identifier_text<'a>(node: Node<'a>, source: &'a [u8]) -> Option<&'a str> {
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            if child.kind() == "identifier" || child.kind() == "number_literal" {
                return child.utf8_text(source).ok();
            }
        }
    }
    None
}

// ── Pass 1: bitstream parsing tree ──────────────────────────────────────────

/// Walk `ast` and build a graph of:
/// * `Function` nodes for each top-level C function
/// * `SwitchCase` nodes for each `case` inside a switch
/// * `LoopBody` nodes for each `for`/`while`/`do` loop
/// * `Calls` edges from caller to callee (by name)
/// * `Transition` edges between consecutive switch cases
pub fn extract_bitstream_parsing_tree(ast: &CAst) -> KnowledgeGraph {
    let mut graph = KnowledgeGraph::new();
    let source = ast.source().as_bytes();
    let root = ast.root_node();

    // ── collect function definitions ────────────────────────────────────────
    let mut func_ids: HashMap<String, u64> = HashMap::new();

    // We need two passes: first create all Function nodes so call edges can
    // reference them, then walk each function body for structure.
    let mut func_nodes: Vec<Node> = Vec::new();

    {
        let mut cursor = root.walk();
        // Only direct children of the translation unit are top-level items.
        if cursor.goto_first_child() {
            loop {
                let node = cursor.node();
                if node.kind() == "function_definition" {
                    func_nodes.push(node);
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    for func_node in &func_nodes {
        if let Some(name) = function_name(*func_node, source) {
            let id = graph.add_node(NodeKind::Function {
                name: name.to_string(),
            });
            func_ids.insert(name.to_string(), id);
        }
    }

    // ── walk each function body ─────────────────────────────────────────────
    for func_node in &func_nodes {
        let Some(caller_name) = function_name(*func_node, source) else {
            continue;
        };
        let caller_id = func_ids[caller_name];

        // Find the compound_statement (function body).
        let body = match func_node.child_by_field_name("body") {
            Some(b) => b,
            None => continue,
        };

        // Track calls we have already added to avoid duplicates.
        let mut called: std::collections::HashSet<String> = std::collections::HashSet::new();

        // Loop counter for unique LoopBody labels.
        let mut loop_counter: u32 = 0;

        visit_all(body, &mut |node| {
            match node.kind() {
                // ── call_expression ─────────────────────────────────────────
                "call_expression" => {
                    if let Some(fn_node) = node.child_by_field_name("function") {
                        if let Ok(callee) = fn_node.utf8_text(source) {
                            let callee = callee.to_string();
                            if !called.contains(&callee) {
                                called.insert(callee.clone());
                                if let Some(&callee_id) = func_ids.get(&callee) {
                                    graph.add_edge(caller_id, callee_id, EdgeKind::Calls);
                                }
                            }
                        }
                    }
                }

                // ── switch_statement ────────────────────────────────────────
                "switch_statement" => {
                    collect_switch_cases(node, source, caller_id, &mut graph);
                }

                // ── loop bodies ─────────────────────────────────────────────
                "for_statement" | "while_statement" | "do_statement" => {
                    loop_counter += 1;
                    let label = format!("{caller_name}_loop_{loop_counter}");
                    let loop_id = graph.add_node(NodeKind::LoopBody { label });
                    graph.add_edge(caller_id, loop_id, EdgeKind::Transition);
                }

                _ => {}
            }
        });
    }

    graph
}

/// Collect `case` arms from a single `switch_statement` node, adding
/// `SwitchCase` nodes and `Transition` edges for sequential fall-through.
fn collect_switch_cases(
    switch_node: Node<'_>,
    source: &[u8],
    parent_fn_id: u64,
    graph: &mut KnowledgeGraph,
) {
    // The body is the last child (a compound_statement).
    let body = match switch_node.child_by_field_name("body") {
        Some(b) => b,
        None => return,
    };

    let mut prev_case_id: Option<u64> = None;
    let mut cursor = body.walk();
    if !cursor.goto_first_child() {
        return;
    }
    loop {
        let node = cursor.node();
        if node.kind() == "case_statement" {
            let value = node
                .child_by_field_name("value")
                .and_then(|v| v.utf8_text(source).ok())
                .unwrap_or("<unknown>")
                .to_string();

            let case_id = graph.add_node(NodeKind::SwitchCase {
                value: value.clone(),
            });
            graph.add_edge(parent_fn_id, case_id, EdgeKind::Transition);

            if let Some(prev) = prev_case_id {
                // Check whether the previous case falls through (no break).
                if case_falls_through(prev, graph) {
                    graph.add_edge(prev, case_id, EdgeKind::Transition);
                }
            }
            prev_case_id = Some(case_id);
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }
}

/// Heuristic: a switch case node "falls through" if none of its edges are
/// Transition edges to another case (i.e. it was just added and has no
/// outgoing transitions yet, meaning no explicit break was seen).
///
/// For this extraction pass we conservatively treat every case as falling
/// through unless it already has an outgoing Transition.
fn case_falls_through(case_id: u64, graph: &KnowledgeGraph) -> bool {
    !graph
        .edges
        .iter()
        .any(|e| e.from == case_id && matches!(e.kind, EdgeKind::Transition))
}

// ── Pass 2: macroblock state machine ────────────────────────────────────────

/// Walk `ast` looking for C enum declarations and model each variant as a
/// `MacroblockState` node.  Consecutive variants get `Transition` edges to
/// represent sequential state progression.
pub fn extract_macroblock_state_machine(ast: &CAst) -> KnowledgeGraph {
    let mut graph = KnowledgeGraph::new();
    let source = ast.source().as_bytes();
    let root = ast.root_node();

    visit_all(root, &mut |node| {
        if node.kind() == "enum_specifier" {
            collect_enum_states(node, source, &mut graph);
        }
    });

    graph
}

/// Extract enum variants from an `enum_specifier` node, adding
/// `MacroblockState` nodes and sequential `Transition` edges.
fn collect_enum_states(enum_node: Node<'_>, source: &[u8], graph: &mut KnowledgeGraph) {
    let body = match enum_node.child_by_field_name("body") {
        Some(b) => b,
        None => return,
    };

    let mut prev: Option<u64> = None;
    let mut cursor = body.walk();
    if !cursor.goto_first_child() {
        return;
    }
    loop {
        let node = cursor.node();
        if node.kind() == "enumerator" {
            // The enumerator's name is the first named child (an identifier).
            if let Some(name) = first_identifier_text(node, source) {
                let id = graph.add_node(NodeKind::MacroblockState {
                    name: name.to_string(),
                });
                if let Some(p) = prev {
                    graph.add_edge(p, id, EdgeKind::Transition);
                }
                prev = Some(id);
            }
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::NodeKind;

    fn ast(src: &str) -> CAst {
        CAst::from_source(src).unwrap()
    }

    #[test]
    fn extracts_functions_and_calls() {
        let src = r#"
            int helper(int x) { return x + 1; }
            int decode(int y) { return helper(y); }
        "#;
        let g = extract_bitstream_parsing_tree(&ast(src));

        let funcs: Vec<&str> = g
            .nodes
            .iter()
            .filter_map(|n| match &n.kind {
                NodeKind::Function { name } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert!(funcs.contains(&"helper"));
        assert!(funcs.contains(&"decode"));

        let has_call = g.edges.iter().any(|e| matches!(e.kind, EdgeKind::Calls));
        assert!(has_call, "expected a Calls edge from decode to helper");
    }

    #[test]
    fn extracts_switch_cases() {
        let src = r#"
            void parse(int nal) {
                switch (nal) {
                    case 1: break;
                    case 5: break;
                    case 7: break;
                }
            }
        "#;
        let g = extract_bitstream_parsing_tree(&ast(src));
        let cases: Vec<&str> = g
            .nodes
            .iter()
            .filter_map(|n| match &n.kind {
                NodeKind::SwitchCase { value } => Some(value.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(cases.len(), 3);
        assert!(cases.contains(&"1"));
        assert!(cases.contains(&"5"));
        assert!(cases.contains(&"7"));
    }

    #[test]
    fn extracts_loop_bodies() {
        let src = r#"
            void run(void) {
                for (int i = 0; i < 10; i++) {}
                int j = 0;
                while (j < 5) { j++; }
            }
        "#;
        let g = extract_bitstream_parsing_tree(&ast(src));
        let loops = g
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, NodeKind::LoopBody { .. }))
            .count();
        assert_eq!(loops, 2);
    }

    #[test]
    fn extracts_enum_states() {
        let src = r#"
            enum MbState { MB_INTRA, MB_INTER, MB_SKIP };
        "#;
        let g = extract_macroblock_state_machine(&ast(src));
        let states: Vec<&str> = g
            .nodes
            .iter()
            .filter_map(|n| match &n.kind {
                NodeKind::MacroblockState { name } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(states, vec!["MB_INTRA", "MB_INTER", "MB_SKIP"]);

        let transitions = g
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Transition))
            .count();
        assert_eq!(transitions, 2);
    }

    #[test]
    fn pointer_return_function_name_resolved() {
        let src = "int *get_buffer(int size) { return 0; }";
        let g = extract_bitstream_parsing_tree(&ast(src));
        let found = g
            .nodes
            .iter()
            .any(|n| matches!(&n.kind, NodeKind::Function { name } if name == "get_buffer"));
        assert!(found, "pointer-returning function name should resolve");
    }
}
