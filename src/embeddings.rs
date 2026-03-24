//! Vector embedding support for semantic search.
//!
//! Stores embeddings in a dedicated `.embeddings.bin.zst` file using the same
//! bincode/zstd pattern as `graph.rs`.  Embedding providers (sentence-transformers,
//! Google Gemini) are not yet available in Rust — `available()` always returns false.
//! Keyword search is used as a fallback in all callers.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{CrgError, Result};
use crate::graph::GraphStore;
use crate::types::{GraphNode, node_to_dict};

// ---------------------------------------------------------------------------
// File format (same magic as graph.rs)
// ---------------------------------------------------------------------------

const MAGIC: &[u8; 4] = b"CRG\x01";

// ---------------------------------------------------------------------------
// Serialisable data
// ---------------------------------------------------------------------------

/// All embedding state that gets serialised to disk.
#[derive(Serialize, Deserialize, Default)]
struct EmbeddingData {
    /// qualified_name → (vector, text_hash, provider)
    vectors: HashMap<String, (Vec<f32>, String, String)>,
}

// ---------------------------------------------------------------------------
// Persistence helpers
// ---------------------------------------------------------------------------

fn save_embedding_data(data: &EmbeddingData, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let payload = bincode::serialize(data)?;
    let compressed = zstd::encode_all(&payload[..], 3).map_err(CrgError::Io)?;
    let crc = crc32fast::hash(&compressed);

    let tmp =
        tempfile::NamedTempFile::new_in(path.parent().unwrap_or(Path::new(".")))?;
    {
        let mut f = tmp.as_file();
        f.write_all(MAGIC)?;
        f.write_all(&crc.to_le_bytes())?;
        f.write_all(&compressed)?;
        f.flush()?;
    }
    tmp.persist(path).map_err(|e| CrgError::Io(e.error))?;
    Ok(())
}

fn load_embedding_data(path: &Path) -> Result<EmbeddingData> {
    let bytes = std::fs::read(path)?;
    if bytes.len() < 8 {
        return Err(CrgError::Other("embeddings file too short".into()));
    }
    if &bytes[0..4] != MAGIC {
        return Err(CrgError::Other(
            "corrupt embeddings file (bad magic)".into(),
        ));
    }
    let stored_crc = u32::from_le_bytes(
        bytes[4..8]
            .try_into()
            .map_err(|_| CrgError::Other("corrupt embeddings file (bad crc field)".into()))?,
    );
    let compressed = &bytes[8..];
    if crc32fast::hash(compressed) != stored_crc {
        return Err(CrgError::Other("embeddings file CRC mismatch".into()));
    }
    let decompressed =
        zstd::decode_all(compressed).map_err(CrgError::Io)?;
    let data: EmbeddingData = bincode::deserialize(&decompressed)?;
    Ok(data)
}

// ---------------------------------------------------------------------------
// EmbeddingStore
// ---------------------------------------------------------------------------

/// Embedding storage backed by a bincode/zstd file.
pub struct EmbeddingStore {
    data: EmbeddingData,
    path: PathBuf,
}

impl EmbeddingStore {
    /// Open (or create) the embedding store.
    ///
    /// `store_path` should point to a `.embeddings.bin.zst` file.
    pub fn new(store_path: &Path) -> Result<Self> {
        let data = if store_path.exists() {
            match load_embedding_data(store_path) {
                Ok(d) => d,
                Err(e) => {
                    log::warn!(
                        "Could not load embeddings from {}: {} — starting empty",
                        store_path.display(),
                        e
                    );
                    EmbeddingData::default()
                }
            }
        } else {
            EmbeddingData::default()
        };
        Ok(Self {
            data,
            path: store_path.to_path_buf(),
        })
    }

    /// Whether the embedding provider is available.
    ///
    /// Always returns `false` — no sentence-transformers equivalent in Rust yet.
    pub fn available(&self) -> bool {
        false
    }

    /// Count embedded nodes.
    pub fn count(&self) -> Result<usize> {
        Ok(self.data.vectors.len())
    }

    /// Search for nodes by semantic similarity.
    ///
    /// Returns an empty list because no provider is available yet.
    pub fn search(&self, _query: &str, _limit: usize) -> Result<Vec<(String, f32)>> {
        Ok(vec![])
    }

    /// Remove a node's embedding.
    #[allow(dead_code)]
    pub fn remove_node(&mut self, qualified_name: &str) -> Result<()> {
        self.data.vectors.remove(qualified_name);
        Ok(())
    }

    /// Persist in-memory state to disk.
    #[allow(dead_code)]
    pub fn save(&self) -> Result<()> {
        save_embedding_data(&self.data, &self.path)
    }

    /// Close the store (no-op — nothing to flush unless explicitly saved).
    pub fn close(self) -> Result<()> {
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
