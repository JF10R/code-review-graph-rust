//! Shared types used across all modules.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

// ---------------------------------------------------------------------------
// Node kinds
// ---------------------------------------------------------------------------

/// The kind of a graph node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NodeKind {
    File,
    Class,
    Function,
    Type,
    Test,
}

impl NodeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::File => "File",
            Self::Class => "Class",
            Self::Function => "Function",
            Self::Type => "Type",
            Self::Test => "Test",
        }
    }
}

impl FromStr for NodeKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "File" => Ok(Self::File),
            "Class" => Ok(Self::Class),
            "Function" => Ok(Self::Function),
            "Type" => Ok(Self::Type),
            "Test" => Ok(Self::Test),
            _ => Err(format!("unknown NodeKind: '{s}'")),
        }
    }
}

impl fmt::Display for NodeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Edge kinds
// ---------------------------------------------------------------------------

/// The kind of a graph edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EdgeKind {
    Calls,
    ImportsFrom,
    Contains,
    Inherits,
    Implements,
    TestedBy,
}

impl EdgeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Calls => "CALLS",
            Self::ImportsFrom => "IMPORTS_FROM",
            Self::Contains => "CONTAINS",
            Self::Inherits => "INHERITS",
            Self::Implements => "IMPLEMENTS",
            Self::TestedBy => "TESTED_BY",
        }
    }
}

impl FromStr for EdgeKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "CALLS" => Ok(Self::Calls),
            "IMPORTS_FROM" => Ok(Self::ImportsFrom),
            "CONTAINS" => Ok(Self::Contains),
            "INHERITS" => Ok(Self::Inherits),
            "IMPLEMENTS" => Ok(Self::Implements),
            "TESTED_BY" => Ok(Self::TestedBy),
            _ => Err(format!("unknown EdgeKind: '{s}'")),
        }
    }
}

impl fmt::Display for EdgeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Parser output types (produced by parser, consumed by graph store)
// ---------------------------------------------------------------------------

/// A node extracted from source code by the parser.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfo {
    pub name: String,
    pub qualified_name: String,
    pub kind: NodeKind,
    pub file_path: String,
    pub line_start: usize,
    pub line_end: usize,
    pub language: String,
    pub is_test: bool,
    pub docstring: String,
    pub signature: String,
    pub body_hash: String,
}

/// An edge extracted from source code by the parser.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeInfo {
    pub source_qualified: String,
    pub target_qualified: String,
    pub kind: EdgeKind,
    pub file_path: String,
    pub line: usize,
}

// ---------------------------------------------------------------------------
// Graph store types (persisted in SQLite, returned by queries)
// ---------------------------------------------------------------------------

/// A node stored in the graph database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNode {
    pub name: String,
    pub qualified_name: String,
    pub kind: NodeKind,
    pub file_path: String,
    pub line_start: usize,
    pub line_end: usize,
    pub language: String,
    pub is_test: bool,
    pub docstring: String,
    pub signature: String,
    pub body_hash: String,
    pub file_hash: String,
}

/// An edge stored in the graph database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEdge {
    pub source_qualified: String,
    pub target_qualified: String,
    pub kind: EdgeKind,
    pub file_path: String,
    pub line: usize,
}

/// Aggregate statistics about the knowledge graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphStats {
    pub total_nodes: usize,
    pub total_edges: usize,
    pub nodes_by_kind: HashMap<NodeKind, usize>,
    pub edges_by_kind: HashMap<EdgeKind, usize>,
    pub languages: Vec<String>,
    pub files_count: usize,
    pub last_updated: Option<String>,
}

// ---------------------------------------------------------------------------
// Algorithm kind
// ---------------------------------------------------------------------------

/// Algorithm used for impact radius computation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlgorithmKind {
    WeightedBfs,
    PersonalizedPageRank,
}

impl fmt::Display for AlgorithmKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WeightedBfs => f.write_str("weighted_bfs"),
            Self::PersonalizedPageRank => f.write_str("personalized_pagerank"),
        }
    }
}

// ---------------------------------------------------------------------------
// Impact radius result
// ---------------------------------------------------------------------------

/// Result of an impact radius analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImpactResult {
    pub changed_nodes: Vec<GraphNode>,
    pub impacted_nodes: Vec<GraphNode>,
    /// qualified_name → impact score (higher = more impacted).
    pub impact_scores: HashMap<String, f64>,
    pub impacted_files: Vec<String>,
    pub edges: Vec<GraphEdge>,
    pub truncated: bool,
    pub total_impacted: usize,
    /// Algorithm used for this analysis.
    pub algorithm: String,
}

// ---------------------------------------------------------------------------
// Serialization helpers (dict representations for MCP responses)
// ---------------------------------------------------------------------------

/// Convert a GraphNode to a JSON-serializable map.
///
/// When `compact` is true, a compressed 6-field format is returned with
/// repo-relative paths, merged line range, and shortened keys. When false,
/// the full 11-field output is returned unchanged.
pub fn node_to_dict(node: &GraphNode, compact: bool, root: &camino::Utf8Path) -> serde_json::Value {
    if compact {
        let rel_path = camino::Utf8Path::new(&node.file_path)
            .strip_prefix(root)
            .map(|p| p.as_str().to_owned())
            .unwrap_or_else(|_| node.file_path.clone());
        serde_json::json!({
            "name": node.name,
            "qn": node.qualified_name,
            "kind": node.kind.as_str(),
            "file": rel_path,
            "lines": format!("{}-{}", node.line_start, node.line_end),
            "sig": node.signature,
        })
    } else {
        serde_json::json!({
            "name": node.name,
            "qualified_name": node.qualified_name,
            "kind": node.kind.as_str(),
            "file_path": node.file_path,
            "line_start": node.line_start,
            "line_end": node.line_end,
            "language": node.language,
            "is_test": node.is_test,
            "docstring": node.docstring,
            "signature": node.signature,
            "body_hash": node.body_hash,
        })
    }
}

/// Convert a GraphEdge to a JSON-serializable map.
pub fn edge_to_dict(edge: &GraphEdge) -> serde_json::Value {
    serde_json::json!({
        "source_qualified": edge.source_qualified,
        "target_qualified": edge.target_qualified,
        "kind": edge.kind.as_str(),
        "file_path": edge.file_path,
        "line": edge.line,
    })
}
