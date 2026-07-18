//! `tpt-kinetix-kg` — knowledge-graph ingestion and Rust codegen CLI.
//!
//! # Subcommands
//!
//! | Command | Description |
//! |---------|-------------|
//! | `ingest`  | Parse a C source file and print graph statistics |
//! | `graph`   | Build the full knowledge graph and write it as JSON |
//! | `analyze` | Run dependency analysis on a graph JSON file |
//! | `codegen` | Generate Rust scaffold from a graph JSON file |
//! | `run`     | End-to-end: ingest → graph → analyze → codegen |

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use tpt_kinetix_kg::{
    analysis::{find_independent_sets, mark_parallel_regions},
    codegen::{generate, CodegenOptions},
    extraction::{extract_bitstream_parsing_tree, extract_macroblock_state_machine},
    graph::KnowledgeGraph,
    ingestion::CAst,
};

// ── CLI definition ────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "tpt-kinetix-kg",
    about = "Knowledge-graph ingestion, analysis, and Rust codegen for codec source",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Parse a C source file and print graph statistics.
    Ingest {
        /// Path to the C source file.
        c_source_path: PathBuf,
    },

    /// Build the full knowledge graph and write it as JSON.
    Graph {
        /// Path to the C source file.
        c_source_path: PathBuf,

        /// Output JSON file (default: stdout).
        #[arg(short, long)]
        output: Option<PathBuf>,
    },

    /// Run dependency analysis on a graph JSON file and print independent sets.
    Analyze {
        /// Path to a graph JSON file produced by `tpt-kinetix-kg graph`.
        graph_json: PathBuf,
    },

    /// Generate Rust scaffold from a graph JSON file.
    Codegen {
        /// Path to a graph JSON file produced by `tpt-kinetix-kg graph`.
        graph_json: PathBuf,

        /// Crate name for the emitted scaffold.
        #[arg(long, default_value = "tpt-kinetix-codec")]
        crate_name: String,

        /// Inject rayon parallel iterators at independence points.
        #[arg(long)]
        inject_rayon: bool,

        /// Directory to write generated files into.
        #[arg(long, default_value = "generated")]
        output_dir: PathBuf,
    },

    /// End-to-end pipeline: ingest → graph → analyze → codegen.
    Run {
        /// Path to the C source file.
        c_source_path: PathBuf,

        /// Crate name for the emitted scaffold.
        #[arg(long, default_value = "tpt-kinetix-codec")]
        crate_name: String,

        /// Directory to write generated files into.
        #[arg(long, default_value = "generated")]
        output_dir: PathBuf,

        /// Inject rayon parallel iterators at independence points.
        #[arg(long)]
        inject_rayon: bool,
    },
}

// ── entry point ───────────────────────────────────────────────────────────────

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Ingest { c_source_path } => cmd_ingest(&c_source_path),
        Commands::Graph {
            c_source_path,
            output,
        } => cmd_graph(&c_source_path, output.as_deref()),
        Commands::Analyze { graph_json } => cmd_analyze(&graph_json),
        Commands::Codegen {
            graph_json,
            crate_name,
            inject_rayon,
            output_dir,
        } => cmd_codegen(&graph_json, &crate_name, inject_rayon, &output_dir),
        Commands::Run {
            c_source_path,
            crate_name,
            output_dir,
            inject_rayon,
        } => cmd_run(&c_source_path, &crate_name, inject_rayon, &output_dir),
    }
}

// ── subcommand implementations ────────────────────────────────────────────────

fn cmd_ingest(path: &std::path::Path) -> anyhow::Result<()> {
    let ast = CAst::from_file(path)?;
    let graph = build_combined_graph(&ast);
    println!("Ingested: {}", path.display());
    println!("Graph:    {}", graph.stats());
    Ok(())
}

fn cmd_graph(path: &std::path::Path, output: Option<&std::path::Path>) -> anyhow::Result<()> {
    let ast = CAst::from_file(path)?;
    let graph = build_combined_graph(&ast);
    let json = graph.to_json()?;

    match output {
        Some(out_path) => {
            std::fs::write(out_path, &json)?;
            println!("Graph written to {}", out_path.display());
            println!("{}", graph.stats());
        }
        None => {
            println!("{json}");
        }
    }
    Ok(())
}

fn cmd_analyze(graph_json_path: &std::path::Path) -> anyhow::Result<()> {
    let json = std::fs::read_to_string(graph_json_path)?;
    let graph = KnowledgeGraph::from_json(&json)?;
    let sets = find_independent_sets(&graph);

    println!("Graph: {}", graph.stats());
    println!("Found {} independent set(s):", sets.len());
    for (i, set) in sets.iter().enumerate() {
        println!(
            "  Set #{i}: {} node(s) — {}",
            set.node_ids.len(),
            set.description
        );
        for &id in &set.node_ids {
            if let Some(node) = graph.nodes.iter().find(|n| n.id == id) {
                println!("    [{id}] {:?}", node.kind);
            }
        }
    }
    Ok(())
}

fn cmd_codegen(
    graph_json_path: &std::path::Path,
    crate_name: &str,
    inject_rayon: bool,
    output_dir: &std::path::Path,
) -> anyhow::Result<()> {
    let json = std::fs::read_to_string(graph_json_path)?;
    let graph = KnowledgeGraph::from_json(&json)?;
    let sets = find_independent_sets(&graph);

    let opts = CodegenOptions {
        crate_name: crate_name.to_string(),
        inject_rayon,
    };

    let files = generate(&graph, &sets, &opts)?;
    write_generated_files(&files, output_dir)?;

    println!(
        "Generated {} file(s) under {}",
        files.len(),
        output_dir.display()
    );
    for path in files.keys() {
        println!("  {path}");
    }
    Ok(())
}

fn cmd_run(
    path: &std::path::Path,
    crate_name: &str,
    inject_rayon: bool,
    output_dir: &std::path::Path,
) -> anyhow::Result<()> {
    println!("==> Ingesting {}", path.display());
    let ast = CAst::from_file(path)?;

    println!("==> Building knowledge graph");
    let mut graph = build_combined_graph(&ast);
    println!("    {}", graph.stats());

    println!("==> Dependency analysis");
    let sets = find_independent_sets(&graph);
    println!("    {} independent set(s) found", sets.len());

    println!("==> Marking parallel regions");
    mark_parallel_regions(&mut graph, &sets);

    println!("==> Code generation");
    let opts = CodegenOptions {
        crate_name: crate_name.to_string(),
        inject_rayon,
    };
    let files = generate(&graph, &sets, &opts)?;
    write_generated_files(&files, output_dir)?;

    println!(
        "    {} file(s) written to {}",
        files.len(),
        output_dir.display()
    );
    Ok(())
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Run both extraction passes and merge the resulting graphs.
fn build_combined_graph(ast: &CAst) -> KnowledgeGraph {
    let mut graph = extract_bitstream_parsing_tree(ast);
    let mb_graph = extract_macroblock_state_machine(ast);

    // Merge macroblock state nodes/edges into the main graph.
    // Node ids in mb_graph must be offset to avoid collisions.
    let offset = graph.nodes.len() as u64;
    for node in mb_graph.nodes {
        graph.nodes.push(tpt_kinetix_kg::graph::Node {
            id: node.id + offset,
            kind: node.kind,
        });
    }
    for edge in mb_graph.edges {
        graph.edges.push(tpt_kinetix_kg::graph::Edge {
            from: edge.from + offset,
            to: edge.to + offset,
            kind: edge.kind,
        });
    }

    graph
}

fn write_generated_files(
    files: &std::collections::HashMap<String, String>,
    output_dir: &std::path::Path,
) -> anyhow::Result<()> {
    for (rel_path, content) in files {
        let abs_path = output_dir.join(rel_path);
        if let Some(parent) = abs_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&abs_path, content)?;
    }
    Ok(())
}
