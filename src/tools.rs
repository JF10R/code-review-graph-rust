//! MCP tool implementations.
//!
//! 9 tools exposed via the MCP server:
//! 1. build_or_update_graph
//! 2. get_impact_radius
//! 3. query_graph
//! 4. get_review_context
//! 5. semantic_search_nodes
//! 6. list_graph_stats
//! 7. embed_graph
//! 8. get_docs_section
//! 9. find_large_functions

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use camino::{Utf8Path, Utf8PathBuf};

use serde_json::{json, Value};

use crate::embeddings::{embed_all_files, embed_all_nodes, semantic_search, EmbeddingStore};
use crate::error::{CrgError, Result};
use crate::graph::GraphStore;
use crate::incremental;
use crate::types::{edge_to_dict, node_to_dict, EdgeKind, NodeKind};

/// RRF constant: standard k=60 from the original RRF paper.
const RRF_K: f64 = 60.0;

// ---------------------------------------------------------------------------
// Ablation configuration for leave-one-out experiments
// ---------------------------------------------------------------------------

/// Controls which components of the file-mode pipeline are active.
/// Used for ablation studies to isolate each component's contribution.
#[derive(Debug, Clone)]
pub struct AblationConfig {
    /// Multi-channel fanout (channels 2-6). When false, only KeywordRelaxed is used.
    pub fanout: bool,
    /// Graph expansion (1-hop CALLS/CONTAINS from top candidates).
    pub expansion: bool,
    /// Conditional priors (test/compiled/multi-source/symbol-path/exact-symbol).
    pub priors: bool,
    /// Capped top-k scorer (top*1.5 + 0.3*rest). When false, uses simple sum.
    pub scorer: bool,
    /// Query decomposition. When false, all tokens go to domain_terms bucket.
    pub decomposition: bool,
    /// Semantic retrieval channel. When false, skips channel 5.
    pub semantic: bool,
}

impl AblationConfig {
    /// Production default. Fanout disabled — ablation study (2×2 interaction
    /// test) showed fanout × decomposition is a negative interaction that loses
    /// 2 Hit@5 with no MRR benefit. Config B (fanout OFF, decomposition ON)
    /// strictly dominates config D (both ON): same MRR, +2 Hit@5, less complexity.
    pub fn full() -> Self {
        Self { fanout: false, expansion: true, priors: true, scorer: true, decomposition: true, semantic: true }
    }

    /// All components enabled, including fanout. Use for tests that exercise
    /// specific channels, or for future recalibration experiments.
    pub fn all_enabled() -> Self {
        Self { fanout: true, expansion: true, priors: true, scorer: true, decomposition: true, semantic: true }
    }

    /// All components enabled except the named one. Derives from
    /// `all_enabled()`, not `full()`, so ablation baselines are correct.
    pub fn without(component: &str) -> Self {
        let mut cfg = Self::all_enabled();
        match component {
            "fanout" => cfg.fanout = false,
            "expansion" => cfg.expansion = false,
            "priors" => cfg.priors = false,
            "scorer" => cfg.scorer = false,
            "decomposition" => cfg.decomposition = false,
            "semantic" => cfg.semantic = false,
            _ => {}
        }
        cfg
    }

    /// Human-readable label for this configuration.
    pub fn label(&self) -> String {
        let disabled: Vec<&str> = [
            (!self.fanout, "fanout"),
            (!self.expansion, "expansion"),
            (!self.priors, "priors"),
            (!self.scorer, "scorer"),
            (!self.decomposition, "decomposition"),
            (!self.semantic, "semantic"),
        ]
        .iter()
        .filter(|(off, _)| *off)
        .map(|(_, name)| *name)
        .collect();
        if disabled.is_empty() {
            "full".to_string()
        } else {
            format!("-{}", disabled.join(",-"))
        }
    }
}

// ---------------------------------------------------------------------------
// File-mode fanout+rerank: types
// ---------------------------------------------------------------------------

/// Decomposed query parts used for multi-channel fanout retrieval.
struct QueryParts {
    /// camelCase/snake_case/PascalCase/ALL_CAPS tokens.
    symbols: Vec<String>,
    /// Tokens containing '/' or ending with known source extensions.
    path_fragments: Vec<String>,
    /// Non-stop-word terms that are neither symbols nor path fragments.
    domain_terms: Vec<String>,
    /// Original query, passed as-is to semantic search.
    raw: String,
    /// True if any token matches test/spec/fixture/mock/assert (case-insensitive).
    mentions_tests: bool,
    /// Exact text extracted from quoted or backtick-delimited spans in the query.
    error_strings: Vec<String>,
    /// Multi-word technical phrases kept together (e.g., "race condition").
    compound_terms: Vec<String>,
}

/// Which evidence channel produced a candidate node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum CandidateSource {
    KeywordRelaxed,
    KeywordExact,
    PathBoosted,
    ConfigBoosted,
    Semantic,
    Tantivy,
    Expansion,
}

impl CandidateSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::KeywordRelaxed => "keyword_relaxed",
            Self::KeywordExact  => "keyword_exact",
            Self::PathBoosted   => "path_boosted",
            Self::ConfigBoosted => "config_boosted",
            Self::Semantic      => "semantic",
            Self::Tantivy       => "tantivy",
            Self::Expansion     => "expansion",
        }
    }
}

/// A candidate node gathered from one or more evidence channels.
struct NodeCandidate {
    qualified_name: String,
    file_path: String,
    kind: NodeKind,
    is_test: bool,
    score: f64,
    sources: HashSet<CandidateSource>,
}

/// A file-level result with aggregated score and top supporting nodes.
struct FileResult {
    file_path: String,
    score: f64,
    top_nodes: Vec<NodeEvidence>,
    sources: HashSet<CandidateSource>,
}

/// Evidence node attached to a `FileResult`.
struct NodeEvidence {
    name: String,
    qualified_name: String,
    kind: String,
    line_start: usize,
    line_end: usize,
    score: f64,
    sources: Vec<String>,
    /// Which query terms (symbols or domain_terms) matched this node's name/qualified_name.
    matched_terms: Vec<String>,
    /// Whether this node is a test node.
    is_test: bool,
}

// ---------------------------------------------------------------------------
// File-mode fanout+rerank: stop words (mirrors graph.rs search_nodes_relaxed)
// ---------------------------------------------------------------------------

const FILE_MODE_STOP_WORDS: &[&str] = &[
    "a", "an", "the", "in", "on", "at", "to", "for", "is", "it", "of",
    "and", "or", "not", "with", "from", "by", "as", "be", "was", "that",
    "this", "which", "when", "where", "how", "what", "who", "why", "does",
    "do", "did", "has", "have", "had", "can", "could", "will", "would",
    "should", "may", "might", "are", "were", "been", "being", "into",
    "after", "before", "between", "through", "during", "about", "than",
    "both", "each", "all", "any", "some", "most", "other", "such", "only",
    "very", "more", "also", "just", "so", "up", "out", "if", "then", "but",
    "its", "no", "because", "instead", "due", "via", "using", "causes",
    "cause", "causing", "breaks", "break", "fails", "fail", "returns",
    "return", "incorrectly", "correctly",
];

/// Known source file extensions used to classify path-fragment tokens.
const SOURCE_EXTENSIONS: &[&str] = &[
    ".ts", ".tsx", ".js", ".jsx", ".rs", ".go", ".py", ".java",
    ".kt", ".swift", ".rb", ".cs", ".php", ".cpp", ".c", ".h",
    ".vue", ".svelte", ".toml", ".yaml", ".yml", ".json",
];

/// Multi-word technical phrases that carry meaning as a unit.
const COMPOUND_TECH_TERMS: &[&str] = &[
    "race condition",
    "memory leak",
    "stack overflow",
    "null pointer",
    "off by one",
    "content length",
    "status code",
    "error message",
    "return type",
    "dead lock",
    "deadlock",
    "buffer overflow",
    "integer overflow",
    "use after free",
    "double free",
    "type error",
    "syntax error",
    "import cycle",
    "circular dependency",
    "infinite loop",
    "out of bounds",
    "index out of bounds",
    "key error",
    "value error",
    "name error",
    "attribute error",
    "connection refused",
    "timeout error",
    "parse error",
    "compile error",
    "link error",
];

/// Decompose a natural-language query into typed token buckets.
///
/// Tokens are classified as symbols, path fragments, or domain terms.
/// No camelCase sub-word splitting — symbols stay intact so that exact
/// searches match nodes named "serverPatchReducer" precisely.
/// Extract text spans delimited by double-quotes or backticks from a query string.
/// Returns each matched span's inner content (trimmed), skipping empty spans.
fn extract_quoted_spans(query: &str) -> Vec<String> {
    let mut results: Vec<String> = Vec::new();
    let chars: Vec<char> = query.chars().collect();
    let n = chars.len();
    let mut i = 0;

    while i < n {
        let delim = chars[i];
        if delim == '"' || delim == '`' {
            let start = i + 1;
            let mut j = start;
            while j < n && chars[j] != delim {
                j += 1;
            }
            if j > start && j < n {
                let span: String = chars[start..j].iter().collect();
                let trimmed = span.trim().to_string();
                if !trimmed.is_empty() {
                    results.push(trimmed);
                }
            }
            i = j + 1;
        } else {
            i += 1;
        }
    }

    results
}

/// Scan the lowercased token sequence for known compound technical terms.
/// Returns each matched phrase (lowercased, space-separated) without duplicates.
fn extract_compound_terms(tokens: &[String]) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();

    for phrase in COMPOUND_TECH_TERMS {
        let words: Vec<&str> = phrase.split_whitespace().collect();
        if words.len() == 1 {
            if tokens.iter().any(|t| t.as_str() == words[0]) && !found.contains(&phrase.to_string()) {
                found.push(phrase.to_string());
            }
            continue;
        }
        let wlen = words.len();
        if tokens.len() >= wlen {
            for window in tokens.windows(wlen) {
                let matches = window.iter().zip(words.iter()).all(|(tok, pw)| tok.as_str() == *pw);
                if matches {
                    let phrase_str = phrase.to_string();
                    if !found.contains(&phrase_str) {
                        found.push(phrase_str);
                    }
                    break;
                }
            }
        }
    }

    found
}

fn decompose_query(query: &str) -> QueryParts {
    // Phase 1: extract quoted/backtick spans before tokenising.
    let error_strings = extract_quoted_spans(query);

    // Strip the quoted spans so their words do not also land in other buckets.
    let query_stripped = {
        let mut s = query.to_string();
        for span in &error_strings {
            s = s.replace(&format!("\"{}\"", span), " ");
            s = s.replace(&format!("`{}`", span), " ");
        }
        s
    };

    let mut symbols: Vec<String> = Vec::new();
    let mut path_fragments: Vec<String> = Vec::new();
    let mut domain_terms: Vec<String> = Vec::new();
    let mut mentions_tests = false;
    // Collect lowercase plain tokens for compound-term scanning.
    let mut plain_tokens: Vec<String> = Vec::new();

    for raw_token in query_stripped.split_whitespace() {
        // Strip leading/trailing punctuation for classification purposes,
        // but keep the original token for the buckets.
        let token = raw_token.trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '/' && c != '.');
        if token.is_empty() {
            continue;
        }

        let lower = token.to_lowercase();

        // Test-mention detection (on the cleaned token).
        if !mentions_tests
            && ["test", "spec", "fixture", "mock", "assert"]
                .iter()
                .any(|kw| lower.contains(kw))
        {
            mentions_tests = true;
        }

        // Path fragment: contains '/' or ends with a known source extension.
        if token.contains('/') || SOURCE_EXTENSIONS.iter().any(|ext| lower.ends_with(ext)) {
            path_fragments.push(token.to_string());
            continue;
        }

        // Symbol: contains '_', or has both upper- and lowercase chars (camelCase/PascalCase),
        // or is ALL_CAPS (with at least 2 chars so "I" doesn't qualify).
        let has_upper = token.chars().any(|c| c.is_uppercase());
        let has_lower = token.chars().any(|c| c.is_lowercase());
        let has_underscore = token.contains('_');
        let all_caps = has_upper && !has_lower && token.len() >= 2;
        if has_underscore || (has_upper && has_lower) || all_caps {
            symbols.push(token.to_string());
            plain_tokens.push(lower);
            continue;
        }

        // Stop-word filter.
        if FILE_MODE_STOP_WORDS.contains(&lower.as_str()) {
            continue;
        }

        plain_tokens.push(lower.clone());
        domain_terms.push(lower);
    }

    // Phase 2: compound term extraction from the plain token sequence.
    let compound_terms = extract_compound_terms(&plain_tokens);

    QueryParts {
        symbols,
        path_fragments,
        domain_terms,
        raw: query.to_string(),
        mentions_tests,
        error_strings,
        compound_terms,
    }
}

/// Passthrough decomposition: all tokens go to `domain_terms`, no classification.
/// Used in ablation to isolate the contribution of query decomposition.
fn decompose_query_passthrough(query: &str) -> QueryParts {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|t| t.trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '/' && c != '.'))
        .filter(|t| !t.is_empty())
        .filter(|t| !FILE_MODE_STOP_WORDS.contains(&t.to_lowercase().as_str()))
        .map(|t| t.to_lowercase())
        .collect();
    QueryParts {
        symbols: Vec::new(),
        path_fragments: Vec::new(),
        domain_terms: terms,
        raw: query.to_string(),
        mentions_tests: false,
        error_strings: Vec::new(),
        compound_terms: Vec::new(),
    }
}

/// Build a node dict.
fn node_dict(node: &crate::types::GraphNode, compact: bool, _root: &Utf8Path) -> Value {
    node_to_dict(node, compact)
}

/// Batch node dict (paths already normalized at source; no prefix stripping needed).
fn node_dict_batch(node: &crate::types::GraphNode, compact: bool, _prefix: &Option<()>) -> Value {
    node_to_dict(node, compact)
}

/// Common JS/TS builtin method names filtered from callers_of results.
/// "Who calls .map()?" returns hundreds of hits and is never useful.
static BUILTIN_SET: OnceLock<HashSet<&'static str>> = OnceLock::new();

fn builtin_call_set() -> &'static HashSet<&'static str> {
    BUILTIN_SET.get_or_init(|| {
        [
            "map", "filter", "reduce", "reduceRight", "forEach", "find", "findIndex",
            "some", "every", "includes", "indexOf", "lastIndexOf",
            "push", "pop", "shift", "unshift", "splice", "slice",
            "concat", "join", "flat", "flatMap", "sort", "reverse", "fill",
            "keys", "values", "entries", "from", "isArray", "of", "at",
            "trim", "trimStart", "trimEnd", "split", "replace", "replaceAll",
            "match", "matchAll", "search", "substring", "substr",
            "toLowerCase", "toUpperCase", "startsWith", "endsWith",
            "padStart", "padEnd", "repeat", "charAt", "charCodeAt",
            "assign", "freeze", "defineProperty", "getOwnPropertyNames",
            "hasOwnProperty", "create", "is", "fromEntries",
            "log", "warn", "error", "info", "debug", "trace", "dir", "table",
            "time", "timeEnd", "assert", "clear", "count",
            "then", "catch", "finally", "resolve", "reject", "all", "allSettled", "race", "any",
            "parse", "stringify",
            "floor", "ceil", "round", "random", "max", "min", "abs", "pow", "sqrt",
            "addEventListener", "removeEventListener", "querySelector", "querySelectorAll",
            "getElementById", "createElement", "appendChild", "removeChild",
            "setAttribute", "getAttribute", "preventDefault", "stopPropagation",
            "setTimeout", "clearTimeout", "setInterval", "clearInterval",
            "toString", "valueOf", "toJSON", "toISOString",
            "getTime", "getFullYear", "now",
            "isNaN", "parseInt", "parseFloat", "toFixed",
            "encodeURIComponent", "decodeURIComponent",
            "call", "apply", "bind", "next",
            "emit", "on", "off", "once",
            "pipe", "write", "read", "end", "close", "destroy",
            "send", "status", "json", "redirect",
            "set", "get", "delete", "has",
            "findUnique", "findFirst", "findMany", "createMany",
            "update", "updateMany", "deleteMany", "upsert",
            "aggregate", "groupBy", "transaction",
            "describe", "it", "test", "expect", "beforeEach", "afterEach",
            "beforeAll", "afterAll", "mock", "spyOn",
            "require", "fetch",
        ]
        .into_iter()
        .collect()
    })
}

fn is_builtin_call(name: &str) -> bool {
    builtin_call_set().contains(name)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Validate that a path is a plausible project root.
fn validate_repo_root(path: &Utf8Path) -> Result<Utf8PathBuf> {
    let resolved_std = path.as_std_path().canonicalize().map_err(|e| {
        CrgError::InvalidRepoRoot(format!("{}: {}", path, e))
    })?;
    let resolved_str = crate::paths::normalize_path(&resolved_std.to_string_lossy());
    let resolved = camino::Utf8PathBuf::from(resolved_str);
    if !resolved.is_dir() {
        return Err(CrgError::InvalidRepoRoot(format!(
            "not a directory: {}",
            resolved
        )));
    }
    if !resolved.join(".git").exists() && !resolved.join(".code-review-graph").exists() {
        return Err(CrgError::InvalidRepoRoot(format!(
            "no .git or .code-review-graph found: {}",
            resolved
        )));
    }
    Ok(resolved)
}

/// Resolve repo root and open the graph store.
fn get_store(repo_root: Option<&str>) -> Result<(GraphStore, Utf8PathBuf)> {
    let root = match repo_root {
        Some(p) => validate_repo_root(Utf8Path::new(p))?,
        None => incremental::find_project_root(None),
    };
    let db_path = incremental::get_db_path(&root);
    let store = {
        let _span = tracing::info_span!("graph_open", path = %db_path).entered();
        GraphStore::new(&db_path)?
    };
    Ok((store, root))
}

// ---------------------------------------------------------------------------
// Lazy staleness check
// ---------------------------------------------------------------------------

/// Check if the graph is stale and run a quick incremental update if needed.
/// Only checks git status (fast, ~10-50ms) — doesn't re-hash all files.
/// Skipped if the graph was updated less than 2 seconds ago.
fn maybe_auto_update(store: &mut GraphStore, repo_root: &Utf8Path) {
    let _span = tracing::info_span!("auto_update_check").entered();
    // Skip if graph was updated less than 2 seconds ago
    if let Ok(Some(last)) = store.get_metadata("last_updated") {
        if let Ok(last_time) = chrono::NaiveDateTime::parse_from_str(&last, "%Y-%m-%dT%H:%M:%S") {
            let now = chrono::Utc::now().naive_utc();
            if (now - last_time).num_seconds() < 2 {
                return;
            }
        }
    }

    let changed = crate::incremental::get_staged_and_unstaged(repo_root);
    if changed.is_empty() {
        return;
    }
    // Filter to only files with supported source extensions — skip .json, .md,
    // .lock, etc. to avoid triggering incremental_update for non-parseable files.
    let source_changed: Vec<String> = changed
        .into_iter()
        .filter(|f| {
            crate::parser::detect_language(std::path::Path::new(f)).is_some()
        })
        .collect();
    if source_changed.is_empty() {
        return;
    }
    // Only update if there are actually changed source files not yet in the graph
    if let Err(e) = crate::incremental::incremental_update(repo_root, store, "HEAD", Some(source_changed)) {
        tracing::warn!("auto-update failed: {}", e);
    }
}

// ---------------------------------------------------------------------------
// Tool 1: build_or_update_graph
// ---------------------------------------------------------------------------

pub fn build_or_update_graph(
    full_rebuild: bool,
    repo_root: Option<&str>,
    base: &str,
) -> Result<Value> {
    let (mut store, root) = get_store(repo_root)?;

    let result = if full_rebuild {
        let r = incremental::full_build(&root, &mut store)?;
        store.close()?;
        json!({
            "status": "ok",
            "build_type": "full",
            "summary": format!(
                "Full build complete: parsed {} files, created {} nodes and {} edges.",
                r.files_parsed, r.total_nodes, r.total_edges
            ),
            "files_parsed": r.files_parsed,
            "total_nodes": r.total_nodes,
            "total_edges": r.total_edges,
            "errors": r.errors,
        })
    } else {
        let r = incremental::incremental_update(&root, &mut store, base, None)?;
        store.close()?;
        if r.files_updated == 0 {
            json!({
                "status": "ok",
                "build_type": "incremental",
                "summary": "No changes detected. Graph is up to date.",
                "files_updated": 0,
                "total_nodes": r.total_nodes,
                "total_edges": r.total_edges,
                "changed_files": r.changed_files,
                "dependent_files": r.dependent_files,
                "errors": r.errors,
            })
        } else {
            json!({
                "status": "ok",
                "build_type": "incremental",
                "summary": format!(
                    "Incremental update: {} files re-parsed, {} nodes and {} edges updated. \
                     Changed: {:?}. Dependents also updated: {:?}.",
                    r.files_updated, r.total_nodes, r.total_edges,
                    r.changed_files, r.dependent_files
                ),
                "files_updated": r.files_updated,
                "total_nodes": r.total_nodes,
                "total_edges": r.total_edges,
                "changed_files": r.changed_files,
                "dependent_files": r.dependent_files,
                "changed_qualified_names": r.changed_qualified_names,
                "errors": r.errors,
            })
        }
    };
    Ok(result)
}

// ---------------------------------------------------------------------------
// Tool 2: get_impact_radius
// ---------------------------------------------------------------------------

pub fn get_impact_radius(
    changed_files: Option<Vec<String>>,
    max_depth: usize,
    repo_root: Option<&str>,
    base: &str,
    compact: bool,
) -> Result<Value> {
    let (mut store, root) = get_store(repo_root)?;
    maybe_auto_update(&mut store, &root);
    let files = resolve_changed_files(changed_files, &root, base);
    let result = get_impact_radius_with_store(&store, &root, files, max_depth, compact)?;
    store.close()?;
    Ok(result)
}

pub fn get_impact_radius_with_store(
    store: &GraphStore,
    root: &Utf8Path,
    files: Vec<String>,
    max_depth: usize,
    compact: bool,
) -> Result<Value> {
    const MAX_RESULTS: usize = 500;

    if files.is_empty() {
        return Ok(json!({
            "status": "ok",
            "summary": "No changed files detected.",
            "changed_nodes": [],
            "impacted_nodes": [],
            "impacted_files": [],
            "truncated": false,
            "total_impacted": 0,
        }));
    }

    let abs_files: Vec<String> = files.iter()
        .map(|f| root.join(f).as_str().to_owned())
        .collect();

    let impact = store.get_impact_radius(&abs_files, max_depth, MAX_RESULTS, None)?;

    let prefix: Option<()> = None;
    let changed_dicts: Vec<Value> = impact.changed_nodes.iter().map(|n| node_dict_batch(n, compact, &prefix)).collect();
    let impacted_dicts: Vec<Value> = impact.impacted_nodes.iter().map(|n| node_dict_batch(n, compact, &prefix)).collect();
    let edge_dicts: Vec<Value> = impact.edges.iter().map(edge_to_dict).collect();

    let mut summary_parts = vec![
        format!("Blast radius for {} changed file(s):", files.len()),
        format!("  - {} nodes directly changed", changed_dicts.len()),
        format!(
            "  - {} nodes impacted (within {} hops)",
            impacted_dicts.len(),
            max_depth
        ),
        format!(
            "  - {} additional files affected",
            impact.impacted_files.len()
        ),
    ];
    if impact.truncated {
        summary_parts.push(format!(
            "  - Results truncated: showing {} of {} impacted nodes",
            impacted_dicts.len(),
            impact.total_impacted
        ));
    }

    // impacted_nodes is already sorted by score descending (insertion order from ranked vec)
    let scores_vec: Vec<Value> = impact.impacted_nodes.iter()
        .filter_map(|n| {
            impact.impact_scores.get(&n.qualified_name).map(|&s| {
                json!({ "qualified_name": n.qualified_name, "score": s })
            })
        })
        .collect();

    Ok(json!({
        "status": "ok",
        "summary": summary_parts.join("\n"),
        "algorithm": impact.algorithm,
        "changed_files": files,
        "changed_nodes": changed_dicts,
        "impacted_nodes": impacted_dicts,
        "impact_scores": scores_vec,
        "impacted_files": impact.impacted_files,
        "edges": edge_dicts,
        "truncated": impact.truncated,
        "total_impacted": impact.total_impacted,
    }))
}

// ---------------------------------------------------------------------------
// Tool 3: query_graph
// ---------------------------------------------------------------------------

/// Strongly-typed enum for query patterns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueryPattern {
    CallersOf,
    CalleesOf,
    ImportsOf,
    ImportersOf,
    ChildrenOf,
    TestsFor,
    InheritorsOf,
    FileSummary,
}

impl QueryPattern {
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "callers_of" => Some(Self::CallersOf),
            "callees_of" => Some(Self::CalleesOf),
            "imports_of" => Some(Self::ImportsOf),
            "importers_of" => Some(Self::ImportersOf),
            "children_of" => Some(Self::ChildrenOf),
            "tests_for" => Some(Self::TestsFor),
            "inheritors_of" => Some(Self::InheritorsOf),
            "file_summary" => Some(Self::FileSummary),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::CallersOf => "callers_of",
            Self::CalleesOf => "callees_of",
            Self::ImportsOf => "imports_of",
            Self::ImportersOf => "importers_of",
            Self::ChildrenOf => "children_of",
            Self::TestsFor => "tests_for",
            Self::InheritorsOf => "inheritors_of",
            Self::FileSummary => "file_summary",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::CallersOf => "Find all functions that call a given function",
            Self::CalleesOf => "Find all functions called by a given function",
            Self::ImportsOf => "Find all imports of a given file or module",
            Self::ImportersOf => "Find all files that import a given file or module",
            Self::ChildrenOf => "Find all nodes contained in a file or class",
            Self::TestsFor => "Find all tests for a given function or class",
            Self::InheritorsOf => "Find all classes that inherit from a given class",
            Self::FileSummary => "Get a summary of all nodes in a file",
        }
    }
}

pub fn query_graph(
    pattern: &str,
    target: &str,
    repo_root: Option<&str>,
    compact: bool,
) -> Result<Value> {
    let (mut store, root) = get_store(repo_root)?;
    maybe_auto_update(&mut store, &root);
    let result = query_graph_with_store(&store, &root, pattern, target, compact)?;
    store.close()?;
    Ok(result)
}

pub fn query_graph_with_store(
    store: &GraphStore,
    root: &Utf8Path,
    pattern: &str,
    target: &str,
    compact: bool,
) -> Result<Value> {
    let qp = match QueryPattern::from_str(pattern) {
        Some(p) => p,
        None => {
            let available = [
                "callers_of", "callees_of", "imports_of", "importers_of",
                "children_of", "tests_for", "inheritors_of", "file_summary",
            ];
            return Ok(json!({
                "status": "error",
                "error": format!("Unknown pattern '{}'. Available: {:?}", pattern, available),
            }));
        }
    };
    let description = qp.description();

    // Filter common builtins for callers_of
    if qp == QueryPattern::CallersOf && is_builtin_call(target) && !target.contains("::") {
        return Ok(json!({
            "status": "ok",
            "pattern": pattern,
            "target": target,
            "description": description,
            "summary": format!("'{}' is a common builtin — callers_of skipped to avoid noise.", target),
            "results": [],
            "edges": [],
        }));
    }

    // Resolve the target node (file_summary bypasses ambiguous-node early return)
    let mut target_name = target.to_string();
    let node_opt = resolve_target_node(store, target, root)?;

    let node = match node_opt {
        ResolveResult::Found(n) => {
            target_name = n.qualified_name.clone();
            Some(n)
        }
        ResolveResult::Ambiguous(candidates) if qp != QueryPattern::FileSummary => {
            return Ok(json!({
                "status": "ambiguous",
                "summary": format!("Multiple matches for '{}'. Please use a qualified name.", target),
                "candidates": candidates,
            }));
        }
        ResolveResult::Ambiguous(_) => None,
        ResolveResult::NotFound => None,
    };

    if node.is_none() && qp != QueryPattern::FileSummary {
        return Ok(json!({
            "status": "not_found",
            "summary": format!("No node found matching '{}'.", target),
        }));
    }

    let qn: String = node.as_ref().map(|n| n.qualified_name.clone()).unwrap_or_else(|| target_name.clone());

    let mut results: Vec<Value> = vec![];
    let mut edges_out: Vec<Value> = vec![];

    match qp {
        QueryPattern::CallersOf => {
            for e in store.get_edges_by_target(&qn)? {
                if e.kind == EdgeKind::Calls {
                    if let Some(caller) = store.get_node(&e.source_qualified)? {
                        results.push(node_dict(&caller, compact, root));
                    }
                    edges_out.push(edge_to_dict(&e));
                }
            }
            // Fallback: search by plain name when qualified lookup found nothing
            if results.is_empty() {
                if let Some(ref n) = node {
                    for e in store.search_edges_by_target_name(&n.name)? {
                        if let Some(caller) = store.get_node(&e.source_qualified)? {
                            results.push(node_dict(&caller, compact, root));
                        }
                        edges_out.push(edge_to_dict(&e));
                    }
                }
            }
        }
        QueryPattern::CalleesOf => {
            for e in store.get_edges_by_source(&qn)? {
                if e.kind == EdgeKind::Calls {
                    if let Some(callee) = store.get_node(&e.target_qualified)? {
                        results.push(node_dict(&callee, compact, root));
                    }
                    edges_out.push(edge_to_dict(&e));
                }
            }
        }
        QueryPattern::ImportsOf => {
            for e in store.get_edges_by_source(&qn)? {
                if e.kind == EdgeKind::ImportsFrom {
                    results.push(json!({ "import_target": e.target_qualified }));
                    edges_out.push(edge_to_dict(&e));
                }
            }
        }
        QueryPattern::ImportersOf => {
            let abs_target = node.as_ref()
                .map(|n| n.file_path.clone())
                .unwrap_or_else(|| root.join(target).as_str().to_owned());
            for e in store.get_edges_by_target(&abs_target)? {
                if e.kind == EdgeKind::ImportsFrom {
                    results.push(json!({ "importer": e.source_qualified, "file": e.file_path }));
                    edges_out.push(edge_to_dict(&e));
                }
            }
        }
        QueryPattern::ChildrenOf => {
            for e in store.get_edges_by_source(&qn)? {
                if e.kind == EdgeKind::Contains {
                    if let Some(child) = store.get_node(&e.target_qualified)? {
                        results.push(node_dict(&child, compact, root));
                    }
                }
            }
        }
        QueryPattern::TestsFor => {
            for e in store.get_edges_by_target(&qn)? {
                if e.kind == EdgeKind::TestedBy {
                    if let Some(t) = store.get_node(&e.source_qualified)? {
                        results.push(node_dict(&t, compact, root));
                    }
                }
            }
            // Naming convention fallback
            let name = node.as_ref().map(|n| n.name.as_str()).unwrap_or(target);
            let seen: HashSet<String> = results.iter()
                .filter_map(|r| r.get("qualified_name").and_then(|v| v.as_str()).map(|s| s.to_string()))
                .collect();
            for prefix in &[format!("test_{name}"), format!("Test{name}")] {
                for t in store.search_nodes(prefix, 10)? {
                    if !seen.contains(&t.qualified_name) && t.is_test {
                        results.push(node_dict(&t, compact, root));
                    }
                }
            }
        }
        QueryPattern::InheritorsOf => {
            for e in store.get_edges_by_target(&qn)? {
                if matches!(e.kind, EdgeKind::Inherits | EdgeKind::Implements) {
                    if let Some(child) = store.get_node(&e.source_qualified)? {
                        results.push(node_dict(&child, compact, root));
                    }
                    edges_out.push(edge_to_dict(&e));
                }
            }
        }
        QueryPattern::FileSummary => {
            // Prefer the canonical file_path stored in the graph node — this
            // avoids root.join() mangling on Windows when `target` is already
            // an absolute path or uses a different path separator.
            let abs_path = match &node {
                Some(n) => n.file_path.clone(),
                None => {
                    // Try absolute target first (handles Windows absolute paths),
                    // then fall back to root-relative join.
                    let as_is = target.to_string();
                    if store.get_nodes_by_file(&as_is)?.is_empty() {
                        root.join(target).as_str().to_owned()
                    } else {
                        as_is
                    }
                }
            };
            for n in store.get_nodes_by_file(&abs_path)? {
                results.push(node_dict(&n, compact, root));
            }
        }
    }

    Ok(json!({
        "status": "ok",
        "pattern": qp.as_str(),
        "target": target_name,
        "description": description,
        "summary": format!("Found {} result(s) for {}('{}')", results.len(), qp.as_str(), target_name),
        "results": results,
        "edges": edges_out,
    }))
}

// ---------------------------------------------------------------------------
// Tool 4: get_review_context
// ---------------------------------------------------------------------------

pub fn get_review_context(
    changed_files: Option<Vec<String>>,
    max_depth: usize,
    include_source: bool,
    max_lines_per_file: usize,
    repo_root: Option<&str>,
    base: &str,
    compact: bool,
) -> Result<Value> {
    let (mut store, root) = get_store(repo_root)?;
    maybe_auto_update(&mut store, &root);
    let files = resolve_changed_files(changed_files, &root, base);
    let result = get_review_context_with_store(&store, &root, files, max_depth, include_source, max_lines_per_file, compact)?;
    store.close()?;
    Ok(result)
}

pub fn get_review_context_with_store(
    store: &GraphStore,
    root: &Utf8Path,
    files: Vec<String>,
    max_depth: usize,
    include_source: bool,
    max_lines_per_file: usize,
    compact: bool,
) -> Result<Value> {
    if files.is_empty() {
        return Ok(json!({
            "status": "ok",
            "summary": "No changes detected. Nothing to review.",
            "context": {},
        }));
    }

    let abs_files: Vec<String> = files.iter()
        .map(|f| root.join(f).as_str().to_owned())
        .collect();

    let impact = store.get_impact_radius(&abs_files, max_depth, 500, None)?;

    let rc_prefix: Option<()> = None;
    let rc_changed: Vec<Value> = impact.changed_nodes.iter().map(|n| node_dict_batch(n, compact, &rc_prefix)).collect();
    let rc_impacted: Vec<Value> = impact.impacted_nodes.iter().map(|n| node_dict_batch(n, compact, &rc_prefix)).collect();

    let mut context = json!({
        "changed_files": files,
        "impacted_files": impact.impacted_files,
        "graph": {
            "changed_nodes": rc_changed,
            "impacted_nodes": rc_impacted,
            "edges": impact.edges.iter().map(edge_to_dict).collect::<Vec<_>>(),
        },
    });

    if include_source {
        let mut snippets = serde_json::Map::new();
        for rel_path in &files {
            let full_path = root.join(rel_path);
            let content = match std::fs::read_to_string(&full_path) {
                Ok(s) => s,
                Err(_) => {
                    snippets.insert(rel_path.clone(), json!("(could not read file)"));
                    continue;
                }
            };
            let lines: Vec<&str> = content.lines().collect();
            let snippet = if lines.len() > max_lines_per_file {
                extract_relevant_lines(&lines, &impact.changed_nodes, full_path.as_str())
            } else {
                lines.iter().enumerate()
                    .map(|(i, line)| format!("{}: {}", i + 1, line))
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            snippets.insert(rel_path.clone(), json!(snippet));
        }
        context["source_snippets"] = Value::Object(snippets);
    }

    let guidance = generate_review_guidance(&impact);
    context["review_guidance"] = json!(guidance);

    let summary_parts = [
        format!("Review context for {} changed file(s):", files.len()),
        format!("  - {} directly changed nodes", impact.changed_nodes.len()),
        format!(
            "  - {} impacted nodes in {} files",
            impact.impacted_nodes.len(),
            impact.impacted_files.len()
        ),
        String::new(),
        "Review guidance:".to_string(),
        guidance.clone(),
    ];

    Ok(json!({
        "status": "ok",
        "summary": summary_parts.join("\n"),
        "context": context,
    }))
}

// ---------------------------------------------------------------------------
// Tool 5: semantic_search_nodes
// ---------------------------------------------------------------------------

pub fn semantic_search_nodes(
    query: &str,
    kind: Option<&str>,
    limit: usize,
    repo_root: Option<&str>,
    compact: bool,
) -> Result<Value> {
    let (mut store, root) = get_store(repo_root)?;
    maybe_auto_update(&mut store, &root);
    let emb_db_path = incremental::get_embeddings_db_path(&root);
    let mut emb_store = {
        let _span = tracing::info_span!("embedding_load").entered();
        EmbeddingStore::new(&emb_db_path)?
    };
    let result = semantic_search_nodes_with_store(&store, &mut emb_store, &root, query, kind, limit, compact, None)?;
    emb_store.close()?;
    store.close()?;
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
pub fn semantic_search_nodes_with_store(
    store: &GraphStore,
    emb_store: &mut EmbeddingStore,
    root: &Utf8Path,
    query: &str,
    kind: Option<&str>,
    limit: usize,
    compact: bool,
    keyword_hits: Option<Vec<crate::types::GraphNode>>,
) -> Result<Value> {
    let search_mode;

    let results: Vec<Value> = if emb_store.available() && emb_store.count()? > 0 {
        search_mode = "semantic";
        let mut raw = semantic_search(query, store, emb_store, limit * 2, compact, root)?;
        if let Some(k) = kind {
            raw.retain(|r| r.get("kind").and_then(|v| v.as_str()) == Some(k));
        }
        raw.truncate(limit);
        raw
    } else {
        search_mode = "keyword";
        let mut nodes = match keyword_hits {
            Some(nodes) => nodes,
            None => store.search_nodes(query, limit * 2)?,
        };
        if let Some(k) = kind {
            nodes.retain(|n| n.kind.as_str() == k);
        }
        let q_lower = query.to_lowercase();
        nodes.sort_by_key(|n| {
            let name_lower = n.name.to_lowercase();
            if name_lower == q_lower { 0 }
            else if name_lower.starts_with(&q_lower) { 1 }
            else { 2 }
        });
        nodes.truncate(limit);
        nodes.iter().map(|n| node_dict(n, compact, root)).collect()
    };

    // Adaptive source snippets: top 3 results get 10 lines of source inline.
    // This eliminates follow-up Read calls for the most relevant hits.
    let mut enriched = results;
    for (i, result) in enriched.iter_mut().enumerate() {
        if i >= 3 { break; }
        if let (Some(fp), Some(ls)) = (
            result.get("file_path").and_then(|v| v.as_str()),
            result.get("line_start").and_then(|v| v.as_u64()),
        ) {
            // Try to resolve the file path (may be relative after compact stripping)
            let full_path = if std::path::Path::new(fp).is_absolute() {
                fp.to_string()
            } else {
                root.join(fp).to_string()
            };
            if let Ok(content) = std::fs::read_to_string(&full_path) {
                let start = ls.saturating_sub(1) as usize;
                let snippet: String = content
                    .lines()
                    .skip(start)
                    .take(10)
                    .enumerate()
                    .map(|(j, line)| format!("{:>4}: {}", start + j + 1, line))
                    .collect::<Vec<_>>()
                    .join("\n");
                result["source_preview"] = json!(snippet);
            }
        }
    }

    let kind_suffix = kind.map(|k| format!(" (kind={k})")).unwrap_or_default();
    Ok(json!({
        "status": "ok",
        "query": query,
        "search_mode": search_mode,
        "summary": format!("Found {} node(s) matching '{}'{}", enriched.len(), query, kind_suffix),
        "results": enriched,
    }))
}

// ---------------------------------------------------------------------------
// Tool 6: list_graph_stats
// ---------------------------------------------------------------------------

pub fn list_graph_stats(repo_root: Option<&str>) -> Result<Value> {
    let (mut store, root) = get_store(repo_root)?;
    maybe_auto_update(&mut store, &root);
    let result = list_graph_stats_with_store(&store, &root)?;
    store.close()?;
    Ok(result)
}

pub fn list_graph_stats_with_store(
    store: &GraphStore,
    root: &Utf8Path,
) -> Result<Value> {
    let stats = store.get_stats()?;

    let root_name = root.file_name()
        .map(|n| n.to_owned())
        .unwrap_or_else(|| root.as_str().to_owned());

    let languages = if stats.languages.is_empty() {
        "none".to_string()
    } else {
        stats.languages.join(", ")
    };

    let mut summary_parts = vec![
        format!("Graph statistics for {root_name}:"),
        format!("  Files: {}", stats.files_count),
        format!("  Total nodes: {}", stats.total_nodes),
        format!("  Total edges: {}", stats.total_edges),
        format!("  Languages: {languages}"),
        format!("  Last updated: {}", stats.last_updated.as_deref().unwrap_or("never")),
        String::new(),
        "Nodes by kind:".to_string(),
    ];
    let mut nodes_by_kind_sorted: Vec<_> = stats.nodes_by_kind.iter().collect();
    nodes_by_kind_sorted.sort_by_key(|(k, _)| k.as_str());
    for (kind, count) in &nodes_by_kind_sorted {
        summary_parts.push(format!("  {kind}: {count}"));
    }
    summary_parts.push(String::new());
    summary_parts.push("Edges by kind:".to_string());
    let mut edges_by_kind_sorted: Vec<_> = stats.edges_by_kind.iter().collect();
    edges_by_kind_sorted.sort_by_key(|(k, _)| k.as_str());
    for (kind, count) in &edges_by_kind_sorted {
        summary_parts.push(format!("  {kind}: {count}"));
    }

    // Embedding info
    let emb_db_path = incremental::get_embeddings_db_path(root);
    let emb_count = EmbeddingStore::new(&emb_db_path)
        .and_then(|es| es.count())
        .unwrap_or(0);
    summary_parts.push(String::new());
    summary_parts.push(format!("Embeddings: {emb_count} nodes embedded"));

    Ok(json!({
        "status": "ok",
        "summary": summary_parts.join("\n"),
        "total_nodes": stats.total_nodes,
        "total_edges": stats.total_edges,
        "nodes_by_kind": stats.nodes_by_kind,
        "edges_by_kind": stats.edges_by_kind,
        "languages": stats.languages,
        "files_count": stats.files_count,
        "last_updated": stats.last_updated,
        "embeddings_count": emb_count,
    }))
}

// ---------------------------------------------------------------------------
// Tool 7: embed_graph
// ---------------------------------------------------------------------------

pub fn embed_graph(repo_root: Option<&str>) -> Result<Value> {
    let (store, root) = get_store(repo_root)?;
    let emb_db_path = incremental::get_embeddings_db_path(&root);
    let mut emb_store = EmbeddingStore::new(&emb_db_path)?;

    if !emb_store.available() {
        emb_store.close()?;
        store.close()?;
        return Ok(json!({
            "status": "error",
            "error": "No embedding provider configured. Set EMBEDDING_PROVIDER=openai|voyage|gemini \
                      and the corresponding API key. Semantic search falls back to keyword matching.",
        }));
    }

    let newly_embedded = embed_all_nodes(&store, &mut emb_store)?;
    let newly_embedded_files = embed_all_files(&store, &mut emb_store)?;
    let total = emb_store.count()?;
    let total_files = emb_store.file_count()?;
    emb_store.close()?;
    store.close()?;

    Ok(json!({
        "status": "ok",
        "summary": format!(
            "Embedded {} new node(s), {} new file(s). Total: {} nodes, {} files. Semantic search is now active.",
            newly_embedded, newly_embedded_files, total, total_files
        ),
        "newly_embedded": newly_embedded,
        "newly_embedded_files": newly_embedded_files,
        "total_embeddings": total,
        "total_file_embeddings": total_files,
    }))
}

// ---------------------------------------------------------------------------
// Tool 8: get_docs_section
// ---------------------------------------------------------------------------

pub fn get_docs_section(section_name: &str, repo_root: Option<&str>) -> Result<Value> {
    let mut search_roots: Vec<Utf8PathBuf> = vec![];

    if let Some(p) = repo_root {
        search_roots.push(Utf8PathBuf::from(p));
    }

    if let Ok((store, root)) = get_store(repo_root) {
        let _ = store.close();
        if !search_roots.contains(&root) {
            search_roots.push(root);
        }
    }

    let section_re = regex::Regex::new(&format!(
        r#"(?is)<section name="{}">(.*?)</section>"#,
        regex::escape(section_name)
    ))
    .map_err(|e| CrgError::Other(e.to_string()))?;

    for search_root in &search_roots {
        let candidate = search_root.join("docs").join("LLM-OPTIMIZED-REFERENCE.md");
        if candidate.exists() {
            let content = std::fs::read_to_string(&candidate)?;
            if let Some(cap) = section_re.captures(&content) {
                let text = cap.get(1).map(|m| m.as_str().trim().to_string()).unwrap_or_default();
                return Ok(json!({
                    "status": "ok",
                    "section": section_name,
                    "content": text,
                }));
            }
        }
    }

    let available = [
        "usage", "review-delta", "review-pr", "commands",
        "legal", "watch", "embeddings", "languages", "troubleshooting",
    ];
    Ok(json!({
        "status": "not_found",
        "error": format!(
            "Section '{}' not found. Available: {}",
            section_name,
            available.join(", ")
        ),
    }))
}

// ---------------------------------------------------------------------------
// Tool 9: find_large_functions
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Tool 10: hybrid_query
// ---------------------------------------------------------------------------

/// Merge graph keyword search with semantic search via configurable fusion.
///
/// Supports two fusion modes via the `fusion` parameter:
/// - `"rrf"` (default): Reciprocal Rank Fusion — Σ 1 / (k + rank_i), k = 60.
///   Results include an `rrf_score` field.
/// - `"cc"`: Convex Combination — min-max normalised scores combined as
///   α·keyword + (1-α)·semantic, α = 0.5. Results include a `cc_score` field.
///
/// Falls back to keyword-only when no embeddings are available, and sets
/// `method: "keyword_only"` in the returned JSON to signal this.
/// CC mode also falls back to RRF when embeddings are unavailable.
// ---------------------------------------------------------------------------
// Query classification for adaptive routing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub(crate) enum QueryRoute {
    ExactSymbol,  // camelCase, PascalCase, snake_case identifiers
    FilePath,     // contains / or file extensions
    Causal,       // "why", "how does", "where does" patterns
    ConfigLookup, // config-related terms
    General,      // default fallback
}

pub(crate) struct Classification {
    pub route: QueryRoute,
    pub confidence: f64, // 0.0 to 1.0
}

pub(crate) fn classify_query(query: &str) -> Classification {
    let trimmed = query.trim();
    let words: Vec<&str> = trimmed.split_whitespace().collect();

    // Single token that looks like an identifier (camelCase, PascalCase, snake_case)
    if words.len() == 1 {
        let word = words[0];
        let has_uppercase = word.chars().any(|c| c.is_uppercase());
        let has_lowercase = word.chars().any(|c| c.is_lowercase());
        let has_underscore = word.contains('_');
        if (has_uppercase && has_lowercase) || has_underscore {
            return Classification { route: QueryRoute::ExactSymbol, confidence: 0.9 };
        }
    }

    // Contains file path indicators
    if trimmed.contains('/')
        || trimmed.contains(".ts")
        || trimmed.contains(".js")
        || trimmed.contains(".tsx")
        || trimmed.contains(".rs")
    {
        return Classification { route: QueryRoute::FilePath, confidence: 0.85 };
    }

    // Causal/architectural patterns
    let lower = trimmed.to_lowercase();
    if lower.starts_with("why ")
        || lower.starts_with("how does ")
        || lower.starts_with("where does ")
        || lower.starts_with("what causes ")
        || lower.contains("included in")
        || lower.contains("loaded by")
    {
        return Classification { route: QueryRoute::Causal, confidence: 0.8 };
    }

    // Config-related terms
    let config_terms = ["config", "option", "setting", "experimental", "flag", "enable", "disable"];
    let config_matches = config_terms.iter().filter(|t| lower.contains(*t)).count();
    if config_matches >= 2 {
        return Classification { route: QueryRoute::ConfigLookup, confidence: 0.75 };
    }
    if config_matches == 1 && words.len() <= 4 {
        return Classification { route: QueryRoute::ConfigLookup, confidence: 0.6 };
    }

    // Default: general hybrid
    Classification { route: QueryRoute::General, confidence: 0.5 }
}

// ---------------------------------------------------------------------------
// File-mode fanout+rerank: implementation
// ---------------------------------------------------------------------------

/// Convert a ranked list of `GraphNode`s into `NodeCandidate` entries using
/// standard RRF rank-to-score: `1.0 / (60.0 + rank + 1.0)`.
fn nodes_to_candidates(
    nodes: &[crate::types::GraphNode],
    source: CandidateSource,
    pool: &mut HashMap<String, NodeCandidate>,
) {
    for (rank, node) in nodes.iter().enumerate() {
        let rrf_score = 1.0 / (RRF_K + rank as f64 + 1.0);
        let entry = pool.entry(node.qualified_name.clone()).or_insert_with(|| NodeCandidate {
            qualified_name: node.qualified_name.clone(),
            file_path: node.file_path.clone(),
            kind: node.kind,
            is_test: node.is_test,
            score: 0.0,
            sources: HashSet::new(),
        });
        entry.score += rrf_score;
        entry.sources.insert(source);
    }
}

/// Fan out a query to multiple evidence channels and return a pool of
/// `NodeCandidate`s keyed by `qualified_name`.
///
/// Channels used:
/// 1. Keyword relaxed    — always run; results are also reused by path/config channels.
/// 2. Keyword exact      — symbols + compound_terms, when fanout enabled.
/// 2b. Error strings     — exact search for each quoted/backtick span, when fanout enabled.
/// 3. Path-boosted       — only when `parts.path_fragments` is non-empty.
/// 4. Config-boosted     — only when domain_terms contains config/settings/options/env.
/// 5. Semantic           — only when `emb_store` has embeddings.
/// 6. Tantivy            — only when `kw_hits` is `Some`.
#[allow(clippy::too_many_arguments)]
fn fanout_retrieve(
    store: &GraphStore,
    emb_store: &mut EmbeddingStore,
    root: &Utf8Path,
    parts: &QueryParts,
    limit: usize,
    kw_hits: Option<Vec<crate::types::GraphNode>>,
    ablation: &AblationConfig,
) -> Result<HashMap<String, NodeCandidate>> {
    let pool_limit = limit * 5;
    let mut pool: HashMap<String, NodeCandidate> = HashMap::new();

    // Channel 1: keyword relaxed (always).
    let relaxed_hits = store.search_nodes_relaxed(&parts.raw, pool_limit)?;
    nodes_to_candidates(&relaxed_hits, CandidateSource::KeywordRelaxed, &mut pool);

    // Channel 2: keyword exact — symbols + compound_terms when fanout is enabled.
    if ablation.fanout && (!parts.symbols.is_empty() || !parts.compound_terms.is_empty()) {
        let mut exact_terms: Vec<&str> = parts.symbols.iter().map(|s| s.as_str()).collect();
        for ct in &parts.compound_terms {
            exact_terms.push(ct.as_str());
        }
        let exact_query = exact_terms.join(" ");
        let exact_hits = store.search_nodes(&exact_query, pool_limit)?;
        nodes_to_candidates(&exact_hits, CandidateSource::KeywordExact, &mut pool);
    }

    // Channel 2b: error strings — exact search per quoted/backtick span.
    if ablation.fanout && !parts.error_strings.is_empty() {
        for err_str in &parts.error_strings {
            let err_hits = store.search_nodes(err_str, pool_limit)?;
            nodes_to_candidates(&err_hits, CandidateSource::KeywordExact, &mut pool);
        }
    }

    // Channel 3: path-boosted — reweight relaxed results for nodes with path-like qualified names.
    if ablation.fanout && !parts.path_fragments.is_empty() {
        let mut path_boosted: Vec<crate::types::GraphNode> = relaxed_hits
            .iter()
            .filter(|n| n.qualified_name.contains('/') || n.qualified_name.contains('.'))
            .cloned()
            .collect();
        // Re-sort boosted list (boosted nodes first, then append the rest).
        let mut non_path: Vec<crate::types::GraphNode> = relaxed_hits
            .iter()
            .filter(|n| !n.qualified_name.contains('/') && !n.qualified_name.contains('.'))
            .cloned()
            .collect();
        path_boosted.append(&mut non_path);
        path_boosted.truncate(pool_limit);
        nodes_to_candidates(&path_boosted, CandidateSource::PathBoosted, &mut pool);
    }

    // Channel 4: config-boosted — reweight relaxed results for Type-kind nodes.
    let config_trigger_terms = ["config", "settings", "options", "env"];
    let triggers_config = parts.domain_terms.iter().any(|t| config_trigger_terms.contains(&t.as_str()));
    if ablation.fanout && triggers_config {
        let mut config_boosted: Vec<crate::types::GraphNode> = relaxed_hits
            .iter()
            .filter(|n| matches!(n.kind, NodeKind::Type))
            .cloned()
            .collect();
        let mut non_config: Vec<crate::types::GraphNode> = relaxed_hits
            .iter()
            .filter(|n| !matches!(n.kind, NodeKind::Type))
            .cloned()
            .collect();
        config_boosted.append(&mut non_config);
        config_boosted.truncate(pool_limit);
        nodes_to_candidates(&config_boosted, CandidateSource::ConfigBoosted, &mut pool);
    }

    // Channel 5: semantic — only when embeddings are available and semantic is enabled.
    let embeddings_available = emb_store.available() && emb_store.count().unwrap_or(0) > 0;
    if ablation.semantic && embeddings_available {
        let sem_hits = crate::embeddings::semantic_search(&parts.raw, store, emb_store, pool_limit, true, root)?;
        let sem_nodes: Vec<crate::types::GraphNode> = sem_hits
            .iter()
            .filter_map(|hit| {
                hit.get("qualified_name")
                    .and_then(|v| v.as_str())
                    .and_then(|qn| store.get_node(qn).ok().flatten())
            })
            .collect();
        nodes_to_candidates(&sem_nodes, CandidateSource::Semantic, &mut pool);
    }

    // Channel 6: Tantivy — only when pre-computed hits are provided and fanout is enabled.
    if ablation.fanout {
        if let Some(tantivy_nodes) = kw_hits {
            nodes_to_candidates(&tantivy_nodes, CandidateSource::Tantivy, &mut pool);
        }
    }

    Ok(pool)
}

/// Expand the candidate pool by following CALLS and CONTAINS edges 1 hop from
/// the highest-scoring candidates. This surfaces nodes in adjacent files that
/// the keyword/semantic channels might have missed.
///
/// - Seeds: top 20% of candidates by score, or at least the top 5.
/// - For each seed: follow outgoing CALLS/CONTAINS edges (callees) and incoming
///   CALLS edges (callers).
/// - New candidates get score = `0.3 * parent_score` with source `Expansion`.
/// - Existing candidates with a higher score are never overwritten.
/// - Total new candidates added is capped at `limit * 2`.
fn expand_candidates(
    store: &GraphStore,
    candidates: &mut HashMap<String, NodeCandidate>,
    limit: usize,
) -> Result<()> {
    if candidates.is_empty() {
        return Ok(());
    }

    // Collect and sort by score descending to identify seeds.
    let mut ranked: Vec<(String, f64)> = candidates
        .iter()
        .map(|(qn, c)| (qn.clone(), c.score))
        .collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let seed_count = {
        let twenty_pct = (ranked.len() as f64 * 0.2).ceil() as usize;
        twenty_pct.max(5).min(ranked.len())
    };
    let seeds: Vec<(String, f64)> = ranked.into_iter().take(seed_count).collect();

    let cap = limit * 2;
    let mut added = 0usize;

    // Insert a new expansion candidate, returning true if the cap is now reached.
    let mut try_insert = |candidates: &mut HashMap<String, NodeCandidate>,
                          neighbor_qn: &str,
                          node: crate::types::GraphNode,
                          score: f64| {
        candidates.insert(
            neighbor_qn.to_owned(),
            NodeCandidate {
                qualified_name: neighbor_qn.to_owned(),
                file_path: node.file_path,
                kind: node.kind,
                is_test: node.is_test,
                score,
                sources: HashSet::from([CandidateSource::Expansion]),
            },
        );
        added += 1;
        added >= cap
    };

    'seeds: for (qn, parent_score) in seeds {
        let expansion_score = 0.3 * parent_score;

        // Outgoing edges: callees and contained nodes.
        let out_edges = store.get_edges_by_source(&qn)?;
        for edge in &out_edges {
            if !matches!(edge.kind, EdgeKind::Calls | EdgeKind::Contains) {
                continue;
            }
            let neighbor_qn = &edge.target_qualified;
            if candidates.contains_key(neighbor_qn) {
                continue;
            }
            if let Some(node) = store.get_node(neighbor_qn)? {
                if try_insert(candidates, neighbor_qn, node, expansion_score) {
                    break 'seeds;
                }
            }
        }

        // Incoming edges: callers (CALLS only).
        let in_edges = store.get_edges_by_target(&qn)?;
        for edge in &in_edges {
            if !matches!(edge.kind, EdgeKind::Calls) {
                continue;
            }
            let neighbor_qn = &edge.source_qualified;
            if candidates.contains_key(neighbor_qn) {
                continue;
            }
            if let Some(node) = store.get_node(neighbor_qn)? {
                if try_insert(candidates, neighbor_qn, node, expansion_score) {
                    break 'seeds;
                }
            }
        }
    }

    Ok(())
}

/// Aggregate a candidate pool into file-level results with capped top-k scoring
/// and conditional priors.
fn aggregate_to_files(
    candidates: HashMap<String, NodeCandidate>,
    parts: &QueryParts,
    limit: usize,
    ablation: &AblationConfig,
) -> Vec<FileResult> {
    // Group by file_path.
    let mut file_map: HashMap<String, Vec<NodeCandidate>> = HashMap::new();
    for (_, candidate) in candidates {
        file_map.entry(candidate.file_path.clone()).or_default().push(candidate);
    }

    let mut results: Vec<FileResult> = file_map
        .into_iter()
        .map(|(file_path, mut nodes)| {
            // Sort nodes by score descending for capped top-k aggregation.
            nodes.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

            // Capped top-k base score: top node * 1.5 + 0.3 * sum of next 2.
            // Heavier top-node weighting prevents many weak-match files from beating
            // a file with one strong KeywordExact hit.
            // When scorer is ablated, use simple sum of all node scores.
            let base = if ablation.scorer {
                let top = nodes[0].score;
                let rest: f64 = nodes[1..nodes.len().min(3)]
                    .iter()
                    .map(|n| n.score)
                    .sum::<f64>();
                top * 1.5 + 0.3 * rest
            } else {
                nodes.iter().map(|n| n.score).sum::<f64>()
            };

            // KeywordExact boost: the query contained the precise symbol name; reward specificity.
            let exact_symbol_boost = if ablation.priors && nodes[0].sources.contains(&CandidateSource::KeywordExact) {
                1.5
            } else {
                1.0
            };

            // Gather all unique sources across nodes in this file.
            let file_sources: HashSet<CandidateSource> = nodes
                .iter()
                .flat_map(|n| n.sources.iter().copied())
                .collect();
            let n_unique_sources = file_sources.len();

            // Conditional priors (all neutralized when priors ablation is off).

            // Test demotion: 0.5x when ALL nodes are Test-kind AND query doesn't mention tests.
            let test_prior = if ablation.priors && !parts.mentions_tests
                && nodes.iter().all(|n| matches!(n.kind, NodeKind::Test) || n.is_test)
            {
                0.5
            } else {
                1.0
            };

            // Compiled/generated demotion.
            let compiled_prior = if ablation.priors && ["/compiled/", "/node_modules/", "/.next/", "/dist/", "/vendor/"]
                .iter()
                .any(|seg| file_path.contains(seg))
            {
                0.5
            } else {
                1.0
            };

            // Multi-source boost.
            let multi_source = if ablation.priors {
                1.0 + 0.15 * (n_unique_sources.saturating_sub(1)) as f64
            } else {
                1.0
            };

            // Symbol-in-path boost: 1.3x if any query symbol appears as a substring of the path.
            let file_path_lower = file_path.to_lowercase();
            let symbol_path = if ablation.priors && parts.symbols.iter().any(|sym| {
                file_path_lower.contains(&sym.to_lowercase())
            }) {
                1.3
            } else {
                1.0
            };

            let final_score = base * exact_symbol_boost * test_prior * compiled_prior * multi_source * symbol_path;

            // Top-3 evidence nodes.
            let top_nodes: Vec<NodeEvidence> = nodes
                .iter()
                .take(3)
                .map(|n| {
                    let short_name = n.qualified_name.split("::").last().unwrap_or(&n.qualified_name).to_string();
                    let name_lower = short_name.to_lowercase();
                    let qn_lower = n.qualified_name.to_lowercase();
                    // symbols are original-case; domain_terms are already lowercase (from decompose_query).
                    let matched_terms: Vec<String> = parts.symbols.iter()
                        .filter(|sym| {
                            let s = sym.to_lowercase();
                            name_lower.contains(&s) || qn_lower.contains(&s) || file_path_lower.contains(&s)
                        })
                        .chain(parts.domain_terms.iter().filter(|t| {
                            name_lower.contains(t.as_str()) || qn_lower.contains(t.as_str()) || file_path_lower.contains(t.as_str())
                        }))
                        .cloned()
                        .collect();
                    NodeEvidence {
                        name: short_name,
                        qualified_name: n.qualified_name.clone(),
                        kind: n.kind.as_str().to_string(),
                        line_start: 0, // populated below from the store if needed; see note
                        line_end: 0,
                        score: n.score,
                        sources: n.sources.iter().map(|s| s.as_str().to_string()).collect(),
                        matched_terms,
                        is_test: n.is_test,
                    }
                })
                .collect();

            FileResult {
                file_path,
                score: final_score,
                top_nodes,
                sources: file_sources,
            }
        })
        .collect();

    results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    results.truncate(limit);
    results
}

/// Serialise file-level results into the MCP response JSON.
fn file_results_to_json(
    results: Vec<FileResult>,
    query: &str,
    parts: &QueryParts,
    debug: Option<bool>,
    route: Option<&str>,
    fusion: Option<&str>,
    total_candidates: usize,
    files_scored: usize,
) -> Value {
    let result_arr: Vec<Value> = results
        .iter()
        .map(|r| {
            let mut sources: Vec<&str> = r.sources.iter().map(|s| s.as_str()).collect();
            sources.sort();
            let top_nodes: Vec<Value> = r.top_nodes.iter().map(|n| {
                let mut srcs: Vec<&str> = n.sources.iter().map(|s| s.as_str()).collect();
                srcs.sort();
                json!({
                    "name": n.name,
                    "qualified_name": n.qualified_name,
                    "kind": n.kind,
                    "line_start": n.line_start,
                    "line_end": n.line_end,
                    "score": n.score,
                    "sources": srcs,
                    "matched_terms": n.matched_terms,
                    "is_test": n.is_test,
                })
            }).collect();
            json!({
                "file_path": r.file_path,
                "score": r.score,
                "sources": sources,
                "top_nodes": top_nodes,
            })
        })
        .collect();

    let mut response = json!({
        "status": "ok",
        "query": query,
        "method": "fanout_file",
        "result_mode": "file",
        "results": result_arr,
    });

    if debug.unwrap_or(false) {
        let channels_used: Vec<&str> = {
            let mut all_sources: HashSet<CandidateSource> = HashSet::new();
            for r in &results {
                for s in &r.sources {
                    all_sources.insert(*s);
                }
            }
            let mut v: Vec<&str> = all_sources.iter().map(|s| s.as_str()).collect();
            v.sort();
            v
        };

        let mut debug_obj = json!({
            "channels_used": channels_used,
            "total_candidates": total_candidates,
            "files_scored": files_scored,
            "query_parts": {
                "symbols": parts.symbols,
                "path_fragments": parts.path_fragments,
                "domain_terms": parts.domain_terms,
                "mentions_tests": parts.mentions_tests,
                "error_strings": parts.error_strings,
                "compound_terms": parts.compound_terms,
            },
        });

        // Note ignored params when route/fusion are provided alongside file mode.
        let mut ignored: Vec<&str> = Vec::new();
        if route.is_some() { ignored.push("route"); }
        if fusion.is_some() { ignored.push("fusion"); }
        if !ignored.is_empty() {
            debug_obj["ignored_params"] = json!(ignored);
        }

        response["_debug"] = debug_obj;
    }

    response
}

pub fn hybrid_query(
    query: &str,
    limit: usize,
    repo_root: Option<&str>,
    compact: bool,
    fusion: Option<&str>,
    route: Option<&str>,
    debug: Option<bool>,
    result_mode: Option<&str>,
) -> Result<Value> {
    if query.trim().is_empty() {
        return Ok(json!({
            "status": "ok",
            "query": query,
            "method": "keyword_only",
            "results": [],
        }));
    }

    let (mut store, root) = get_store(repo_root)?;
    maybe_auto_update(&mut store, &root);

    let emb_db_path = incremental::get_embeddings_db_path(&root);
    let mut emb_store = EmbeddingStore::new(&emb_db_path)?;
    let result = hybrid_query_with_store(&store, &mut emb_store, &root, query, limit, compact, fusion, None, route, debug, result_mode, None)?;
    emb_store.close()?;
    store.close()?;
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
pub fn hybrid_query_with_store(
    store: &GraphStore,
    emb_store: &mut EmbeddingStore,
    root: &Utf8Path,
    query: &str,
    limit: usize,
    compact: bool,
    fusion: Option<&str>,
    keyword_hits: Option<Vec<crate::types::GraphNode>>,
    route: Option<&str>,
    debug: Option<bool>,
    result_mode: Option<&str>,
    ablation: Option<&AblationConfig>,
) -> Result<Value> {
    if query.trim().is_empty() {
        return Ok(json!({
            "status": "ok",
            "query": query,
            "method": "keyword_only",
            "results": [],
        }));
    }

    // Validate result_mode early so we can return a clear error before doing any work.
    let result_mode_str = result_mode.unwrap_or("node");
    if result_mode_str != "node" && result_mode_str != "file" {
        return Ok(json!({
            "status": "error",
            "message": format!("Unknown result_mode '{}'; expected 'node' or 'file'", result_mode_str),
        }));
    }

    // --- File mode: fanout+rerank early return ---
    if result_mode_str == "file" {
        let abl = ablation.cloned().unwrap_or_else(AblationConfig::full);
        let parts = if abl.decomposition { decompose_query(query) } else { decompose_query_passthrough(query) };
        let mut candidates = fanout_retrieve(store, emb_store, root, &parts, limit, keyword_hits, &abl)?;
        if abl.expansion {
            expand_candidates(store, &mut candidates, limit)?;
        }
        let total_candidates = candidates.len();
        let files_scored = {
            let mut files: HashSet<&str> = HashSet::new();
            for c in candidates.values() {
                files.insert(&c.file_path);
            }
            files.len()
        };
        let file_results = aggregate_to_files(candidates, &parts, limit, &abl);
        return Ok(file_results_to_json(
            file_results, query, &parts, debug, route, fusion, total_candidates, files_scored,
        ));
    }
    // --- End file mode ---

    let route_param = route.unwrap_or("auto");

    // Validate route parameter
    if !["auto", "legacy", "exact", "semantic", "path"].contains(&route_param) {
        return Ok(json!({
            "status": "error",
            "message": format!("Unknown route '{}'; expected 'auto', 'legacy', 'exact', 'semantic', or 'path'", route_param),
        }));
    }

    let classification = if route_param == "auto" {
        classify_query(query)
    } else {
        // Explicit route override
        Classification {
            route: match route_param {
                "exact" => QueryRoute::ExactSymbol,
                "semantic" => QueryRoute::Causal,
                "path" => QueryRoute::FilePath,
                _ => QueryRoute::General, // "legacy"
            },
            confidence: 1.0,
        }
    };

    // Low confidence → fall back to legacy behavior
    let effective_route = if classification.confidence < 0.6 {
        QueryRoute::General
    } else {
        classification.route
    };

    // Keyword results (used by most routes).
    // Use relaxed OR-matching for natural-language queries when Tantivy is unavailable.
    let keyword_hits = match keyword_hits {
        Some(hits) => hits,
        None => store.search_nodes_relaxed(query, limit * 2)?,
    };
    let embeddings_available = emb_store.available() && emb_store.count().unwrap_or(0) > 0;

    let fusion_method = fusion.unwrap_or("rrf");
    if fusion_method != "rrf" && fusion_method != "cc" {
        return Ok(json!({
            "status": "error",
            "message": format!("Unknown fusion method '{}'; expected 'rrf' or 'cc'", fusion_method),
        }));
    }

    // Try specialized route first; fall through to General if it returns empty.
    let (specialized_method, specialized_ranked): (Option<&str>, Vec<(String, f64)>) = match effective_route {
        QueryRoute::ExactSymbol => {
            // Keyword-only: no semantic search needed for exact identifiers
            let mut scores: Vec<(String, f64)> = keyword_hits
                .iter()
                .enumerate()
                .map(|(rank, node)| {
                    let score = 1.0 / (RRF_K + rank as f64 + 1.0);
                    (node.qualified_name.clone(), score)
                })
                .collect();
            scores.truncate(limit);
            (Some("keyword_only"), scores)
        }

        QueryRoute::FilePath => {
            // Keyword search, boost file-kind nodes
            let mut scores: Vec<(String, f64)> = keyword_hits
                .iter()
                .enumerate()
                .map(|(rank, node)| {
                    let base = 1.0 / (RRF_K + rank as f64 + 1.0);
                    let boost = if node.qualified_name.contains('/') || node.qualified_name.contains('.') {
                        1.5
                    } else {
                        1.0
                    };
                    (node.qualified_name.clone(), base * boost)
                })
                .collect();
            scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            scores.truncate(limit);
            (Some("keyword_path_boosted"), scores)
        }

        QueryRoute::ConfigLookup => {
            // Keyword search, prefer Type-kind nodes
            let mut scores: Vec<(String, f64)> = keyword_hits
                .iter()
                .enumerate()
                .map(|(rank, node)| {
                    let base = 1.0 / (RRF_K + rank as f64 + 1.0);
                    let boost = if matches!(node.kind, NodeKind::Type) { 1.5 } else { 1.0 };
                    (node.qualified_name.clone(), base * boost)
                })
                .collect();
            scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            scores.truncate(limit);
            (Some("keyword_config_boosted"), scores)
        }

        QueryRoute::Causal | QueryRoute::General => (None, vec![]),
    };

    // Use specialized result if non-empty, otherwise fall through to General.
    let use_general = specialized_method.is_none() || specialized_ranked.is_empty();

    let (method, ranked): (&str, Vec<(String, f64)>) = if !use_general {
        (specialized_method.unwrap(), specialized_ranked)
    } else {
        // General / fallback: full hybrid keyword + semantic + fusion
        if fusion_method == "cc" && embeddings_available {
            const ALPHA: f64 = 0.5;

            let mut keyword_scores: HashMap<String, f64> = HashMap::new();
            for (rank, node) in keyword_hits.iter().enumerate() {
                keyword_scores.insert(node.qualified_name.clone(), 1.0 / (1.0 + rank as f64));
            }

            let mut semantic_scores: HashMap<String, f64> = HashMap::new();
            let semantic_hits = crate::embeddings::semantic_search(query, store, emb_store, limit * 2, compact, root)?;
            for (rank, hit) in semantic_hits.iter().enumerate() {
                if let Some(qn) = hit.get("qualified_name").and_then(|v| v.as_str()) {
                    semantic_scores.insert(qn.to_string(), 1.0 / (1.0 + rank as f64));
                }
            }

            // Min-max normalise a score map to [0, 1]. All-equal scores map to 1.
            let normalize = |scores: &HashMap<String, f64>| -> HashMap<String, f64> {
                if scores.is_empty() {
                    return HashMap::new();
                }
                let min = scores.values().cloned().fold(f64::INFINITY, f64::min);
                let max = scores.values().cloned().fold(f64::NEG_INFINITY, f64::max);
                let range = max - min;
                if range < f64::EPSILON {
                    scores.keys().map(|k| (k.clone(), 1.0)).collect()
                } else {
                    scores.iter().map(|(k, v)| (k.clone(), (v - min) / range)).collect()
                }
            };

            let kw_norm = normalize(&keyword_scores);
            let sem_norm = normalize(&semantic_scores);

            let all_qns: HashSet<&String> = kw_norm.keys().chain(sem_norm.keys()).collect();
            let mut combined: Vec<(String, f64)> = all_qns
                .into_iter()
                .map(|qn| {
                    let kw = kw_norm.get(qn).copied().unwrap_or(0.0);
                    let sem = sem_norm.get(qn).copied().unwrap_or(0.0);
                    (qn.clone(), ALPHA * kw + (1.0 - ALPHA) * sem)
                })
                .collect();

            combined.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            combined.truncate(limit);
            ("hybrid_cc", combined)
        } else {
            let mut rrf_scores: HashMap<String, f64> = HashMap::new();

            for (rank, node) in keyword_hits.iter().enumerate() {
                let score = 1.0 / (RRF_K + rank as f64 + 1.0);
                *rrf_scores.entry(node.qualified_name.clone()).or_insert(0.0) += score;
            }

            let method = if embeddings_available {
                let semantic_hits = crate::embeddings::semantic_search(query, store, emb_store, limit * 2, compact, root)?;
                for (rank, hit) in semantic_hits.iter().enumerate() {
                    if let Some(qn) = hit.get("qualified_name").and_then(|v| v.as_str()) {
                        let score = 1.0 / (RRF_K + rank as f64 + 1.0);
                        *rrf_scores.entry(qn.to_string()).or_insert(0.0) += score;
                    }
                }
                "hybrid_rrf"
            } else {
                "keyword_only"
            };

            let mut ranked: Vec<(String, f64)> = rrf_scores.into_iter().collect();
            ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            ranked.truncate(limit);
            (method, ranked)
        }
    };

    // Derive score field from the algorithm actually executed, not the requested fusion —
    // CC falls back to RRF when embeddings are absent, so method is the source of truth.
    let score_field = if method == "hybrid_cc" { "cc_score" } else { "rrf_score" };

    let results: Vec<Value> = ranked
        .iter()
        .filter_map(|(qn, score)| {
            store.get_node(qn).ok().flatten().map(|node| {
                let mut d = node_dict(&node, compact, root);
                d[score_field] = json!(score);
                d
            })
        })
        .collect();

    let mut response = json!({
        "status": "ok",
        "query": query,
        "method": method,
        "results": results,
    });

    if debug.unwrap_or(false) {
        response["_debug"] = json!({
            "route": format!("{:?}", effective_route),
            "confidence": classification.confidence,
            "requested_route": route_param,
        });
    }

    Ok(response)
}

// ---------------------------------------------------------------------------
// Tool 12: open_node_context
// ---------------------------------------------------------------------------

/// Get complete context for a node — source preview, callers, callees, tests,
/// and file siblings — in a single store-borrowed call.
pub fn open_node_context(
    target: &str,
    compact: bool,
    repo_root: Option<&str>,
) -> Result<Value> {
    let (mut store, root) = get_store(repo_root)?;
    maybe_auto_update(&mut store, &root);
    let result = open_node_context_with_store(&store, &root, target, compact)?;
    store.close()?;
    Ok(result)
}

pub fn open_node_context_with_store(
    store: &GraphStore,
    root: &Utf8Path,
    target: &str,
    compact: bool,
) -> Result<Value> {
    let node = match resolve_target_node(store, target, root)? {
        ResolveResult::Found(n) => n,
        ResolveResult::Ambiguous(candidates) => {
            return Ok(json!({
                "status": "ambiguous",
                "summary": format!("Multiple matches for '{}'. Please use a qualified name.", target),
                "candidates": candidates,
            }));
        }
        ResolveResult::NotFound => {
            return Ok(json!({
                "status": "not_found",
                "summary": format!("No node found matching '{}'.", target),
            }));
        }
    };

    let qn = &node.qualified_name;

    // Single pass over incoming edges splits callers (Calls) from tests (TestedBy),
    // avoiding a second get_edges_by_target call.
    let mut caller_edges: Vec<_> = Vec::new();
    let mut test_edges: Vec<_> = Vec::new();
    for e in store.get_edges_by_target(qn)? {
        match e.kind {
            EdgeKind::Calls => caller_edges.push(e),
            EdgeKind::TestedBy => test_edges.push(e),
            _ => {}
        }
    }
    let callers_count = caller_edges.len();
    let callers: Vec<Value> = caller_edges.iter()
        .take(5)
        .filter_map(|e| store.get_node(&e.source_qualified).ok().flatten())
        .map(|n| node_to_dict(&n, compact))
        .collect();

    let all_callees_edges: Vec<_> = store.get_edges_by_source(qn)?
        .into_iter()
        .filter(|e| e.kind == EdgeKind::Calls)
        .collect();
    let callees_count = all_callees_edges.len();
    let callees: Vec<Value> = all_callees_edges.iter()
        .take(5)
        .filter_map(|e| store.get_node(&e.target_qualified).ok().flatten())
        .map(|n| node_to_dict(&n, compact))
        .collect();

    let tests_count = test_edges.len();
    let tests: Vec<Value> = test_edges.iter()
        .filter_map(|e| store.get_node(&e.source_qualified).ok().flatten())
        .map(|n| node_to_dict(&n, compact))
        .collect();

    let node_kind = node.kind;
    let node_line = node.line_start;
    let node_qn = node.qualified_name.clone();
    let mut siblings: Vec<_> = store.get_nodes_by_file(&node.file_path)?
        .into_iter()
        .filter(|n| n.qualified_name != node_qn)
        .collect();
    siblings.sort_by_key(|n| {
        let kind_score: u8 = if n.kind == node_kind { 0 } else { 1 };
        let line_dist = (n.line_start as i64 - node_line as i64).unsigned_abs() as usize;
        (kind_score, line_dist)
    });
    siblings.truncate(10);
    let file_siblings: Vec<Value> = siblings.iter().map(|n| node_to_dict(n, compact)).collect();

    let (source_preview, source_truncated) = {
        let file_path_str = &node.file_path;
        match std::fs::read_to_string(file_path_str) {
            Ok(content) => {
                let lines: Vec<&str> = content.lines().collect();
                let start = node.line_start.saturating_sub(1);
                let end = node.line_end.min(lines.len());
                let available = end.saturating_sub(start);
                let take = available.min(30);
                let truncated = available > 30;
                let preview: String = lines[start..start + take]
                    .iter()
                    .enumerate()
                    .map(|(i, line)| format!("{:>4}: {}", start + i + 1, line))
                    .collect::<Vec<_>>()
                    .join("\n");
                (preview, truncated)
            }
            Err(_) => (String::new(), false),
        }
    };

    let node_name = node.name.clone();
    let node_dict_val = node_to_dict(&node, compact);

    Ok(json!({
        "status": "ok",
        "node": node_dict_val,
        "source_preview": source_preview,
        "source_truncated": source_truncated,
        "callers": callers,
        "callers_count": callers_count,
        "callees": callees,
        "callees_count": callees_count,
        "tests": tests,
        "tests_count": tests_count,
        "file_siblings": file_siblings,
        "summary": format!(
            "Context for {}: {} callers, {} callees, {} tests",
            node_name, callers_count, callees_count, tests_count
        ),
    }))
}

// ---------------------------------------------------------------------------
// Tool 13: batch_open_node_context
// ---------------------------------------------------------------------------

/// Inspect multiple nodes at once — opens the store once and calls
/// `open_node_context_with_store` for each target (max 5).
pub fn batch_open_node_context(
    targets: Vec<String>,
    compact: bool,
    repo_root: Option<&str>,
) -> Result<Value> {
    let (mut store, root) = get_store(repo_root)?;
    maybe_auto_update(&mut store, &root);

    let capped: Vec<String> = targets.into_iter().take(5).collect();
    let mut results: Vec<Value> = Vec::with_capacity(capped.len());

    for target in &capped {
        match open_node_context_with_store(&store, &root, target, compact) {
            Ok(v) => {
                if v["status"] == "not_found" || v["status"] == "ambiguous" {
                    results.push(json!({
                        "target": target,
                        "status": v["status"],
                        "summary": v["summary"],
                    }));
                } else {
                    results.push(v);
                }
            }
            Err(_) => {
                results.push(json!({
                    "target": target,
                    "status": "not_found",
                }));
            }
        }
    }

    let count = results.len();
    store.close()?;
    Ok(json!({
        "status": "ok",
        "results": results,
        "count": count,
    }))
}

pub fn batch_open_node_context_with_store(
    store: &GraphStore,
    root: &Utf8Path,
    targets: Vec<String>,
    compact: bool,
) -> Result<Value> {
    let capped: Vec<String> = targets.into_iter().take(5).collect();
    let mut results: Vec<Value> = Vec::with_capacity(capped.len());
    for target in &capped {
        match open_node_context_with_store(store, root, target, compact) {
            Ok(v) => {
                if v["status"] == "not_found" || v["status"] == "ambiguous" {
                    results.push(json!({
                        "target": target,
                        "status": v["status"],
                        "summary": v["summary"],
                    }));
                } else {
                    results.push(v);
                }
            }
            Err(_) => results.push(json!({ "target": target, "status": "not_found" })),
        }
    }
    let count = results.len();
    Ok(json!({ "status": "ok", "results": results, "count": count }))
}

// ---------------------------------------------------------------------------
// Tool 11: measure_token_reduction
// ---------------------------------------------------------------------------

/// Compute how much smaller the graph-filtered review context is compared
/// to naively concatenating all source files in the repo.
///
/// Returns `reduction_percent` = 100 * (1 - context_bytes / naive_bytes).
/// A value of 80 means the context is 80 % smaller than a full-repo dump.
pub fn measure_token_reduction(
    changed_files: Option<Vec<String>>,
    repo_root: Option<&str>,
    base: &str,
) -> Result<Value> {
    let (mut store, root) = get_store(repo_root)?;
    maybe_auto_update(&mut store, &root);

    // Naive bytes: sum of all source files in the repo
    let naive_bytes: u64 = walkdir::WalkDir::new(&root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_type().is_file()
                && !e.path().components().any(|c| {
                    matches!(c.as_os_str().to_str(), Some(".git") | Some("target") | Some("node_modules"))
                })
        })
        .filter(|e| {
            matches!(
                e.path().extension().and_then(|s| s.to_str()),
                Some("py" | "ts" | "tsx" | "js" | "jsx" | "rs" | "go" | "java" | "kt" | "swift" | "rb" | "cs" | "php" | "cpp" | "c" | "h" | "vue")
            )
        })
        .filter_map(|e| std::fs::metadata(e.path()).ok())
        .map(|m| m.len())
        .sum();

    // Context bytes: size of graph-filtered impact context
    let files = resolve_changed_files(changed_files, &root, base);
    let abs_files: Vec<String> = files.iter()
        .map(|f| root.join(f).as_str().to_owned())
        .collect();

    let context_bytes: u64 = if abs_files.is_empty() {
        // No changed files — context is just the changed file names (minimal)
        0
    } else {
        let impact = store.get_impact_radius(&abs_files, 3, 200, None)?;
        // Context = source text of only the changed files (trimmed to relevant nodes)
        abs_files.iter()
            .filter_map(|p| std::fs::read(p).ok())
            .map(|b| b.len() as u64)
            .sum::<u64>()
            + impact.impacted_nodes.iter()
                .filter_map(|n| std::fs::metadata(&n.file_path).ok())
                .map(|m| m.len() / 4) // impacted files contribute 1/4 (signature-level context)
                .sum::<u64>()
    };

    let reduction_percent = if naive_bytes == 0 {
        0.0_f64
    } else {
        100.0 * (1.0 - (context_bytes as f64 / naive_bytes as f64)).max(0.0)
    };

    store.close()?;
    Ok(json!({
        "status": "ok",
        "naive_bytes": naive_bytes,
        "context_bytes": context_bytes,
        "reduction_percent": reduction_percent,
        "changed_files": files,
    }))
}

pub fn find_large_functions(
    min_lines: usize,
    kind: Option<&str>,
    file_path_pattern: Option<&str>,
    limit: usize,
    repo_root: Option<&str>,
    compact: bool,
) -> Result<Value> {
    let (mut store, root) = get_store(repo_root)?;
    maybe_auto_update(&mut store, &root);
    let result = find_large_functions_with_store(&store, &root, min_lines, kind, file_path_pattern, limit, compact)?;
    store.close()?;
    Ok(result)
}

pub fn find_large_functions_with_store(
    store: &GraphStore,
    root: &Utf8Path,
    min_lines: usize,
    kind: Option<&str>,
    file_path_pattern: Option<&str>,
    limit: usize,
    compact: bool,
) -> Result<Value> {
    let nodes = store.get_nodes_by_size(min_lines, kind, file_path_pattern, limit)?;

    let results: Vec<Value> = nodes.iter().map(|n| {
        let line_count = if n.line_end >= n.line_start {
            n.line_end - n.line_start + 1
        } else {
            0
        };
        let relative_path = Utf8Path::new(&n.file_path)
            .strip_prefix(root)
            .map(|p| p.as_str().to_owned())
            .unwrap_or_else(|_| n.file_path.clone());
        let mut d = node_dict(n, compact, root);
        d["line_count"] = json!(line_count);
        d["relative_path"] = json!(relative_path);
        d
    }).collect();

    let kind_suffix = kind.map(|k| format!(" (kind={k})")).unwrap_or_default();
    let pat_suffix = file_path_pattern
        .map(|p| format!(" matching '{p}'"))
        .unwrap_or_default();

    let mut summary_parts = vec![
        format!(
            "Found {} node(s) with >= {} lines{}{}:",
            results.len(), min_lines, kind_suffix, pat_suffix
        ),
    ];
    for r in results.iter().take(10) {
        summary_parts.push(format!(
            "  {:>4} lines | {:>8} | {} ({}:{})",
            r["line_count"],
            r["kind"].as_str().unwrap_or(""),
            r["name"].as_str().unwrap_or(""),
            r["relative_path"].as_str().unwrap_or(""),
            r["line_start"],
        ));
    }
    if results.len() > 10 {
        summary_parts.push(format!("  ... and {} more", results.len() - 10));
    }

    Ok(json!({
        "status": "ok",
        "summary": summary_parts.join("\n"),
        "total_found": results.len(),
        "min_lines": min_lines,
        "results": results,
    }))
}

pub fn trace_call_chain(
    from: &str,
    to: &str,
    max_depth: usize,
    compact: bool,
    repo_root: Option<&str>,
) -> Result<Value> {
    let (mut store, root) = get_store(repo_root)?;
    maybe_auto_update(&mut store, &root);
    let result = trace_call_chain_with_store(&store, &root, from, to, max_depth, compact)?;
    store.close()?;
    Ok(result)
}

pub fn trace_call_chain_with_store(
    store: &GraphStore,
    root: &Utf8Path,
    from: &str,
    to: &str,
    max_depth: usize,
    compact: bool,
) -> Result<Value> {
    let from_node = match resolve_target_node(store, from, root)? {
        ResolveResult::Found(n) => n,
        ResolveResult::NotFound => {
            return Ok(json!({
                "status": "error",
                "summary": format!("Node not found: '{from}'"),
            }));
        }
        ResolveResult::Ambiguous(candidates) => {
            return Ok(json!({
                "status": "ambiguous",
                "summary": format!("Ambiguous name '{from}' — qualify with file path or full qualified name"),
                "candidates": candidates,
            }));
        }
    };

    let to_node = match resolve_target_node(store, to, root)? {
        ResolveResult::Found(n) => n,
        ResolveResult::NotFound => {
            return Ok(json!({
                "status": "error",
                "summary": format!("Node not found: '{to}'"),
            }));
        }
        ResolveResult::Ambiguous(candidates) => {
            return Ok(json!({
                "status": "ambiguous",
                "summary": format!("Ambiguous name '{to}' — qualify with file path or full qualified name"),
                "candidates": candidates,
            }));
        }
    };

    let result = store.trace_call_chain(
        &from_node.qualified_name,
        &to_node.qualified_name,
        max_depth,
    )?;

    match result {
        Some(path) => {
            let hops = path.len().saturating_sub(1);
            let path_json: Vec<Value> = path.iter().map(|(node, edge)| {
                json!({
                    "node": node_dict(node, compact, root),
                    "edge_to_next": edge.is_some().then_some("CALLS"),
                })
            }).collect();
            Ok(json!({
                "status": "ok",
                "summary": format!(
                    "Found {hops}-hop call chain from '{}' to '{}'",
                    from_node.qualified_name,
                    to_node.qualified_name,
                ),
                "hops": hops,
                "path": path_json,
            }))
        }
        None => Ok(json!({
            "status": "no_path",
            "summary": format!(
                "No call chain found from '{}' to '{}' within {max_depth} hops",
                from_node.qualified_name,
                to_node.qualified_name,
            ),
        })),
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Resolve changed files from the provided list or from git diff.
pub fn resolve_changed_files(
    changed_files: Option<Vec<String>>,
    root: &Utf8Path,
    base: &str,
) -> Vec<String> {
    if let Some(files) = changed_files {
        return files;
    }
    let files = incremental::get_changed_files(root, base);
    if !files.is_empty() {
        return files;
    }
    incremental::get_staged_and_unstaged(root)
}

enum ResolveResult {
    Found(crate::types::GraphNode),
    Ambiguous(Vec<Value>),
    NotFound,
}

/// Try to resolve a target name to a single graph node.
fn resolve_target_node(
    store: &GraphStore,
    target: &str,
    root: &Utf8Path,
) -> Result<ResolveResult> {
    if let Some(node) = store.get_node(target)? {
        return Ok(ResolveResult::Found(node));
    }
    let abs_target = root.join(target).as_str().to_owned();
    if let Some(node) = store.get_node(&abs_target)? {
        return Ok(ResolveResult::Found(node));
    }
    let candidates = store.search_nodes(target, 5)?;
    match candidates.len() {
        0 => Ok(ResolveResult::NotFound),
        1 => Ok(ResolveResult::Found(candidates.into_iter().next().unwrap())),
        _ => Ok(ResolveResult::Ambiguous(
            candidates.iter().map(|n| node_to_dict(n, false)).collect(),
        )),
    }
}

/// Extract only the lines relevant to changed nodes.
fn extract_relevant_lines(
    lines: &[&str],
    nodes: &[crate::types::GraphNode],
    file_path: &str,
) -> String {
    let mut ranges: Vec<(usize, usize)> = nodes.iter()
        .filter(|n| n.file_path == file_path)
        .map(|n| {
            let start = n.line_start.saturating_sub(3);
            let end = (n.line_end + 2).min(lines.len());
            (start, end)
        })
        .collect();

    if ranges.is_empty() {
        return lines.iter().take(50).enumerate()
            .map(|(i, line)| format!("{}: {}", i + 1, line))
            .collect::<Vec<_>>()
            .join("\n");
    }

    ranges.sort_unstable();
    let mut merged: Vec<(usize, usize)> = vec![ranges[0]];
    for (start, end) in ranges.into_iter().skip(1) {
        let last = merged.last_mut().unwrap();
        if start <= last.1 + 1 {
            last.1 = last.1.max(end);
        } else {
            merged.push((start, end));
        }
    }

    let mut parts: Vec<String> = vec![];
    for (start, end) in merged {
        if !parts.is_empty() {
            parts.push("...".to_string());
        }
        for (i, line) in lines.iter().enumerate().take(end.min(lines.len())).skip(start) {
            parts.push(format!("{}: {}", i + 1, line));
        }
    }
    parts.join("\n")
}

/// Generate review guidance from impact analysis.
fn generate_review_guidance(impact: &crate::types::ImpactResult) -> String {
    let mut parts: Vec<String> = vec![];

    // Untested changed functions
    let changed_funcs: Vec<_> = impact.changed_nodes.iter()
        .filter(|n| n.kind == NodeKind::Function)
        .collect();
    let tested_sources: std::collections::HashSet<&str> = impact.edges.iter()
        .filter(|e| e.kind == EdgeKind::TestedBy)
        .map(|e| e.source_qualified.as_str())
        .collect();
    let untested: Vec<_> = changed_funcs.iter()
        .filter(|n| !tested_sources.contains(n.qualified_name.as_str()) && !n.is_test)
        .collect();
    if !untested.is_empty() {
        let names: Vec<&str> = untested.iter().take(5).map(|n| n.name.as_str()).collect();
        parts.push(format!(
            "- {} changed function(s) lack test coverage: {}",
            untested.len(),
            names.join(", ")
        ));
    }

    // Wide blast radius
    if impact.impacted_nodes.len() > 20 {
        parts.push(format!(
            "- Wide blast radius: {} nodes impacted. Review callers and dependents carefully.",
            impact.impacted_nodes.len()
        ));
    }

    // Inheritance changes
    let inheritance_count = impact.edges.iter()
        .filter(|e| matches!(e.kind, EdgeKind::Inherits | EdgeKind::Implements))
        .count();
    if inheritance_count > 0 {
        parts.push(format!(
            "- {inheritance_count} inheritance/implementation relationship(s) affected. \
             Check for Liskov substitution violations."
        ));
    }

    // Cross-file impact
    if impact.impacted_files.len() > 3 {
        parts.push(format!(
            "- Changes impact {} other files. Consider splitting into smaller PRs.",
            impact.impacted_files.len()
        ));
    }

    if parts.is_empty() {
        parts.push("- Changes appear well-contained with minimal blast radius.".to_string());
    }

    parts.join("\n")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Create a temp dir with a .git dir (so validate_repo_root accepts it)
    /// and return (dir, root_path_string).
    fn make_git_repo() -> (TempDir, String) {
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        let path = dir.path().to_str().expect("temp dir path is valid UTF-8").to_owned();
        (dir, path)
    }

    // -----------------------------------------------------------------------
    // validate_repo_root
    // -----------------------------------------------------------------------

    fn tp(path: &std::path::Path) -> &Utf8Path {
        Utf8Path::from_path(path).expect("test path is valid UTF-8")
    }

    #[test]
    fn validate_repo_root_rejects_nonexistent() {
        let result = validate_repo_root(Utf8Path::new("/nonexistent/path/that/does/not/exist"));
        assert!(result.is_err());
    }

    #[test]
    fn validate_repo_root_rejects_dir_without_git() {
        let dir = TempDir::new().unwrap();
        // No .git, no .code-review-graph
        let result = validate_repo_root(tp(dir.path()));
        assert!(result.is_err(), "dir without .git should be rejected");
    }

    #[test]
    fn validate_repo_root_accepts_git_repo() {
        let (dir, _) = make_git_repo();
        let result = validate_repo_root(tp(dir.path()));
        assert!(result.is_ok(), "dir with .git should be accepted");
    }

    #[test]
    fn validate_repo_root_accepts_code_review_graph_dir() {
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join(".code-review-graph")).unwrap();
        let result = validate_repo_root(tp(dir.path()));
        assert!(result.is_ok(), "dir with .code-review-graph should be accepted");
    }

    // -----------------------------------------------------------------------
    // is_builtin_call
    // -----------------------------------------------------------------------

    #[test]
    fn is_builtin_call_identifies_builtins() {
        assert!(is_builtin_call("map"));
        assert!(is_builtin_call("filter"));
        assert!(is_builtin_call("forEach"));
        assert!(is_builtin_call("reduce"));
        assert!(is_builtin_call("push"));
        assert!(is_builtin_call("then"));
        assert!(is_builtin_call("catch"));
        assert!(is_builtin_call("log"));
    }

    #[test]
    fn is_builtin_call_rejects_non_builtins() {
        assert!(!is_builtin_call("myCustomFunction"));
        assert!(!is_builtin_call("processPayment"));
        assert!(!is_builtin_call("authenticate"));
        assert!(!is_builtin_call("buildGraph"));
    }

    // -----------------------------------------------------------------------
    // build_or_update_graph
    // -----------------------------------------------------------------------

    #[test]
    fn build_or_update_graph_returns_ok_status() {
        let (dir, path) = make_git_repo();
        // Write a Python file to parse
        fs::write(dir.path().join("hello.py"), b"def hello(): pass\n").unwrap();

        let result = build_or_update_graph(true, Some(&path), "HEAD");
        assert!(result.is_ok(), "build_or_update_graph should succeed: {:?}", result);
        let val = result.unwrap();
        assert_eq!(val["status"], "ok");
        assert_eq!(val["build_type"], "full");
    }

    // -----------------------------------------------------------------------
    // list_graph_stats
    // -----------------------------------------------------------------------

    #[test]
    fn list_graph_stats_returns_correct_structure() {
        let (dir, path) = make_git_repo();
        fs::write(dir.path().join("mod.py"), b"def compute(): pass\n").unwrap();
        // First build so stats are non-trivial
        build_or_update_graph(true, Some(&path), "HEAD").unwrap();

        let result = list_graph_stats(Some(&path));
        assert!(result.is_ok(), "list_graph_stats should succeed");
        let val = result.unwrap();
        assert_eq!(val["status"], "ok");
        assert!(val["total_nodes"].is_number());
        assert!(val["total_edges"].is_number());
        assert!(val["nodes_by_kind"].is_object());
        assert!(val["edges_by_kind"].is_object());
    }

    // -----------------------------------------------------------------------
    // query_graph — unknown pattern returns error
    // -----------------------------------------------------------------------

    #[test]
    fn query_graph_unknown_pattern_returns_error() {
        let (_dir, path) = make_git_repo();
        build_or_update_graph(true, Some(&path), "HEAD").unwrap();

        let result = query_graph("totally_unknown_pattern", "some_target", Some(&path), false);
        assert!(result.is_ok());
        let val = result.unwrap();
        assert_eq!(val["status"], "error");
        assert!(val["error"].as_str().unwrap().contains("totally_unknown_pattern"));
    }

    #[test]
    fn query_graph_known_pattern_not_found_returns_not_found() {
        let (_dir, path) = make_git_repo();
        build_or_update_graph(true, Some(&path), "HEAD").unwrap();

        let result = query_graph("callers_of", "definitely_not_a_real_function_xyz", Some(&path), false);
        assert!(result.is_ok());
        let val = result.unwrap();
        // Could be "not_found" or "ok" with empty results
        let status = val["status"].as_str().unwrap();
        assert!(
            status == "not_found" || status == "ok",
            "unexpected status: {status}"
        );
    }

    // -----------------------------------------------------------------------
    // get_docs_section — unknown section returns not_found
    // -----------------------------------------------------------------------

    #[test]
    fn get_docs_section_unknown_section_returns_not_found() {
        let (_dir, path) = make_git_repo();
        let result = get_docs_section("completely_unknown_section_xyz", Some(&path));
        assert!(result.is_ok());
        let val = result.unwrap();
        assert_eq!(val["status"], "not_found");
        assert!(val["error"].as_str().unwrap().contains("completely_unknown_section_xyz"));
    }

    // -----------------------------------------------------------------------
    // hybrid_query
    // -----------------------------------------------------------------------

    #[test]
    fn hybrid_query_empty_query_returns_empty_results() {
        let (dir, path) = make_git_repo();
        fs::write(dir.path().join("mod.py"), b"def compute(): pass\n").unwrap();
        build_or_update_graph(true, Some(&path), "HEAD").unwrap();

        let result = hybrid_query("", 10, Some(&path), false, None, None, None, None);
        assert!(result.is_ok(), "hybrid_query should succeed: {:?}", result);
        let val = result.unwrap();
        assert_eq!(val["status"], "ok");
        let results = val["results"].as_array().unwrap();
        assert!(results.is_empty(), "empty query should return empty results");
    }

    #[test]
    fn hybrid_query_returns_keyword_only_when_no_embeddings() {
        let (dir, path) = make_git_repo();
        fs::write(
            dir.path().join("utils.py"),
            b"def add(a, b): return a + b\ndef subtract(a, b): return a - b\n",
        )
        .unwrap();
        build_or_update_graph(true, Some(&path), "HEAD").unwrap();

        let result = hybrid_query("add", 5, Some(&path), false, None, None, None, None);
        assert!(result.is_ok(), "hybrid_query should succeed: {:?}", result);
        let val = result.unwrap();
        assert_eq!(val["status"], "ok");
        // No embeddings in a fresh temp repo — must fall back to keyword_only
        assert_eq!(
            val["method"], "keyword_only",
            "should use keyword_only when no embeddings available"
        );
    }

    #[test]
    fn hybrid_query_results_have_rrf_score_field() {
        let (dir, path) = make_git_repo();
        fs::write(
            dir.path().join("calc.py"),
            b"def square(x): return x * x\ndef cube(x): return x * x * x\n",
        )
        .unwrap();
        build_or_update_graph(true, Some(&path), "HEAD").unwrap();

        let result = hybrid_query("square", 5, Some(&path), false, None, None, None, None);
        assert!(result.is_ok());
        let val = result.unwrap();
        assert_eq!(val["status"], "ok");
        let results = val["results"].as_array().unwrap();
        if !results.is_empty() {
            assert!(
                results[0].get("rrf_score").is_some(),
                "each result should have an rrf_score field"
            );
            let score = results[0]["rrf_score"].as_f64().unwrap();
            assert!(score > 0.0, "rrf_score should be positive, got {score}");
        }
    }

    #[test]
    fn hybrid_query_rrf_fusion_uses_hybrid_rrf_or_keyword_only_method() {
        let (dir, path) = make_git_repo();
        fs::write(
            dir.path().join("ops.py"),
            b"def multiply(a, b): return a * b\ndef divide(a, b): return a / b\n",
        )
        .unwrap();
        build_or_update_graph(true, Some(&path), "HEAD").unwrap();

        // Explicit "rrf" fusion — no embeddings, so falls back to keyword_only
        let result = hybrid_query("multiply", 5, Some(&path), false, Some("rrf"), None, None, None);
        assert!(result.is_ok());
        let val = result.unwrap();
        assert_eq!(val["status"], "ok");
        let method = val["method"].as_str().unwrap();
        assert!(
            method == "hybrid_rrf" || method == "keyword_only",
            "rrf fusion should use hybrid_rrf or keyword_only, got {method}"
        );
    }

    #[test]
    fn hybrid_query_cc_fusion_falls_back_to_keyword_only_without_embeddings() {
        let (dir, path) = make_git_repo();
        fs::write(
            dir.path().join("math.py"),
            b"def abs_val(x): return x if x >= 0 else -x\n",
        )
        .unwrap();
        build_or_update_graph(true, Some(&path), "HEAD").unwrap();

        // CC mode without embeddings: embeddings_available = false, so falls into RRF branch
        // which returns "keyword_only" (since no embeddings are present)
        let result = hybrid_query("abs_val", 5, Some(&path), false, Some("cc"), None, None, None);
        assert!(result.is_ok());
        let val = result.unwrap();
        assert_eq!(val["status"], "ok");
        // Without embeddings the cc branch is skipped entirely, keyword_only is used
        assert_eq!(
            val["method"], "keyword_only",
            "cc fusion without embeddings should fall back to keyword_only"
        );
    }

    #[test]
    fn hybrid_query_cc_fusion_returns_valid_results_structure() {
        let (dir, path) = make_git_repo();
        fs::write(
            dir.path().join("sort.py"),
            b"def bubble_sort(lst): return lst\ndef merge_sort(lst): return lst\n",
        )
        .unwrap();
        build_or_update_graph(true, Some(&path), "HEAD").unwrap();

        let result = hybrid_query("sort", 5, Some(&path), false, Some("cc"), None, None, None);
        assert!(result.is_ok(), "cc fusion hybrid_query should succeed: {:?}", result);
        let val = result.unwrap();
        assert_eq!(val["status"], "ok");
        assert!(val.get("method").is_some(), "result must have method field");
        assert!(val.get("results").is_some(), "result must have results field");
        assert!(val["results"].is_array(), "results must be an array");
    }

    // -----------------------------------------------------------------------
    // measure_token_reduction
    // -----------------------------------------------------------------------

    #[test]
    fn measure_token_reduction_returns_ok_with_required_fields() {
        let (dir, path) = make_git_repo();
        fs::write(
            dir.path().join("utils.py"),
            b"def add(a, b): return a + b\ndef subtract(a, b): return a - b\n",
        )
        .unwrap();
        build_or_update_graph(true, Some(&path), "HEAD").unwrap();

        let result = measure_token_reduction(None, Some(&path), "HEAD");
        assert!(result.is_ok(), "measure_token_reduction should succeed: {:?}", result);
        let val = result.unwrap();
        assert_eq!(val["status"], "ok");
        assert!(val["naive_bytes"].is_number(), "naive_bytes should be a number");
        assert!(val["context_bytes"].is_number(), "context_bytes should be a number");
        assert!(val["reduction_percent"].is_number(), "reduction_percent should be a number");
    }

    #[test]
    fn measure_token_reduction_naive_bytes_positive_when_source_exists() {
        let (dir, path) = make_git_repo();
        fs::write(
            dir.path().join("service.py"),
            b"def process(data): return data\ndef validate(data): return bool(data)\n",
        )
        .unwrap();
        build_or_update_graph(true, Some(&path), "HEAD").unwrap();

        let result = measure_token_reduction(None, Some(&path), "HEAD");
        assert!(result.is_ok());
        let val = result.unwrap();
        let naive = val["naive_bytes"].as_u64().unwrap();
        assert!(naive > 0, "naive_bytes should be > 0 when source files exist, got {naive}");
    }

    #[test]
    fn measure_token_reduction_with_explicit_changed_files() {
        let (dir, path) = make_git_repo();
        fs::write(
            dir.path().join("utils.py"),
            b"def add(a, b): return a + b\ndef subtract(a, b): return a - b\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("main.py"),
            b"from utils import add\ndef run(): return add(1, 2)\n",
        )
        .unwrap();
        build_or_update_graph(true, Some(&path), "HEAD").unwrap();

        let result = measure_token_reduction(
            Some(vec!["utils.py".to_string()]),
            Some(&path),
            "HEAD",
        );
        assert!(result.is_ok(), "measure_token_reduction should succeed: {:?}", result);
        let val = result.unwrap();
        assert_eq!(val["status"], "ok");
        let changed = val["changed_files"].as_array().unwrap();
        assert_eq!(changed.len(), 1, "should report exactly 1 changed file");
        assert_eq!(changed[0], "utils.py");
    }

    // -----------------------------------------------------------------------
    // classify_query
    // -----------------------------------------------------------------------

    #[test]
    fn classify_camel_case_as_exact_symbol() {
        let c = classify_query("turbopackUseBuiltinSass");
        assert!(matches!(c.route, QueryRoute::ExactSymbol));
        assert!(c.confidence >= 0.8);
    }

    #[test]
    fn classify_pascal_case_as_exact_symbol() {
        let c = classify_query("GraphStore");
        assert!(matches!(c.route, QueryRoute::ExactSymbol));
        assert!(c.confidence >= 0.8);
    }

    #[test]
    fn classify_snake_case_as_exact_symbol() {
        let c = classify_query("get_store");
        assert!(matches!(c.route, QueryRoute::ExactSymbol));
        assert!(c.confidence >= 0.8);
    }

    #[test]
    fn classify_file_path_query() {
        let c = classify_query("config-shared.ts");
        assert!(matches!(c.route, QueryRoute::FilePath));
        assert!(c.confidence >= 0.8);
    }

    #[test]
    fn classify_path_with_slash() {
        let c = classify_query("src/tools.rs");
        assert!(matches!(c.route, QueryRoute::FilePath));
    }

    #[test]
    fn classify_causal_query() {
        let c = classify_query("why does not-found CSS get included globally");
        assert!(matches!(c.route, QueryRoute::Causal));
        assert!(c.confidence >= 0.7);
    }

    #[test]
    fn classify_how_does_query() {
        let c = classify_query("how does the cache invalidation work");
        assert!(matches!(c.route, QueryRoute::Causal));
    }

    #[test]
    fn classify_config_query() {
        let c = classify_query("sassOptions experimental config");
        assert!(matches!(c.route, QueryRoute::ConfigLookup));
        assert!(c.confidence >= 0.6);
    }

    #[test]
    fn classify_general_query() {
        let c = classify_query("CSS chunk splitting optimization");
        assert!(matches!(c.route, QueryRoute::General));
    }

    #[test]
    fn low_confidence_falls_back_to_general() {
        let c = classify_query("something");
        // Single lowercase word with no uppercase or underscore → General or low confidence
        assert!(c.confidence < 0.6 || matches!(c.route, QueryRoute::General));
    }

    // -----------------------------------------------------------------------
    // decompose_query
    // -----------------------------------------------------------------------

    #[test]
    fn decompose_extracts_symbols() {
        let parts = decompose_query("serverPatchReducer loses push");
        assert!(
            parts.symbols.contains(&"serverPatchReducer".to_string()),
            "camelCase token should be in symbols; got {:?}",
            parts.symbols
        );
        assert!(
            !parts.domain_terms.contains(&"serverpatchreducer".to_string()),
            "symbol should not also appear in domain_terms"
        );
    }

    #[test]
    fn decompose_extracts_paths() {
        let parts = decompose_query("bug in config-shared.ts");
        assert!(
            parts.path_fragments.contains(&"config-shared.ts".to_string()),
            "token ending in .ts should be in path_fragments; got {:?}",
            parts.path_fragments
        );
    }

    #[test]
    fn decompose_detects_test_mentions() {
        let parts = decompose_query("test_ssl_verify fails");
        assert!(
            parts.mentions_tests,
            "token containing 'test' should set mentions_tests"
        );
    }

    #[test]
    fn decompose_does_not_mention_tests_for_normal_query() {
        let parts = decompose_query("serverPatchReducer loses push replace history");
        assert!(
            !parts.mentions_tests,
            "query without test/spec/mock/fixture/assert should not set mentions_tests"
        );
    }

    #[test]
    fn decompose_filters_stop_words() {
        let parts = decompose_query("why does the router fail");
        assert!(
            !parts.domain_terms.contains(&"why".to_string()),
            "stop word 'why' should be filtered"
        );
        assert!(
            !parts.domain_terms.contains(&"does".to_string()),
            "stop word 'does' should be filtered"
        );
        assert!(
            !parts.domain_terms.contains(&"the".to_string()),
            "stop word 'the' should be filtered"
        );
    }

    // -----------------------------------------------------------------------
    // decompose_query -- error_strings and compound_terms
    // -----------------------------------------------------------------------

    #[test]
    fn decompose_extracts_double_quoted_error_string() {
        let parts = decompose_query(
            "getting \"SyntaxError: unexpected end of JSON\" on parse",
        );
        assert!(
            parts.error_strings.contains(&"SyntaxError: unexpected end of JSON".to_string()),
            "double-quoted span should be in error_strings; got {:?}",
            parts.error_strings
        );
        // Words from inside the quotes must not pollute domain_terms.
        assert!(
            !parts.domain_terms.contains(&"unexpected".to_string()),
            "quoted words should not spill into domain_terms; got {:?}",
            parts.domain_terms
        );
    }

    #[test]
    fn decompose_extracts_backtick_error_string() {
        let parts = decompose_query("calling `readFile` throws unexpectedly");
        assert!(
            parts.error_strings.contains(&"readFile".to_string()),
            "backtick span should be in error_strings; got {:?}",
            parts.error_strings
        );
    }

    #[test]
    fn decompose_detects_race_condition_compound_term() {
        let parts = decompose_query("there is a race condition in the scheduler");
        assert!(
            parts.compound_terms.contains(&"race condition".to_string()),
            "race condition should be in compound_terms; got {:?}",
            parts.compound_terms
        );
    }

    #[test]
    fn decompose_detects_memory_leak_compound_term() {
        let parts = decompose_query("suspect memory leak in connection pool");
        assert!(
            parts.compound_terms.contains(&"memory leak".to_string()),
            "memory leak should be in compound_terms; got {:?}",
            parts.compound_terms
        );
    }

    #[test]
    fn decompose_mixed_symbols_error_string_and_compound_term() {
        let parts = decompose_query(
            "parseResponse throws \"invalid JSON\" causing race condition in apiClient",
        );
        assert!(
            parts.symbols.contains(&"parseResponse".to_string()),
            "parseResponse should be a symbol"
        );
        assert!(
            parts.symbols.contains(&"apiClient".to_string()),
            "apiClient should be a symbol"
        );
        assert!(
            parts.error_strings.contains(&"invalid JSON".to_string()),
            "quoted span should be in error_strings; got {:?}",
            parts.error_strings
        );
        assert!(
            parts.compound_terms.contains(&"race condition".to_string()),
            "race condition should be in compound_terms; got {:?}",
            parts.compound_terms
        );
        assert!(
            !parts.domain_terms.contains(&"invalid".to_string()),
            "quoted words must not spill into domain_terms; got {:?}",
            parts.domain_terms
        );
    }

    #[test]
    fn decompose_passthrough_has_empty_new_fields() {
        let parts = decompose_query_passthrough(
            "parseResponse \"invalid JSON\" race condition",
        );
        assert!(parts.error_strings.is_empty(), "passthrough error_strings should be empty");
        assert!(parts.compound_terms.is_empty(), "passthrough compound_terms should be empty");
    }

    // -----------------------------------------------------------------------
    // fanout_retrieve helpers
    // -----------------------------------------------------------------------

    /// Build a minimal NodeCandidate for testing.
    fn make_candidate(qn: &str, file: &str, kind: NodeKind, score: f64, source: CandidateSource) -> NodeCandidate {
        let mut sources = HashSet::new();
        sources.insert(source);
        NodeCandidate {
            qualified_name: qn.to_string(),
            file_path: file.to_string(),
            kind,
            is_test: matches!(kind, NodeKind::Test),
            score,
            sources,
        }
    }

    #[test]
    fn fanout_merges_multi_source() {
        // Simulate two channels returning the same node — pool should merge scores and sources.
        let mut pool: HashMap<String, NodeCandidate> = HashMap::new();

        // Fake search results (rank 0 from each channel).
        let nodes_a = vec![crate::types::GraphNode {
            name: "foo".to_string(),
            qualified_name: "src/foo::foo".to_string(),
            kind: NodeKind::Function,
            file_path: "src/foo.ts".to_string(),
            line_start: 1,
            line_end: 10,
            language: "typescript".to_string(),
            is_test: false,
            docstring: String::new(),
            signature: String::new(),
            body_hash: String::new(),
            file_hash: String::new(),
        }];

        nodes_to_candidates(&nodes_a, CandidateSource::KeywordRelaxed, &mut pool);
        nodes_to_candidates(&nodes_a, CandidateSource::KeywordExact, &mut pool);

        let candidate = pool.get("src/foo::foo").expect("candidate should be in pool");
        assert!(
            candidate.score > 1.0 / (RRF_K + 1.0),
            "score should be summed from two channels; got {}",
            candidate.score
        );
        assert!(
            candidate.sources.contains(&CandidateSource::KeywordRelaxed),
            "KeywordRelaxed should be in sources"
        );
        assert!(
            candidate.sources.contains(&CandidateSource::KeywordExact),
            "KeywordExact should be in sources"
        );
        assert_eq!(candidate.sources.len(), 2, "should have exactly 2 unique sources");
    }

    #[test]
    fn fanout_includes_exact_symbol_channel() {
        let (dir, path) = make_git_repo();
        fs::write(
            dir.path().join("service.py"),
            b"def processRequest(req): return req\ndef handleRequest(req): return req\n",
        )
        .unwrap();
        build_or_update_graph(true, Some(&path), "HEAD").unwrap();

        let (store, root) = get_store(Some(&path)).unwrap();
        let emb_db_path = crate::incremental::get_embeddings_db_path(&root);
        let mut emb_store = EmbeddingStore::new(&emb_db_path).unwrap();

        // Query with a symbol — exact channel should be triggered.
        let parts = decompose_query("processRequest fails");
        assert!(!parts.symbols.is_empty(), "processRequest should be detected as symbol");

        let pool = fanout_retrieve(&store, &mut emb_store, &root, &parts, 10, None, &AblationConfig::all_enabled()).unwrap();

        // The candidate pool must be non-empty (keyword relaxed at minimum).
        assert!(!pool.is_empty(), "fanout pool should not be empty");

        emb_store.close().unwrap();
        store.close().unwrap();
    }

    // -----------------------------------------------------------------------
    // aggregate_to_files
    // -----------------------------------------------------------------------

    #[test]
    fn aggregate_capped_topk() {
        // File A: 1 strong node (score 0.1)
        // File B: 5 weak nodes (score 0.01 each)
        // File A should win.
        let mut candidates: HashMap<String, NodeCandidate> = HashMap::new();

        candidates.insert("a::strong".to_string(), make_candidate(
            "a::strong", "src/a.ts", NodeKind::Function, 0.1, CandidateSource::KeywordRelaxed,
        ));

        for i in 0..5_u8 {
            let qn = format!("b::weak{i}");
            candidates.insert(qn.clone(), make_candidate(
                &qn, "src/b.ts", NodeKind::Function, 0.01, CandidateSource::KeywordRelaxed,
            ));
        }

        let parts = decompose_query("something");
        let results = aggregate_to_files(candidates, &parts, 10, &AblationConfig::full());

        assert!(results.len() >= 2);
        assert_eq!(results[0].file_path, "src/a.ts", "strong file should rank first");
    }

    #[test]
    fn aggregate_demotes_test_files() {
        // File A (test-only): all nodes are Test kind.
        // File B (source): Function nodes.
        // Query doesn't mention tests.
        let mut candidates: HashMap<String, NodeCandidate> = HashMap::new();

        // Test file with score 0.08.
        candidates.insert("test::testFoo".to_string(), make_candidate(
            "test::testFoo", "src/__tests__/foo.test.ts", NodeKind::Test, 0.08, CandidateSource::KeywordRelaxed,
        ));

        // Source file with a slightly lower raw score 0.06 — but after test demotion file A should drop below.
        candidates.insert("src::fooImpl".to_string(), make_candidate(
            "src::fooImpl", "src/foo.ts", NodeKind::Function, 0.06, CandidateSource::KeywordRelaxed,
        ));

        let parts = decompose_query("foo implementation detail");
        let results = aggregate_to_files(candidates, &parts, 10, &AblationConfig::full());

        assert!(results.len() >= 2);
        let test_file_rank = results.iter().position(|r| r.file_path.contains("test")).unwrap();
        let src_file_rank = results.iter().position(|r| r.file_path == "src/foo.ts").unwrap();
        assert!(
            src_file_rank < test_file_rank,
            "source file should rank above test-only file when query doesn't mention tests; \
             src_rank={src_file_rank}, test_rank={test_file_rank}"
        );
    }

    #[test]
    fn aggregate_symbol_path_boost() {
        // File whose path contains the query symbol (case-insensitive substring match) should
        // get 1.3x boost.
        //
        // We use a snake_case symbol "cache_reducer" so the path "src/cache_reducer.ts" is a
        // clear substring match. The "other" file has a slightly higher raw score (0.052) but
        // after the 1.3x boost the symbol-matching file wins (0.05 * 1.3 = 0.065 > 0.052).
        let mut candidates: HashMap<String, NodeCandidate> = HashMap::new();

        // File whose path contains "cache_reducer" — symbol-path match.
        candidates.insert("reducers::cache_reducer".to_string(), make_candidate(
            "reducers::cache_reducer",
            "src/cache_reducer.ts",
            NodeKind::Function,
            0.05,
            CandidateSource::KeywordExact,
        ));

        // Another file, slightly higher raw score but no symbol-in-path match.
        candidates.insert("other::someFunc".to_string(), make_candidate(
            "other::someFunc",
            "src/other/util.ts",
            NodeKind::Function,
            0.052,
            CandidateSource::KeywordRelaxed,
        ));

        // Query contains "cache_reducer" as a symbol token.
        let parts = decompose_query("cache_reducer broken history");
        assert!(
            parts.symbols.contains(&"cache_reducer".to_string()),
            "cache_reducer should be detected as symbol token"
        );

        let results = aggregate_to_files(candidates, &parts, 10, &AblationConfig::full());

        assert!(results.len() >= 2);
        // After symbol-path boost the cache_reducer file should rank first.
        assert!(
            results[0].file_path.contains("cache_reducer"),
            "symbol-path-boosted file should rank first; got {:?}",
            results.iter().map(|r| &r.file_path).collect::<Vec<_>>()
        );
    }

    #[test]
    fn aggregate_exact_symbol_floor() {
        // File A: 1 node (score 0.05, source: KeywordExact)
        // File B: 3 nodes (scores 0.04, 0.03, 0.02, source: KeywordRelaxed)
        // File A should rank higher because the KeywordExact boost (1.5x) lifts it above the volume.
        //
        // File A score: 0.05 * 1.5 (top weight) * 1.5 (exact boost) = 0.1125
        // File B score: 0.04 * 1.5 + 0.3 * (0.03 + 0.02) = 0.06 + 0.015 = 0.075
        let mut candidates: HashMap<String, NodeCandidate> = HashMap::new();

        candidates.insert("a::coerce_from_fn_pointer".to_string(), make_candidate(
            "a::coerce_from_fn_pointer",
            "src/coercion.rs",
            NodeKind::Function,
            0.05,
            CandidateSource::KeywordExact,
        ));

        let weak_scores = [0.04_f64, 0.03, 0.02];
        for (i, &score) in weak_scores.iter().enumerate() {
            let qn = format!("b::weak{i}");
            candidates.insert(qn.clone(), make_candidate(
                &qn,
                "src/types.rs",
                NodeKind::Function,
                score,
                CandidateSource::KeywordRelaxed,
            ));
        }

        let parts = decompose_query("coerce_from_fn_pointer");
        let results = aggregate_to_files(candidates, &parts, 10, &AblationConfig::full());

        assert!(results.len() >= 2);
        assert_eq!(
            results[0].file_path, "src/coercion.rs",
            "KeywordExact file should rank first; got {:?}",
            results.iter().map(|r| &r.file_path).collect::<Vec<_>>()
        );
    }

    #[test]
    fn aggregate_max_dominance() {
        // File A: 1 node (score 0.08, source: KeywordRelaxed)
        // File B: 5 nodes (scores 0.03 each, source: KeywordRelaxed)
        // File A should rank higher; top node weighting prevents volume from winning.
        //
        // File A score: 0.08 * 1.5 = 0.12
        // File B score: 0.03 * 1.5 + 0.3 * (0.03 + 0.03) = 0.045 + 0.018 = 0.063
        let mut candidates: HashMap<String, NodeCandidate> = HashMap::new();

        candidates.insert("a::dominant".to_string(), make_candidate(
            "a::dominant",
            "src/dominant.rs",
            NodeKind::Function,
            0.08,
            CandidateSource::KeywordRelaxed,
        ));

        for i in 0..5_u8 {
            let qn = format!("b::weak{i}");
            candidates.insert(qn.clone(), make_candidate(
                &qn,
                "src/scattered.rs",
                NodeKind::Function,
                0.03,
                CandidateSource::KeywordRelaxed,
            ));
        }

        let parts = decompose_query("something");
        let results = aggregate_to_files(candidates, &parts, 10, &AblationConfig::full());

        assert!(results.len() >= 2);
        assert_eq!(
            results[0].file_path, "src/dominant.rs",
            "strong single-node file should rank first; got {:?}",
            results.iter().map(|r| &r.file_path).collect::<Vec<_>>()
        );
    }

    // -----------------------------------------------------------------------
    // file mode integration tests
    // -----------------------------------------------------------------------

    #[test]
    fn file_mode_returns_file_results() {
        let (dir, path) = make_git_repo();
        fs::write(
            dir.path().join("router.py"),
            b"def handleNavigation(path): return path\ndef pushHistory(entry): pass\n",
        )
        .unwrap();
        build_or_update_graph(true, Some(&path), "HEAD").unwrap();

        let result = hybrid_query(
            "handleNavigation push history",
            5,
            Some(&path),
            true,
            None,
            None,
            None,
            Some("file"),
        );
        assert!(result.is_ok(), "file mode should succeed: {:?}", result);
        let val = result.unwrap();
        assert_eq!(val["status"], "ok");
        assert_eq!(val["method"], "fanout_file");
        assert_eq!(val["result_mode"], "file");
        assert!(val["results"].is_array(), "results must be array");
        let arr = val["results"].as_array().unwrap();
        if !arr.is_empty() {
            let first = &arr[0];
            assert!(first.get("file_path").is_some(), "each result must have file_path");
            assert!(first.get("score").is_some(), "each result must have score");
            assert!(first.get("sources").is_some(), "each result must have sources");
            assert!(first.get("top_nodes").is_some(), "each result must have top_nodes");
        }
    }

    #[test]
    fn node_mode_unchanged() {
        let (dir, path) = make_git_repo();
        fs::write(
            dir.path().join("math.py"),
            b"def add(a, b): return a + b\ndef sub(a, b): return a - b\n",
        )
        .unwrap();
        build_or_update_graph(true, Some(&path), "HEAD").unwrap();

        // result_mode omitted → node mode, existing behaviour must be preserved.
        let result = hybrid_query("add", 5, Some(&path), true, None, None, None, None);
        assert!(result.is_ok(), "node mode should succeed: {:?}", result);
        let val = result.unwrap();
        assert_eq!(val["status"], "ok");
        // method field must be one of the existing node-mode methods.
        let method = val["method"].as_str().unwrap_or("");
        assert!(
            ["keyword_only", "hybrid_rrf", "hybrid_cc", "keyword_path_boosted", "keyword_config_boosted"]
                .contains(&method),
            "node mode method should be a known node-mode method; got {method}"
        );
        // Must NOT have result_mode: "file"
        assert_ne!(val.get("result_mode").and_then(|v| v.as_str()), Some("file"));
    }

    #[test]
    fn file_mode_debug_includes_query_parts() {
        let (dir, path) = make_git_repo();
        fs::write(
            dir.path().join("reducer.py"),
            b"def serverPatchReducer(state, action): return state\n",
        )
        .unwrap();
        build_or_update_graph(true, Some(&path), "HEAD").unwrap();

        let result = hybrid_query(
            "serverPatchReducer loses push",
            5,
            Some(&path),
            true,
            None,
            None,
            Some(true), // debug = true
            Some("file"),
        );
        assert!(result.is_ok(), "debug file mode should succeed: {:?}", result);
        let val = result.unwrap();
        assert_eq!(val["status"], "ok");
        let debug = val.get("_debug").expect("_debug must be present when debug=true");
        assert!(debug.get("query_parts").is_some(), "_debug must contain query_parts");
        assert!(debug.get("total_candidates").is_some(), "_debug must contain total_candidates");
        assert!(debug.get("files_scored").is_some(), "_debug must contain files_scored");
    }

    // -----------------------------------------------------------------------
    // NodeEvidence: matched_terms and is_test
    // -----------------------------------------------------------------------

    #[test]
    fn evidence_includes_matched_terms() {
        // Node whose name matches a query symbol should have that symbol in matched_terms.
        let mut candidates: HashMap<String, NodeCandidate> = HashMap::new();
        candidates.insert(
            "reducers::serverPatchReducer".to_string(),
            make_candidate(
                "reducers::serverPatchReducer",
                "src/reducers/serverPatchReducer.ts",
                NodeKind::Function,
                0.1,
                CandidateSource::KeywordExact,
            ),
        );

        // symbols=["serverPatchReducer"], domain_terms=["history"]
        let parts = decompose_query("serverPatchReducer history");
        assert!(
            parts.symbols.contains(&"serverPatchReducer".to_string()),
            "serverPatchReducer should be detected as a symbol"
        );

        let results = aggregate_to_files(candidates, &parts, 10, &AblationConfig::full());
        assert!(!results.is_empty(), "should have at least one file result");

        let top = &results[0];
        assert!(!top.top_nodes.is_empty(), "should have at least one evidence node");
        let evidence = &top.top_nodes[0];

        assert!(
            evidence.matched_terms.contains(&"serverPatchReducer".to_string()),
            "matched_terms should contain 'serverPatchReducer'; got {:?}",
            evidence.matched_terms
        );
    }

    #[test]
    fn evidence_includes_is_test() {
        // A NodeCandidate with is_test=true should produce NodeEvidence with is_test=true.
        let mut sources = HashSet::new();
        sources.insert(CandidateSource::KeywordRelaxed);
        let mut candidates: HashMap<String, NodeCandidate> = HashMap::new();
        candidates.insert(
            "tests::testFoo".to_string(),
            NodeCandidate {
                qualified_name: "tests::testFoo".to_string(),
                file_path: "src/__tests__/foo.test.ts".to_string(),
                kind: NodeKind::Test,
                is_test: true,
                score: 0.05,
                sources,
            },
        );

        let parts = decompose_query("foo");
        let results = aggregate_to_files(candidates, &parts, 10, &AblationConfig::full());
        assert!(!results.is_empty(), "should have at least one file result");

        let top = &results[0];
        assert!(!top.top_nodes.is_empty(), "should have at least one evidence node");
        let evidence = &top.top_nodes[0];

        assert!(
            evidence.is_test,
            "NodeEvidence.is_test should be true for a test node"
        );
    }

    // -----------------------------------------------------------------------
    // expand_candidates
    // -----------------------------------------------------------------------

    fn make_store_node(name: &str, qn: &str, file: &str) -> crate::types::NodeInfo {
        crate::types::NodeInfo {
            name: name.to_string(),
            qualified_name: qn.to_string(),
            kind: crate::types::NodeKind::Function,
            file_path: file.to_string(),
            line_start: 1,
            line_end: 10,
            language: "python".to_string(),
            is_test: false,
            docstring: String::new(),
            signature: String::new(),
            body_hash: format!("hash_{name}"),
        }
    }

    fn make_store_edge(src: &str, tgt: &str, kind: crate::types::EdgeKind) -> crate::types::EdgeInfo {
        crate::types::EdgeInfo {
            source_qualified: src.to_string(),
            target_qualified: tgt.to_string(),
            kind,
            file_path: src.split("::").next().unwrap_or("f.py").to_string(),
            line: 1,
        }
    }

    /// After fanout finds node A (file1.py), expansion should add node B (file2.py)
    /// as a candidate with source Expansion, because A CALLS B.
    #[test]
    fn expand_adds_callee_to_candidates() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.bin.zst");
        let mut store = GraphStore::new(tp(&path)).unwrap();

        let nodes_file1 = vec![make_store_node("func_a", "file1.py::func_a", "file1.py")];
        let nodes_file2 = vec![make_store_node("func_b", "file2.py::func_b", "file2.py")];
        let edge = make_store_edge("file1.py::func_a", "file2.py::func_b", crate::types::EdgeKind::Calls);

        // Store file2.py first so func_b is in the graph before the cross-file edge is inserted.
        store.store_file_nodes_edges("file2.py", &nodes_file2, &[], "h2").unwrap();
        store.store_file_nodes_edges("file1.py", &nodes_file1, &[edge], "h1").unwrap();

        // Seed candidates with only func_a (as if fanout found it).
        let mut sources = HashSet::new();
        sources.insert(CandidateSource::KeywordRelaxed);
        let mut candidates: HashMap<String, NodeCandidate> = HashMap::new();
        candidates.insert(
            "file1.py::func_a".to_string(),
            NodeCandidate {
                qualified_name: "file1.py::func_a".to_string(),
                file_path: "file1.py".to_string(),
                kind: crate::types::NodeKind::Function,
                is_test: false,
                score: 1.0,
                sources,
            },
        );

        expand_candidates(&store, &mut candidates, 10).unwrap();

        // func_b should now be in candidates via expansion.
        assert!(
            candidates.contains_key("file2.py::func_b"),
            "expansion should add func_b as a candidate"
        );
        let expanded = &candidates["file2.py::func_b"];
        assert!(
            expanded.sources.contains(&CandidateSource::Expansion),
            "expanded candidate must have Expansion source"
        );
        assert_eq!(expanded.file_path, "file2.py");
        // Score should be 0.3 * parent score (1.0)
        assert!(
            (expanded.score - 0.3).abs() < 1e-9,
            "expanded score should be 0.3 * parent score, got {}",
            expanded.score
        );
    }

    /// A node with 100 callees must not cause expansion to exceed limit * 2 new candidates.
    #[test]
    fn expand_caps_new_candidates() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.bin.zst");
        let mut store = GraphStore::new(tp(&path)).unwrap();

        // Create hub node in file1.py
        let hub = make_store_node("hub", "file1.py::hub", "file1.py");

        // Create 100 callee nodes in file2.py, each with a CALLS edge from hub.
        let callee_count = 100usize;
        let callees: Vec<crate::types::NodeInfo> = (0..callee_count)
            .map(|i| make_store_node(&format!("callee_{i}"), &format!("file2.py::callee_{i}"), "file2.py"))
            .collect();
        let edges: Vec<crate::types::EdgeInfo> = (0..callee_count)
            .map(|i| make_store_edge("file1.py::hub", &format!("file2.py::callee_{i}"), crate::types::EdgeKind::Calls))
            .collect();

        // Store callees first so cross-file edges from hub resolve correctly.
        store.store_file_nodes_edges("file2.py", &callees, &[], "h2").unwrap();
        store.store_file_nodes_edges("file1.py", &[hub], &edges, "h1").unwrap();

        // Seed with only hub.
        let mut sources = HashSet::new();
        sources.insert(CandidateSource::KeywordRelaxed);
        let mut candidates: HashMap<String, NodeCandidate> = HashMap::new();
        candidates.insert(
            "file1.py::hub".to_string(),
            NodeCandidate {
                qualified_name: "file1.py::hub".to_string(),
                file_path: "file1.py".to_string(),
                kind: crate::types::NodeKind::Function,
                is_test: false,
                score: 1.0,
                sources,
            },
        );

        let limit = 10usize;
        expand_candidates(&store, &mut candidates, limit).unwrap();

        // Total new candidates added must not exceed limit * 2.
        let new_candidates = candidates.len() - 1; // subtract the original hub
        assert!(
            new_candidates <= limit * 2,
            "expansion should add at most limit*2={} new candidates, got {new_candidates}",
            limit * 2
        );
    }
}
