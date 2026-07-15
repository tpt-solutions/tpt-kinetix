//! Knowledge graph schema and construction.
//!
//! Nodes: parsing states, syntax elements, macroblock states.
//! Edges: transitions, data dependencies.
//!
//! TODO (Phase 1): Design and implement the full graph schema.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NodeKind {
    ParsingState(String),
    SyntaxElement(String),
    MacroblockState(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: u64,
    pub kind: NodeKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EdgeKind {
    Transition,
    DataDependency,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub from: u64,
    pub to: u64,
    pub kind: EdgeKind,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct KnowledgeGraph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

impl KnowledgeGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_node(&mut self, kind: NodeKind) -> u64 {
        let id = self.nodes.len() as u64;
        self.nodes.push(Node { id, kind });
        id
    }

    pub fn add_edge(&mut self, from: u64, to: u64, kind: EdgeKind) {
        self.edges.push(Edge { from, to, kind });
    }
}
