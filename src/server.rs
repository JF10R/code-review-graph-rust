//! MCP server wiring via rmcp.
//!
//! Registers all tools and runs the server over stdio transport.
//!
//! Architecture: a single worker OS thread owns GraphStore + EmbeddingStore.
//! MCP tool handlers send typed WorkerCommand messages over a std::sync::mpsc
//! channel and await replies via tokio::sync::oneshot. The background file
//! watcher runs as a notify debouncer and also sends commands to the worker
//! (single-writer guarantee — no concurrent SQLite/disk writes).
//!
//! tree_sitter::Tree is !Send, so the worker must be a plain OS thread (not
//! a tokio task). Reply channels use tokio::sync::oneshot so async handlers
//! can await them without blocking the event loop.

use rmcp::{
    ServerHandler,
    handler::server::router::tool::ToolRouter,
    handler::server::wrapper::Parameters,
    model::{ServerCapabilities, ServerInfo},
    schemars, serve_server, tool, tool_handler, tool_router,
    transport,
};
use serde::Deserialize;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use camino::Utf8PathBuf;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Parameter structs — one per tool that takes arguments
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct BuildOrUpdateParams {
    #[schemars(description = "If true, re-parse all files instead of only changed ones")]
    #[serde(default)]
    full_rebuild: bool,
    #[schemars(description = "Repository root path. Auto-detected if omitted.")]
    #[serde(default)]
    repo_root: Option<String>,
    #[schemars(description = "Git ref to diff against for incremental updates")]
    #[serde(default = "default_base")]
    base: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ImpactRadiusParams {
    #[schemars(description = "List of changed file paths (relative to repo root). Auto-detected if omitted.")]
    #[serde(default)]
    changed_files: Option<Vec<String>>,
    #[schemars(description = "Number of hops to traverse in the dependency graph")]
    #[serde(default = "default_max_depth")]
    max_depth: usize,
    #[schemars(description = "Repository root path. Auto-detected if omitted.")]
    #[serde(default)]
    repo_root: Option<String>,
    #[schemars(description = "Git ref for auto-detecting changes")]
    #[serde(default = "default_base")]
    base: String,
    #[schemars(description = "Compact output (default: true). Set false for full node details including docstring, body_hash, language.")]
    #[serde(default = "default_true")]
    compact: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct QueryGraphParams {
    #[schemars(description = "Query pattern name: callers_of, callees_of, imports_of, importers_of, children_of, tests_for, inheritors_of, file_summary")]
    pattern: String,
    #[schemars(description = "Node name, qualified name, or file path to query")]
    target: String,
    #[schemars(description = "Repository root path. Auto-detected if omitted.")]
    #[serde(default)]
    repo_root: Option<String>,
    #[schemars(description = "Compact output (default: true). Set false for full node details including docstring, body_hash, language.")]
    #[serde(default = "default_true")]
    compact: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ReviewContextParams {
    #[schemars(description = "Files to review. Auto-detected from git diff if omitted.")]
    #[serde(default)]
    changed_files: Option<Vec<String>>,
    #[schemars(description = "Impact radius depth")]
    #[serde(default = "default_max_depth")]
    max_depth: usize,
    #[schemars(description = "Include source code snippets")]
    #[serde(default = "default_true")]
    include_source: bool,
    #[schemars(description = "Max source lines per file")]
    #[serde(default = "default_max_lines")]
    max_lines_per_file: usize,
    #[schemars(description = "Repository root path. Auto-detected if omitted.")]
    #[serde(default)]
    repo_root: Option<String>,
    #[schemars(description = "Git ref for change detection")]
    #[serde(default = "default_base")]
    base: String,
    #[schemars(description = "Compact output (default: true). Set false for full node details including docstring, body_hash, language.")]
    #[serde(default = "default_true")]
    compact: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SemanticSearchParams {
    #[schemars(description = "Search string to match against node names")]
    query: String,
    #[schemars(description = "Optional filter: File, Class, Function, Type, or Test")]
    #[serde(default)]
    kind: Option<String>,
    #[schemars(description = "Maximum results")]
    #[serde(default = "default_search_limit")]
    limit: usize,
    #[schemars(description = "Repository root path. Auto-detected if omitted.")]
    #[serde(default)]
    repo_root: Option<String>,
    #[schemars(description = "Compact output (default: true). Set false for full node details including docstring, body_hash, language.")]
    #[serde(default = "default_true")]
    compact: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct StatsParams {
    #[schemars(description = "Repository root path. Auto-detected if omitted.")]
    #[serde(default)]
    repo_root: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct EmbedParams {
    #[schemars(description = "Repository root path. Auto-detected if omitted.")]
    #[serde(default)]
    repo_root: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct DocsParams {
    #[schemars(description = "Section to retrieve: usage, review-delta, review-pr, commands, legal, watch, embeddings, languages, troubleshooting")]
    section_name: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct LargeFunctionsParams {
    #[schemars(description = "Minimum line count to flag")]
    #[serde(default = "default_min_lines")]
    min_lines: usize,
    #[schemars(description = "Optional filter: Function, Class, File, or Test")]
    #[serde(default)]
    kind: Option<String>,
    #[schemars(description = "Filter by file path substring")]
    #[serde(default)]
    file_path_pattern: Option<String>,
    #[schemars(description = "Maximum results")]
    #[serde(default = "default_large_limit")]
    limit: usize,
    #[schemars(description = "Repository root path. Auto-detected if omitted.")]
    #[serde(default)]
    repo_root: Option<String>,
    #[schemars(description = "Compact output (default: true). Set false for full node details including docstring, body_hash, language.")]
    #[serde(default = "default_true")]
    compact: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TraceCallChainParams {
    #[schemars(description = "Function name or qualified name for the start of the chain")]
    from: String,
    #[schemars(description = "Function name or qualified name for the end of the chain")]
    to: String,
    #[schemars(description = "Maximum number of hops to traverse (default: 10)")]
    #[serde(default = "default_chain_depth")]
    max_depth: usize,
    #[schemars(description = "Compact output (default: true). Set false for full node details.")]
    #[serde(default = "default_true")]
    compact: bool,
    #[schemars(description = "Repository root path. Auto-detected if omitted.")]
    #[serde(default)]
    repo_root: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct HybridQueryParams {
    #[schemars(description = "Search query — combines keyword matching with semantic similarity for best results")]
    query: String,
    #[schemars(description = "Maximum results (default: 10)")]
    #[serde(default = "default_search_limit")]
    limit: usize,
    #[schemars(description = "Compact output (default: true). Set false for full node details.")]
    #[serde(default = "default_true")]
    compact: bool,
    #[schemars(description = "Repository root path. Auto-detected if omitted.")]
    #[serde(default)]
    repo_root: Option<String>,
    #[schemars(description = "Internal: fusion method. Leave unset to use the default (RRF).")]
    #[serde(default)]
    fusion: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct OpenNodeContextParams {
    #[schemars(description = "Function, class, or file name to inspect")]
    target: String,
    #[schemars(description = "Compact output (default: true)")]
    #[serde(default = "default_true")]
    compact: bool,
    #[schemars(description = "Repository root path")]
    #[serde(default)]
    repo_root: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct BatchNodeContextParams {
    #[schemars(description = "List of function/class names to inspect (max 5)")]
    targets: Vec<String>,
    #[schemars(description = "Compact output (default: true)")]
    #[serde(default = "default_true")]
    compact: bool,
    #[schemars(description = "Repository root path")]
    #[serde(default)]
    repo_root: Option<String>,
}

// Default value helpers
fn default_base() -> String { "HEAD~1".to_string() }
fn default_max_depth() -> usize { 2 }
fn default_true() -> bool { true }
fn default_max_lines() -> usize { 200 }
fn default_search_limit() -> usize { 10 }
fn default_min_lines() -> usize { 50 }
fn default_large_limit() -> usize { 50 }
fn default_chain_depth() -> usize { 10 }

// ---------------------------------------------------------------------------
// Worker command channel
// ---------------------------------------------------------------------------

/// Commands sent to the worker thread that owns GraphStore + EmbeddingStore.
///
/// Read-only variants carry a `reply` oneshot so the async handler can await
/// the result without blocking the tokio runtime. Mutation variants (BuildGraph,
/// EmbedGraph) reload the worker's in-memory store from disk after the tool
/// function finishes (those functions open their own stores and write to disk).
/// Watcher variants have no reply — they are fire-and-forget.
enum WorkerCommand {
    // Read-only queries
    QueryGraph {
        pattern: String,
        target: String,
        compact: bool,
        reply: tokio::sync::oneshot::Sender<Result<serde_json::Value, String>>,
    },
    SemanticSearch {
        query: String,
        kind: Option<String>,
        limit: usize,
        compact: bool,
        reply: tokio::sync::oneshot::Sender<Result<serde_json::Value, String>>,
    },
    OpenNodeContext {
        target: String,
        compact: bool,
        reply: tokio::sync::oneshot::Sender<Result<serde_json::Value, String>>,
    },
    BatchNodeContext {
        targets: Vec<String>,
        compact: bool,
        reply: tokio::sync::oneshot::Sender<Result<serde_json::Value, String>>,
    },
    ImpactRadius {
        changed_files: Option<Vec<String>>,
        max_depth: usize,
        compact: bool,
        base: String,
        reply: tokio::sync::oneshot::Sender<Result<serde_json::Value, String>>,
    },
    TraceCallChain {
        from: String,
        to: String,
        max_depth: usize,
        compact: bool,
        reply: tokio::sync::oneshot::Sender<Result<serde_json::Value, String>>,
    },
    HybridQuery {
        query: String,
        limit: usize,
        compact: bool,
        fusion: Option<String>,
        reply: tokio::sync::oneshot::Sender<Result<serde_json::Value, String>>,
    },
    ListStats {
        reply: tokio::sync::oneshot::Sender<Result<serde_json::Value, String>>,
    },
    FindLargeFunctions {
        min_lines: usize,
        kind: Option<String>,
        file_path_pattern: Option<String>,
        limit: usize,
        compact: bool,
        reply: tokio::sync::oneshot::Sender<Result<serde_json::Value, String>>,
    },
    GetReviewContext {
        changed_files: Option<Vec<String>>,
        max_depth: usize,
        include_source: bool,
        max_lines: usize,
        compact: bool,
        base: String,
        reply: tokio::sync::oneshot::Sender<Result<serde_json::Value, String>>,
    },
    // Mutations — these tools open their own stores and write to disk;
    // the worker reloads from disk afterwards.
    BuildGraph {
        full_rebuild: bool,
        base: String,
        reply: tokio::sync::oneshot::Sender<Result<serde_json::Value, String>>,
    },
    EmbedGraph {
        reply: tokio::sync::oneshot::Sender<Result<serde_json::Value, String>>,
    },
    // Watcher events — fire-and-forget (no reply)
    WatcherUpdate {
        paths: Vec<std::path::PathBuf>,
    },
    WatcherRemove {
        paths: Vec<std::path::PathBuf>,
    },
    #[allow(dead_code)]
    Shutdown,
}

// ---------------------------------------------------------------------------
// Worker thread
// ---------------------------------------------------------------------------

/// OS thread that owns GraphStore, EmbeddingStore, CodeParser, and tree_cache.
///
/// Processes WorkerCommand messages sequentially (single writer), eliminating
/// concurrent disk-write races between tool handlers and the file watcher.
fn run_worker_thread(root: Utf8PathBuf, cmd_rx: std::sync::mpsc::Receiver<WorkerCommand>) {
    use crate::incremental::{find_dependents, get_db_path, get_embeddings_db_path};

    let db_path = get_db_path(&root);
    let emb_path = get_embeddings_db_path(&root);

    // GraphStore::new creates an empty graph when the file doesn't exist yet.
    let mut store = crate::graph::GraphStore::new(&db_path)
        .unwrap_or_else(|e| panic!("Worker: cannot open store: {e}"));
    let mut emb_store = match crate::embeddings::EmbeddingStore::new(&emb_path) {
        Ok(s) => s,
        Err(e) => panic!("Worker: cannot open embedding store: {e}"),
    };
    let parser = crate::parser::CodeParser::new();
    let mut tree_cache: HashMap<String, tree_sitter::Tree> = HashMap::new();

    /// Reload the graph store from disk (called after BuildGraph mutations).
    macro_rules! reload_store {
        () => {
            match crate::graph::GraphStore::new(&db_path) {
                Ok(s) => { store = s; }
                Err(e) => tracing::error!("Worker: reload store failed: {e}"),
            }
        };
    }

    /// Reload the embedding store from disk (called after EmbedGraph mutations).
    macro_rules! reload_emb {
        () => {
            match crate::embeddings::EmbeddingStore::new(&emb_path) {
                Ok(s) => { emb_store = s; }
                Err(e) => tracing::error!("Worker: reload emb store failed: {e}"),
            }
        };
    }

    for cmd in cmd_rx {
        match cmd {
            WorkerCommand::QueryGraph { pattern, target, compact, reply } => {
                let _span = tracing::info_span!("worker_cmd", cmd = "query_graph").entered();
                let result = crate::tools::query_graph_with_store(&store, &root, &pattern, &target, compact)
                    .map_err(|e| e.to_string());
                let _ = reply.send(result);
            }

            WorkerCommand::SemanticSearch { query, kind, limit, compact, reply } => {
                let _span = tracing::info_span!("worker_cmd", cmd = "semantic_search").entered();
                let result = crate::tools::semantic_search_nodes_with_store(
                    &store, &mut emb_store, &root, &query, kind.as_deref(), limit, compact,
                ).map_err(|e| e.to_string());
                let _ = reply.send(result);
            }

            WorkerCommand::OpenNodeContext { target, compact, reply } => {
                let _span = tracing::info_span!("worker_cmd", cmd = "open_node_context").entered();
                let result = crate::tools::open_node_context_with_store(&store, &root, &target, compact)
                    .map_err(|e| e.to_string());
                let _ = reply.send(result);
            }

            WorkerCommand::BatchNodeContext { targets, compact, reply } => {
                let _span = tracing::info_span!("worker_cmd", cmd = "batch_node_context").entered();
                let result = crate::tools::batch_open_node_context_with_store(&store, &root, targets, compact)
                    .map_err(|e| e.to_string());
                let _ = reply.send(result);
            }

            WorkerCommand::ImpactRadius { changed_files, max_depth, compact, base, reply } => {
                let _span = tracing::info_span!("worker_cmd", cmd = "impact_radius").entered();
                let files = crate::tools::resolve_changed_files(changed_files, &root, &base);
                let result = crate::tools::get_impact_radius_with_store(&store, &root, files, max_depth, compact)
                    .map_err(|e| e.to_string());
                let _ = reply.send(result);
            }

            WorkerCommand::TraceCallChain { from, to, max_depth, compact, reply } => {
                let _span = tracing::info_span!("worker_cmd", cmd = "trace_call_chain").entered();
                let result = crate::tools::trace_call_chain_with_store(&store, &root, &from, &to, max_depth, compact)
                    .map_err(|e| e.to_string());
                let _ = reply.send(result);
            }

            WorkerCommand::HybridQuery { query, limit, compact, fusion, reply } => {
                let _span = tracing::info_span!("worker_cmd", cmd = "hybrid_query").entered();
                let result = crate::tools::hybrid_query_with_store(&store, &mut emb_store, &root, &query, limit, compact, fusion.as_deref())
                    .map_err(|e| e.to_string());
                let _ = reply.send(result);
            }

            WorkerCommand::ListStats { reply } => {
                let _span = tracing::info_span!("worker_cmd", cmd = "list_stats").entered();
                let result = crate::tools::list_graph_stats_with_store(&store, &root)
                    .map_err(|e| e.to_string());
                let _ = reply.send(result);
            }

            WorkerCommand::FindLargeFunctions { min_lines, kind, file_path_pattern, limit, compact, reply } => {
                let _span = tracing::info_span!("worker_cmd", cmd = "find_large_functions").entered();
                let result = crate::tools::find_large_functions_with_store(
                    &store, &root, min_lines, kind.as_deref(), file_path_pattern.as_deref(), limit, compact,
                ).map_err(|e| e.to_string());
                let _ = reply.send(result);
            }

            WorkerCommand::GetReviewContext { changed_files, max_depth, include_source, max_lines, compact, base, reply } => {
                let _span = tracing::info_span!("worker_cmd", cmd = "get_review_context").entered();
                let files = crate::tools::resolve_changed_files(changed_files, &root, &base);
                let result = crate::tools::get_review_context_with_store(
                    &store, &root, files, max_depth, include_source, max_lines, compact,
                ).map_err(|e| e.to_string());
                let _ = reply.send(result);
            }

            WorkerCommand::BuildGraph { full_rebuild, base, reply } => {
                let _span = tracing::info_span!("worker_cmd", cmd = "build_graph").entered();
                // build_or_update_graph opens its own store, writes to disk, closes it.
                let root_str = root.as_str().to_owned();
                let result = crate::tools::build_or_update_graph(full_rebuild, Some(&root_str), &base)
                    .map_err(|e| e.to_string());
                // Reload worker's in-memory store from disk.
                reload_store!();
                let _ = reply.send(result);
            }

            WorkerCommand::EmbedGraph { reply } => {
                let _span = tracing::info_span!("worker_cmd", cmd = "embed_graph").entered();
                // embed_graph opens its own stores, writes to disk, closes them.
                let root_str = root.as_str().to_owned();
                let result = crate::tools::embed_graph(Some(&root_str))
                    .map_err(|e| e.to_string());
                // Reload worker's in-memory embedding store from disk.
                reload_emb!();
                let _ = reply.send(result);
            }

            WorkerCommand::WatcherUpdate { paths } => {
                let _span = tracing::info_span!("worker_cmd", cmd = "watcher_update").entered();
                // Track processed paths to guard against circular import cycles.
                let mut processed: HashSet<String> = paths
                    .iter()
                    .map(|p| crate::paths::normalize_path(&p.to_string_lossy()))
                    .collect();

                for path in &paths {
                    let abs_str = crate::paths::normalize_path(&path.to_string_lossy());
                    let old_tree = tree_cache.get(&abs_str);
                    let incremental = old_tree.is_some();
                    match watcher_parse_and_store(&parser, &mut store, path, old_tree) {
                        Ok(Some((n, e, new_tree))) => {
                            if let Some(t) = new_tree {
                                tree_cache.insert(abs_str.clone(), t);
                            }
                            let _ = store.set_metadata(
                                "last_updated",
                                &chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S").to_string(),
                            );
                            let rel = path
                                .strip_prefix(root.as_std_path())
                                .map(|p| p.display().to_string())
                                .unwrap_or_else(|_| abs_str.clone());
                            tracing::info!(
                                "Worker updated: {} ({n} nodes, {e} edges, {})",
                                rel,
                                if incremental { "incremental" } else { "full parse" }
                            );

                            // Re-parse dependents so cross-file edges stay fresh.
                            let deps = find_dependents(&store, &abs_str).unwrap_or_default();
                            for dep_path in &deps {
                                if processed.contains(dep_path.as_str()) {
                                    continue;
                                }
                                processed.insert(dep_path.clone());
                                let dep = std::path::Path::new(dep_path);
                                let dep_old_tree = tree_cache.get(dep_path.as_str());
                                match watcher_parse_and_store(&parser, &mut store, dep, dep_old_tree) {
                                    Ok(Some((dn, de, dep_new_tree))) => {
                                        if let Some(t) = dep_new_tree {
                                            tree_cache.insert(dep_path.clone(), t);
                                        }
                                        tracing::debug!(
                                            "Worker re-parsed dependent: {} ({dn} nodes, {de} edges)",
                                            dep_path
                                        );
                                    }
                                    Ok(None) => tracing::debug!(
                                        "Worker dependent unchanged (hash match): {dep_path}"
                                    ),
                                    Err(e) => tracing::warn!("Worker dependent {dep_path}: {e}"),
                                }
                            }
                        }
                        Ok(None) => {
                            let rel = path
                                .strip_prefix(root.as_std_path())
                                .map(|p| p.display().to_string())
                                .unwrap_or_else(|_| abs_str.clone());
                            tracing::debug!("Worker skipped (hash unchanged): {rel}");
                        }
                        Err(err) => tracing::error!("Worker parse: {err}"),
                    }
                }

                if let Err(e) = store.commit() {
                    tracing::error!("Worker commit error: {e}");
                }
            }

            WorkerCommand::WatcherRemove { paths } => {
                let _span = tracing::info_span!("worker_cmd", cmd = "watcher_remove").entered();
                for path in &paths {
                    let abs_str = crate::paths::normalize_path(&path.to_string_lossy());
                    tree_cache.remove(&abs_str);
                    if let Err(e) = store.remove_file_data(&abs_str) {
                        tracing::error!("Worker remove {abs_str}: {e}");
                    } else {
                        tracing::info!("Worker removed: {abs_str}");
                    }
                }
                if let Err(e) = store.commit() {
                    tracing::error!("Worker commit error (remove): {e}");
                }
            }

            WorkerCommand::Shutdown => break,
        }
    }

    tracing::info!("Worker thread shutting down");
    if let Err(e) = store.close() {
        tracing::error!("Worker: store close error: {e}");
    }
}

// ---------------------------------------------------------------------------
// MCP server struct
// ---------------------------------------------------------------------------

/// The MCP server for code-review-graph.
///
/// `worker_tx` is `Some` in server mode (the worker thread is running).
/// `Clone` is required by rmcp — `Sender<WorkerCommand>` is `Clone`.
#[derive(Clone)]
pub struct CodeReviewServer {
    /// Default repo root passed via the CLI `--repo` flag.
    repo_root: Option<Arc<String>>,
    tool_router: ToolRouter<Self>,
    /// Channel to the worker thread. None only in unit-test / CLI shim contexts.
    worker_tx: Option<std::sync::mpsc::Sender<WorkerCommand>>,
}

impl std::fmt::Debug for CodeReviewServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodeReviewServer")
            .field("repo_root", &self.repo_root)
            .finish_non_exhaustive()
    }
}

impl CodeReviewServer {
    fn new_inner(repo_root: Option<String>, worker_tx: Option<std::sync::mpsc::Sender<WorkerCommand>>) -> Self {
        Self {
            repo_root: repo_root.map(Arc::new),
            tool_router: Self::tool_router(),
            worker_tx,
        }
    }

    /// CLI / test constructor — no worker thread, falls back to spawn_blocking.
    pub fn new(repo_root: Option<String>) -> Self {
        Self::new_inner(repo_root, None)
    }

    /// Resolve the effective repo root: prefer the per-call value, fall back to
    /// the server-level default set via `--repo`.
    fn resolve_repo_root(&self, per_call: Option<String>) -> Option<String> {
        per_call.or_else(|| self.repo_root.as_deref().map(|s| s.to_string()))
    }

    /// True when the worker thread can handle this request — i.e., the worker
    /// exists AND the resolved repo_root matches the worker's repo (or is None).
    /// When the caller asks for a *different* repo, we must bypass the worker
    /// and use the fallback path which opens its own stores.
    fn use_worker(&self, resolved_root: &Option<String>) -> bool {
        if self.worker_tx.is_none() {
            return false;
        }
        match (resolved_root, &self.repo_root) {
            // No override — use worker's default repo.
            (None, _) => true,
            // Override matches worker's repo.
            (Some(req), Some(default)) => {
                crate::paths::normalize_path(req) == crate::paths::normalize_path(default)
            }
            // Override specified but no default — can't compare, fallback.
            (Some(_), None) => false,
        }
    }

    /// Send a command to the worker and await the reply.
    ///
    /// Returns `Err("worker died")` if the channel is closed.
    async fn worker_call(
        &self,
        make_cmd: impl FnOnce(tokio::sync::oneshot::Sender<Result<serde_json::Value, String>>) -> WorkerCommand,
    ) -> Result<String, String> {
        let tx = self.worker_tx.as_ref().ok_or_else(|| "no worker channel".to_string())?;
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        tx.send(make_cmd(reply_tx)).map_err(|_| "worker died".to_string())?;
        reply_rx.await
            .map_err(|_| "worker died (reply dropped)".to_string())?
            .map(|v| v.to_string())
    }

    /// Fallback path used only when no worker_tx is available (CLI / tests).
    async fn spawn_blocking_fallback<F>(&self, f: F) -> Result<String, String>
    where
        F: FnOnce() -> crate::error::Result<serde_json::Value> + Send + 'static,
    {
        tokio::task::spawn_blocking(move || {
            f().map(|v| v.to_string()).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }
}

// ---------------------------------------------------------------------------
// Tool implementations
// ---------------------------------------------------------------------------

#[tool_router]
impl CodeReviewServer {
    /// Build or incrementally update the code knowledge graph.
    ///
    /// Call this first to initialize the graph, or after making changes.
    /// By default performs an incremental update (only changed files).
    /// Set full_rebuild=True to re-parse every file.
    #[tool(name = "build_or_update_graph")]
    async fn build_or_update_graph_tool(
        &self,
        Parameters(p): Parameters<BuildOrUpdateParams>,
    ) -> std::result::Result<String, String> {
        let repo_root = self.resolve_repo_root(p.repo_root);
        if self.use_worker(&repo_root) {
            self.worker_call(|reply| WorkerCommand::BuildGraph {
                full_rebuild: p.full_rebuild,
                base: p.base,
                reply,
            }).await
        } else {
            self.spawn_blocking_fallback(move || {
                crate::tools::build_or_update_graph(p.full_rebuild, repo_root.as_deref(), &p.base)
            }).await
        }
    }

    /// Analyze the blast radius of code changes — shows which functions,
    /// classes, and files are affected. Use during code review to understand
    /// what a change impacts. Pass changed file paths, or let it auto-detect
    /// from git diff. Follow up with Read tool on impacted files.
    #[tool(name = "get_impact_radius")]
    async fn get_impact_radius_tool(
        &self,
        Parameters(p): Parameters<ImpactRadiusParams>,
    ) -> std::result::Result<String, String> {
        let repo_root = self.resolve_repo_root(p.repo_root);
        if self.use_worker(&repo_root) {
            self.worker_call(|reply| WorkerCommand::ImpactRadius {
                changed_files: p.changed_files,
                max_depth: p.max_depth,
                compact: p.compact,
                base: p.base,
                reply,
            }).await
        } else {
            self.spawn_blocking_fallback(move || {
                crate::tools::get_impact_radius(p.changed_files, p.max_depth, repo_root.as_deref(), &p.base, p.compact)
            }).await
        }
    }

    /// Explore structural code relationships — use INSTEAD of grepping for
    /// function names. callers_of: who calls this? callees_of: what does it
    /// call? children_of: what's in this file/class? file_summary: overview.
    /// Use callers_of/callees_of to navigate between functions instead of
    /// running grep in bash. After finding connected functions, use the
    /// Read tool to examine their logic.
    #[tool(name = "query_graph")]
    async fn query_graph_tool(
        &self,
        Parameters(p): Parameters<QueryGraphParams>,
    ) -> std::result::Result<String, String> {
        let repo_root = self.resolve_repo_root(p.repo_root);
        if self.use_worker(&repo_root) {
            self.worker_call(|reply| WorkerCommand::QueryGraph {
                pattern: p.pattern,
                target: p.target,
                compact: p.compact,
                reply,
            }).await
        } else {
            self.spawn_blocking_fallback(move || {
                crate::tools::query_graph(&p.pattern, &p.target, repo_root.as_deref(), p.compact)
            }).await
        }
    }

    /// Generate a focused, token-efficient review context for code changes.
    ///
    /// Combines impact analysis with source snippets and review guidance.
    /// Use this for comprehensive code reviews.
    #[tool(name = "get_review_context")]
    async fn get_review_context_tool(
        &self,
        Parameters(p): Parameters<ReviewContextParams>,
    ) -> std::result::Result<String, String> {
        let repo_root = self.resolve_repo_root(p.repo_root);
        if self.use_worker(&repo_root) {
            self.worker_call(|reply| WorkerCommand::GetReviewContext {
                changed_files: p.changed_files,
                max_depth: p.max_depth,
                include_source: p.include_source,
                max_lines: p.max_lines_per_file,
                compact: p.compact,
                base: p.base,
                reply,
            }).await
        } else {
            self.spawn_blocking_fallback(move || {
                crate::tools::get_review_context(
                    p.changed_files, p.max_depth, p.include_source,
                    p.max_lines_per_file, repo_root.as_deref(), &p.base, p.compact,
                )
            }).await
        }
    }

    /// Search for code entities by name, keyword, or semantic similarity.
    ///
    /// Faster and more precise than grep for discovering WHERE code lives.
    /// Use as your FIRST tool when investigating a new concept (e.g.,
    /// "CSS chunk splitting", "auth middleware", "database connection pool").
    /// Returns ranked results with similarity scores. After finding targets,
    /// use the Read tool (not bash cat) to examine their source code.
    /// Always pass compact: true to reduce response size.
    #[tool(name = "semantic_search_nodes")]
    async fn semantic_search_nodes_tool(
        &self,
        Parameters(p): Parameters<SemanticSearchParams>,
    ) -> std::result::Result<String, String> {
        let repo_root = self.resolve_repo_root(p.repo_root);
        if self.use_worker(&repo_root) {
            self.worker_call(|reply| WorkerCommand::SemanticSearch {
                query: p.query,
                kind: p.kind,
                limit: p.limit,
                compact: p.compact,
                reply,
            }).await
        } else {
            self.spawn_blocking_fallback(move || {
                crate::tools::semantic_search_nodes(&p.query, p.kind.as_deref(), p.limit, repo_root.as_deref(), p.compact)
            }).await
        }
    }

    /// Get aggregate statistics about the code knowledge graph.
    ///
    /// Shows total nodes, edges, languages, files, and last update time.
    /// Useful for checking if the graph is built and up to date.
    #[tool(name = "list_graph_stats")]
    async fn list_graph_stats_tool(
        &self,
        Parameters(p): Parameters<StatsParams>,
    ) -> std::result::Result<String, String> {
        let repo_root = self.resolve_repo_root(p.repo_root);
        if self.use_worker(&repo_root) {
            self.worker_call(|reply| WorkerCommand::ListStats { reply }).await
        } else {
            self.spawn_blocking_fallback(move || {
                crate::tools::list_graph_stats(repo_root.as_deref())
            }).await
        }
    }

    /// Compute vector embeddings for all graph nodes to enable semantic search.
    ///
    /// Uses Jina Code Embeddings v2 (768-dim) by default via fastembed.
    /// Only computes embeddings for nodes that don't already have them.
    /// After running this, semantic_search_nodes uses vector similarity.
    #[tool(name = "embed_graph")]
    async fn embed_graph_tool(
        &self,
        Parameters(p): Parameters<EmbedParams>,
    ) -> std::result::Result<String, String> {
        let repo_root = self.resolve_repo_root(p.repo_root);
        if self.use_worker(&repo_root) {
            self.worker_call(|reply| WorkerCommand::EmbedGraph { reply }).await
        } else {
            self.spawn_blocking_fallback(move || {
                crate::tools::embed_graph(repo_root.as_deref())
            }).await
        }
    }

    /// Get a specific section from the LLM-optimized documentation reference.
    ///
    /// Returns only the requested section content for minimal token usage.
    /// Available sections: usage, review-delta, review-pr, commands, legal,
    /// watch, embeddings, languages, troubleshooting.
    #[tool(name = "get_docs_section")]
    async fn get_docs_section_tool(
        &self,
        Parameters(p): Parameters<DocsParams>,
    ) -> std::result::Result<String, String> {
        // get_docs_section only does a filesystem scan — no store read required.
        let repo_root = self.resolve_repo_root(None);
        self.spawn_blocking_fallback(move || {
            crate::tools::get_docs_section(&p.section_name, repo_root.as_deref())
        }).await
    }

    /// Find complex functions by line count — useful for identifying where
    /// business logic concentrates and finding likely bug locations.
    /// Use for decomposition audits and code quality checks.
    /// Results ordered by size, largest first.
    #[tool(name = "find_large_functions")]
    async fn find_large_functions_tool(
        &self,
        Parameters(p): Parameters<LargeFunctionsParams>,
    ) -> std::result::Result<String, String> {
        let repo_root = self.resolve_repo_root(p.repo_root);
        if self.use_worker(&repo_root) {
            self.worker_call(|reply| WorkerCommand::FindLargeFunctions {
                min_lines: p.min_lines,
                kind: p.kind,
                file_path_pattern: p.file_path_pattern,
                limit: p.limit,
                compact: p.compact,
                reply,
            }).await
        } else {
            self.spawn_blocking_fallback(move || {
                crate::tools::find_large_functions(
                    p.min_lines, p.kind.as_deref(), p.file_path_pattern.as_deref(),
                    p.limit, repo_root.as_deref(), p.compact,
                )
            }).await
        }
    }

    /// Trace how data flows between two functions — finds the shortest
    /// chain of function calls connecting them. Use when you know WHERE
    /// two pieces of code are but need to understand HOW they connect.
    /// Example: trace_call_chain(from: "parseConfig", to: "renderPage")
    /// returns the full intermediate call path. This REPLACES reading
    /// files one-by-one to manually follow call chains. Try this BEFORE
    /// reading multiple files — it often answers "how does A reach B?"
    /// in one call. Tries callee direction first, then caller direction.
    #[tool(name = "trace_call_chain")]
    async fn trace_call_chain_tool(
        &self,
        Parameters(p): Parameters<TraceCallChainParams>,
    ) -> std::result::Result<String, String> {
        let repo_root = self.resolve_repo_root(p.repo_root);
        if self.use_worker(&repo_root) {
            self.worker_call(|reply| WorkerCommand::TraceCallChain {
                from: p.from,
                to: p.to,
                max_depth: p.max_depth,
                compact: p.compact,
                reply,
            }).await
        } else {
            self.spawn_blocking_fallback(move || {
                crate::tools::trace_call_chain(&p.from, &p.to, p.max_depth, p.compact, repo_root.as_deref())
            }).await
        }
    }

    /// Smart search combining keyword matching with semantic similarity
    /// via Reciprocal Rank Fusion (RRF). Prefer this over semantic_search_nodes
    /// when you want the best of both worlds — exact name matches AND
    /// conceptual similarity in one ranked result set. Falls back to
    /// keyword-only when embeddings are unavailable.
    #[tool(name = "hybrid_query")]
    #[tracing::instrument(skip(self))]
    async fn hybrid_query_tool(
        &self,
        Parameters(p): Parameters<HybridQueryParams>,
    ) -> std::result::Result<String, String> {
        let repo_root = self.resolve_repo_root(p.repo_root);
        if self.use_worker(&repo_root) {
            self.worker_call(|reply| WorkerCommand::HybridQuery {
                query: p.query,
                limit: p.limit,
                compact: p.compact,
                fusion: p.fusion,
                reply,
            }).await
        } else {
            self.spawn_blocking_fallback(move || {
                crate::tools::hybrid_query(&p.query, p.limit, repo_root.as_deref(), p.compact, p.fusion.as_deref())
            }).await
        }
    }

    /// Get complete context for a function or class in one call — source preview,
    /// callers, callees, tests, and file siblings. Use this as your PRIMARY
    /// investigation tool instead of making separate query_graph + Read calls.
    /// Returns everything you need to understand a symbol's role in the codebase.
    #[tool(name = "open_node_context")]
    async fn open_node_context_tool(
        &self,
        Parameters(p): Parameters<OpenNodeContextParams>,
    ) -> std::result::Result<String, String> {
        let repo_root = self.resolve_repo_root(p.repo_root);
        if self.use_worker(&repo_root) {
            self.worker_call(|reply| WorkerCommand::OpenNodeContext {
                target: p.target,
                compact: p.compact,
                reply,
            }).await
        } else {
            self.spawn_blocking_fallback(move || {
                crate::tools::open_node_context(&p.target, p.compact, repo_root.as_deref())
            }).await
        }
    }

    /// Inspect multiple functions at once — saves N-1 tool calls compared to
    /// calling open_node_context separately for each. Max 5 targets per call.
    /// Use when you've identified several candidate symbols to investigate.
    #[tool(name = "batch_open_node_context")]
    async fn batch_open_node_context_tool(
        &self,
        Parameters(p): Parameters<BatchNodeContextParams>,
    ) -> std::result::Result<String, String> {
        let repo_root = self.resolve_repo_root(p.repo_root);
        if self.use_worker(&repo_root) {
            self.worker_call(|reply| WorkerCommand::BatchNodeContext {
                targets: p.targets,
                compact: p.compact,
                reply,
            }).await
        } else {
            self.spawn_blocking_fallback(move || {
                crate::tools::batch_open_node_context(p.targets, p.compact, repo_root.as_deref())
            }).await
        }
    }
}

// ---------------------------------------------------------------------------
// ServerHandler implementation (wired to the tool router)
// ---------------------------------------------------------------------------

#[tool_handler(router = self.tool_router)]
impl ServerHandler for CodeReviewServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .build(),
        )
        .with_instructions(
            "Persistent incremental knowledge graph for token-efficient code reviews.\n\n\
             RECOMMENDED WORKFLOW:\n\
             1. semantic_search_nodes — find code by concept (use INSTEAD OF grep for discovery)\n\
             2. trace_call_chain(from, to) — when you know two function names, trace the call path between them (use INSTEAD OF reading files one-by-one)\n\
             3. query_graph(callers_of/callees_of) — explore who calls what (use INSTEAD OF grepping for function names)\n\
             4. Use Read tool (not bash cat) and Grep tool (not bash grep) for examining file contents\n\n\
             Always pass compact: true to reduce response size. \
             Use these tools for discovery, then switch to Read/Grep for detailed analysis.",
        )
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Resolve a server-level repo root string to a `Utf8PathBuf`, or fall back to
/// the project root auto-detection logic used by `tools.rs`.
fn resolve_root(repo_root: Option<&str>) -> Utf8PathBuf {
    match repo_root {
        Some(p) => Utf8PathBuf::from(p),
        None => crate::incremental::find_project_root(None),
    }
}

// ---------------------------------------------------------------------------
// Parse + store helper (used by worker for watcher events)
// ---------------------------------------------------------------------------

/// Parse `path` from disk and store its nodes/edges into `store`.
///
/// When `old_tree` is `Some`, uses tree-sitter incremental parsing to reuse
/// unchanged AST regions (~5× faster for typical edits).
///
/// Returns `Ok(None)` when the file hash is unchanged (skip).
/// Returns `Ok(Some((node_count, edge_count, new_tree)))` on a successful update.
/// `new_tree` is `None` for Vue SFC files (incremental parsing unsupported there).
/// Returns `Err(String)` on failure.
fn watcher_parse_and_store(
    parser: &crate::parser::CodeParser,
    store: &mut crate::graph::GraphStore,
    path: &std::path::Path,
    old_tree: Option<&tree_sitter::Tree>,
) -> Result<Option<(usize, usize, Option<tree_sitter::Tree>)>, String> {
    use crate::incremental::{is_binary_pub, sha256_bytes_pub};
    if is_binary_pub(path) {
        return Err(format!("{}: binary file skipped", path.display()));
    }
    let source = std::fs::read(path).map_err(|e| format!("{}: {}", path.display(), e))?;
    let fhash = sha256_bytes_pub(&source);
    let abs_str = crate::paths::normalize_path(&path.to_string_lossy());

    // Hash-skip: avoid re-parsing when file content hasn't changed.
    if store.get_file_hash(&abs_str) == Some(fhash.as_str()) {
        return Ok(None);
    }

    // Try incremental parse; fall back to full parse for Vue SFC files.
    let (nodes, edges, new_tree) = match parser.parse_bytes_with_tree(path, &source, old_tree) {
        Ok((n, e, t)) => (n, e, Some(t)),
        Err(_) => {
            let (n, e) = parser
                .parse_bytes(path, &source)
                .map_err(|e| format!("{}: {}", path.display(), e))?;
            (n, e, None)
        }
    };
    let n = nodes.len();
    let e = edges.len();
    store
        .store_file_nodes_edges(&abs_str, &nodes, &edges, &fhash)
        .map_err(|e| format!("{}: {}", path.display(), e))?;
    Ok(Some((n, e, new_tree)))
}

// ---------------------------------------------------------------------------
// Background watcher notifier (sends paths to worker, no store access here)
// ---------------------------------------------------------------------------

/// Spawn a notify debouncer that watches `repo_root` for source-file changes
/// and forwards path batches to the worker via `cmd_tx`.
///
/// The worker owns GraphStore and performs all actual parsing + writes.
/// This function only handles filesystem events and routing.
fn run_watcher_notifier(
    repo_root: Utf8PathBuf,
    cmd_tx: std::sync::mpsc::Sender<WorkerCommand>,
) -> crate::error::Result<()> {
    use notify::RecursiveMode;
    use notify_debouncer_mini::{new_debouncer, DebounceEventResult};
    use crate::incremental::{load_ignore_patterns_pub, should_ignore_pub};
    use crate::parser::CodeParser;

    let ignore_patterns = load_ignore_patterns_pub(&repo_root);
    let parser = CodeParser::new();

    let (tx, rx) = std::sync::mpsc::channel::<DebounceEventResult>();
    let mut debouncer = new_debouncer(Duration::from_millis(300), tx)
        .map_err(|e| crate::error::CrgError::Other(e.to_string()))?;
    debouncer
        .watcher()
        .watch(repo_root.as_std_path(), RecursiveMode::Recursive)
        .map_err(|e| crate::error::CrgError::Other(e.to_string()))?;

    tracing::info!("Background watcher active — watching {}", repo_root);

    for result in rx {
        let events = match result {
            Ok(evts) => evts,
            Err(e) => {
                tracing::error!("Watcher error: {:?}", e);
                continue;
            }
        };

        let mut paths_to_update: HashSet<std::path::PathBuf> = HashSet::new();
        let mut paths_to_remove: HashSet<std::path::PathBuf> = HashSet::new();

        for event in events {
            let path = event.path;
            if path.is_symlink() {
                continue;
            }
            let rel = match path.strip_prefix(repo_root.as_std_path()) {
                Ok(r) => r.to_string_lossy().replace('\\', "/"),
                Err(_) => continue,
            };
            if should_ignore_pub(&rel, &ignore_patterns) {
                continue;
            }
            if path.is_file() {
                if parser.detect_language(&path).is_some() {
                    paths_to_update.insert(path);
                }
            } else {
                paths_to_remove.insert(path);
            }
        }

        if paths_to_update.is_empty() && paths_to_remove.is_empty() {
            continue;
        }

        if !paths_to_remove.is_empty() {
            let remove_vec: Vec<_> = paths_to_remove.into_iter().collect();
            if cmd_tx.send(WorkerCommand::WatcherRemove { paths: remove_vec }).is_err() {
                tracing::warn!("Watcher: worker channel closed, stopping");
                break;
            }
        }

        if !paths_to_update.is_empty() {
            let update_vec: Vec<_> = paths_to_update.into_iter().collect();
            if cmd_tx.send(WorkerCommand::WatcherUpdate { paths: update_vec }).is_err() {
                tracing::warn!("Watcher: worker channel closed, stopping");
                break;
            }
        }
    }

    tracing::info!("Background watcher stopped.");
    Ok(())
}

/// Run the MCP server over stdio. Blocks until the client disconnects.
pub async fn run_server(repo_root: Option<String>) -> crate::error::Result<()> {
    // Resolve the repository root once.
    let root = resolve_root(repo_root.as_deref());

    // Spawn the worker thread and watcher only when we have a usable root.
    let worker_tx = if root.exists() {
        let (tx, rx) = std::sync::mpsc::channel::<WorkerCommand>();

        // Worker thread — owns GraphStore, EmbeddingStore, CodeParser, tree_cache.
        let worker_root = root.clone();
        std::thread::spawn(move || run_worker_thread(worker_root, rx));

        // Watcher notifier thread — only routes paths to the worker.
        let watcher_root = root.clone();
        let watcher_tx = tx.clone();
        std::thread::spawn(move || {
            if let Err(e) = run_watcher_notifier(watcher_root, watcher_tx) {
                tracing::error!("Background watcher error: {}", e);
            }
        });

        Some(tx)
    } else {
        tracing::info!("No repo root detected, worker + watcher disabled");
        None
    };

    let server = CodeReviewServer::new_inner(repo_root, worker_tx);
    let (stdin, stdout) = transport::io::stdio();
    serve_server(server, (stdin, stdout))
        .await
        .map_err(|e| crate::error::CrgError::Other(e.to_string()))?
        .waiting()
        .await
        .map_err(|e| crate::error::CrgError::Other(e.to_string()))?;
    Ok(())
}
