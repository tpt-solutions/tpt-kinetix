//! Dependency analysis: find independent decode units within the knowledge graph.
//!
//! Uses Kahn's topological-sort algorithm over `DataDependency` edges.
//! Nodes at the same "level" (same BFS depth from sources) share no data
//! dependencies and form an [`IndependentSet`].

use std::collections::{HashMap, HashSet, VecDeque};

use crate::graph::{EdgeKind, KnowledgeGraph, NodeKind};

/// A set of graph nodes that can be processed concurrently because they share
/// no `DataDependency` edges between them.
#[derive(Debug, Clone)]
pub struct IndependentSet {
    /// Ids of the nodes in this set.
    pub node_ids: Vec<u64>,
    /// Human-readable description of why these nodes are independent.
    pub description: String,
}

/// Analyse `graph` and return groups of nodes that are safe to process in
/// parallel.
///
/// The algorithm:
/// 1. Build an adjacency map restricted to [`EdgeKind::DataDependency`] edges.
/// 2. Compute in-degrees for every node.
/// 3. Seed a BFS queue with all nodes whose in-degree is 0.
/// 4. Drain the queue level-by-level; each level becomes one
///    [`IndependentSet`].
pub fn find_independent_sets(graph: &KnowledgeGraph) -> Vec<IndependentSet> {
    if graph.nodes.is_empty() {
        return vec![];
    }

    // ── build in-degree map using only DataDependency edges ─────────────────
    let node_ids: Vec<u64> = graph.nodes.iter().map(|n| n.id).collect();
    let mut in_degree: HashMap<u64, usize> = node_ids.iter().map(|&id| (id, 0usize)).collect();
    let mut successors: HashMap<u64, Vec<u64>> = node_ids.iter().map(|&id| (id, vec![])).collect();

    for edge in &graph.edges {
        if matches!(edge.kind, EdgeKind::DataDependency) {
            *in_degree.entry(edge.to).or_insert(0) += 1;
            successors.entry(edge.from).or_default().push(edge.to);
        }
    }

    // ── Kahn's BFS ──────────────────────────────────────────────────────────
    let mut queue: VecDeque<u64> = in_degree
        .iter()
        .filter(|(_, &deg)| deg == 0)
        .map(|(&id, _)| id)
        .collect();

    // Sort for deterministic output.
    let mut initial: Vec<u64> = queue.drain(..).collect();
    initial.sort_unstable();
    queue.extend(initial);

    let mut sets: Vec<IndependentSet> = Vec::new();
    let mut processed: HashSet<u64> = HashSet::new();

    while !queue.is_empty() {
        // Collect everything currently in the queue as one independent level.
        let level: Vec<u64> = queue.drain(..).collect();
        if level.is_empty() {
            break;
        }

        let description = format!(
            "Level {} — {} node(s) with no unresolved data dependencies",
            sets.len(),
            level.len()
        );
        sets.push(IndependentSet {
            node_ids: level.clone(),
            description,
        });

        // Decrement in-degrees for all successors.
        let mut next: Vec<u64> = Vec::new();
        for id in &level {
            processed.insert(*id);
            if let Some(succs) = successors.get(id) {
                for &s in succs {
                    let deg = in_degree.entry(s).or_insert(0);
                    *deg = deg.saturating_sub(1);
                    if *deg == 0 && !processed.contains(&s) {
                        next.push(s);
                    }
                }
            }
        }
        next.sort_unstable();
        next.dedup();
        queue.extend(next);
    }

    sets
}

/// Add [`EdgeKind::Independent`] edges between every pair of nodes in the same
/// [`IndependentSet`], and wrap each set that contains more than one node in a
/// `ParallelRegion` meta-node.
pub fn mark_parallel_regions(graph: &mut KnowledgeGraph, sets: &[IndependentSet]) {
    for (idx, set) in sets.iter().enumerate() {
        if set.node_ids.len() < 2 {
            continue;
        }

        // Add Independent edges between all pairs.
        let ids = &set.node_ids;
        for i in 0..ids.len() {
            for j in (i + 1)..ids.len() {
                graph.add_edge(ids[i], ids[j], EdgeKind::Independent);
                graph.add_edge(ids[j], ids[i], EdgeKind::Independent);
            }
        }

        // Add a ParallelRegion meta-node and connect it to each member.
        let region_id = graph.add_node(NodeKind::ParallelRegion {
            description: format!("Parallel region #{idx} — {} nodes", set.node_ids.len()),
        });
        for &member_id in ids {
            graph.add_edge(region_id, member_id, EdgeKind::Independent);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::NodeKind;

    fn func_node(g: &mut KnowledgeGraph, name: &str) -> u64 {
        g.add_node(NodeKind::Function {
            name: name.to_string(),
        })
    }

    #[test]
    fn empty_graph_has_no_sets() {
        let g = KnowledgeGraph::new();
        assert!(find_independent_sets(&g).is_empty());
    }

    #[test]
    fn nodes_without_dependencies_form_single_level() {
        let mut g = KnowledgeGraph::new();
        func_node(&mut g, "a");
        func_node(&mut g, "b");
        func_node(&mut g, "c");
        let sets = find_independent_sets(&g);
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].node_ids, vec![0, 1, 2]);
    }

    #[test]
    fn data_dependencies_create_levels() {
        let mut g = KnowledgeGraph::new();
        let a = func_node(&mut g, "a");
        let b = func_node(&mut g, "b");
        let c = func_node(&mut g, "c");
        g.add_edge(a, b, EdgeKind::DataDependency);
        g.add_edge(b, c, EdgeKind::DataDependency);
        let sets = find_independent_sets(&g);
        assert_eq!(sets.len(), 3);
        assert_eq!(sets[0].node_ids, vec![a]);
        assert_eq!(sets[1].node_ids, vec![b]);
        assert_eq!(sets[2].node_ids, vec![c]);
    }

    #[test]
    fn diamond_dependency_groups_middle_level() {
        let mut g = KnowledgeGraph::new();
        let a = func_node(&mut g, "a");
        let b = func_node(&mut g, "b");
        let c = func_node(&mut g, "c");
        let d = func_node(&mut g, "d");
        g.add_edge(a, b, EdgeKind::DataDependency);
        g.add_edge(a, c, EdgeKind::DataDependency);
        g.add_edge(b, d, EdgeKind::DataDependency);
        g.add_edge(c, d, EdgeKind::DataDependency);
        let sets = find_independent_sets(&g);
        assert_eq!(sets.len(), 3);
        assert_eq!(sets[0].node_ids, vec![a]);
        assert_eq!(sets[1].node_ids, vec![b, c]);
        assert_eq!(sets[2].node_ids, vec![d]);
    }

    #[test]
    fn non_data_edges_do_not_constrain() {
        let mut g = KnowledgeGraph::new();
        let a = func_node(&mut g, "a");
        let b = func_node(&mut g, "b");
        g.add_edge(a, b, EdgeKind::Calls);
        let sets = find_independent_sets(&g);
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].node_ids, vec![a, b]);
    }

    #[test]
    fn mark_parallel_regions_adds_region_and_independent_edges() {
        let mut g = KnowledgeGraph::new();
        func_node(&mut g, "a");
        func_node(&mut g, "b");
        let sets = find_independent_sets(&g);
        assert_eq!(sets[0].node_ids.len(), 2);

        mark_parallel_regions(&mut g, &sets);

        let regions = g
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, NodeKind::ParallelRegion { .. }))
            .count();
        assert_eq!(regions, 1);

        let independent_edges = g
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Independent))
            .count();
        // 2 pairwise (a<->b) + 2 region->member = 4.
        assert_eq!(independent_edges, 4);
    }

    #[test]
    fn mark_parallel_regions_skips_singletons() {
        let mut g = KnowledgeGraph::new();
        let a = func_node(&mut g, "a");
        let b = func_node(&mut g, "b");
        g.add_edge(a, b, EdgeKind::DataDependency);
        let sets = find_independent_sets(&g);
        mark_parallel_regions(&mut g, &sets);
        let regions = g
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, NodeKind::ParallelRegion { .. }))
            .count();
        assert_eq!(regions, 0);
    }
}
