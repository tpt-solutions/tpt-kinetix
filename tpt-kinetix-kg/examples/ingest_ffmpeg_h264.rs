//! Ingest a C source file (e.g. FFmpeg's `h264dec.c`) into a knowledge graph,
//! run dependency analysis, and print the independent decode units found.
//!
//! This is the library-level equivalent of `tpt-kinetix-kg run <file>` and
//! shows how to drive the ingest → graph → analyze steps programmatically
//! instead of via the CLI.
//!
//! Run with:
//!
//! ```sh
//! cargo run -p tpt-kinetix-kg --example ingest_ffmpeg_h264 -- path/to/h264dec.c
//! ```

use tpt_kinetix_kg::{
    analysis::find_independent_sets,
    extraction::{extract_bitstream_parsing_tree, extract_macroblock_state_machine},
    graph::{Edge, Node},
    ingestion::CAst,
};

fn main() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: ingest_ffmpeg_h264 <path/to/codec.c>"))?;

    let ast = CAst::from_file(&path)?;

    // Run both extraction passes and merge them into one graph, offsetting the
    // macroblock-state-machine node ids so they don't collide with the
    // bitstream-parsing-tree ids.
    let mut graph = extract_bitstream_parsing_tree(&ast);
    let mb_graph = extract_macroblock_state_machine(&ast);
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

    println!("Ingested: {path}");
    println!("Graph:    {}", graph.stats());

    let sets = find_independent_sets(&graph);
    println!("Found {} independent set(s):", sets.len());
    for (i, set) in sets.iter().enumerate() {
        println!(
            "  Set #{i}: {} node(s) — {}",
            set.node_ids.len(),
            set.description
        );
    }

    Ok(())
}
