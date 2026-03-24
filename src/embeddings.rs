//! Vector embedding support for semantic search.
//!
//! The default provider is a local candle-based all-MiniLM-L6-v2 model — no
//! API key required.  API providers (OpenAI, Voyage, Gemini) take priority when
//! `EMBEDDING_PROVIDER` is set explicitly.
//!
//! ```text
//! EMBEDDING_PROVIDER=openai|voyage|gemini|none   (unset → local candle model)
//! OPENAI_API_KEY=sk-...
//! VOYAGE_API_KEY=pa-...
//! GEMINI_API_KEY=...
//! EMBEDDING_MODEL=<model>   (provider-specific default if not set)
//! ```
//!
//! Stores embeddings in a `.embeddings.bin.zst` file using the same
//! bincode/zstd pattern as `graph.rs`.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{CrgError, Result};
use crate::graph::GraphStore;
use crate::incremental::sha256_bytes_pub;
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
// Candle local provider (default when embeddings-local feature is enabled)
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

                // Single pass: build all three tensor lists (tokenizer already padded to batch-longest)
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

    // Helper: env var first, then config file fallback.
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

    // Default: free local provider via candle (all-MiniLM-L6-v2)
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

/// Embedding storage backed by a bincode/zstd file.
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

    let mut scored: Vec<(String, f64)> = emb_store
        .data
        .vectors
        .iter()
        .map(|(qn, (vec, _, _))| (qn.clone(), cosine_similarity(query_vec, vec)))
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(limit);

    let mut results = Vec::new();
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
