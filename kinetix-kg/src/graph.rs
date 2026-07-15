//! Knowledge graph schema: nodes, edges, and the graph container.
//!
//! Nodes represent parsing states, macroblock states, syntax elements, and
//! parallel regions discovered from the codec source.  Edges represent
//! control-flow transitions, data dependencies, function calls, and
//! parallelism opportunities.

use serde::{Deserialize, Serialize};

/// The semantic kind attached to a graph node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NodeKind {
    /// A C function definition extracted from the source.
    Function { name: String },
    /// A `case` arm in a `switch` statement (represents a bitstream state).
    SwitchCase { value: String },
    /// A loop body that processes data elements independently.
    LoopBody { label: String },
    /// A named macroblock processing state (from an enum or naming convention).
    MacroblockState { name: String },
    /// A parsed bitstream syntax element, optionally with a known bit-width.
    SyntaxElement { name: String, bits: Option<u32> },
    /// A group of operations confirmed safe to parallelise.
    ParallelRegion { description: String },
}

/// A node in the knowledge graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: u64,
    pub kind: NodeKind,
}

/// The semantic kind attached to a graph edge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EdgeKind {
    /// Function A calls function B.
    Calls,
    /// State A transitions to state B (control flow).
    Transition,
    /// Node B reads data produced by node A.
    DataDependency,
    /// Nodes A and B have no data dependencies and may run in parallel.
    Independent,
}

/// A directed edge in the knowledge graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub from: u64,
    pub to: u64,
    pub kind: EdgeKind,
}

/// The full knowledge graph: a set of typed nodes connected by typed edges.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct KnowledgeGraph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

impl KnowledgeGraph {
    /// Create an empty graph.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a node and return its assigned id.
    pub fn add_node(&mut self, kind: NodeKind) -> u64 {
        let id = self.nodes.len() as u64;
        self.nodes.push(Node { id, kind });
        id
    }

    /// Add a directed edge between two existing node ids.
    pub fn add_edge(&mut self, from: u64, to: u64, kind: EdgeKind) {
        self.edges.push(Edge { from, to, kind });
    }

    /// Return all nodes whose kind satisfies `kind_filter`.
    pub fn nodes_by_kind(&self, kind_filter: impl Fn(&NodeKind) -> bool) -> Vec<&Node> {
        self.nodes.iter().filter(|n| kind_filter(&n.kind)).collect()
    }

    /// Return the ids of all nodes that have an edge pointing *to* `node_id`.
    pub fn predecessors(&self, node_id: u64) -> Vec<u64> {
        self.edges
            .iter()
            .filter(|e| e.to == node_id)
            .map(|e| e.from)
            .collect()
    }

    /// Return the ids of all nodes that `node_id` has an edge pointing *to*.
    pub fn successors(&self, node_id: u64) -> Vec<u64> {
        self.edges
            .iter()
            .filter(|e| e.from == node_id)
            .map(|e| e.to)
            .collect()
    }

    /// Serialise the graph to pretty-printed JSON.
    pub fn to_json(&self) -> anyhow::Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// Deserialise a graph from JSON produced by [`Self::to_json`].
    pub fn from_json(s: &str) -> anyhow::Result<Self> {
        Ok(serde_json::from_str(s)?)
    }

    /// Return a human-readable summary of graph statistics.
    pub fn stats(&self) -> String {
        let functions = self
            .nodes_by_kind(|k| matches!(k, NodeKind::Function { .. }))
            .len();
        let mb_states = self
            .nodes_by_kind(|k| matches!(k, NodeKind::MacroblockState { .. }))
            .len();
        let switch_cases = self
            .nodes_by_kind(|k| matches!(k, NodeKind::SwitchCase { .. }))
            .len();
        let loops = self
            .nodes_by_kind(|k| matches!(k, NodeKind::LoopBody { .. }))
            .len();
        let parallel = self
            .nodes_by_kind(|k| matches!(k, NodeKind::ParallelRegion { .. }))
            .len();
        format!(
            "nodes={} edges={} (functions={} mb_states={} switch_cases={} loops={} parallel_regions={})",
            self.nodes.len(),
            self.edges.len(),
            functions,
            mb_states,
            switch_cases,
            loops,
            parallel,
        )
    }
}
