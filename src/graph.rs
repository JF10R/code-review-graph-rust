//! Petgraph-backed graph store with postcard+zstd persistence.
//!
//! The entire graph lives in memory as a `StableGraph`. Persistence is a
//! single atomic file write: `graph.bin.zst` (zstd-compressed postcard with a
//! 4-byte magic header and a CRC-32 integrity check).
//!
//! SQLite is no longer used here — see `embeddings.rs` for the embeddings DB.

use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};
use std::path::Path;

use petgraph::stable_graph::StableGraph;
use petgraph::visit::{EdgeRef, IntoEdgeReferences};
use petgraph::Direction;
use serde::{Deserialize, Serialize};

pub use petgraph::stable_graph::NodeIndex;

use crate::error::Result;
use crate::persistence;
use crate::types::{
    EdgeInfo, EdgeKind, GraphEdge, GraphNode, GraphStats, ImpactResult, NodeInfo, NodeKind,
};

// ---------------------------------------------------------------------------
// Serializable graph data
// ---------------------------------------------------------------------------

/// All graph state that gets serialized to disk.
#[derive(Serialize, Deserialize)]
pub struct GraphData {
    graph: StableGraph<GraphNode, EdgeKind>,
    /// qualified_name → NodeIndex
    node_index: HashMap<String, NodeIndex>,
    /// file_path → [NodeIndex]
    file_index: HashMap<String, Vec<NodeIndex>>,
    metadata: HashMap<String, String>,
    /// file_path → SHA-256 (kept for hash-skip in incremental)
    file_hashes: HashMap<String, String>,
}

impl GraphData {
    fn new() -> Self {
        Self {
            graph: StableGraph::new(),
            node_index: HashMap::new(),
            file_index: HashMap::new(),
            metadata: HashMap::new(),
            file_hashes: HashMap::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Persistence helpers (delegates to crate::persistence)
// ---------------------------------------------------------------------------

fn save(data: &GraphData, path: &Path) -> Result<()> {
    persistence::save_blob(data, path, "graph")
}

fn load(path: &Path) -> Result<GraphData> {
    persistence::load_blob(path, "graph")
}

// ---------------------------------------------------------------------------
// GraphStore — public API
// ---------------------------------------------------------------------------

/// In-memory graph store backed by petgraph, persisted to disk as
/// a zstd-compressed postcard blob.
pub struct GraphStore {
    data: GraphData,
    /// Path to the `.bin.zst` file.
    bin_path: std::path::PathBuf,
}

impl GraphStore {
    /// Open (or create) the graph store.
    ///
    /// `db_path` is the path returned by `incremental::get_db_path()` —
    /// i.e. `<repo>/.code-review-graph/graph.bin.zst`.
    pub fn new(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let data = if db_path.exists() {
            match load(db_path) {
                Ok(d) => d,
                Err(e) => {
                    log::warn!(
                        "Could not load graph from {}: {} — starting empty",
                        db_path.display(),
                        e
                    );
                    GraphData::new()
                }
            }
        } else {
            GraphData::new()
        };

        Ok(Self {
            data,
            bin_path: db_path.to_path_buf(),
        })
    }

    // -- Write operations --

    /// Replace all nodes and edges for a file with the freshly-parsed data.
    ///
    /// Cross-file edges whose target is not yet in the graph are silently
    /// dropped.  For full builds, use [`store_file_nodes_only`] +
    /// [`insert_edges`] (two-phase) to guarantee all cross-file edges resolve.
    pub fn store_file_nodes_edges(
        &mut self,
        file_path: &str,
        nodes: &[NodeInfo],
        edges: &[EdgeInfo],
        file_hash: &str,
    ) -> Result<()> {
        self.store_file_nodes_only(file_path, nodes, file_hash)?;
        self.insert_edges(edges, false);
        Ok(())
    }

    /// Insert only nodes for a file (no edges).  Used as phase 1 of the
    /// two-phase full-build path so that all nodes are in `node_index`
    /// before any edges are resolved.
    pub fn store_file_nodes_only(
        &mut self,
        file_path: &str,
        nodes: &[NodeInfo],
        file_hash: &str,
    ) -> Result<()> {
        self.remove_file_data_inner(file_path);

        let file_hash_owned = file_hash.to_string();
        let file_path_owned = file_path.to_string();
        let mut new_idxs: Vec<NodeIndex> = Vec::with_capacity(nodes.len());
        for node_info in nodes {
            let graph_node = GraphNode {
                kind: node_info.kind,
                name: node_info.name.clone(),
                qualified_name: node_info.qualified_name.clone(),
                file_path: node_info.file_path.clone(),
                line_start: node_info.line_start,
                line_end: node_info.line_end,
                language: node_info.language.clone(),
                is_test: node_info.is_test,
                docstring: node_info.docstring.clone(),
                signature: node_info.signature.clone(),
                body_hash: node_info.body_hash.clone(),
                file_hash: file_hash_owned.clone(),
            };
            let idx = self.data.graph.add_node(graph_node);
            self.data
                .node_index
                .insert(node_info.qualified_name.clone(), idx);
            new_idxs.push(idx);
        }
        self.data.file_index.insert(file_path_owned.clone(), new_idxs);
        self.data.file_hashes.insert(file_path_owned, file_hash_owned);
        Ok(())
    }

    /// Resolve and insert edges into the graph.
    ///
    /// When `skip_dedup` is true, the O(degree) duplicate-edge check is
    /// skipped — safe when the graph was just cleared (full-build phase 2).
    pub fn insert_edges(&mut self, edges: &[EdgeInfo], skip_dedup: bool) {
        for edge_info in edges {
            let src_idx = self.data.node_index.get(&edge_info.source_qualified).copied();
            let tgt_idx = self.data.node_index.get(&edge_info.target_qualified).copied();
            if let (Some(src), Some(tgt)) = (src_idx, tgt_idx) {
                if skip_dedup {
                    self.data.graph.add_edge(src, tgt, edge_info.kind);
                } else {
                    let already_exists = self
                        .data
                        .graph
                        .edges_connecting(src, tgt)
                        .any(|e| *e.weight() == edge_info.kind);
                    if !already_exists {
                        self.data.graph.add_edge(src, tgt, edge_info.kind);
                    }
                }
            }
        }
    }

    /// Remove all nodes and edges associated with a file.
    pub fn remove_file_data(&mut self, file_path: &str) -> Result<()> {
        self.remove_file_data_inner(file_path);
        Ok(())
    }

    /// Persist in-memory state to disk.
    pub fn commit(&self) -> Result<()> {
        save(&self.data, &self.bin_path)
    }

    /// Compact the in-memory graph by saving to disk and reloading.
    ///
    /// StableGraph uses tombstones for removed nodes — they never shrink.
    /// After many incremental updates, the internal Vec accumulates vacant
    /// slots.  A save+reload cycle naturally compacts because postcard only
    /// serializes live data, and deserialization builds a fresh graph.
    pub fn compact(&mut self) -> Result<()> {
        self.commit()?;
        self.data = load(&self.bin_path)?;
        Ok(())
    }

    // -- Read operations --

    /// Get a node by qualified name.
    pub fn get_node(&self, qualified_name: &str) -> Result<Option<GraphNode>> {
        let node = self
            .data
            .node_index
            .get(qualified_name)
            .map(|&idx| self.data.graph[idx].clone());
        Ok(node)
    }

    /// Get all nodes in a file.
    pub fn get_nodes_by_file(&self, file_path: &str) -> Result<Vec<GraphNode>> {
        let nodes = self
            .data
            .file_index
            .get(file_path)
            .map(|idxs| {
                idxs.iter()
                    .map(|&idx| self.data.graph[idx].clone())
                    .collect()
            })
            .unwrap_or_default();
        Ok(nodes)
    }

    /// Get all file paths that have a `File` node.
    pub fn get_all_files(&self) -> Result<Vec<String>> {
        Ok(self.data.file_index.keys().cloned().collect())
    }

    /// Search nodes by name substring (multi-word AND logic, case-insensitive).
    ///
    /// Results are sorted by relevance: exact name match (0) > prefix match (1) > contains (2),
    /// then alphabetically by name for stable ordering within each tier.
    pub fn search_nodes(&self, query: &str, limit: usize) -> Result<Vec<GraphNode>> {
        let words: Vec<String> = query
            .split_whitespace()
            .map(|w| w.to_lowercase())
            .collect();
        if words.is_empty() {
            return Ok(vec![]);
        }

        let query_lower = query.to_lowercase();

        let mut results: Vec<(u8, GraphNode)> = self
            .data
            .node_index
            .iter()
            .filter_map(|(qn, &idx)| {
                let node = &self.data.graph[idx];
                let name_lower = node.name.to_lowercase();
                let qn_lower = qn.to_lowercase();
                if !words
                    .iter()
                    .all(|w| name_lower.contains(w.as_str()) || qn_lower.contains(w.as_str()))
                {
                    return None;
                }
                let relevance = if name_lower == query_lower || qn_lower == query_lower {
                    0u8
                } else if name_lower.starts_with(&query_lower) {
                    1u8
                } else {
                    2u8
                };
                Some((relevance, node.clone()))
            })
            .collect();

        results.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.name.cmp(&b.1.name)));
        results.truncate(limit);
        Ok(results.into_iter().map(|(_, n)| n).collect())
    }

    /// Get nodes exceeding a line count threshold, ordered by size descending.
    pub fn get_nodes_by_size(
        &self,
        min_lines: usize,
        kind: Option<&str>,
        file_path_pattern: Option<&str>,
        limit: usize,
    ) -> Result<Vec<GraphNode>> {
        let pattern_lower = file_path_pattern.map(|p| p.to_lowercase());
        let kind_filter = kind.and_then(|k| k.parse::<NodeKind>().ok());

        let mut results: Vec<GraphNode> = self
            .data
            .node_index
            .values()
            .map(|&idx| &self.data.graph[idx])
            .filter(|node| {
                let lines = node.line_end.saturating_sub(node.line_start) + 1;
                if lines < min_lines {
                    return false;
                }
                if let Some(kf) = kind_filter {
                    if node.kind != kf {
                        return false;
                    }
                }
                if let Some(ref pat) = pattern_lower {
                    if !node.file_path.to_lowercase().contains(pat.as_str()) {
                        return false;
                    }
                }
                true
            })
            .cloned()
            .collect();

        results.sort_by(|a, b| {
            let size_a = a.line_end.saturating_sub(a.line_start);
            let size_b = b.line_end.saturating_sub(b.line_start);
            size_b.cmp(&size_a)
        });
        results.truncate(limit);
        Ok(results)
    }

    // -- Edge operations --

    /// Get edges originating from a qualified name.
    pub fn get_edges_by_source(&self, qualified_name: &str) -> Result<Vec<GraphEdge>> {
        let edges = match self.data.node_index.get(qualified_name) {
            None => vec![],
            Some(&idx) => self
                .data
                .graph
                .edges_directed(idx, Direction::Outgoing)
                .map(|e| GraphEdge {
                    kind: *e.weight(),
                    source_qualified: self.data.graph[e.source()].qualified_name.clone(),
                    target_qualified: self.data.graph[e.target()].qualified_name.clone(),
                    file_path: self.data.graph[e.source()].file_path.clone(),
                    line: 0,
                })
                .collect(),
        };
        Ok(edges)
    }

    /// Get edges targeting a qualified name.
    pub fn get_edges_by_target(&self, qualified_name: &str) -> Result<Vec<GraphEdge>> {
        let edges = match self.data.node_index.get(qualified_name) {
            None => vec![],
            Some(&idx) => self
                .data
                .graph
                .edges_directed(idx, Direction::Incoming)
                .map(|e| GraphEdge {
                    kind: *e.weight(),
                    source_qualified: self.data.graph[e.source()].qualified_name.clone(),
                    target_qualified: self.data.graph[e.target()].qualified_name.clone(),
                    file_path: self.data.graph[e.source()].file_path.clone(),
                    line: 0,
                })
                .collect(),
        };
        Ok(edges)
    }

    /// Search edges where target_qualified equals `name` and kind is CALLS.
    pub fn search_edges_by_target_name(&self, name: &str) -> Result<Vec<GraphEdge>> {
        // Find any node whose qualified_name ends with or equals `name`
        let edges: Vec<GraphEdge> = self
            .data
            .node_index
            .iter()
            .filter(|(qn, _)| qn.as_str() == name || qn.ends_with(&format!("::{}", name)))
            .flat_map(|(_, &tgt_idx)| {
                self.data
                    .graph
                    .edges_directed(tgt_idx, Direction::Incoming)
                    .filter(|e| *e.weight() == EdgeKind::Calls)
                    .map(|e| GraphEdge {
                        kind: EdgeKind::Calls,
                        source_qualified: self.data.graph[e.source()].qualified_name.clone(),
                        target_qualified: self.data.graph[e.target()].qualified_name.clone(),
                        file_path: self.data.graph[e.source()].file_path.clone(),
                        line: 0,
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        Ok(edges)
    }

    // -- Impact analysis --

    /// Get body_hashes for all nodes in a file.
    /// Returns a map of qualified_name → body_hash.
    pub fn get_body_hashes(&self, file_path: &str) -> HashMap<String, String> {
        self.data
            .file_index
            .get(file_path)
            .map(|idxs| {
                idxs.iter()
                    .map(|&idx| {
                        let node = &self.data.graph[idx];
                        (node.qualified_name.clone(), node.body_hash.clone())
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Compute the blast radius of changed files using Personalized PageRank.
    ///
    /// Teleportation is biased exclusively toward seed nodes, so only nodes
    /// reachable from the seeds accumulate meaningful scores — exactly the
    /// "blast radius" semantic needed for change impact analysis.
    ///
    /// `max_depth` is accepted for API compatibility but is not used by PPR.
    ///
    /// If `changed_nodes` is provided, those qualified names are used as seeds
    /// directly (node-level diff seeding). Otherwise seeds are all nodes in
    /// `changed_files`.
    pub fn get_impact_radius(
        &self,
        changed_files: &[String],
        _max_depth: usize,
        max_nodes: usize,
        changed_nodes: Option<&[String]>,
    ) -> Result<ImpactResult> {
        let mut seeds: HashSet<String> = HashSet::new();
        if let Some(specific_nodes) = changed_nodes {
            for qn in specific_nodes {
                seeds.insert(qn.clone());
            }
        }
        // Fall back to file-level seeding when no specific nodes were provided
        // (or when all provided names are stale and not found in the graph)
        if seeds.is_empty() {
            for f in changed_files {
                for node in self.get_nodes_by_file(f)? {
                    seeds.insert(node.qualified_name.clone());
                }
            }
        }

        let changed_nodes_vec: Vec<GraphNode> = seeds
            .iter()
            .filter_map(|qn| self.get_node(qn).ok().flatten())
            .collect();

        let ranked = pagerank_impact(&seeds, &self.data, max_nodes);
        let algorithm = "personalized_pagerank".to_string();

        let mut impact_scores: HashMap<String, f64> = HashMap::new();
        let mut impacted_nodes: Vec<GraphNode> = Vec::new();

        for (qn, score) in &ranked {
            if seeds.contains(qn) {
                continue;
            }
            impact_scores.insert(qn.clone(), *score);
            if let Some(node) = self.get_node(qn).ok().flatten() {
                impacted_nodes.push(node);
            }
        }

        let total_impacted = impacted_nodes.len();
        let truncated = total_impacted > max_nodes;
        if truncated {
            impacted_nodes.truncate(max_nodes);
        }

        let impacted_files: Vec<String> = impacted_nodes
            .iter()
            .map(|n| n.file_path.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();

        let all_qns: HashSet<String> = seeds
            .iter()
            .cloned()
            .chain(impacted_nodes.iter().map(|n| n.qualified_name.clone()))
            .collect();
        let edges = self.get_edges_among(&all_qns);

        Ok(ImpactResult {
            changed_nodes: changed_nodes_vec,
            impacted_nodes,
            impact_scores,
            impacted_files,
            edges,
            truncated,
            total_impacted,
            algorithm,
        })
    }

    // -- Metadata --

    /// Get aggregate statistics. O(1) for counts; O(n) for breakdowns.
    pub fn get_stats(&self) -> Result<GraphStats> {
        let total_nodes = self.data.graph.node_count();
        let total_edges = self.data.graph.edge_count();

        let mut nodes_by_kind: HashMap<NodeKind, usize> = HashMap::new();
        let mut edges_by_kind: HashMap<EdgeKind, usize> = HashMap::new();
        let mut languages: HashSet<String> = HashSet::new();
        let mut files_count = 0usize;

        for idx in self.data.graph.node_indices() {
            let node = &self.data.graph[idx];
            *nodes_by_kind.entry(node.kind).or_insert(0) += 1;
            if node.kind == NodeKind::File {
                files_count += 1;
            }
            if !node.language.is_empty() {
                languages.insert(node.language.clone());
            }
        }

        for edge_ref in (&self.data.graph).edge_references() {
            *edges_by_kind.entry(*edge_ref.weight()).or_insert(0) += 1;
        }

        let last_updated = self.data.metadata.get("last_updated").cloned();

        Ok(GraphStats {
            total_nodes,
            total_edges,
            nodes_by_kind,
            edges_by_kind,
            languages: languages.into_iter().collect(),
            files_count,
            last_updated,
        })
    }

    /// Set a metadata key-value pair.
    pub fn set_metadata(&mut self, key: &str, value: &str) -> Result<()> {
        self.data
            .metadata
            .insert(key.to_string(), value.to_string());
        Ok(())
    }

    /// Get a metadata value.
    pub fn get_metadata(&self, key: &str) -> Result<Option<String>> {
        Ok(self.data.metadata.get(key).cloned())
    }

    /// Save to disk and drop.
    pub fn close(self) -> Result<()> {
        save(&self.data, &self.bin_path)
    }

    /// Get all incoming edges whose target nodes belong to a given file.
    ///
    /// More efficient than iterating all nodes when the file has many nodes:
    /// uses `file_index` for O(nodes_in_file) lookup instead of O(all_nodes).
    pub fn get_incoming_edges_for_file_nodes(&self, file_path: &str) -> Result<Vec<GraphEdge>> {
        let node_indices = self
            .data
            .file_index
            .get(file_path)
            .map(Vec::as_slice)
            .unwrap_or(&[]);

        let mut edges = Vec::new();
        for &idx in node_indices {
            for edge in self.data.graph.edges_directed(idx, Direction::Incoming) {
                let source_idx = edge.source();
                let source = &self.data.graph[source_idx];
                let target = &self.data.graph[idx];
                edges.push(GraphEdge {
                    source_qualified: source.qualified_name.clone(),
                    target_qualified: target.qualified_name.clone(),
                    kind: *edge.weight(),
                    file_path: source.file_path.clone(),
                    line: 0,
                });
            }
        }
        Ok(edges)
    }

    // -- Internal helpers --

    /// Remove all graph nodes/edges belonging to `file_path`.
    fn remove_file_data_inner(&mut self, file_path: &str) {
        // Collect node indices to remove
        let idxs_to_remove: Vec<NodeIndex> = self
            .data
            .file_index
            .remove(file_path)
            .unwrap_or_default();

        for idx in &idxs_to_remove {
            // Remove this node from node_index
            if let Some(node) = self.data.graph.node_weight(*idx) {
                let qn = node.qualified_name.clone();
                self.data.node_index.remove(&qn);
            }
            // StableGraph::remove_node also removes all incident edges
            self.data.graph.remove_node(*idx);
        }

        self.data.file_hashes.remove(file_path);
    }

    /// Collect edges where both endpoints are in `qualified_names`.
    fn get_edges_among(&self, qualified_names: &HashSet<String>) -> Vec<GraphEdge> {
        use petgraph::stable_graph::EdgeReference;
        (&self.data.graph)
            .edge_references()
            .filter(|e: &EdgeReference<'_, EdgeKind>| {
                let src_qn = &self.data.graph[e.source()].qualified_name;
                let tgt_qn = &self.data.graph[e.target()].qualified_name;
                qualified_names.contains(src_qn) && qualified_names.contains(tgt_qn)
            })
            .map(|e: EdgeReference<'_, EdgeKind>| GraphEdge {
                kind: *e.weight(),
                source_qualified: self.data.graph[e.source()].qualified_name.clone(),
                target_qualified: self.data.graph[e.target()].qualified_name.clone(),
                file_path: self.data.graph[e.source()].file_path.clone(),
                line: 0,
            })
            .collect()
    }

    /// Find the shortest call path between two nodes using BFS on Calls edges.
    /// Tries outgoing direction first (callee chain), then incoming (caller chain).
    /// Returns the path as a sequence of `(GraphNode, Option<GraphEdge>)` pairs.
    /// The last element has `edge=None` (destination, no outgoing edge in path).
    pub fn trace_call_chain(
        &self,
        from_qn: &str,
        to_qn: &str,
        max_depth: usize,
    ) -> Result<Option<Vec<(GraphNode, Option<GraphEdge>)>>> {
        let Some(&from_idx) = self.data.node_index.get(from_qn) else {
            return Ok(None);
        };
        let Some(&to_idx) = self.data.node_index.get(to_qn) else {
            return Ok(None);
        };

        // Same node → 0-hop path.
        if from_idx == to_idx {
            let node = self.data.graph[from_idx].clone();
            return Ok(Some(vec![(node, None)]));
        }

        // BFS over Calls edges in `direction`. Returns parent map on success.
        let bfs = |direction: Direction| -> Option<HashMap<NodeIndex, NodeIndex>> {
            let mut queue: VecDeque<(NodeIndex, usize)> = VecDeque::new();
            // parent[child] = parent; from_idx maps to itself as the BFS root sentinel.
            let mut parent: HashMap<NodeIndex, NodeIndex> = HashMap::new();

            queue.push_back((from_idx, 0));
            parent.insert(from_idx, from_idx);

            while let Some((idx, depth)) = queue.pop_front() {
                if depth >= max_depth {
                    continue;
                }
                for edge_ref in self.data.graph.edges_directed(idx, direction) {
                    if *edge_ref.weight() != EdgeKind::Calls {
                        continue;
                    }
                    let neighbor = match direction {
                        Direction::Outgoing => edge_ref.target(),
                        Direction::Incoming => edge_ref.source(),
                    };
                    if parent.contains_key(&neighbor) {
                        continue;
                    }
                    parent.insert(neighbor, idx);
                    if neighbor == to_idx {
                        return Some(parent);
                    }
                    queue.push_back((neighbor, depth + 1));
                }
            }
            None
        };

        // Try outgoing (callee chain) first, then incoming (caller chain).
        let Some(parent) = bfs(Direction::Outgoing).or_else(|| bfs(Direction::Incoming)) else {
            return Ok(None);
        };

        // Walk parent map from to_idx back to from_idx, then reverse.
        let mut path_indices: Vec<NodeIndex> = Vec::new();
        let mut cur = to_idx;
        loop {
            path_indices.push(cur);
            if cur == from_idx {
                break;
            }
            cur = parent[&cur];
        }
        path_indices.reverse();

        // Each step: (node, Some(CALLS edge to next)) except the last (None).
        let result: Vec<(GraphNode, Option<GraphEdge>)> = path_indices
            .windows(2)
            .map(|w| {
                let node = self.data.graph[w[0]].clone();
                let edge = Some(GraphEdge {
                    kind: EdgeKind::Calls,
                    source_qualified: node.qualified_name.clone(),
                    target_qualified: self.data.graph[w[1]].qualified_name.clone(),
                    file_path: node.file_path.clone(),
                    line: 0,
                });
                (node, edge)
            })
            .chain(std::iter::once({
                let last = self.data.graph[*path_indices.last().unwrap()].clone();
                (last, None)
            }))
            .collect();

        Ok(Some(result))
    }
}

// ---------------------------------------------------------------------------
// Impact analysis helpers (pure functions, operate on GraphData)
// ---------------------------------------------------------------------------

/// Sort `(qualified_name, score)` pairs descending by score, truncate to `limit`.
fn sort_and_truncate(mut results: Vec<(String, f64)>, limit: usize) -> Vec<(String, f64)> {
    results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    results.truncate(limit);
    results
}

/// Weight assigned to each edge kind for impact propagation.
/// Higher = more impactful relationship.
fn edge_impact_weight(kind: EdgeKind) -> f64 {
    match kind {
        EdgeKind::Calls => 1.0,
        EdgeKind::Inherits => 1.2,
        EdgeKind::Implements => 1.0,
        EdgeKind::ImportsFrom => 0.5,
        EdgeKind::TestedBy => 0.8,
        EdgeKind::Contains => 0.1,
    }
}

/// Wrapper for `f64` that implements `Ord` so it can live in a `BinaryHeap`.
/// NaN is treated as less than any finite value.
#[derive(PartialEq)]
#[allow(dead_code)]
struct OrdF64(f64);

impl Eq for OrdF64 {}

impl PartialOrd for OrdF64 {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrdF64 {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.partial_cmp(&other.0).unwrap_or(std::cmp::Ordering::Less)
    }
}

/// Direction-aware, weighted BFS (Dijkstra-style) for impact analysis.
///
/// Rules:
/// - INCOMING edges to a changed node → high impact (callers/inheritors depend on me).
///   Multiplier: weight × 1.0
/// - OUTGOING edges from a changed node → negligible (my deps didn't change).
///   Exception: TestedBy outgoing edges DO propagate (the test should be flagged).
///   Multiplier: weight × 0.0 for all except TestedBy (weight × 1.0)
///
/// Each node accumulates a score = parent_score × edge_weight × decay_factor.
/// Traversal continues only for nodes with score > threshold.
///
/// Returns (qualified_name, score) pairs sorted by score descending.
#[allow(dead_code)]
fn weighted_bfs_impact(
    seeds: &HashSet<String>,
    data: &GraphData,
    max_depth: usize,
    max_results: usize,
) -> Vec<(String, f64)> {
    const DECAY: f64 = 0.7;
    const THRESHOLD: f64 = 0.01;

    // Max-heap ordered by score descending. Depth is tracked separately.
    let mut heap: BinaryHeap<(OrdF64, usize, String)> = BinaryHeap::new();
    let mut best_score: HashMap<String, f64> = HashMap::new();

    for qn in seeds {
        best_score.insert(qn.clone(), 1.0);
        heap.push((OrdF64(1.0), 0, qn.clone()));
    }

    while let Some((OrdF64(score), depth, qn)) = heap.pop() {
        // Lazy deletion: skip stale entries superseded by a better path
        if best_score.get(&qn).copied().unwrap_or(0.0) > score + f64::EPSILON {
            continue;
        }
        if depth >= max_depth {
            continue;
        }

        let Some(&idx) = data.node_index.get(&qn) else { continue };

        // INCOMING edges: callers/inheritors/importers depend on me — impacted
        for edge_ref in data.graph.edges_directed(idx, Direction::Incoming) {
            let new_score = score * edge_impact_weight(*edge_ref.weight()) * DECAY;
            if new_score < THRESHOLD {
                continue;
            }
            let nb_qn = data.graph[edge_ref.source()].qualified_name.clone();
            let prev = best_score.get(&nb_qn).copied().unwrap_or(0.0);
            if new_score > prev {
                best_score.insert(nb_qn.clone(), new_score);
                heap.push((OrdF64(new_score), depth + 1, nb_qn));
            }
        }

        // OUTGOING edges: only TestedBy propagates — tests must be re-run
        for edge_ref in data.graph.edges_directed(idx, Direction::Outgoing) {
            if *edge_ref.weight() != EdgeKind::TestedBy {
                continue;
            }
            let new_score = score * edge_impact_weight(EdgeKind::TestedBy) * DECAY;
            if new_score < THRESHOLD {
                continue;
            }
            let nb_qn = data.graph[edge_ref.target()].qualified_name.clone();
            let prev = best_score.get(&nb_qn).copied().unwrap_or(0.0);
            if new_score > prev {
                best_score.insert(nb_qn.clone(), new_score);
                heap.push((OrdF64(new_score), depth + 1, nb_qn));
            }
        }
    }

    let results: Vec<(String, f64)> = best_score
        .into_iter()
        .filter(|(qn, _)| !seeds.contains(qn))
        .collect();
    sort_and_truncate(results, max_results)
}

/// Personalized PageRank for impact analysis (reverse propagation).
///
/// Runs PPR on the *reversed* graph so that score flows FROM seeds TOWARD nodes
/// that depend on (call/inherit/import) those seeds.  This is the "blast radius"
/// semantic: if `b` changes, every node `a` that has an outgoing edge `a → b`
/// (a calls b) is a dependent and accumulates impact score.
///
/// Concretely: for each node `v`, we sum contributions from its OUTGOING
/// successors `s` (nodes it depends on) weighted by `w(v→s) / in_degree_weighted(s)`.
/// Seeds receive the teleport probability; non-seeds get zero teleport.
///
/// Returns (qualified_name, score) pairs sorted by score descending (seeds excluded).
fn pagerank_impact(
    seeds: &HashSet<String>,
    data: &GraphData,
    max_results: usize,
) -> Vec<(String, f64)> {
    let damping: f64 = 0.85;
    let max_iterations: usize = 20;
    let epsilon: f64 = 1e-6;
    if data.graph.node_count() == 0 || seeds.is_empty() {
        return vec![];
    }

    let seed_score = 1.0 / seeds.len() as f64;

    // Precompute seed node indices for O(1) teleport lookup.
    let seed_indices: HashSet<NodeIndex> = seeds
        .iter()
        .filter_map(|qn| data.node_index.get(qn).copied())
        .collect();

    // Initialize scores and active frontier.
    // Only nodes with non-zero scores + their incoming neighbors need processing.
    let mut scores: HashMap<NodeIndex, f64> = HashMap::new();
    let mut active: HashSet<NodeIndex> = HashSet::new();
    for &idx in &seed_indices {
        scores.insert(idx, seed_score);
        active.insert(idx);
        // Seed's incoming neighbors (callers) will receive impact next iteration
        for pred in data.graph.neighbors_directed(idx, Direction::Incoming) {
            active.insert(pred);
        }
    }

    // Cache weighted in-degree only for nodes we actually visit (lazy).
    let mut in_degree_cache: HashMap<NodeIndex, f64> = HashMap::new();
    let mut get_in_degree = |idx: NodeIndex| -> f64 {
        *in_degree_cache.entry(idx).or_insert_with(|| {
            data.graph
                .edges_directed(idx, Direction::Incoming)
                .map(|e| edge_impact_weight(*e.weight()))
                .sum()
        })
    };

    for _ in 0..max_iterations {
        let mut new_scores: HashMap<NodeIndex, f64> = HashMap::new();
        let mut next_active: HashSet<NodeIndex> = HashSet::new();
        let mut max_diff: f64 = 0.0;

        // Process only active nodes (seeds + nodes reachable from scored nodes)
        for &idx in &active {
            let teleport = if seed_indices.contains(&idx) {
                (1.0 - damping) * seed_score
            } else {
                0.0
            };

            let mut dep_sum: f64 = 0.0;
            for succ_idx in data.graph.neighbors_directed(idx, Direction::Outgoing) {
                let succ_score = scores.get(&succ_idx).copied().unwrap_or(0.0);
                if succ_score == 0.0 {
                    continue;
                }
                let d = get_in_degree(succ_idx);
                if d > 0.0 {
                    for edge_ref in data.graph.edges_connecting(idx, succ_idx) {
                        let w = edge_impact_weight(*edge_ref.weight());
                        dep_sum += succ_score * w / d;
                    }
                }
            }

            let new_score = teleport + damping * dep_sum;
            let old_score = scores.get(&idx).copied().unwrap_or(0.0);
            max_diff = max_diff.max((new_score - old_score).abs());
            if new_score > epsilon {
                new_scores.insert(idx, new_score);
                // Newly scored nodes' predecessors become active next iteration
                for pred in data.graph.neighbors_directed(idx, Direction::Incoming) {
                    next_active.insert(pred);
                }
            }
        }

        // Seeds always stay active (they receive teleportation)
        for &idx in &seed_indices {
            next_active.insert(idx);
        }

        scores = new_scores;
        active = next_active;
        if max_diff < epsilon || active.is_empty() {
            break;
        }
    }

    let results: Vec<(String, f64)> = scores
        .iter()
        .filter(|(idx, _)| !seed_indices.contains(idx))
        .map(|(idx, score)| (data.graph[*idx].qualified_name.clone(), *score))
        .collect();
    sort_and_truncate(results, max_results)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // -----------------------------------------------------------------------
    // Helper: create a fresh in-memory-backed GraphStore with a temp file path
    // -----------------------------------------------------------------------

    fn test_store() -> (GraphStore, TempDir) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.bin.zst");
        let store = GraphStore::new(&path).unwrap();
        (store, dir)
    }

    fn make_node(name: &str, qn: &str, file: &str, kind: NodeKind) -> NodeInfo {
        crate::types::NodeInfo {
            name: name.to_string(),
            qualified_name: qn.to_string(),
            kind,
            file_path: file.to_string(),
            line_start: 1,
            line_end: 10,
            language: "python".to_string(),
            is_test: false,
            docstring: String::new(),
            signature: String::new(),
            body_hash: format!("hash_{name}"),
        }
    }

    fn make_edge(src: &str, tgt: &str, kind: EdgeKind, file: &str) -> EdgeInfo {
        crate::types::EdgeInfo {
            source_qualified: src.to_string(),
            target_qualified: tgt.to_string(),
            kind,
            file_path: file.to_string(),
            line: 1,
        }
    }

    // -----------------------------------------------------------------------
    // store_file_nodes_edges + get_node
    // -----------------------------------------------------------------------

    #[test]
    fn store_and_get_node() {
        let (mut store, _dir) = test_store();
        let nodes = vec![make_node("foo", "file.py::foo", "file.py", NodeKind::Function)];
        let edges = vec![];
        store
            .store_file_nodes_edges("file.py", &nodes, &edges, "abc123")
            .unwrap();

        let node = store.get_node("file.py::foo").unwrap();
        assert!(node.is_some());
        let node = node.unwrap();
        assert_eq!(node.name, "foo");
        assert_eq!(node.qualified_name, "file.py::foo");
        assert_eq!(node.kind, NodeKind::Function);
    }

    #[test]
    fn store_replaces_old_data_on_recall() {
        let (mut store, _dir) = test_store();
        // First store: two nodes
        let nodes1 = vec![
            make_node("foo", "file.py::foo", "file.py", NodeKind::Function),
            make_node("bar", "file.py::bar", "file.py", NodeKind::Function),
        ];
        store
            .store_file_nodes_edges("file.py", &nodes1, &[], "hash1")
            .unwrap();

        // Second store: replace with one node
        let nodes2 = vec![make_node("baz", "file.py::baz", "file.py", NodeKind::Function)];
        store
            .store_file_nodes_edges("file.py", &nodes2, &[], "hash2")
            .unwrap();

        // Old nodes must be gone
        assert!(store.get_node("file.py::foo").unwrap().is_none());
        assert!(store.get_node("file.py::bar").unwrap().is_none());
        // New node is present
        assert!(store.get_node("file.py::baz").unwrap().is_some());
    }

    // -----------------------------------------------------------------------
    // remove_file_data
    // -----------------------------------------------------------------------

    #[test]
    fn remove_file_data_clears_nodes() {
        let (mut store, _dir) = test_store();
        let nodes = vec![make_node("foo", "file.py::foo", "file.py", NodeKind::Function)];
        store
            .store_file_nodes_edges("file.py", &nodes, &[], "hash1")
            .unwrap();

        store.remove_file_data("file.py").unwrap();

        assert!(store.get_node("file.py::foo").unwrap().is_none());
        assert!(store.get_nodes_by_file("file.py").unwrap().is_empty());
    }

    // -----------------------------------------------------------------------
    // get_nodes_by_file
    // -----------------------------------------------------------------------

    #[test]
    fn get_nodes_by_file_returns_all_nodes() {
        let (mut store, _dir) = test_store();
        let nodes = vec![
            make_node("foo", "file.py::foo", "file.py", NodeKind::Function),
            make_node("Bar", "file.py::Bar", "file.py", NodeKind::Class),
        ];
        store
            .store_file_nodes_edges("file.py", &nodes, &[], "h1")
            .unwrap();

        let file_nodes = store.get_nodes_by_file("file.py").unwrap();
        assert_eq!(file_nodes.len(), 2);
        let names: Vec<&str> = file_nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"foo"));
        assert!(names.contains(&"Bar"));
    }

    #[test]
    fn get_nodes_by_file_empty_for_unknown_file() {
        let (store, _dir) = test_store();
        assert!(store.get_nodes_by_file("nonexistent.py").unwrap().is_empty());
    }

    // -----------------------------------------------------------------------
    // get_all_files
    // -----------------------------------------------------------------------

    #[test]
    fn get_all_files_lists_file_nodes() {
        let (mut store, _dir) = test_store();
        // File node has NodeKind::File
        let nodes_a = vec![make_node("a.py", "a.py", "a.py", NodeKind::File)];
        let nodes_b = vec![make_node("b.py", "b.py", "b.py", NodeKind::File)];
        store
            .store_file_nodes_edges("a.py", &nodes_a, &[], "h1")
            .unwrap();
        store
            .store_file_nodes_edges("b.py", &nodes_b, &[], "h2")
            .unwrap();

        let files = store.get_all_files().unwrap();
        assert!(files.contains(&"a.py".to_string()));
        assert!(files.contains(&"b.py".to_string()));
    }

    // -----------------------------------------------------------------------
    // search_nodes
    // -----------------------------------------------------------------------

    #[test]
    fn search_nodes_case_insensitive() {
        let (mut store, _dir) = test_store();
        let nodes = vec![make_node("FooBar", "file.py::FooBar", "file.py", NodeKind::Function)];
        store
            .store_file_nodes_edges("file.py", &nodes, &[], "h1")
            .unwrap();

        let results = store.search_nodes("foobar", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "FooBar");
    }

    #[test]
    fn search_nodes_multi_word_and_logic() {
        let (mut store, _dir) = test_store();
        let nodes = vec![
            make_node("process_request", "f.py::process_request", "f.py", NodeKind::Function),
            make_node("process_response", "f.py::process_response", "f.py", NodeKind::Function),
            make_node("send_email", "f.py::send_email", "f.py", NodeKind::Function),
        ];
        store
            .store_file_nodes_edges("f.py", &nodes, &[], "h1")
            .unwrap();

        // Both words must match
        let results = store.search_nodes("process request", 10).unwrap();
        let names: Vec<&str> = results.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"process_request"), "should find process_request");
        assert!(!names.contains(&"send_email"), "should not find send_email");
    }

    #[test]
    fn search_nodes_empty_query_returns_empty() {
        let (mut store, _dir) = test_store();
        let nodes = vec![make_node("foo", "f.py::foo", "f.py", NodeKind::Function)];
        store
            .store_file_nodes_edges("f.py", &nodes, &[], "h1")
            .unwrap();

        let results = store.search_nodes("   ", 10).unwrap();
        assert!(results.is_empty());
    }

    // -----------------------------------------------------------------------
    // get_nodes_by_size
    // -----------------------------------------------------------------------

    #[test]
    fn get_nodes_by_size_filters_small() {
        let (mut store, _dir) = test_store();
        // large node: 100 lines
        let mut large = make_node("large_fn", "f.py::large_fn", "f.py", NodeKind::Function);
        large.line_start = 1;
        large.line_end = 100;
        // small node: 5 lines
        let mut small = make_node("small_fn", "f.py::small_fn", "f.py", NodeKind::Function);
        small.line_start = 110;
        small.line_end = 114;

        store
            .store_file_nodes_edges("f.py", &[large, small], &[], "h1")
            .unwrap();

        let results = store.get_nodes_by_size(50, None, None, 10).unwrap();
        let names: Vec<&str> = results.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"large_fn"));
        assert!(!names.contains(&"small_fn"));
    }

    #[test]
    fn get_nodes_by_size_sorted_descending() {
        let (mut store, _dir) = test_store();
        let mut n50 = make_node("fifty", "f.py::fifty", "f.py", NodeKind::Function);
        n50.line_start = 1;
        n50.line_end = 50;
        let mut n200 = make_node("twohundred", "f.py::twohundred", "f.py", NodeKind::Function);
        n200.line_start = 60;
        n200.line_end = 259;

        store
            .store_file_nodes_edges("f.py", &[n50, n200], &[], "h1")
            .unwrap();

        let results = store.get_nodes_by_size(1, None, None, 10).unwrap();
        assert_eq!(results[0].name, "twohundred");
    }

    // -----------------------------------------------------------------------
    // get_edges_by_source / get_edges_by_target
    // -----------------------------------------------------------------------

    #[test]
    fn edges_by_source_and_target() {
        let (mut store, _dir) = test_store();
        let nodes = vec![
            make_node("caller", "f.py::caller", "f.py", NodeKind::Function),
            make_node("callee", "f.py::callee", "f.py", NodeKind::Function),
        ];
        let edges = vec![make_edge("f.py::caller", "f.py::callee", EdgeKind::Calls, "f.py")];
        store
            .store_file_nodes_edges("f.py", &nodes, &edges, "h1")
            .unwrap();

        let by_src = store.get_edges_by_source("f.py::caller").unwrap();
        assert_eq!(by_src.len(), 1);
        assert_eq!(by_src[0].kind, EdgeKind::Calls);
        assert_eq!(by_src[0].target_qualified, "f.py::callee");

        let by_tgt = store.get_edges_by_target("f.py::callee").unwrap();
        assert_eq!(by_tgt.len(), 1);
        assert_eq!(by_tgt[0].source_qualified, "f.py::caller");
    }

    #[test]
    fn duplicate_edges_not_added() {
        let (mut store, _dir) = test_store();
        let nodes = vec![
            make_node("a", "f.py::a", "f.py", NodeKind::Function),
            make_node("b", "f.py::b", "f.py", NodeKind::Function),
        ];
        let edges = vec![
            make_edge("f.py::a", "f.py::b", EdgeKind::Calls, "f.py"),
            make_edge("f.py::a", "f.py::b", EdgeKind::Calls, "f.py"),
        ];
        store
            .store_file_nodes_edges("f.py", &nodes, &edges, "h1")
            .unwrap();

        let by_src = store.get_edges_by_source("f.py::a").unwrap();
        // Should only have one edge, not two
        assert_eq!(by_src.len(), 1);
    }

    // -----------------------------------------------------------------------
    // get_impact_radius — Personalized PageRank
    // -----------------------------------------------------------------------

    #[test]
    fn impact_radius_bfs_finds_callers() {
        let (mut store, _dir) = test_store();
        // "a" (in caller.py) calls "b" (in lib.py).
        // When lib.py changes, "a" (the caller in caller.py) should be impacted.
        // Nodes in different files so that seeds (lib.py nodes) ≠ callers (caller.py nodes).
        let lib_nodes = vec![
            make_node("b", "lib.py::b", "lib.py", NodeKind::Function),
        ];
        let caller_nodes = vec![
            make_node("a", "caller.py::a", "caller.py", NodeKind::Function),
        ];
        // a calls b — edge from a → b
        let edges = vec![make_edge("caller.py::a", "lib.py::b", EdgeKind::Calls, "caller.py")];
        store
            .store_file_nodes_edges("lib.py", &lib_nodes, &[], "h_lib")
            .unwrap();
        // Store the edge with the caller nodes
        store
            .store_file_nodes_edges("caller.py", &caller_nodes, &edges, "h_caller")
            .unwrap();

        // Change lib.py — seeds are lib.py nodes only
        let result = store
            .get_impact_radius(&["lib.py".to_string()], 5, 50, None)
            .unwrap();

        assert_eq!(result.algorithm, "personalized_pagerank");
        // a is in caller.py (not a seed) and calls b (in lib.py which changed)
        // so a should appear as impacted
        let impacted_names: Vec<&str> = result
            .impacted_nodes
            .iter()
            .map(|n| n.name.as_str())
            .collect();
        assert!(
            impacted_names.contains(&"a"),
            "caller 'a' should be impacted when 'b' (which it calls) changes; \
             impacted: {:?}",
            impacted_names
        );
    }

    #[test]
    fn impact_radius_with_changed_nodes_seed() {
        let (mut store, _dir) = test_store();
        let nodes = vec![
            make_node("x", "f.py::x", "f.py", NodeKind::Function),
            make_node("y", "f.py::y", "f.py", NodeKind::Function),
        ];
        let edges = vec![make_edge("f.py::x", "f.py::y", EdgeKind::Calls, "f.py")];
        store
            .store_file_nodes_edges("f.py", &nodes, &edges, "h1")
            .unwrap();

        // Seed with changed_nodes=["f.py::y"] — x (caller) should be impacted
        let result = store
            .get_impact_radius(
                &["f.py".to_string()],
                5,
                50,
                Some(&["f.py::y".to_string()]),
            )
            .unwrap();

        let impacted_names: Vec<&str> = result
            .impacted_nodes
            .iter()
            .map(|n| n.name.as_str())
            .collect();
        assert!(impacted_names.contains(&"x"));
    }

    // -----------------------------------------------------------------------
    // get_stats
    // -----------------------------------------------------------------------

    #[test]
    fn get_stats_counts_correctly() {
        let (mut store, _dir) = test_store();
        let nodes = vec![
            make_node("Foo", "f.py::Foo", "f.py", NodeKind::Class),
            make_node("bar", "f.py::bar", "f.py", NodeKind::Function),
        ];
        let edges = vec![make_edge("f.py::Foo", "f.py::bar", EdgeKind::Contains, "f.py")];
        store
            .store_file_nodes_edges("f.py", &nodes, &edges, "h1")
            .unwrap();

        let stats = store.get_stats().unwrap();
        assert_eq!(stats.total_nodes, 2);
        assert_eq!(stats.total_edges, 1);
        assert!(stats.nodes_by_kind.contains_key(&NodeKind::Class));
        assert!(stats.nodes_by_kind.contains_key(&NodeKind::Function));
        assert!(stats.edges_by_kind.contains_key(&EdgeKind::Contains));
    }

    // -----------------------------------------------------------------------
    // set_metadata / get_metadata
    // -----------------------------------------------------------------------

    #[test]
    fn metadata_roundtrip() {
        let (mut store, _dir) = test_store();
        store.set_metadata("last_updated", "2024-01-01T00:00:00").unwrap();
        let val = store.get_metadata("last_updated").unwrap();
        assert_eq!(val, Some("2024-01-01T00:00:00".to_string()));
    }

    #[test]
    fn metadata_missing_key_returns_none() {
        let (store, _dir) = test_store();
        let val = store.get_metadata("nonexistent").unwrap();
        assert!(val.is_none());
    }

    // -----------------------------------------------------------------------
    // save + load roundtrip
    // -----------------------------------------------------------------------

    #[test]
    fn save_and_load_roundtrip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("graph.bin.zst");

        {
            let mut store = GraphStore::new(&path).unwrap();
            let nodes = vec![make_node("roundtrip_fn", "f.py::roundtrip_fn", "f.py", NodeKind::Function)];
            store
                .store_file_nodes_edges("f.py", &nodes, &[], "hash99")
                .unwrap();
            store.set_metadata("key", "value").unwrap();
            store.commit().unwrap();
        }

        // Reload from disk
        let store2 = GraphStore::new(&path).unwrap();
        let node = store2.get_node("f.py::roundtrip_fn").unwrap();
        assert!(node.is_some(), "node should survive save+load");
        assert_eq!(store2.get_metadata("key").unwrap(), Some("value".to_string()));
    }

    // -----------------------------------------------------------------------
    // Corrupt file triggers error
    // -----------------------------------------------------------------------

    #[test]
    fn corrupt_file_bad_magic_starts_empty() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("bad.bin.zst");
        // Write garbage — bad magic bytes
        std::fs::write(&path, b"BADBYTES_NOT_CRG_MAGIC").unwrap();

        // GraphStore::new should not panic — it falls back to empty graph
        let store = GraphStore::new(&path).unwrap();
        assert_eq!(store.get_stats().unwrap().total_nodes, 0);
    }

    #[test]
    fn corrupt_file_bad_crc_starts_empty() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("crc.bin.zst");
        // Write valid magic but broken CRC (all zeros after magic)
        let mut data = b"CRG\x01".to_vec();
        data.extend_from_slice(&[0u8; 100]); // bad CRC + garbage compressed payload
        std::fs::write(&path, &data).unwrap();

        let store = GraphStore::new(&path).unwrap();
        assert_eq!(store.get_stats().unwrap().total_nodes, 0);
    }

    // -----------------------------------------------------------------------
    // get_body_hashes
    // -----------------------------------------------------------------------

    #[test]
    fn get_body_hashes_returns_map() {
        let (mut store, _dir) = test_store();
        let nodes = vec![
            make_node("fn1", "f.py::fn1", "f.py", NodeKind::Function),
            make_node("fn2", "f.py::fn2", "f.py", NodeKind::Function),
        ];
        store
            .store_file_nodes_edges("f.py", &nodes, &[], "h1")
            .unwrap();

        let hashes = store.get_body_hashes("f.py");
        assert!(hashes.contains_key("f.py::fn1"));
        assert!(hashes.contains_key("f.py::fn2"));
        assert!(!hashes["f.py::fn1"].is_empty());
    }

    #[test]
    fn get_body_hashes_empty_for_unknown_file() {
        let (store, _dir) = test_store();
        let hashes = store.get_body_hashes("nonexistent.py");
        assert!(hashes.is_empty());
    }

    // -----------------------------------------------------------------------
    // PPR edge-weight test: high-weight edges (Inherits=1.2) contribute more
    // than low-weight edges (Contains=0.1) for equal predecessor scores.
    // -----------------------------------------------------------------------

    #[test]
    fn ppr_high_weight_edge_yields_higher_score_than_low_weight() {
        let (mut store, _dir) = test_store();

        // high_target → seed via Inherits (weight=1.2): high_target depends on seed
        // seed → low_target  via Contains (weight=0.1): seed contains low_target
        // Only high_target has an outgoing edge to the seed, so it accumulates score.
        let nodes = vec![
            make_node("seed", "f.py::seed", "f.py", NodeKind::Class),
            make_node("high_target", "f.py::high_target", "f.py", NodeKind::Class),
            make_node("low_target", "f.py::low_target", "f.py", NodeKind::Function),
        ];
        let edges = vec![
            make_edge("f.py::high_target", "f.py::seed", EdgeKind::Inherits, "f.py"),
            make_edge("f.py::seed", "f.py::low_target", EdgeKind::Contains, "f.py"),
        ];
        store
            .store_file_nodes_edges("f.py", &nodes, &edges, "h1")
            .unwrap();

        // Seed on "seed"; high_target inherits from seed (incoming to high_target),
        // low_target is contained by seed (outgoing from seed → incoming to low_target).
        let result = store
            .get_impact_radius(&["f.py".to_string()], 5, 50, Some(&["f.py::seed".to_string()]))
            .unwrap();

        let high_score = result
            .impact_scores
            .get("f.py::high_target")
            .copied()
            .unwrap_or(0.0);
        let low_score = result
            .impact_scores
            .get("f.py::low_target")
            .copied()
            .unwrap_or(0.0);

        assert!(
            high_score > low_score,
            "Inherits edge (weight=1.2) should yield higher PPR score than Contains (weight=0.1); \
             high_target={high_score:.6}, low_target={low_score:.6}"
        );
    }

    // -----------------------------------------------------------------------
    // search_nodes determinism: exact match ranks before prefix, prefix before contains
    // -----------------------------------------------------------------------

    #[test]
    fn search_nodes_relevance_ordering() {
        let (mut store, _dir) = test_store();
        let nodes = vec![
            make_node("process", "f.py::process", "f.py", NodeKind::Function),
            make_node("process_request", "f.py::process_request", "f.py", NodeKind::Function),
            make_node("pre_process", "f.py::pre_process", "f.py", NodeKind::Function),
        ];
        store
            .store_file_nodes_edges("f.py", &nodes, &[], "h1")
            .unwrap();

        let results = store.search_nodes("process", 10).unwrap();
        assert_eq!(results.len(), 3);
        // Exact match must be first
        assert_eq!(results[0].name, "process");
        // Prefix match before contains match
        assert_eq!(results[1].name, "process_request");
        assert_eq!(results[2].name, "pre_process");
    }

    // -----------------------------------------------------------------------
    // get_incoming_edges_for_file_nodes
    // -----------------------------------------------------------------------

    #[test]
    fn incoming_edges_for_file_nodes_finds_callers() {
        let (mut store, _dir) = test_store();
        let lib_nodes = vec![make_node("b", "lib.py::b", "lib.py", NodeKind::Function)];
        let caller_nodes = vec![make_node("a", "caller.py::a", "caller.py", NodeKind::Function)];
        let edges = vec![make_edge("caller.py::a", "lib.py::b", EdgeKind::Calls, "caller.py")];
        store
            .store_file_nodes_edges("lib.py", &lib_nodes, &[], "h_lib")
            .unwrap();
        store
            .store_file_nodes_edges("caller.py", &caller_nodes, &edges, "h_caller")
            .unwrap();

        let incoming = store.get_incoming_edges_for_file_nodes("lib.py").unwrap();
        assert_eq!(incoming.len(), 1);
        assert_eq!(incoming[0].source_qualified, "caller.py::a");
        assert_eq!(incoming[0].target_qualified, "lib.py::b");
        assert_eq!(incoming[0].kind, EdgeKind::Calls);
    }

    #[test]
    fn incoming_edges_for_file_nodes_empty_for_unknown_file() {
        let (store, _dir) = test_store();
        let edges = store
            .get_incoming_edges_for_file_nodes("nonexistent.py")
            .unwrap();
        assert!(edges.is_empty());
    }
}
