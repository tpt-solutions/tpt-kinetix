//! End-to-end integration test for the `tpt-kinetix-kg` Phase 1 pipeline.
//!
//! Validates the full ingest → graph → analyze → codegen flow against a
//! synthetic, FFmpeg-shaped H.264 decoder fixture. This is the Phase 1
//! proof-of-concept target: run the tooling against realistic decoder C source
//! and confirm every stage produces sensible output.

use std::path::PathBuf;

use tpt_kinetix_kg::{
    analysis::{find_independent_sets, mark_parallel_regions},
    codegen::{generate, CodegenOptions},
    extraction::{extract_bitstream_parsing_tree, extract_macroblock_state_machine},
    graph::{Edge, KnowledgeGraph, Node, NodeKind},
    ingestion::CAst,
};

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("h264dec_sample.c")
}

/// Mirror of `main::build_combined_graph`: merge the two extraction passes.
fn build_combined_graph(ast: &CAst) -> KnowledgeGraph {
    let mut graph = extract_bitstream_parsing_tree(ast);
    let mb_graph = extract_macroblock_state_machine(ast);

    let offset = graph.nodes.len() as u64;
    for node in mb_graph.nodes {
        graph.nodes.push(Node {
            id: node.id + offset,
            kind: node.kind,
        });
    }
    for edge in mb_graph.edges {
        graph.edges.push(Edge {
            from: edge.from + offset,
            to: edge.to + offset,
            kind: edge.kind,
        });
    }
    graph
}

#[test]
fn ingests_h264_fixture_into_graph() {
    let ast = CAst::from_file(fixture_path()).expect("fixture parses");
    let graph = build_combined_graph(&ast);

    // Every decode function in the fixture should be discovered.
    let func_names: Vec<String> = graph
        .nodes
        .iter()
        .filter_map(|n| match &n.kind {
            NodeKind::Function { name } => Some(name.clone()),
            _ => None,
        })
        .collect();

    for expected in [
        "decode_sps",
        "decode_pps",
        "decode_slice_header",
        "decode_macroblock",
        "decode_slice_data",
        "decode_slice",
        "h264_decode_nal",
        "h264_decode_frame",
    ] {
        assert!(
            func_names.iter().any(|n| n == expected),
            "expected function `{expected}` in graph, got {func_names:?}"
        );
    }
}

#[test]
fn extracts_nal_switch_cases() {
    let ast = CAst::from_file(fixture_path()).unwrap();
    let graph = build_combined_graph(&ast);

    let cases = graph
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::SwitchCase { .. }))
        .count();
    // NAL_SPS, NAL_PPS, NAL_IDR_SLICE, NAL_SLICE, NAL_SEI = 5 case arms.
    assert!(
        cases >= 5,
        "expected at least 5 NAL switch cases, found {cases}"
    );
}

#[test]
fn extracts_macroblock_state_enum() {
    let ast = CAst::from_file(fixture_path()).unwrap();
    let graph = build_combined_graph(&ast);

    let states: Vec<String> = graph
        .nodes
        .iter()
        .filter_map(|n| match &n.kind {
            NodeKind::MacroblockState { name } => Some(name.clone()),
            _ => None,
        })
        .collect();

    for expected in ["MB_TYPE_INTRA_4x4", "MB_TYPE_INTRA_16x16", "MB_TYPE_SKIP"] {
        assert!(
            states.iter().any(|s| s == expected),
            "expected macroblock state `{expected}`, got {states:?}"
        );
    }
}

#[test]
fn discovers_call_edges() {
    let ast = CAst::from_file(fixture_path()).unwrap();
    let graph = build_combined_graph(&ast);

    // Find the ids for h264_decode_nal and decode_slice.
    let id_of = |name: &str| -> u64 {
        graph
            .nodes
            .iter()
            .find(|n| matches!(&n.kind, NodeKind::Function { name: n } if n == name))
            .unwrap_or_else(|| panic!("function {name} not found"))
            .id
    };

    let nal = id_of("h264_decode_nal");
    // h264_decode_nal calls decode_sps/decode_pps/decode_slice — it should have
    // outgoing Calls edges.
    let out_calls = graph
        .edges
        .iter()
        .filter(|e| e.from == nal && matches!(e.kind, tpt_kinetix_kg::graph::EdgeKind::Calls))
        .count();
    assert!(
        out_calls >= 1,
        "h264_decode_nal should call at least one decode function"
    );
}

#[test]
fn end_to_end_generates_scaffold() {
    let ast = CAst::from_file(fixture_path()).unwrap();
    let mut graph = build_combined_graph(&ast);

    let sets = find_independent_sets(&graph);
    assert!(!sets.is_empty(), "analysis should produce independent sets");

    mark_parallel_regions(&mut graph, &sets);
    let sets = find_independent_sets(&graph);

    let opts = CodegenOptions {
        crate_name: "tpt-kinetix-h264".into(),
        inject_rayon: true,
    };
    let files = generate(&graph, &sets, &opts).expect("codegen succeeds");

    // Core outputs must be present.
    assert!(files.contains_key("src/generated/functions.rs"));
    assert!(files.contains_key("src/generated/mb_states.rs"));
    assert!(files.contains_key("src/generated/mod.rs"));

    let funcs = &files["src/generated/functions.rs"];
    assert!(funcs.contains("pub fn h264_decode_frame"));
    assert!(funcs.contains("pub fn decode_macroblock"));

    let states = &files["src/generated/mb_states.rs"];
    assert!(states.contains("pub enum MacroblockState"));
    assert!(states.contains("MB_TYPE_INTRA_4x4"));
}
