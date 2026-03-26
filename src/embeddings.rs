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

use std::collections::{HashMap, HashSet};
use camino::{Utf8Path, Utf8PathBuf};

#[cfg(feature = "hnsw-index")]
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

use serde::{Deserialize, Serialize};

use crate::error::{CrgError, Result};
use crate::graph::GraphStore;
use crate::incremental::sha256_bytes_pub;
use crate::persistence;
use crate::types::{GraphNode, node_to_dict};

// ---------------------------------------------------------------------------
// Format version
// ---------------------------------------------------------------------------

const FORMAT_VERSION: u32 = 3;

// ---------------------------------------------------------------------------
// V2 serialisable data
// ---------------------------------------------------------------------------

/// Store-level metadata written once per file.
#[derive(Serialize, Deserialize, Clone, Debug)]
struct StoreMetadata {
    /// Format version (currently 2).
    version: u32,
    /// Embedding provider name (e.g. `"fastembed-jina-v2-code"`).
    provider: String,
    /// Model name (e.g. `"JinaEmbeddingsV2BaseCode"`).
    model: String,
    /// Vector dimensionality.
    dimensions: u32,
}

impl Default for StoreMetadata {
    fn default() -> Self {
        Self {
            version: FORMAT_VERSION,
            provider: String::new(),
            model: String::new(),
            dimensions: 0,
        }
    }
}

/// Per-node entry: (string_table_index, vector_index, text_hash).
///
/// `string_idx` is an index into `EmbeddingDataV2::names`.
/// `vector_idx` is an index into `EmbeddingDataV2::vectors`.
/// `text_hash` is the first 8 raw bytes of SHA-256 of the node text.
#[derive(Serialize, Deserialize, Clone)]
struct NodeEntry {
    string_idx: u32,
    vector_idx: u32,
    text_hash: [u8; 8],
}

/// V2 on-disk format: metadata header + string table + deduplicated vectors + entries.
#[derive(Serialize, Deserialize, Default)]
struct EmbeddingDataV2 {
    meta: StoreMetadata,
    /// Deduplicated qualified names (string table).
    names: Vec<String>,
    /// Deduplicated embedding vectors.
    vectors: Vec<Vec<f32>>,
    /// Per-node entries referencing the string table and vector table.
    entries: Vec<NodeEntry>,
}

impl EmbeddingDataV2 {
    /// Build the runtime lookup: qualified_name → (vector_idx, text_hash).
    fn build_lookup(&self) -> HashMap<String, (usize, [u8; 8])> {
        let mut map = HashMap::with_capacity(self.entries.len());
        for entry in &self.entries {
            if let Some(name) = self.names.get(entry.string_idx as usize) {
                map.insert(
                    name.clone(),
                    (entry.vector_idx as usize, entry.text_hash),
                );
            }
        }
        map
    }

    /// Build reverse map: name → string_idx.
    fn build_name_to_idx(&self) -> HashMap<String, u32> {
        self.names.iter().enumerate().map(|(i, n)| (n.clone(), i as u32)).collect()
    }

    /// Build reverse map: text_hash → vector_idx.
    fn build_hash_to_vec(&self) -> HashMap<[u8; 8], u32> {
        let mut map = HashMap::with_capacity(self.entries.len());
        for entry in &self.entries {
            map.insert(entry.text_hash, entry.vector_idx);
        }
        map
    }
}

// ---------------------------------------------------------------------------
// V3 serialisable data (f16 vector storage — ~50% smaller on disk)
// ---------------------------------------------------------------------------

/// V3 store-level metadata (extends V2 with storage_dtype).
#[derive(Serialize, Deserialize, Clone, Debug)]
struct StoreMetadataV3 {
    version: u32,
    provider: String,
    model: String,
    dimensions: u32,
    /// Storage data type: `"f16"` for V3 format.
    storage_dtype: String,
}

/// V3 on-disk format: f16 vectors for ~50% size reduction.
///
/// Vectors are stored as `u16` raw bits of IEEE 754 binary16 (half precision).
/// Promoted to `f32` at load time for runtime computation.
#[derive(Serialize, Deserialize)]
struct EmbeddingDataV3 {
    meta: StoreMetadataV3,
    names: Vec<String>,
    vectors_f16: Vec<Vec<u16>>,
    entries: Vec<NodeEntry>,
}

/// Convert f32 vector to f16 raw bits for storage.
fn f32_vec_to_f16(v: &[f32]) -> Vec<u16> {
    v.iter().map(|&x| half::f16::from_f32(x).to_bits()).collect()
}

/// Convert f16 raw bits to f32 vector for runtime.
fn f16_vec_to_f32(v: &[u16]) -> Vec<f32> {
    v.iter().map(|&bits| half::f16::from_bits(bits).to_f32()).collect()
}

/// Convert V3 on-disk data (f16 vectors) to V2 runtime format (f32 vectors).
fn v3_to_runtime(v3: EmbeddingDataV3) -> EmbeddingDataV2 {
    EmbeddingDataV2 {
        meta: StoreMetadata {
            version: FORMAT_VERSION,
            provider: v3.meta.provider,
            model: v3.meta.model,
            dimensions: v3.meta.dimensions,
        },
        names: v3.names,
        vectors: v3.vectors_f16.iter().map(|v| f16_vec_to_f32(v)).collect(),
        entries: v3.entries,
    }
}

// ---------------------------------------------------------------------------
// V1 serialisable data (for migration only)
// ---------------------------------------------------------------------------

/// V1 on-disk format: qualified_name → (vector, hex_hash, provider).
#[derive(Serialize, Deserialize, Default)]
struct EmbeddingDataV1 {
    vectors: HashMap<String, (Vec<f32>, String, String)>,
}

// ---------------------------------------------------------------------------
// Persistence helpers
// ---------------------------------------------------------------------------

fn save_embedding_data(data: &EmbeddingDataV2, path: &std::path::Path) -> Result<()> {
    let v3 = EmbeddingDataV3 {
        meta: StoreMetadataV3 {
            version: FORMAT_VERSION,
            provider: data.meta.provider.clone(),
            model: data.meta.model.clone(),
            dimensions: data.meta.dimensions,
            storage_dtype: "f16".to_string(),
        },
        names: data.names.clone(),
        vectors_f16: data.vectors.iter().map(|v| f32_vec_to_f16(v)).collect(),
        entries: data.entries.clone(),
    };
    persistence::save_blob(&v3, path, "embeddings")
}

fn load_embedding_data(path: &std::path::Path) -> Result<EmbeddingDataV2> {
    // Try V3 first (f16 vectors on disk).
    if let Ok(v3) = persistence::load_blob::<EmbeddingDataV3>(path, "embeddings") {
        if v3.meta.version == 3 {
            tracing::info!(
                "embeddings: loaded v3 store ({} entries, storage=f16)",
                v3.entries.len(),
            );
            return Ok(v3_to_runtime(v3));
        }
    }

    // Try V2 (f32 vectors on disk).
    match persistence::load_blob::<EmbeddingDataV2>(path, "embeddings") {
        Ok(d) if d.meta.version == 2 => return Ok(d),
        Ok(d) => {
            tracing::warn!(
                "embeddings: unrecognised format version {} in {:?}; starting empty",
                d.meta.version,
                path
            );
            return Ok(EmbeddingDataV2::default());
        }
        Err(_) => {}
    }

    // Try V1 migration.
    match persistence::load_blob::<EmbeddingDataV1>(path, "embeddings") {
        Ok(v1) => {
            tracing::info!(
                "embeddings: migrating v1 store ({} entries) to v2 format",
                v1.vectors.len()
            );
            Ok(migrate_v1_to_v2(v1))
        }
        Err(e) => Err(e),
    }
}

/// Convert a V1 store to V2 in-memory.
fn migrate_v1_to_v2(v1: EmbeddingDataV1) -> EmbeddingDataV2 {
    let mut data = EmbeddingDataV2::default();

    // Detect provider from any entry (all share the same provider in V1).
    if let Some((_, _, provider)) = v1.vectors.values().next() {
        data.meta.provider = provider.clone();
    }

    // Build vector dedup table: vector content → vector_idx.
    // V1 has no dedup, so we deduplicate by identity (exact f32 match).
    // For migration we skip the expensive content dedup and just assign one entry per node.
    let mut name_idx: HashMap<String, u32> = HashMap::new();

    for (qn, (vec, hex_hash, _provider)) in v1.vectors {
        // Convert hex_hash string to [u8; 8] best-effort.
        let text_hash = hex_hash_to_bytes8(&hex_hash);

        let string_idx = {
            let next = data.names.len() as u32;
            *name_idx.entry(qn.clone()).or_insert_with(|| {
                data.names.push(qn);
                next
            })
        };

        let dims = vec.len() as u32;
        if data.meta.dimensions == 0 {
            data.meta.dimensions = dims;
        }

        let vector_idx = data.vectors.len() as u32;
        data.vectors.push(vec);

        data.entries.push(NodeEntry {
            string_idx,
            vector_idx,
            text_hash,
        });
    }

    data.meta.version = FORMAT_VERSION;
    data
}

/// Parse the first 8 bytes from a lowercase hex string (best-effort).
fn hex_hash_to_bytes8(hex: &str) -> [u8; 8] {
    let mut out = [0u8; 8];
    let hex = hex.as_bytes();
    for i in 0..8 {
        let hi = hex.get(i * 2).copied().unwrap_or(b'0');
        let lo = hex.get(i * 2 + 1).copied().unwrap_or(b'0');
        out[i] = (hex_nibble(hi) << 4) | hex_nibble(lo);
    }
    out
}

fn hex_nibble(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        _ => 0,
    }
}

/// Compute a text hash as the first 8 raw bytes of SHA-256.
fn text_hash(text: &str) -> [u8; 8] {
    let hex = sha256_bytes_pub(text.as_bytes());
    hex_hash_to_bytes8(&hex)
}

// ---------------------------------------------------------------------------
// Provider trait
// ---------------------------------------------------------------------------

#[allow(dead_code)]
trait EmbeddingProvider: Send + Sync {
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    fn dimensions(&self) -> usize;
    fn name(&self) -> &str;
    /// Optional human-readable model identifier (defaults to `name()`).
    fn model(&self) -> &str {
        self.name()
    }
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

    fn model(&self) -> &str {
        &self.model
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

    fn model(&self) -> &str {
        &self.model
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

    fn model(&self) -> &str {
        &self.model
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
        #[allow(unused_mut)]
        let mut init = fastembed::InitOptions::new(fastembed::EmbeddingModel::JinaEmbeddingsV2BaseCode)
            .with_show_download_progress(true);

        #[cfg(feature = "gpu-directml")]
        {
            use ort::execution_providers::DirectMLExecutionProvider;
            tracing::info!("DirectML GPU acceleration enabled — trying GPU first, CPU fallback");
            init = init.with_execution_providers(vec![
                DirectMLExecutionProvider::default().build(),
            ]);
        }

        let model = fastembed::TextEmbedding::try_new(init)
            .map_err(|e| CrgError::Other(format!("fastembed init: {e}")))?;
        Ok(Self { model })
    }
}

#[cfg(feature = "embeddings-fastembed")]
impl EmbeddingProvider for FastEmbedProvider {
    fn name(&self) -> &str {
        "fastembed-jina-v2-code"
    }

    fn model(&self) -> &str {
        "JinaEmbeddingsV2BaseCode"
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
                tracing::info!("Embedding provider: OpenAI (model={})", model);
                return Some(Box::new(OpenAiProvider::new(api_key, model)));
            }
            "voyage" => {
                let api_key = get("VOYAGE_API_KEY", "voyage-api-key")?;
                let model = get("EMBEDDING_MODEL", "embedding-model")
                    .unwrap_or_else(|| "voyage-code-3".to_string());
                tracing::info!("Embedding provider: Voyage AI (model={})", model);
                return Some(Box::new(VoyageProvider::new(api_key, model)));
            }
            "gemini" => {
                let api_key = get("GEMINI_API_KEY", "gemini-api-key")?;
                let model = get("EMBEDDING_MODEL", "embedding-model")
                    .unwrap_or_else(|| "text-embedding-004".to_string());
                tracing::info!("Embedding provider: Gemini (model={})", model);
                return Some(Box::new(GeminiProvider::new(api_key, model)));
            }
            "none" | "disabled" => {
                tracing::info!("Embeddings explicitly disabled via embedding-provider=none");
                return None;
            }
            other => {
                tracing::warn!("Unknown embedding-provider='{}'; falling back to local", other);
            }
        }
    }

    // Default: fastembed (JinaEmbeddingsV2BaseCode, local, free)
    #[cfg(feature = "embeddings-fastembed")]
    {
        match FastEmbedProvider::new() {
            Ok(p) => {
                tracing::info!("Embedding provider: fastembed-jina-v2-code (local, free)");
                return Some(Box::new(p));
            }
            Err(e) => {
                tracing::warn!("fastembed init failed: {}; trying candle fallback", e);
            }
        }
    }

    // Fallback: candle all-MiniLM-L6-v2
    #[cfg(feature = "embeddings-local")]
    {
        match CandleProvider::new() {
            Ok(p) => {
                tracing::info!("Embedding provider: candle-minilm (all-MiniLM-L6-v2, local, free)");
                return Some(Box::new(p));
            }
            Err(e) => {
                tracing::warn!("Local embedding provider unavailable: {}; embeddings disabled", e);
            }
        }
    }

    None
}

// ---------------------------------------------------------------------------
// EmbeddingStore
// ---------------------------------------------------------------------------

/// Runtime state for the embedding store.
///
/// The on-disk V2 format uses a string table + vector table for compactness.
/// At runtime we keep a `HashMap<qname, (vector_idx, text_hash)>` for O(1)
/// lookups without deserialising the full `EmbeddingDataV2` on every access.
pub struct EmbeddingStore {
    data: EmbeddingDataV2,
    /// Runtime lookup: qualified_name → (vector_idx, text_hash).
    lookup: HashMap<String, (usize, [u8; 8])>,
    /// Reverse map: name → string_idx (O(1) insert instead of linear scan).
    name_to_idx: HashMap<String, u32>,
    /// Reverse map: text_hash → vector_idx (cross-run dedup).
    hash_to_vec: HashMap<[u8; 8], u32>,
    /// File-level embeddings (separate on-disk store).
    file_data: EmbeddingDataV2,
    /// Runtime lookup: file_path → (vector_idx, text_hash).
    file_lookup: HashMap<String, (usize, [u8; 8])>,
    /// Reverse maps for file data.
    file_name_to_idx: HashMap<String, u32>,
    file_hash_to_vec: HashMap<[u8; 8], u32>,
    path: Utf8PathBuf,
    file_path: Utf8PathBuf,
    provider: Option<Box<dyn EmbeddingProvider>>,
    #[cfg(feature = "hnsw-index")]
    hnsw_cache: Option<HnswIndex>,
}

/// Derive the file-embeddings path from the node-embeddings path.
fn file_embeddings_path(node_path: &Utf8Path) -> Utf8PathBuf {
    let parent = node_path.parent().unwrap_or(Utf8Path::new("."));
    parent.join("file-embeddings.bin.zst")
}

impl EmbeddingStore {
    /// Open (or create) the embedding store.
    ///
    /// `store_path` should point to a `.embeddings.bin.zst` file.
    pub fn new(store_path: &Utf8Path) -> Result<Self> {
        let data = if store_path.exists() {
            match load_embedding_data(store_path.as_std_path()) {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!(
                        "Could not load embeddings from {}: {} — starting empty",
                        store_path,
                        e
                    );
                    EmbeddingDataV2::default()
                }
            }
        } else {
            EmbeddingDataV2::default()
        };

        let lookup = data.build_lookup();
        let name_to_idx = data.build_name_to_idx();
        let hash_to_vec = data.build_hash_to_vec();

        // Load file-level embeddings from sibling file.
        let fp = file_embeddings_path(store_path);
        let file_data = if fp.exists() {
            load_embedding_data(fp.as_std_path()).unwrap_or_default()
        } else {
            EmbeddingDataV2::default()
        };
        let file_lookup = file_data.build_lookup();
        let file_name_to_idx = file_data.build_name_to_idx();
        let file_hash_to_vec = file_data.build_hash_to_vec();

        let provider = detect_provider();
        Ok(Self {
            data,
            lookup,
            name_to_idx,
            hash_to_vec,
            file_data,
            file_lookup,
            file_name_to_idx,
            file_hash_to_vec,
            path: store_path.to_path_buf(),
            file_path: fp,
            provider,
            #[cfg(feature = "hnsw-index")]
            hnsw_cache: None,
        })
    }

    /// Whether an embedding provider is available.
    pub fn available(&self) -> bool {
        self.provider.is_some()
    }

    /// Count embedded nodes.
    pub fn count(&self) -> Result<usize> {
        Ok(self.lookup.len())
    }

    /// Count embedded files.
    pub fn file_count(&self) -> Result<usize> {
        Ok(self.file_lookup.len())
    }

    /// Remove a node's embedding.
    #[allow(dead_code)]
    pub fn remove_node(&mut self, qualified_name: &str) -> Result<()> {
        if self.lookup.remove(qualified_name).is_some() {
            self.rebuild_data_from_lookup();
        }
        Ok(())
    }

    /// Remove embeddings for nodes that no longer exist in the graph.
    ///
    /// Builds the set of live qualified names from the graph store and drops
    /// any vector whose key is not present.  Returns the number of entries
    /// removed.
    pub fn gc(&mut self, store: &GraphStore) -> Result<usize> {
        let live_qns: HashSet<String> = store
            .get_all_files()?
            .iter()
            .flat_map(|f| store.get_nodes_by_file(f).unwrap_or_default())
            .map(|n| n.qualified_name.clone())
            .collect();
        let stale_keys: Vec<String> = self
            .lookup
            .keys()
            .filter(|k| !live_qns.contains(k.as_str()))
            .cloned()
            .collect();
        let removed = stale_keys.len();
        if removed > 0 {
            tracing::info!("Embedding GC: removing {} stale vector(s)", removed);
            for k in &stale_keys {
                self.lookup.remove(k);
            }
            self.rebuild_data_from_lookup();
            #[cfg(feature = "hnsw-index")]
            { self.hnsw_cache = None; }
        }
        Ok(removed)
    }

    /// Persist in-memory state to disk (nodes + files).
    pub fn save(&self) -> Result<()> {
        save_embedding_data(&self.data, self.path.as_std_path())?;
        if !self.file_data.entries.is_empty() {
            save_embedding_data(&self.file_data, self.file_path.as_std_path())?;
        }
        Ok(())
    }

    /// Close the store (no-op — nothing to flush unless explicitly saved).
    pub fn close(self) -> Result<()> {
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Insert or update a node's embedding in both the runtime lookup and the
    /// underlying V2 data structures.  Uses reverse maps for O(1) insert.
    fn insert_node(&mut self, qname: &str, vec: Vec<f32>, text_hash: [u8; 8], vector_idx: usize) {
        // O(1) string table lookup via reverse map.
        let string_idx = match self.name_to_idx.get(qname) {
            Some(&idx) => idx,
            None => {
                let idx = self.data.names.len() as u32;
                self.data.names.push(qname.to_string());
                self.name_to_idx.insert(qname.to_string(), idx);
                idx
            }
        };

        // Update or append vector table entry.
        if vector_idx < self.data.vectors.len() {
            self.data.vectors[vector_idx] = vec;
        } else {
            self.data.vectors.push(vec);
        }

        // Update hash_to_vec reverse map.
        self.hash_to_vec.insert(text_hash, vector_idx as u32);

        // Update or append the entries list.
        let existing_entry = self
            .data
            .entries
            .iter_mut()
            .find(|e| e.string_idx == string_idx);
        if let Some(entry) = existing_entry {
            entry.vector_idx = vector_idx as u32;
            entry.text_hash = text_hash;
        } else {
            self.data.entries.push(NodeEntry {
                string_idx,
                vector_idx: vector_idx as u32,
                text_hash,
            });
        }

        self.lookup.insert(qname.to_string(), (vector_idx, text_hash));
    }

    /// Rebuild `EmbeddingDataV2` entries/names/vectors from the runtime lookup.
    ///
    /// Used after GC or remove_node to compact the data.
    fn rebuild_data_from_lookup(&mut self) {
        let meta = self.data.meta.clone();
        let mut new_data = EmbeddingDataV2 {
            meta,
            names: Vec::with_capacity(self.lookup.len()),
            vectors: Vec::with_capacity(self.lookup.len()),
            entries: Vec::with_capacity(self.lookup.len()),
        };

        // Deduplicate vectors by vector_idx from old data.
        // Assign new compact indices.
        let mut old_to_new_vec: HashMap<usize, u32> = HashMap::new();

        for (qname, (old_vec_idx, hash)) in &self.lookup {
            let new_vec_idx = if let Some(&existing) = old_to_new_vec.get(old_vec_idx) {
                existing
            } else {
                let new_idx = new_data.vectors.len() as u32;
                if let Some(v) = self.data.vectors.get(*old_vec_idx) {
                    new_data.vectors.push(v.clone());
                    old_to_new_vec.insert(*old_vec_idx, new_idx);
                    new_idx
                } else {
                    continue;
                }
            };

            let string_idx = new_data.names.len() as u32;
            new_data.names.push(qname.clone());
            new_data.entries.push(NodeEntry {
                string_idx,
                vector_idx: new_vec_idx,
                text_hash: *hash,
            });
        }

        self.data = new_data;
        // Rebuild all maps from fresh data.
        self.lookup = self.data.build_lookup();
        self.name_to_idx = self.data.build_name_to_idx();
        self.hash_to_vec = self.data.build_hash_to_vec();
    }

    // -----------------------------------------------------------------------
    // File embedding helpers
    // -----------------------------------------------------------------------

    /// Insert or update a file's embedding.  Uses reverse maps for O(1) insert.
    fn insert_file(&mut self, file_path: &str, vec: Vec<f32>, hash: [u8; 8], vector_idx: usize) {
        let string_idx = match self.file_name_to_idx.get(file_path) {
            Some(&idx) => idx,
            None => {
                let idx = self.file_data.names.len() as u32;
                self.file_data.names.push(file_path.to_string());
                self.file_name_to_idx.insert(file_path.to_string(), idx);
                idx
            }
        };

        if vector_idx < self.file_data.vectors.len() {
            self.file_data.vectors[vector_idx] = vec;
        } else {
            self.file_data.vectors.push(vec);
        }

        let existing = self.file_data.entries.iter_mut().find(|e| e.string_idx == string_idx);
        if let Some(entry) = existing {
            entry.vector_idx = vector_idx as u32;
            entry.text_hash = hash;
        } else {
            self.file_data.entries.push(NodeEntry {
                string_idx,
                vector_idx: vector_idx as u32,
                text_hash: hash,
            });
        }

        self.file_hash_to_vec.insert(hash, vector_idx as u32);
        self.file_lookup.insert(file_path.to_string(), (vector_idx, hash));
    }

    /// Remove file embeddings for files no longer in the graph.
    fn gc_files(&mut self, store: &GraphStore) -> Result<usize> {
        let live_files: HashSet<String> = store.get_all_files()?.into_iter().collect();
        let stale: Vec<String> = self
            .file_lookup
            .keys()
            .filter(|k| !live_files.contains(k.as_str()))
            .cloned()
            .collect();
        let removed = stale.len();
        if removed > 0 {
            tracing::info!("File embedding GC: removing {} stale file vector(s)", removed);
            for k in &stale {
                self.file_lookup.remove(k);
            }
            self.rebuild_file_data_from_lookup();
        }
        Ok(removed)
    }

    /// Rebuild file_data from file_lookup (compact after GC).
    fn rebuild_file_data_from_lookup(&mut self) {
        let meta = self.file_data.meta.clone();
        let mut new_data = EmbeddingDataV2 {
            meta,
            names: Vec::with_capacity(self.file_lookup.len()),
            vectors: Vec::with_capacity(self.file_lookup.len()),
            entries: Vec::with_capacity(self.file_lookup.len()),
        };

        let mut old_to_new: HashMap<usize, u32> = HashMap::new();
        for (fp, (old_idx, hash)) in &self.file_lookup {
            let new_idx = if let Some(&existing) = old_to_new.get(old_idx) {
                existing
            } else {
                let idx = new_data.vectors.len() as u32;
                if let Some(v) = self.file_data.vectors.get(*old_idx) {
                    new_data.vectors.push(v.clone());
                    old_to_new.insert(*old_idx, idx);
                    idx
                } else {
                    continue;
                }
            };

            let string_idx = new_data.names.len() as u32;
            new_data.names.push(fp.clone());
            new_data.entries.push(NodeEntry {
                string_idx,
                vector_idx: new_idx,
                text_hash: *hash,
            });
        }

        self.file_data = new_data;
        self.file_lookup = self.file_data.build_lookup();
        self.file_name_to_idx = self.file_data.build_name_to_idx();
        self.file_hash_to_vec = self.file_data.build_hash_to_vec();
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
            .first()
            .map(|v| v.len())
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
            .reserve(emb_store.lookup.len())
            .map_err(|e| CrgError::Other(format!("HNSW reserve: {e}")))?;

        let mut key_to_name = HashMap::new();
        for (i, (qn, (vec_idx, _))) in emb_store.lookup.iter().enumerate() {
            let key = i as u64;
            if let Some(vec) = emb_store.data.vectors.get(*vec_idx) {
                index
                    .add(key, vec)
                    .map_err(|e| CrgError::Other(format!("HNSW add: {e}")))?;
                key_to_name.insert(key, qn.clone());
            }
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
    if emb_store.provider.is_none() {
        return Ok(0);
    }

    // Snapshot provider metadata.
    let (provider_name, provider_model, provider_dims) = {
        let p = emb_store.provider.as_ref().expect("checked above");
        (p.name().to_string(), p.model().to_string(), p.dimensions() as u32)
    };

    // Hard invalidation: if provider/model/dims changed, the entire store is stale.
    let provider_changed = !emb_store.lookup.is_empty()
        && (emb_store.data.meta.provider != provider_name
            || emb_store.data.meta.model != provider_model
            || (emb_store.data.meta.dimensions != 0
                && emb_store.data.meta.dimensions != provider_dims));
    if provider_changed {
        tracing::warn!(
            "embeddings: provider changed ({}/{} → {}/{}); invalidating {} vectors",
            emb_store.data.meta.provider,
            emb_store.data.meta.model,
            provider_name,
            provider_model,
            emb_store.lookup.len()
        );
        emb_store.data = EmbeddingDataV2::default();
        emb_store.lookup.clear();
        emb_store.name_to_idx.clear();
        emb_store.hash_to_vec.clear();
        #[cfg(feature = "hnsw-index")]
        { emb_store.hnsw_cache = None; }
    }

    // GC: remove embeddings for nodes no longer in the graph.
    emb_store.gc(store)?;

    // Update store-level metadata from the active provider.
    emb_store.data.meta.provider = provider_name;
    emb_store.data.meta.model = provider_model;
    emb_store.data.meta.dimensions = provider_dims;
    emb_store.data.meta.version = FORMAT_VERSION;

    // Collect nodes needing (re)embedding.
    // Dedup by text content: nodes with identical text share one vector.
    //
    // `hash_to_text`: text_hash → (text, Vec<qname>) — one embedding per unique text
    let mut hash_to_text: HashMap<[u8; 8], (String, Vec<String>)> = HashMap::new();

    for file in store.get_all_files()? {
        for node in store.get_nodes_by_file(&file)? {
            let text = node_to_text(&node);
            let hash = text_hash(&text);

            let needs_embed = match emb_store.lookup.get(&node.qualified_name) {
                Some((_, existing_hash)) => *existing_hash != hash,
                None => true,
            };

            if needs_embed {
                // Cross-run dedup: skip embedding if we already have this text_hash.
                if emb_store.hash_to_vec.contains_key(&hash) {
                    // Reuse existing vector.
                    hash_to_text.entry(hash)
                        .or_insert_with(|| (text, Vec::new()))
                        .1
                        .push(node.qualified_name.clone());
                } else {
                    hash_to_text
                        .entry(hash)
                        .or_insert_with(|| (text, Vec::new()))
                        .1
                        .push(node.qualified_name.clone());
                }
            }
        }
    }

    if hash_to_text.is_empty() {
        return Ok(0);
    }

    // Partition into texts that need embedding vs those that can reuse existing vectors.
    let mut need_embed: Vec<([u8; 8], String, Vec<String>)> = Vec::new();
    let mut reuse: Vec<([u8; 8], Vec<String>)> = Vec::new();
    for (hash, (text, qnames)) in hash_to_text {
        if emb_store.hash_to_vec.contains_key(&hash) {
            reuse.push((hash, qnames));
        } else {
            need_embed.push((hash, text, qnames));
        }
    }

    // Apply reused vectors (no embedding call needed).
    let mut count = 0;
    for (hash, qnames) in reuse {
        let vector_idx = *emb_store.hash_to_vec.get(&hash).unwrap() as usize;
        let vec = emb_store.data.vectors[vector_idx].clone();
        for qn in &qnames {
            emb_store.insert_node(qn, vec.clone(), hash, vector_idx);
            count += 1;
        }
    }

    // Embed new texts in batches.
    let mut all_results: Vec<([u8; 8], Vec<String>, Vec<f32>)> = Vec::new();
    {
        let provider = emb_store.provider.as_ref().expect("checked above");
        for chunk in need_embed.chunks(100) {
            let texts: Vec<String> = chunk.iter().map(|(_, t, _)| t.clone()).collect();
            let vectors = provider.embed_batch(&texts)?;
            for ((hash, _, qnames), vec) in chunk.iter().zip(vectors) {
                all_results.push((*hash, qnames.clone(), vec));
            }
        }
    }

    for (hash, qnames, vec) in all_results {
        let vector_idx = emb_store.data.vectors.len();
        emb_store.data.vectors.push(vec.clone());

        for qn in &qnames {
            emb_store.insert_node(qn, vec.clone(), hash, vector_idx);
            count += 1;
        }
    }

    emb_store.save()?;
    #[cfg(feature = "hnsw-index")]
    { emb_store.hnsw_cache = None; }
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
    compact: bool,
    repo_root: &camino::Utf8Path,
) -> Result<Vec<serde_json::Value>> {
    if emb_store.provider.is_none() {
        let nodes = store.search_nodes(query, limit)?;
        return Ok(nodes.iter().map(|n| node_to_dict(n, compact)).collect());
    }

    let provider = emb_store.provider.as_ref().expect("provider checked above");

    let query_vecs = provider.embed_batch(&[query.to_string()])?;
    let query_vec = &query_vecs[0];

    // Use HNSW approximate nearest-neighbour search when the feature is compiled in.
    #[cfg(feature = "hnsw-index")]
    {
        let cached = emb_store.hnsw_cache.is_some();
        let _span = tracing::info_span!("hnsw_search", cached).entered();
        if emb_store.hnsw_cache.is_none() {
            match HnswIndex::build(emb_store) {
                Ok(idx) => { emb_store.hnsw_cache = Some(idx); }
                Err(e) => {
                    tracing::warn!("HNSW index build failed ({}); falling back to linear scan", e);
                }
            }
        }
        if let Some(ref idx) = emb_store.hnsw_cache {
            let scored = idx
                .search(query_vec, limit)
                .into_iter()
                .map(|(qn, s)| (qn, s as f64))
                .collect::<Vec<_>>();
            return nodes_from_scored(scored, store, compact, repo_root);
        }
    }

    // Linear scan (also the only path without hnsw-index feature).
    let mut scored: Vec<(String, f64)> = emb_store
        .lookup
        .iter()
        .filter_map(|(qn, (vec_idx, _))| {
            emb_store.data.vectors.get(*vec_idx).map(|vec| {
                (qn.clone(), cosine_similarity(query_vec, vec))
            })
        })
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(limit);
    nodes_from_scored(scored, store, compact, repo_root)
}

/// Resolve a ranked `(qualified_name, score)` list to full node dicts.
fn nodes_from_scored(
    scored: Vec<(String, f64)>,
    store: &GraphStore,
    compact: bool,
    _repo_root: &camino::Utf8Path, // kept for API compatibility; paths already normalized at source
) -> Result<Vec<serde_json::Value>> {
    let mut results = Vec::with_capacity(scored.len());
    for (qn, score) in scored {
        if let Some(node) = store.get_node(&qn)? {
            let mut d = node_to_dict(&node, compact);
            d["similarity_score"] =
                serde_json::Value::from((score * 10_000.0).round() / 10_000.0);
            results.push(d);
        }
    }
    Ok(results)
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() {
        tracing::error!("cosine_similarity: dimension mismatch {} vs {}", a.len(), b.len());
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

/// Convert a file and its child nodes to embeddable text.
///
/// Includes the file path, language, and up to 50 symbol names — gives the
/// embedding model both location and content context.
pub fn file_to_text(file_path: &str, nodes: &[GraphNode]) -> String {
    let mut parts: Vec<String> = vec![file_path.to_string()];

    // Add language from first node that has one.
    if let Some(lang) = nodes.iter().find_map(|n| {
        if !n.language.is_empty() { Some(&n.language) } else { None }
    }) {
        parts.push(lang.clone());
    }

    // Add child symbol names (skip File nodes, cap at 50).
    let mut seen = HashSet::new();
    for node in nodes {
        if node.kind.as_str() != "File" && seen.insert(&node.name) {
            parts.push(node.name.clone());
            if seen.len() >= 50 {
                break;
            }
        }
    }

    parts.join(" ")
}

/// Embed all graph files that don't already have up-to-date file-level embeddings.
///
/// Returns the number of newly embedded files.
pub fn embed_all_files(
    store: &GraphStore,
    emb_store: &mut EmbeddingStore,
) -> Result<usize> {
    if emb_store.provider.is_none() {
        return Ok(0);
    }

    // Snapshot provider metadata.
    let (prov_name, prov_model, prov_dims) = {
        let p = emb_store.provider.as_ref().expect("checked above");
        (p.name().to_string(), p.model().to_string(), p.dimensions() as u32)
    };

    // Hard invalidation on provider change.
    let provider_changed = !emb_store.file_lookup.is_empty()
        && (emb_store.file_data.meta.provider != prov_name
            || emb_store.file_data.meta.model != prov_model
            || (emb_store.file_data.meta.dimensions != 0
                && emb_store.file_data.meta.dimensions != prov_dims));
    if provider_changed {
        tracing::warn!(
            "file embeddings: provider changed; invalidating {} file vectors",
            emb_store.file_lookup.len()
        );
        emb_store.file_data = EmbeddingDataV2::default();
        emb_store.file_lookup.clear();
        emb_store.file_name_to_idx.clear();
        emb_store.file_hash_to_vec.clear();
    }

    // GC stale file embeddings.
    emb_store.gc_files(store)?;

    emb_store.file_data.meta.provider = prov_name;
    emb_store.file_data.meta.model = prov_model;
    emb_store.file_data.meta.dimensions = prov_dims;
    emb_store.file_data.meta.version = FORMAT_VERSION;

    // Collect files needing (re)embedding.
    let mut to_embed: Vec<([u8; 8], String, String)> = Vec::new(); // (hash, text, file_path)
    for file in store.get_all_files()? {
        let nodes = store.get_nodes_by_file(&file)?;
        let text = file_to_text(&file, &nodes);
        let hash = text_hash(&text);

        let needs_embed = match emb_store.file_lookup.get(&file) {
            Some((_, existing_hash)) => *existing_hash != hash,
            None => true,
        };
        if needs_embed {
            to_embed.push((hash, text, file));
        }
    }

    if to_embed.is_empty() {
        return Ok(0);
    }

    // Embed in batches (immutable provider borrow), then mutate.
    let mut all_results: Vec<([u8; 8], String, Vec<f32>)> = Vec::new();
    {
        let provider = emb_store.provider.as_ref().expect("checked above");
        for chunk in to_embed.chunks(100) {
            let texts: Vec<String> = chunk.iter().map(|(_, t, _)| t.clone()).collect();
            let vectors = provider.embed_batch(&texts)?;
            for ((hash, _, fp), vec) in chunk.iter().zip(vectors) {
                all_results.push((*hash, fp.clone(), vec));
            }
        }
    }

    let mut count = 0;
    for (hash, fp, vec) in all_results {
        let vector_idx = emb_store.file_data.vectors.len();
        emb_store.insert_file(&fp, vec, hash, vector_idx);
        count += 1;
    }

    emb_store.save()?;
    Ok(count)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{GraphNode, NodeKind};

    fn p(path: &std::path::Path) -> &Utf8Path {
        Utf8Path::from_path(path).expect("test path is valid UTF-8")
    }

    // Helper: insert a node entry directly into the store (bypasses provider).
    fn insert_test_entry(
        store: &mut EmbeddingStore,
        qname: &str,
        vec: Vec<f32>,
        hash_hex: &str,
    ) {
        let hash = hex_hash_to_bytes8(hash_hex);
        let vector_idx = store.data.vectors.len();
        store.insert_node(qname, vec, hash, vector_idx);
    }

    #[test]
    fn cosine_similarity_identical_vectors() {
        let v = vec![1.0f32, 2.0, 3.0];
        assert!((cosine_similarity(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_opposite_vectors() {
        let a = vec![1.0f32, 2.0, 3.0];
        let b = vec![-1.0f32, -2.0, -3.0];
        assert!((cosine_similarity(&a, &b) - (-1.0)).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_orthogonal_vectors() {
        let a = vec![1.0f32, 0.0, 0.0];
        let b = vec![0.0f32, 1.0, 0.0];
        assert!(cosine_similarity(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_zero_vector() {
        let a = vec![0.0f32, 0.0, 0.0];
        let b = vec![1.0f32, 2.0, 3.0];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
        assert_eq!(cosine_similarity(&b, &a), 0.0);
    }

    #[test]
    fn cosine_similarity_dimension_mismatch() {
        let a = vec![1.0f32, 2.0];
        let b = vec![1.0f32, 2.0, 3.0];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }

    #[test]
    fn node_to_text_with_all_fields() {
        let node = GraphNode {
            name: "parse_config".to_string(),
            qualified_name: "mod::parse_config".to_string(),
            kind: NodeKind::Function,
            file_path: "src/config.rs".to_string(),
            line_start: 10,
            line_end: 30,
            language: "rust".to_string(),
            is_test: false,
            docstring: "Parse configuration from file".to_string(),
            signature: "fn parse_config(path: &Path) -> Config".to_string(),
            body_hash: "abc".to_string(),
            file_hash: "def".to_string(),
        };
        let text = node_to_text(&node);
        assert!(text.contains("parse_config"));
        assert!(text.contains("function"));
        assert!(text.contains("rust"));
    }

    #[test]
    fn node_to_text_without_optional_fields() {
        let node = GraphNode {
            name: "Foo".to_string(),
            qualified_name: "Foo".to_string(),
            kind: NodeKind::Class,
            file_path: "foo.py".to_string(),
            line_start: 1,
            line_end: 5,
            language: String::new(),
            is_test: false,
            docstring: String::new(),
            signature: String::new(),
            body_hash: String::new(),
            file_hash: String::new(),
        };
        let text = node_to_text(&node);
        assert!(text.contains("Foo"));
        assert!(text.contains("class"));
    }

    #[test]
    fn embedding_store_count_empty() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("test.embeddings.bin.zst");
        let store = EmbeddingStore::new(p(&path)).unwrap();
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn embedding_store_save_reload_roundtrip() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("test.embeddings.bin.zst");
        let mut store = EmbeddingStore::new(p(&path)).unwrap();
        insert_test_entry(&mut store, "mod::foo", vec![1.0, 2.0, 3.0], "hash1");
        store.save().unwrap();

        let reloaded = EmbeddingStore::new(p(&path)).unwrap();
        assert_eq!(reloaded.count().unwrap(), 1);

        // Verify the entry is accessible via the runtime lookup.
        let (vec_idx, _hash) = reloaded.lookup.get("mod::foo").expect("entry missing");
        let vec = &reloaded.data.vectors[*vec_idx];
        assert_eq!(vec, &[1.0f32, 2.0, 3.0]);
    }

    #[test]
    fn text_hash_returns_8_bytes() {
        let h = text_hash("hello world");
        assert_eq!(h.len(), 8);
        // Deterministic.
        assert_eq!(h, text_hash("hello world"));
        // Different inputs differ.
        assert_ne!(h, text_hash("goodbye world"));
    }

    #[test]
    fn store_metadata_persisted() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("meta.embeddings.bin.zst");
        let mut store = EmbeddingStore::new(p(&path)).unwrap();
        store.data.meta.provider = "test-provider".to_string();
        store.data.meta.model = "test-model".to_string();
        store.data.meta.dimensions = 3;
        store.save().unwrap();

        let reloaded = EmbeddingStore::new(p(&path)).unwrap();
        assert_eq!(reloaded.data.meta.provider, "test-provider");
        assert_eq!(reloaded.data.meta.model, "test-model");
        assert_eq!(reloaded.data.meta.dimensions, 3);
        assert_eq!(reloaded.data.meta.version, FORMAT_VERSION);
    }

    #[test]
    fn v1_migration_roundtrip() {
        // Serialise a V1 store directly and verify it migrates on load.
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("v1.embeddings.bin.zst");

        let mut v1 = EmbeddingDataV1::default();
        v1.vectors.insert(
            "mod::bar".to_string(),
            (vec![4.0, 5.0, 6.0], "aabbccdd00112233".to_string(), "legacy-provider".to_string()),
        );
        persistence::save_blob(&v1, path.as_path(), "embeddings").unwrap();

        // Load should auto-migrate.
        let store = EmbeddingStore::new(p(&path)).unwrap();
        assert_eq!(store.count().unwrap(), 1);
        assert!(store.lookup.contains_key("mod::bar"));
    }

    #[test]
    fn remove_node_decrements_count() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("rm.embeddings.bin.zst");
        let mut store = EmbeddingStore::new(p(&path)).unwrap();
        insert_test_entry(&mut store, "a::b", vec![1.0, 0.0], "deadbeef00000000");
        insert_test_entry(&mut store, "a::c", vec![0.0, 1.0], "cafebabe00000000");
        assert_eq!(store.count().unwrap(), 2);

        store.remove_node("a::b").unwrap();
        assert_eq!(store.count().unwrap(), 1);
        assert!(!store.lookup.contains_key("a::b"));
        assert!(store.lookup.contains_key("a::c"));
    }

    #[test]
    fn hex_hash_to_bytes8_roundtrip() {
        // Known SHA-256 prefix of "hello world" in hex.
        let hex = "b94d27b9934d3e08";
        let bytes = hex_hash_to_bytes8(hex);
        // Re-encode manually and compare.
        let re_hex = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
        assert_eq!(re_hex, hex);
    }

    #[test]
    fn f16_roundtrip_preserves_cosine_similarity() {
        // Realistic embedding-like vectors (values in [-1, 1]).
        let a = vec![0.123f32, -0.456, 0.789, -0.012, 0.345, -0.678, 0.901, -0.234];
        let b = vec![-0.567f32, 0.890, -0.123, 0.456, -0.789, 0.012, -0.345, 0.678];

        let original_sim = cosine_similarity(&a, &b);

        // Roundtrip through f16.
        let a_rt = f16_vec_to_f32(&f32_vec_to_f16(&a));
        let b_rt = f16_vec_to_f32(&f32_vec_to_f16(&b));
        let roundtrip_sim = cosine_similarity(&a_rt, &b_rt);

        // f16 has ~3 decimal digits of precision; cosine similarity preserved to <0.001.
        assert!(
            (original_sim - roundtrip_sim).abs() < 0.001,
            "f16 roundtrip degraded cosine similarity: {original_sim} → {roundtrip_sim}"
        );
    }

    #[test]
    fn embedding_store_v3_save_reload() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("v3.embeddings.bin.zst");
        let mut store = EmbeddingStore::new(p(&path)).unwrap();

        // Values that are exact in both f32 and f16.
        insert_test_entry(&mut store, "mod::alpha", vec![1.0, 0.0, -1.0], "aabb0011");
        insert_test_entry(&mut store, "mod::beta", vec![0.0, 1.0, 0.5], "ccdd2233");
        store.save().unwrap();

        let reloaded = EmbeddingStore::new(p(&path)).unwrap();
        assert_eq!(reloaded.count().unwrap(), 2);
        assert_eq!(reloaded.data.meta.version, FORMAT_VERSION);

        let (idx_a, _) = reloaded.lookup.get("mod::alpha").unwrap();
        assert_eq!(&reloaded.data.vectors[*idx_a], &[1.0f32, 0.0, -1.0]);

        let (idx_b, _) = reloaded.lookup.get("mod::beta").unwrap();
        assert_eq!(&reloaded.data.vectors[*idx_b], &[0.0f32, 1.0, 0.5]);
    }

    #[test]
    fn v2_file_loads_and_upgrades_to_v3_on_save() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("upgrade.embeddings.bin.zst");

        // Manually write a V2 file.
        let mut v2 = EmbeddingDataV2::default();
        v2.meta.version = 2;
        v2.meta.provider = "test".to_string();
        v2.meta.dimensions = 2;
        v2.names.push("mod::old".to_string());
        v2.vectors.push(vec![0.5, -0.5]);
        v2.entries.push(NodeEntry {
            string_idx: 0,
            vector_idx: 0,
            text_hash: [1, 2, 3, 4, 5, 6, 7, 8],
        });
        persistence::save_blob(&v2, path.as_path(), "embeddings").unwrap();

        // Load should read as V2 (f32).
        let store = EmbeddingStore::new(p(&path)).unwrap();
        assert_eq!(store.count().unwrap(), 1);
        let (idx, _) = store.lookup.get("mod::old").unwrap();
        assert_eq!(&store.data.vectors[*idx], &[0.5f32, -0.5]);

        // Re-save upgrades to V3 on disk.
        store.save().unwrap();

        // Reload now loads V3 format.
        let reloaded = EmbeddingStore::new(p(&path)).unwrap();
        assert_eq!(reloaded.count().unwrap(), 1);
        assert_eq!(reloaded.data.meta.version, FORMAT_VERSION);
        let (idx2, _) = reloaded.lookup.get("mod::old").unwrap();
        // 0.5 and -0.5 are exact in f16.
        assert_eq!(&reloaded.data.vectors[*idx2], &[0.5f32, -0.5]);
    }

    #[test]
    fn file_to_text_includes_path_and_symbols() {
        let nodes = vec![
            GraphNode {
                name: "parse".to_string(),
                qualified_name: "mod::parse".to_string(),
                kind: NodeKind::Function,
                file_path: "src/parser.rs".to_string(),
                line_start: 1, line_end: 10,
                language: "rust".to_string(),
                is_test: false,
                docstring: String::new(),
                signature: String::new(),
                body_hash: String::new(),
                file_hash: String::new(),
            },
            GraphNode {
                name: "Config".to_string(),
                qualified_name: "mod::Config".to_string(),
                kind: NodeKind::Class,
                file_path: "src/parser.rs".to_string(),
                line_start: 12, line_end: 30,
                language: "rust".to_string(),
                is_test: false,
                docstring: String::new(),
                signature: String::new(),
                body_hash: String::new(),
                file_hash: String::new(),
            },
        ];
        let text = super::file_to_text("src/parser.rs", &nodes);
        assert!(text.contains("src/parser.rs"));
        assert!(text.contains("rust"));
        assert!(text.contains("parse"));
        assert!(text.contains("Config"));
    }

    #[test]
    fn file_embeddings_save_reload_roundtrip() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("test.embeddings.bin.zst");
        let mut store = EmbeddingStore::new(p(&path)).unwrap();

        // Insert a file embedding directly.
        let hash = [10u8; 8];
        let vector_idx = store.file_data.vectors.len();
        store.insert_file("src/main.rs", vec![0.5, -0.5, 1.0], hash, vector_idx);
        store.save().unwrap();

        // Reload and verify file embeddings persisted.
        let reloaded = EmbeddingStore::new(p(&path)).unwrap();
        assert_eq!(reloaded.file_count().unwrap(), 1);
        let (idx, h) = reloaded.file_lookup.get("src/main.rs").unwrap();
        assert_eq!(*h, hash);
        // 0.5, -0.5, 1.0 are exact in f16.
        assert_eq!(&reloaded.file_data.vectors[*idx], &[0.5f32, -0.5, 1.0]);
    }

    #[test]
    fn file_to_text_skips_file_nodes_and_deduplicates() {
        let file_node = GraphNode {
            name: "src/lib.rs".to_string(),
            qualified_name: "src/lib.rs".to_string(),
            kind: NodeKind::File,
            file_path: "src/lib.rs".to_string(),
            line_start: 1, line_end: 100,
            language: "rust".to_string(),
            is_test: false,
            docstring: String::new(),
            signature: String::new(),
            body_hash: String::new(),
            file_hash: String::new(),
        };
        let fn_node = GraphNode {
            name: "foo".to_string(),
            qualified_name: "mod::foo".to_string(),
            kind: NodeKind::Function,
            file_path: "src/lib.rs".to_string(),
            line_start: 5, line_end: 10,
            language: "rust".to_string(),
            is_test: false,
            docstring: String::new(),
            signature: String::new(),
            body_hash: String::new(),
            file_hash: String::new(),
        };
        // Duplicate name — should only appear once.
        let fn_node2 = GraphNode {
            name: "foo".to_string(),
            qualified_name: "mod::foo2".to_string(),
            kind: NodeKind::Function,
            file_path: "src/lib.rs".to_string(),
            line_start: 15, line_end: 20,
            language: "rust".to_string(),
            is_test: false,
            docstring: String::new(),
            signature: String::new(),
            body_hash: String::new(),
            file_hash: String::new(),
        };
        let text = super::file_to_text("src/lib.rs", &[file_node, fn_node, fn_node2]);
        // File node name ("src/lib.rs") should NOT appear as a symbol
        // (the path is already the first element).
        let parts: Vec<&str> = text.split_whitespace().collect();
        // "src/lib.rs" "rust" "foo" — no duplicate "foo"
        assert_eq!(parts.iter().filter(|&&p| p == "foo").count(), 1);
    }
}
