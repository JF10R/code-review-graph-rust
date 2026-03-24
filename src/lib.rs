//! code-review-graph — Persistent incremental knowledge graph for code reviews.
//!
//! Parses codebases with tree-sitter, builds a structural graph in SQLite,
//! and provides smart impact analysis via MCP tools.

pub mod config;
pub mod embeddings;
pub mod error;
pub mod graph;
pub mod incremental;
pub mod parser;
pub mod server;
pub mod tools;
pub mod tsconfig;
pub mod types;
pub mod visualization;

#[cfg(feature = "tantivy-search")]
pub mod search;
