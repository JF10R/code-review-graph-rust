//! Vector embedding support for semantic search.
//!
//! Stores embeddings in the same SQLite database as the graph.
//! Supports local sentence-transformers and Google Gemini providers.

use std::path::Path;

use crate::error::Result;
use crate::types::GraphNode;

/// Embedding storage backed by SQLite.
pub struct EmbeddingStore {
    _available: bool,
}

impl EmbeddingStore {
    /// Open the embedding store (uses the same DB as GraphStore).
    pub fn new(db_path: &Path) -> Result<Self> {
        let _ = db_path;
        todo!()
    }

    /// Whether the embedding provider is available.
    pub fn available(&self) -> bool {
        self._available
    }

    /// Count embedded nodes.
    pub fn count(&self) -> Result<usize> {
        todo!()
    }

    /// Close the store.
    pub fn close(self) -> Result<()> {
        todo!()
    }
}

/// Embed all graph nodes that don't already have embeddings.
pub fn embed_all_nodes(
    _store: &crate::graph::GraphStore,
    _emb_store: &EmbeddingStore,
) -> Result<usize> {
    todo!()
}

/// Semantic search across embedded nodes.
pub fn semantic_search(
    _query: &str,
    _store: &crate::graph::GraphStore,
    _emb_store: &EmbeddingStore,
    _limit: usize,
) -> Result<Vec<serde_json::Value>> {
    todo!()
}

/// Convert a GraphNode to embeddable text.
pub fn node_to_text(node: &GraphNode) -> String {
    format!(
        "{} {} in {} ({})",
        node.kind, node.name, node.file_path, node.language
    )
}
