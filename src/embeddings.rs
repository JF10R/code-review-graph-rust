//! Vector embedding support for semantic search.
//!
//! The default provider is a local fastembed JinaEmbeddingsV2BaseCode model
//! (`embeddings-fastembed` feature) — no API key required, 768-dimensional
//! code-optimised embeddings.  Falls back to the candle all-MiniLM-L6-v2 model
//! (`embeddings-local` feature) if fastembed is unavailable.  API providers
//! (OpenAI, Voyage, Gemini) take priority when `EMBEDDING_PROVIDER` is set.
//!
//! ```text
//! EMBEDDING_PROVIDER=openai|voyage|gemini|none   (unset → fastembed local model)
//! OPENAI_API_KEY=sk-...
//! VOYAGE_API_KEY=pa-...
//! GEMINI_API_KEY=...
//! EMBEDDING_MODEL=<model>   (provider-specific default if not set)
//! ```
//!
//! Stores embeddings in a `.embeddings.bin.zst` file using the same
//! postcard/zstd pattern as `graph.rs`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[cfg(feature = "hnsw-index")]
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

use serde::{Deserialize, Serialize};

use crate::error::{CrgError, Result};
use crate::graph::GraphStore;
use crate::incremental::sha256_bytes_pub;
use crate::persistence;
use crate::types::{GraphNode, node_to_dict};

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
// Persistence helpers (delegates to crate::persistence)
// ---------------------------------------------------------------------------

fn save_embedding_data(data: &EmbeddingData, path: &Path) -> Result<()> {
    persistence::save_blob(data, path, "embeddings")
}

fn load_embedding_data(path: &Path) -> Result<EmbeddingData> {
    persistence::load_blob(path, "embeddings")
}

fn text_hash(text: &str) -> String {
    sha256_bytes_pub(text.as_bytes())[..16].to_string()
}

// ---------------------------------------------------------------------------
// Provider trait
// ---------------------------------------------------------------------------

#[allow(dead_code)]
trait EmbeddingProvider: Send + Sync {
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    fn dimensions(&self) -> usize;
    fn name(&self) -> &str;
}

// ---------------------------------------------------------------------------
// OpenAI provider
// ---------------------------------------------------------------------------

struct OpenAiProvider {
    api_key: String,
    model: String,
    client: reqwest::blocking::Client,
}

impl OpenAiProvider {
    fn new(api_key: String, model: String) -> Self {
        Self {
            api_key,
            model,
            client: reqwest::blocking::Client::new(),
        }
    }
}

impl EmbeddingProvider for OpenAiProvider {
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let body = serde_json::json!({
            "input": texts,
            "model": self.model,
        });

        let resp = self
            .client
            .post("https://api.openai.com/v1/embeddings")
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .map_err(|e| CrgError::Other(format!("OpenAI request failed: {e}")))?;

        let status = resp.status();
        let resp_json: serde_json::Value = resp
            .json()
            .map_err(|e| CrgError::Other(format!("OpenAI response parse error: {e}")))?;

        if !status.is_success() {
            let msg = resp_json["error"]["message"]
                .as_str()
                .unwrap_or("unknown error");
            return Err(CrgError::Other(format!("OpenAI API error {status}: {msg}")));
        }

        parse_data_embeddings(&resp_json, texts.len())
    }

    fn dimensions(&self) -> usize {
        if self.model.contains("3-large") { 3072 } else { 1536 }
    }

    fn name(&self) -> &str {
        "openai"
    }
}

/// Parse the `{"data": [{"embedding": [...]}]}` response format (OpenAI + Voyage).
fn parse_data_embeddings(resp: &serde_json::Value, expected: usize) -> Result<Vec<Vec<f32>>> {
    let data = resp["data"]
        .as_array()
        .ok_or_else(|| CrgError::Other("response missing 'data' array".into()))?;

    if data.len() != expected {
        return Err(CrgError::Other(format!(
            "expected {} embeddings, got {}",
            expected,
            data.len(),
        )));
    }

    let mut vecs = Vec::with_capacity(expected);
    for item in data {
        let arr = item["embedding"]
            .as_array()
            .ok_or_else(|| CrgError::Other("embedding item missing 'embedding' field".into()))?;
        let vec: Vec<f32> = arr
            .iter()
            .map(|v| v.as_f64().unwrap_or(0.0) as f32)
            .collect();
        vecs.push(vec);
    }
    Ok(vecs)
}

// ---------------------------------------------------------------------------
// Voyage AI provider
// ---------------------------------------------------------------------------

struct VoyageProvider {
    api_key: String,
    model: String,
    client: reqwest::blocking::Client,
}

impl VoyageProvider {
    fn new(api_key: String, model: String) -> Self {
        Self {
            api_key,
            model,
            client: reqwest::blocking::Client::new(),
        }
    }
}

impl EmbeddingProvider for VoyageProvider {
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let body = serde_json::json!({
            "input": texts,
            "model": self.model,
        });

        let resp = self
            .client
            .post("https://api.voyageai.com/v1/embeddings")
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .map_err(|e| CrgError::Other(format!("Voyage AI request failed: {e}")))?;

        let status = resp.status();
        let resp_json: serde_json::Value = resp
            .json()
            .map_err(|e| CrgError::Other(format!("Voyage AI response parse error: {e}")))?;

        if !status.is_success() {
            let msg = resp_json["detail"]
                .as_str()
                .unwrap_or("unknown error");
            return Err(CrgError::Other(format!("Voyage AI API error {status}: {msg}")));
        }

        parse_data_embeddings(&resp_json, texts.len())
    }

    fn dimensions(&self) -> usize {
        1024
    }

    fn name(&self) -> &str {
        "voyage"
    }
}

// ---------------------------------------------------------------------------
// Gemini provider
// ---------------------------------------------------------------------------

struct GeminiProvider {
    api_key: String,
    model: String,
    client: reqwest::blocking::Client,
}

impl GeminiProvider {
    fn new(api_key: String, model: String) -> Self {
        Self {
            api_key,
            model,
            client: reqwest::blocking::Client::new(),
        }
    }
}

impl EmbeddingProvider for GeminiProvider {
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:batchEmbedContents?key={}",
            self.model, self.api_key
        );

        let requests: Vec<serde_json::Value> = texts
            .iter()
            .map(|t| {
                serde_json::json!({
                    "model": format!("models/{}", self.model),
                    "content": { "parts": [{ "text": t }] }
                })
            })
            .collect();

        let body = serde_json::json!({ "requests": requests });

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .map_err(|e| CrgError::Other(format!("Gemini request failed: {e}")))?;

        let status = resp.status();
        let resp_json: serde_json::Value = resp
            .json()
            .map_err(|e| CrgError::Other(format!("Gemini response parse error: {e}")))?;

        if !status.is_success() {
            let msg = resp_json["error"]["message"]
                .as_str()
                .unwrap_or("unknown error");
            return Err(CrgError::Other(format!("Gemini API error {status}: {msg}")));
        }

        let embeddings = resp_json["embeddings"]
            .as_array()
            .ok_or_else(|| CrgError::Other("Gemini response missing 'embeddings' array".into()))?;

        if embeddings.len() != texts.len() {
            return Err(CrgError::Other(format!(
                "Gemini returned {} embeddings, expected {}",
                embeddings.len(),
                texts.len()
            )));
        }

        let mut vecs = Vec::with_capacity(texts.len());
        for item in embeddings {
            let arr = item["values"]
                .as_array()
                .ok_or_else(|| CrgError::Other("Gemini embedding missing 'values' field".into()))?;
            let vec: Vec<f32> = arr
                .iter()
                .map(|v| v.as_f64().unwrap_or(0.0) as f32)
                .collect();
            vecs.push(vec);
        }
        Ok(vecs)
    }

    fn dimensions(&self) -> usize {
        768
    }

    fn name(&self) -> &str {
        "gemini"
    }
}

// ---------------------------------------------------------------------------
// FastEmbed provider (default when embeddings-fastembed feature is enabled)
// ---------------------------------------------------------------------------

#[cfg(feature = "embeddings-fastembed")]
struct FastEmbedProvider {
    model: fastembed::TextEmbedding,
}

#[cfg(feature = "embeddings-fastembed")]
impl FastEmbedProvider {
    fn new() -> Result<Self> {
        let model = fastembed::TextEmbedding::try_new(
            fastembed::InitOptions::new(fastembed::EmbeddingModel::JinaEmbeddingsV2BaseCode)
                .with_show_download_progress(true),
        )
        .map_err(|e| CrgError::Other(format!("fastembed init: {e}")))?;
        Ok(Self { model })
    }
}

#[cfg(feature = "embeddings-fastembed")]
impl EmbeddingProvider for FastEmbedProvider {
    fn name(&self) -> &str {
        "fastembed-jina-v2-code"
    }

    fn dimensions(&self) -> usize {
        768
    }

    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let embeddings = self
            .model
            .embed(texts.to_vec(), None)
            .map_err(|e| CrgError::Other(format!("fastembed embed: {e}")))?;
        Ok(embeddings)
    }
}

// ---------------------------------------------------------------------------
// Candle local provider (legacy when embeddings-local feature is enabled)
// ---------------------------------------------------------------------------

#[cfg(feature = "embeddings-local")]
mod candle_impl {
    use super::*;

    use candle_core::{DType, Device, Tensor};
    use candle_nn::VarBuilder;
    use candle_transformers::models::bert::{BertModel, Config, DTYPE};
    use hf_hub::{api::sync::Api, Repo, RepoType};
    use tokenizers::{PaddingParams, PaddingStrategy, Tokenizer};

    pub struct CandleProvider {
        model: BertModel,
        tokenizer: Tokenizer,
        device: Device,
    }

    impl CandleProvider {
        pub fn new() -> Result<Self> {
            let device = Device::Cpu;

            let api = Api::new()
                .map_err(|e| CrgError::Other(format!("HF Hub init: {e}")))?;
            let repo = api.repo(Repo::with_revision(
                "sentence-transformers/all-MiniLM-L6-v2".to_string(),
                RepoType::Model,
                "main".to_string(),
            ));

            let model_path = repo
                .get("model.safetensors")
                .map_err(|e| CrgError::Other(format!("Download model weights: {e}")))?;
            let tokenizer_path = repo
                .get("tokenizer.json")
                .map_err(|e| CrgError::Other(format!("Download tokenizer: {e}")))?;
            let config_path = repo
                .get("config.json")
                .map_err(|e| CrgError::Other(format!("Download config: {e}")))?;

            let config: Config = serde_json::from_str(&std::fs::read_to_string(&config_path)?)
                .map_err(|e| CrgError::Other(format!("Parse config.json: {e}")))?;

            // Safety: file is not mutated while the process runs
            let vb = unsafe {
                VarBuilder::from_mmaped_safetensors(&[model_path], DTYPE, &device)
                    .map_err(|e| CrgError::Other(format!("Load weights: {e}")))?
            };

            let model = BertModel::load(vb, &config)
                .map_err(|e| CrgError::Other(format!("Build BERT model: {e}")))?;

            let mut tokenizer = Tokenizer::from_file(&tokenizer_path)
                .map_err(|e| CrgError::Other(format!("Load tokenizer: {e}")))?;
            // Pad all sequences in a batch to the longest one so Tensor::stack works
            tokenizer.with_padding(Some(PaddingParams {
                strategy: PaddingStrategy::BatchLongest,
                ..Default::default()
            }));

            Ok(Self { model, tokenizer, device })
        }
    }

    impl EmbeddingProvider for CandleProvider {
        fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            let mut all_embeddings = Vec::with_capacity(texts.len());

            for chunk in texts.chunks(32) {
                let encodings = self
                    .tokenizer
                    .encode_batch(chunk.to_vec(), true)
                    .map_err(|e| CrgError::Other(format!("Tokenize batch: {e}")))?;

                // Tokenizer already padded to batch-longest, so all encodings have equal length
                let n = chunk.len();
                let mut ids_list = Vec::with_capacity(n);
                let mut type_ids_list = Vec::with_capacity(n);
                let mut mask_list = Vec::with_capacity(n);
                for enc in &encodings {
                    ids_list.push(
                        Tensor::new(enc.get_ids(), &self.device)
                            .map_err(|e| CrgError::Other(format!("Tensor ids: {e}")))?,
                    );
                    type_ids_list.push(
                        Tensor::new(enc.get_type_ids(), &self.device)
                            .map_err(|e| CrgError::Other(format!("Tensor type_ids: {e}")))?,
                    );
                    mask_list.push(
                        Tensor::new(enc.get_attention_mask(), &self.device)
                            .map_err(|e| CrgError::Other(format!("Tensor mask: {e}")))?,
                    );
                }

                let input_ids = Tensor::stack(&ids_list, 0)
                    .map_err(|e| CrgError::Other(format!("Stack ids: {e}")))?;
                let token_type_ids = Tensor::stack(&type_ids_list, 0)
                    .map_err(|e| CrgError::Other(format!("Stack type_ids: {e}")))?;
                let attention_mask = Tensor::stack(&mask_list, 0)
                    .map_err(|e| CrgError::Other(format!("Stack mask: {e}")))?;

                let output = self
                    .model
                    .forward(&input_ids, &token_type_ids, Some(&attention_mask))
                    .map_err(|e| CrgError::Other(format!("BERT forward: {e}")))?;

                // Mean pooling over non-padding tokens, then L2-normalise
                let mask_f32 = attention_mask
                    .to_dtype(DType::F32)
                    .map_err(|e| CrgError::Other(format!("Mask to f32: {e}")))?;
                let mask_expanded = mask_f32
                    .unsqueeze(2)
                    .and_then(|m| m.broadcast_as(output.shape()))
                    .map_err(|e| CrgError::Other(format!("Expand mask: {e}")))?;
                let mean_pooled = output
                    .broadcast_mul(&mask_expanded)
                    .and_then(|t| t.sum(1))
                    .map_err(|e| CrgError::Other(format!("Sum pooling: {e}")))?
                    .broadcast_div(
                        &mask_expanded
                            .sum(1)
                            .map_err(|e| CrgError::Other(format!("Sum mask: {e}")))?,
                    )
                    .map_err(|e| CrgError::Other(format!("Mean pool div: {e}")))?;
                let normalized = mean_pooled
                    .broadcast_div(
                        &mean_pooled
                            .sqr()
                            .and_then(|s| s.sum_keepdim(1))
                            .and_then(|s| s.sqrt())
                            .map_err(|e| CrgError::Other(format!("L2 norm: {e}")))?,
                    )
                    .map_err(|e| CrgError::Other(format!("L2 div: {e}")))?;

                for i in 0..n {
                    let row = normalized
                        .get(i)
                        .map_err(|e| CrgError::Other(format!("Get row {i}: {e}")))?;
                    all_embeddings.push(
                        row.to_vec1()
                            .map_err(|e| CrgError::Other(format!("Row to vec: {e}")))?,
                    );
                }
            }

            Ok(all_embeddings)
        }

        fn dimensions(&self) -> usize {
            384
        }

        fn name(&self) -> &str {
            "candle-minilm"
        }
    }
}

#[cfg(feature = "embeddings-local")]
use candle_impl::CandleProvider;

// ---------------------------------------------------------------------------
// Provider detection from environment
// ---------------------------------------------------------------------------

fn detect_provider() -> Option<Box<dyn EmbeddingProvider>> {
    let config = crate::config::AppConfig::load();

    let get = |env_key: &str, config_key: &str| -> Option<String> {
        std::env::var(env_key)
            .ok()
            .or_else(|| config.get(config_key).map(|s| s.to_string()))
    };

    // Explicit provider (env var or config) takes priority over local candle default.
    if let Some(provider) = get("EMBEDDING_PROVIDER", "embedding-provider") {
        match provider.to_lowercase().as_str() {
            "openai" => {
                let api_key = get("OPENAI_API_KEY", "openai-api-key")?;
                let model = get("EMBEDDING_MODEL", "embedding-model")
                    .unwrap_or_else(|| "text-embedding-3-small".to_string());
                log::info!("Embedding provider: OpenAI (model={})", model);
                return Some(Box::new(OpenAiProvider::new(api_key, model)));
            }
            "voyage" => {
                let api_key = get("VOYAGE_API_KEY", "voyage-api-key")?;
                let model = get("EMBEDDING_MODEL", "embedding-model")
                    .unwrap_or_else(|| "voyage-code-3".to_string());
                log::info!("Embedding provider: Voyage AI (model={})", model);
                return Some(Box::new(VoyageProvider::new(api_key, model)));
            }
            "gemini" => {
                let api_key = get("GEMINI_API_KEY", "gemini-api-key")?;
                let model = get("EMBEDDING_MODEL", "embedding-model")
                    .unwrap_or_else(|| "text-embedding-004".to_string());
                log::info!("Embedding provider: Gemini (model={})", model);
                return Some(Box::new(GeminiProvider::new(api_key, model)));
            }
            "none" | "disabled" => {
                log::info!("Embeddings explicitly disabled via embedding-provider=none");
                return None;
            }
            other => {
                log::warn!("Unknown embedding-provider='{}'; falling back to local", other);
            }
        }
    }

    // Default: fastembed (JinaEmbeddingsV2BaseCode, local, free)
    #[cfg(feature = "embeddings-fastembed")]
    {
        match FastEmbedProvider::new() {
            Ok(p) => {
                log::info!("Embedding provider: fastembed-jina-v2-code (local, free)");
                return Some(Box::new(p));
            }
            Err(e) => {
                log::warn!("fastembed init failed: {}; trying candle fallback", e);
            }
        }
    }

    // Fallback: candle all-MiniLM-L6-v2
    #[cfg(feature = "embeddings-local")]
    {
        match CandleProvider::new() {
            Ok(p) => {
                log::info!("Embedding provider: candle-minilm (all-MiniLM-L6-v2, local, free)");
                return Some(Box::new(p));
            }
            Err(e) => {
                log::warn!("Local embedding provider unavailable: {}; embeddings disabled", e);
            }
        }
    }

    None
}

// ---------------------------------------------------------------------------
// EmbeddingStore
// ---------------------------------------------------------------------------

/// Embedding storage backed by a postcard/zstd file.
pub struct EmbeddingStore {
    data: EmbeddingData,
    path: PathBuf,
    provider: Option<Box<dyn EmbeddingProvider>>,
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

        let provider = detect_provider();
        Ok(Self {
            data,
            path: store_path.to_path_buf(),
            provider,
        })
    }

    /// Whether an embedding provider is available.
    pub fn available(&self) -> bool {
        self.provider.is_some()
    }

    /// Count embedded nodes.
    pub fn count(&self) -> Result<usize> {
        Ok(self.data.vectors.len())
    }

    /// Remove a node's embedding.
    #[allow(dead_code)]
    pub fn remove_node(&mut self, qualified_name: &str) -> Result<()> {
        self.data.vectors.remove(qualified_name);
        Ok(())
    }

    /// Persist in-memory state to disk.
    pub fn save(&self) -> Result<()> {
        save_embedding_data(&self.data, &self.path)
    }

    /// Close the store (no-op — nothing to flush unless explicitly saved).
    pub fn close(self) -> Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// HNSW index (optional — accelerated approximate nearest-neighbour search)
// ---------------------------------------------------------------------------

#[cfg(feature = "hnsw-index")]
pub struct HnswIndex {
    index: Index,
    key_to_name: HashMap<u64, String>,
}

#[cfg(feature = "hnsw-index")]
impl HnswIndex {
    /// Build an HNSW index from an existing `EmbeddingStore`.
    pub fn build(emb_store: &EmbeddingStore) -> Result<Self> {
        let dims = emb_store
            .data
            .vectors
            .values()
            .next()
            .map(|(v, _, _)| v.len())
            .unwrap_or(768);

        let options = IndexOptions {
            dimensions: dims,
            metric: MetricKind::Cos,
            quantization: ScalarKind::F32,
            ..Default::default()
        };

        let index = Index::new(&options)
            .map_err(|e| CrgError::Other(format!("HNSW init: {e}")))?;
        index
            .reserve(emb_store.data.vectors.len())
            .map_err(|e| CrgError::Other(format!("HNSW reserve: {e}")))?;

        let mut key_to_name = HashMap::new();
        for (i, (qn, (vec, _, _))) in emb_store.data.vectors.iter().enumerate() {
            let key = i as u64;
            index
                .add(key, vec)
                .map_err(|e| CrgError::Other(format!("HNSW add: {e}")))?;
            key_to_name.insert(key, qn.clone());
        }

        Ok(Self { index, key_to_name })
    }

    /// Search for the `k` nearest neighbours of `query_vec`.
    ///
    /// Returns `(qualified_name, cosine_similarity)` pairs sorted by
    /// descending similarity (1.0 = identical, 0.0 = orthogonal).
    pub fn search(&self, query_vec: &[f32], k: usize) -> Vec<(String, f32)> {
        match self.index.search(query_vec, k) {
            Ok(results) => results
                .keys
                .iter()
                .zip(results.distances.iter())
                .filter_map(|(&key, &dist)| {
                    self.key_to_name
                        .get(&key)
                        .map(|name| (name.clone(), 1.0 - dist))
                })
                .collect(),
            Err(_) => vec![],
        }
    }
}

// ---------------------------------------------------------------------------
// Free functions
// ---------------------------------------------------------------------------

/// Embed all graph nodes that don't already have up-to-date embeddings.
///
/// Returns the number of newly embedded nodes.  Returns 0 if no provider
/// is configured.
pub fn embed_all_nodes(
    store: &GraphStore,
    emb_store: &mut EmbeddingStore,
) -> Result<usize> {
    let provider = match &emb_store.provider {
        Some(p) => p,
        None => return Ok(0),
    };

    // (qualified_name, text, hash) — hash computed once here, reused on insert
    let mut to_embed: Vec<(String, String, String)> = vec![];
    for file in store.get_all_files()? {
        for node in store.get_nodes_by_file(&file)? {
            let text = node_to_text(&node);
            let hash = text_hash(&text);
            if let Some((_, existing_hash, _)) = emb_store.data.vectors.get(&node.qualified_name) {
                if *existing_hash == hash {
                    continue;
                }
            }
            to_embed.push((node.qualified_name.clone(), text, hash));
        }
    }

    if to_embed.is_empty() {
        return Ok(0);
    }

    let provider_name = provider.name().to_string();
    let mut count = 0;

    for chunk in to_embed.chunks(100) {
        let texts: Vec<String> = chunk.iter().map(|(_, t, _)| t.clone()).collect();
        let vectors = provider.embed_batch(&texts)?;
        for ((qn, _, hash), vec) in chunk.iter().zip(vectors) {
            emb_store.data.vectors.insert(
                qn.clone(),
                (vec, hash.clone(), provider_name.clone()),
            );
            count += 1;
        }
    }

    emb_store.save()?;
    Ok(count)
}

/// Semantic search across embedded nodes.
///
/// Uses cosine similarity when embeddings are available; falls back to
/// keyword search via `GraphStore::search_nodes` otherwise.
pub fn semantic_search(
    query: &str,
    store: &GraphStore,
    emb_store: &mut EmbeddingStore,
    limit: usize,
) -> Result<Vec<serde_json::Value>> {
    let provider = match &emb_store.provider {
        Some(p) => p,
        None => {
            let nodes = store.search_nodes(query, limit)?;
            return Ok(nodes.iter().map(node_to_dict).collect());
        }
    };

    let query_vecs = provider.embed_batch(&[query.to_string()])?;
    let query_vec = &query_vecs[0];

    // Use HNSW approximate nearest-neighbour search when the feature is compiled in.
    #[cfg(feature = "hnsw-index")]
    {
        match HnswIndex::build(emb_store) {
            Ok(idx) => {
                let scored = idx
                    .search(query_vec, limit)
                    .into_iter()
                    .map(|(qn, s)| (qn, s as f64))
                    .collect::<Vec<_>>();
                return nodes_from_scored(scored, store);
            }
            Err(e) => {
                log::warn!("HNSW index build failed ({}); falling back to linear scan", e);
            }
        }
    }

    // Linear scan (also the only path without hnsw-index feature).
    let mut scored: Vec<(String, f64)> = emb_store
        .data
        .vectors
        .iter()
        .map(|(qn, (vec, _, _))| (qn.clone(), cosine_similarity(query_vec, vec)))
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(limit);
    nodes_from_scored(scored, store)
}

/// Resolve a ranked `(qualified_name, score)` list to full node dicts.
fn nodes_from_scored(
    scored: Vec<(String, f64)>,
    store: &GraphStore,
) -> Result<Vec<serde_json::Value>> {
    let mut results = Vec::with_capacity(scored.len());
    for (qn, score) in scored {
        if let Some(node) = store.get_node(&qn)? {
            let mut d = node_to_dict(&node);
            d["similarity_score"] =
                serde_json::Value::from((score * 10_000.0).round() / 10_000.0);
            results.push(d);
        }
    }
    Ok(results)
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() {
        log::error!("cosine_similarity: dimension mismatch {} vs {}", a.len(), b.len());
        return 0.0;
    }
    let dot: f64 = a.iter().zip(b).map(|(x, y)| (*x as f64) * (*y as f64)).sum();
    let norm_a: f64 = a.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    let norm_b: f64 = b.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

/// Convert a GraphNode to embeddable text (mirrors Python `_node_to_text`).
pub fn node_to_text(node: &GraphNode) -> String {
    let kind_lower = node.kind.as_str().to_lowercase();
    let mut parts: Vec<&str> = vec![&node.name];
    if node.kind.as_str() != "File" {
        parts.push(&kind_lower);
    }
    if !node.signature.is_empty() {
        parts.push(&node.signature);
    }
    if !node.language.is_empty() {
        parts.push(&node.language);
    }
    parts.join(" ")
}
