//! Vector embedding support for semantic search.
//!
//! Stores embeddings in a dedicated SQLite database (`embeddings.db`).
//! Embedding providers (sentence-transformers, Google Gemini) are not yet
//! available in Rust — `available()` always returns false.  Keyword search
//! is used as a fallback in all callers.

use std::path::Path;

use rusqlite::{Connection, params};

use crate::error::Result;
use crate::graph::GraphStore;
use crate::types::{GraphNode, node_to_dict};

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

const EMBEDDINGS_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS embeddings (
    qualified_name TEXT PRIMARY KEY,
    vector BLOB NOT NULL,
    text_hash TEXT NOT NULL,
    provider TEXT NOT NULL DEFAULT 'unknown'
);
";

// ---------------------------------------------------------------------------
// EmbeddingStore
// ---------------------------------------------------------------------------

/// Embedding storage backed by SQLite.
pub struct EmbeddingStore {
    conn: Connection,
}

impl EmbeddingStore {
    /// Open the embedding store (uses the same DB as GraphStore).
    pub fn new(db_path: &Path) -> Result<Self> {
        let conn = Connection::open(db_path)?;
        conn.execute_batch(EMBEDDINGS_SCHEMA)?;
        // Migration: add provider column to existing DBs that don't have it
        let has_provider: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('embeddings') WHERE name='provider'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(0)
            > 0;
        if !has_provider {
            conn.execute_batch(
                "ALTER TABLE embeddings ADD COLUMN provider TEXT NOT NULL DEFAULT 'unknown'",
            )?;
        }
        Ok(Self { conn })
    }

    /// Whether the embedding provider is available.
    ///
    /// Always returns `false` — no sentence-transformers equivalent in Rust yet.
    pub fn available(&self) -> bool {
        false
    }

    /// Count embedded nodes.
    pub fn count(&self) -> Result<usize> {
        let n: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM embeddings", [], |row| row.get(0))?;
        Ok(n as usize)
    }

    /// Search for nodes by semantic similarity.
    ///
    /// Returns an empty list because no provider is available yet.
    pub fn search(&self, _query: &str, _limit: usize) -> Result<Vec<(String, f32)>> {
        Ok(vec![])
    }

    /// Remove a node's embedding.
    #[allow(dead_code)]
    pub fn remove_node(&self, qualified_name: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM embeddings WHERE qualified_name = ?1",
            params![qualified_name],
        )?;
        Ok(())
    }

    /// Close the store.
    pub fn close(self) -> Result<()> {
        drop(self.conn);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Free functions
// ---------------------------------------------------------------------------

/// Embed all graph nodes that don't already have embeddings.
///
/// Always returns 0 because no embedding provider is available yet.
pub fn embed_all_nodes(
    _store: &GraphStore,
    _emb_store: &EmbeddingStore,
) -> Result<usize> {
    Ok(0)
}

/// Semantic search across embedded nodes.
///
/// Falls back to keyword search via `GraphStore::search_nodes` when no
/// embeddings are available (which is always the case for now).
pub fn semantic_search(
    query: &str,
    store: &GraphStore,
    emb_store: &EmbeddingStore,
    limit: usize,
) -> Result<Vec<serde_json::Value>> {
    if emb_store.available() {
        let scored = emb_store.search(query, limit)?;
        let mut out = Vec::with_capacity(scored.len());
        for (qn, score) in scored {
            if let Some(node) = store.get_node(&qn)? {
                let mut d = node_to_dict(&node);
                d["similarity_score"] =
                    serde_json::Value::from((score * 10_000.0).round() / 10_000.0);
                out.push(d);
            }
        }
        return Ok(out);
    }

    // Keyword fallback
    let nodes = store.search_nodes(query, limit)?;
    Ok(nodes.iter().map(node_to_dict).collect())
}

/// Convert a GraphNode to embeddable text (mirrors Python `_node_to_text`).
#[allow(dead_code)]
pub fn node_to_text(node: &GraphNode) -> String {
    let kind_lower = node.kind.as_str().to_lowercase();
    let mut parts: Vec<&str> = vec![&node.name];
    // parent_name is not tracked on GraphNode yet; skip.
    if node.kind.as_str() != "File" {
        parts.push(&kind_lower);
    }
    // signature approximates params + return type
    if !node.signature.is_empty() {
        parts.push(&node.signature);
    }
    if !node.language.is_empty() {
        parts.push(&node.language);
    }
    parts.join(" ")
}
