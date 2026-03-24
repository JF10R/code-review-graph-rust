//! Shared types used across all modules.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "File" => Some(Self::File),
            "Class" => Some(Self::Class),
            "Function" => Some(Self::Function),
            "Type" => Some(Self::Type),
            "Test" => Some(Self::Test),
            _ => None,
        }
    }
}

impl std::fmt::Display for NodeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "CALLS" => Some(Self::Calls),
            "IMPORTS_FROM" => Some(Self::ImportsFrom),
            "CONTAINS" => Some(Self::Contains),
            "INHERITS" => Some(Self::Inherits),
            "IMPLEMENTS" => Some(Self::Implements),
            "TESTED_BY" => Some(Self::TestedBy),
            _ => None,
        }
    }
}

impl std::fmt::Display for EdgeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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
    pub nodes_by_kind: HashMap<String, usize>,
    pub edges_by_kind: HashMap<String, usize>,
    pub languages: Vec<String>,
    pub files_count: usize,
    pub last_updated: Option<String>,
}

// ---------------------------------------------------------------------------
// Impact radius result
// ---------------------------------------------------------------------------

/// Result of an impact radius analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImpactResult {
    pub changed_nodes: Vec<GraphNode>,
    pub impacted_nodes: Vec<GraphNode>,
    pub impacted_files: Vec<String>,
    pub edges: Vec<GraphEdge>,
    pub truncated: bool,
    pub total_impacted: usize,
}

// ---------------------------------------------------------------------------
// Serialization helpers (dict representations for MCP responses)
// ---------------------------------------------------------------------------

/// Convert a GraphNode to a JSON-serializable map.
pub fn node_to_dict(node: &GraphNode) -> serde_json::Value {
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
