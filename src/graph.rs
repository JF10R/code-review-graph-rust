//! Petgraph-backed graph store with bincode+zstd persistence.
//!
//! The entire graph lives in memory as a `StableGraph`. Persistence is a
//! single atomic file write: `graph.bin.zst` (zstd-compressed bincode with a
//! 4-byte magic header and a CRC-32 integrity check).
//!
//! SQLite is no longer used here — see `embeddings.rs` for the embeddings DB.

use std::collections::{BinaryHeap, HashMap, HashSet};
use std::io::Write as _;
use std::path::Path;

use petgraph::stable_graph::StableGraph;
use petgraph::visit::{EdgeRef, IntoEdgeReferences};
use petgraph::Direction;
use serde::{Deserialize, Serialize};

pub use petgraph::stable_graph::NodeIndex;

use crate::error::{CrgError, Result};
use crate::types::{
    EdgeInfo, EdgeKind, GraphEdge, GraphNode, GraphStats, ImpactResult, NodeInfo, NodeKind,
};

// ---------------------------------------------------------------------------
// File format constants
// ---------------------------------------------------------------------------

/// Magic bytes at the start of every `.bin.zst` file.
const MAGIC: &[u8; 4] = b"CRG\x01";

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
// Persistence helpers
// ---------------------------------------------------------------------------

/// Serialize, compress, and atomically write `data` to `path`.
fn save(data: &GraphData, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let payload = bincode::serialize(data)?;
    let compressed = zstd::encode_all(&payload[..], 3)
        .map_err(|e| CrgError::Io(e))?;
    let crc = crc32fast::hash(&compressed);

    let tmp = tempfile::NamedTempFile::new_in(path.parent().unwrap_or(Path::new(".")))?;
    {
        let mut f = tmp.as_file();
        f.write_all(MAGIC)?;
        f.write_all(&crc.to_le_bytes())?;
        f.write_all(&compressed)?;
        f.flush()?;
    }
    tmp.persist(path)
        .map_err(|e| CrgError::Io(e.error))?;
    Ok(())
}

/// Load `GraphData` from a `graph.bin.zst` file.
fn load(path: &Path) -> Result<GraphData> {
    let bytes = std::fs::read(path)?;
    if bytes.len() < 8 {
        return Err(CrgError::Other("graph file too short".into()));
    }
    if &bytes[0..4] != MAGIC {
        return Err(CrgError::Other("corrupt graph file (bad magic)".into()));
    }
    let stored_crc = u32::from_le_bytes(
        bytes[4..8]
            .try_into()
            .map_err(|_| CrgError::Other("corrupt graph file (bad crc field)".into()))?,
    );
    let compressed = &bytes[8..];
    if crc32fast::hash(compressed) != stored_crc {
        return Err(CrgError::Other("graph file CRC mismatch".into()));
    }
    let decompressed = zstd::decode_all(compressed)
        .map_err(|e| CrgError::Io(e))?;
    let data: GraphData = bincode::deserialize(&decompressed)?;
    Ok(data)
}

// ---------------------------------------------------------------------------
// GraphStore — public API
// ---------------------------------------------------------------------------

/// In-memory graph store backed by petgraph, persisted to disk as
/// a zstd-compressed bincode blob.
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
    pub fn store_file_nodes_edges(
        &mut self,
        file_path: &str,
        nodes: &[NodeInfo],
        edges: &[EdgeInfo],
        file_hash: &str,
    ) -> Result<()> {
        self.remove_file_data_inner(file_path);

        // Insert new nodes
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
                file_hash: file_hash.to_string(),
            };
            let idx = self.data.graph.add_node(graph_node);
            self.data
                .node_index
                .insert(node_info.qualified_name.clone(), idx);
            new_idxs.push(idx);
        }
        self.data
            .file_index
            .insert(file_path.to_string(), new_idxs);
        self.data
            .file_hashes
            .insert(file_path.to_string(), file_hash.to_string());

        // Insert new edges (resolve endpoints via node_index)
        for edge_info in edges {
            let src_idx = self.data.node_index.get(&edge_info.source_qualified).copied();
            let tgt_idx = self.data.node_index.get(&edge_info.target_qualified).copied();
            if let (Some(src), Some(tgt)) = (src_idx, tgt_idx) {
                // Avoid duplicate edges at the same call site
                let already_exists = self
                    .data
                    .graph
                    .edges_connecting(src, tgt)
                    .any(|e| *e.weight() == edge_info.kind);
                if !already_exists {
                    self.data.graph.add_edge(src, tgt, edge_info.kind);
                }
            }
            // Unresolved edges (target not in graph yet) are silently dropped —
            // cross-file edges will be re-added on the next build that touches
            // the target file.
        }

        Ok(())
    }

    /// Remove all nodes and edges associated with a file.
    pub fn remove_file_data(&mut self, file_path: &str) -> Result<()> {
        self.remove_file_data_inner(file_path);
        Ok(())
    }

    /// Persist in-memory state to disk.
    ///
    /// Previously a no-op (rusqlite auto-committed). Now triggers a real
    /// atomic file write.
    pub fn commit(&self) -> Result<()> {
        save(&self.data, &self.bin_path)
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
        let files: Vec<String> = self
            .data
            .node_index
            .values()
            .filter(|&&idx| self.data.graph[idx].kind == NodeKind::File)
            .map(|&idx| self.data.graph[idx].file_path.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        Ok(files)
    }

    /// Search nodes by name substring (multi-word AND logic, case-insensitive).
    pub fn search_nodes(&self, query: &str, limit: usize) -> Result<Vec<GraphNode>> {
        let words: Vec<String> = query
            .split_whitespace()
            .map(|w| w.to_lowercase())
            .collect();
        if words.is_empty() {
            return Ok(vec![]);
        }

        let results: Vec<GraphNode> = self
            .data
            .node_index
            .iter()
            .filter(|(qn, &idx)| {
                let node = &self.data.graph[idx];
                let name_lower = node.name.to_lowercase();
                let qn_lower = qn.to_lowercase();
                words
                    .iter()
                    .all(|w| name_lower.contains(w.as_str()) || qn_lower.contains(w.as_str()))
            })
            .take(limit)
            .map(|(_, &idx)| self.data.graph[idx].clone())
            .collect();
        Ok(results)
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
        let kind_filter = kind.and_then(NodeKind::from_str);

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

    /// Compute the blast radius of changed files.
    ///
    /// Uses weighted directed BFS for graphs < 10k nodes, personalized PageRank
    /// for larger graphs. Automatically selects algorithm based on graph size.
    ///
    /// If `changed_nodes` is provided, those qualified names are used as seeds
    /// directly (node-level diff seeding). Otherwise seeds are all nodes in
    /// `changed_files`.
    pub fn get_impact_radius(
        &self,
        changed_files: &[String],
        max_depth: usize,
        max_nodes: usize,
        changed_nodes: Option<&[String]>,
    ) -> Result<ImpactResult> {
        // Build seed set
        let mut seeds: HashSet<String> = HashSet::new();
        if let Some(specific_nodes) = changed_nodes {
            // Node-level seeding: only actually-changed nodes
            for qn in specific_nodes {
                seeds.insert(qn.clone());
            }
        }
        // Fall back to (or supplement with) all nodes in changed files if no
        // specific nodes provided or seeds are still empty
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

        let (ranked, algorithm) = if self.data.graph.node_count() >= 10_000 {
            let r = pagerank_impact(&seeds, &self.data, max_nodes);
            (r, "pagerank".to_string())
        } else {
            let r = weighted_bfs_impact(&seeds, &self.data, max_depth, max_nodes);
            (r, "weighted_bfs".to_string())
        };

        // Build impact_scores map and collect impacted nodes (exclude seeds)
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

        let mut nodes_by_kind: HashMap<String, usize> = HashMap::new();
        let mut edges_by_kind: HashMap<String, usize> = HashMap::new();
        let mut languages: HashSet<String> = HashSet::new();
        let mut files_count = 0usize;

        for idx in self.data.graph.node_indices() {
            let node = &self.data.graph[idx];
            *nodes_by_kind
                .entry(node.kind.as_str().to_string())
                .or_insert(0) += 1;
            if node.kind == NodeKind::File {
                files_count += 1;
            }
            if !node.language.is_empty() {
                languages.insert(node.language.clone());
            }
        }

        for edge_ref in (&self.data.graph).edge_references() {
            *edges_by_kind
                .entry(edge_ref.weight().as_str().to_string())
                .or_insert(0) += 1;
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

/// Personalized PageRank for large graphs (>= 10k nodes).
///
/// Propagates scores through INCOMING edges (reverse direction) with edge weights.
/// Seeds receive a teleport component that keeps scores anchored to the changed nodes.
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

    // Precompute out-degree for each node (used in every iteration).
    let out_degree: HashMap<NodeIndex, f64> = data
        .graph
        .node_indices()
        .map(|idx| {
            let d = data.graph.neighbors_directed(idx, Direction::Outgoing).count() as f64;
            (idx, d)
        })
        .collect();

    // Precompute seed node indices for O(1) teleport lookup.
    let seed_indices: HashSet<NodeIndex> = seeds
        .iter()
        .filter_map(|qn| data.node_index.get(qn).copied())
        .collect();

    // Initialize scores: seeds get seed_score, others get 0.0
    let mut scores: HashMap<NodeIndex, f64> = HashMap::new();
    for &idx in &seed_indices {
        scores.insert(idx, seed_score);
    }

    for _ in 0..max_iterations {
        let mut new_scores: HashMap<NodeIndex, f64> = HashMap::new();
        let mut max_diff: f64 = 0.0;

        for idx in data.graph.node_indices() {
            // Personalized teleport: only seed nodes receive the teleport boost
            let teleport = if seed_indices.contains(&idx) {
                (1.0 - damping) * seed_score
            } else {
                0.0
            };

            // Accumulate contributions from incoming neighbors
            let mut incoming_sum: f64 = 0.0;
            for pred_idx in data.graph.neighbors_directed(idx, Direction::Incoming) {
                let pred_score = scores.get(&pred_idx).copied().unwrap_or(0.0);
                if pred_score == 0.0 {
                    continue;
                }
                let d = out_degree[&pred_idx];
                if d > 0.0 {
                    for edge_ref in data.graph.edges_connecting(pred_idx, idx) {
                        let w = edge_impact_weight(*edge_ref.weight());
                        incoming_sum += pred_score * w / d;
                    }
                }
            }

            let new_score = teleport + damping * incoming_sum;
            let old_score = scores.get(&idx).copied().unwrap_or(0.0);
            max_diff = max_diff.max((new_score - old_score).abs());
            if new_score > epsilon {
                new_scores.insert(idx, new_score);
            }
        }

        scores = new_scores;
        if max_diff < epsilon {
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
