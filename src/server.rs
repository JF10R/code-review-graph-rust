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
    handler::server::tool::ToolCallContext,
    handler::server::wrapper::Parameters,
    model::{CallToolRequestParams, CallToolResult, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerInfo},
    schemars, serve_server, tool, tool_router,
    service::{RequestContext, RoleServer},
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
    #[schemars(description = "Routing strategy: 'auto' (default, classifies query and picks best approach), 'legacy' (current RRF, no classification), 'exact' (keyword-only), 'semantic' (semantic-only), 'path' (boost file/path matches).")]
    #[serde(default)]
    route: Option<String>,
    #[schemars(description = "When true, include _debug metadata showing which route fired and classification confidence.")]
    #[serde(default)]
    debug: Option<bool>,
    #[schemars(description = "Experimental: 'node' (default) or 'file' (aggregate results by file with supporting node evidence).")]
    #[serde(default)]
    result_mode: Option<String>,
    #[schemars(description = "'fast' (default: file-mode, top 3, no expansion) or 'thorough' (full pipeline, all channels).")]
    #[serde(default)]
    budget: Option<String>,
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
        route: Option<String>,
        debug: Option<bool>,
        result_mode: Option<String>,
        budget: Option<String>,
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
    /// Reload stores from disk (triggered when graph.bin.zst or embeddings.bin.zst changes externally).
    ReloadStores,
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
    let ignore_patterns = crate::incremental::load_ignore_patterns_pub(&root);
    let mut tree_cache: HashMap<String, tree_sitter::Tree> = HashMap::new();
    const MAX_TREE_CACHE: usize = 2_000;

    // Cached Tantivy full-text index — rebuilt after every store mutation.
    // Eliminates per-query index reconstruction on the keyword search path.
    #[cfg(feature = "tantivy-search")]
    let mut tantivy_cache: Option<crate::search::TantivySearchIndex> = None;

    #[cfg(feature = "tantivy-search")]
    macro_rules! rebuild_tantivy {
        () => {
            match crate::search::TantivySearchIndex::build(&store) {
                Ok(idx) => { tantivy_cache = Some(idx); }
                Err(e) => tracing::warn!("Worker: tantivy index build failed: {e}"),
            }
        };
    }

    /// Compute pre-built keyword hits from the cached Tantivy index, or `None`
    /// when the feature is disabled or the index is not yet populated.
    macro_rules! kw_hits {
        ($query:expr, $limit:expr) => {{
            #[cfg(feature = "tantivy-search")]
            {
                tantivy_cache.as_ref().and_then(|idx| {
                    crate::search::search_nodes_indexed(idx, &store, $query, $limit).ok()
                })
            }
            #[cfg(not(feature = "tantivy-search"))]
            {
                let _: (&str, usize) = ($query, $limit);
                None::<Vec<crate::types::GraphNode>>
            }
        }};
    }

    #[cfg(feature = "tantivy-search")]
    rebuild_tantivy!();

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

    // Helper: wrap a closure in catch_unwind, format panic as Err(String).
    macro_rules! catch_panic {
        ($name:expr, $body:expr) => {{
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| { $body }));
            match result {
                Ok(v) => v,
                Err(panic_info) => {
                    let msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                        format!("worker panic in {}: {s}", $name)
                    } else if let Some(s) = panic_info.downcast_ref::<String>() {
                        format!("worker panic in {}: {s}", $name)
                    } else {
                        format!("worker panic in {}: <unknown>", $name)
                    };
                    tracing::error!("{msg}");
                    Err(msg)
                }
            }
        }};
    }

    for cmd in cmd_rx {
        match cmd {
            WorkerCommand::QueryGraph { pattern, target, compact, reply } => {
                let _span = tracing::info_span!("worker_cmd", cmd = "query_graph").entered();
                let result = catch_panic!("query_graph",
                    crate::tools::query_graph_with_store(&store, &root, &pattern, &target, compact)
                        .map_err(|e| e.to_string())
                );
                let _ = reply.send(result);
            }

            WorkerCommand::SemanticSearch { query, kind, limit, compact, reply } => {
                let _span = tracing::info_span!("worker_cmd", cmd = "semantic_search").entered();
                let result = catch_panic!("semantic_search", {
                    let kw_hits = kw_hits!(&query, limit * 2);
                    crate::tools::semantic_search_nodes_with_store(
                        &store, &mut emb_store, &root, &query, kind.as_deref(), limit, compact, kw_hits,
                    ).map_err(|e| e.to_string())
                });
                let _ = reply.send(result);
            }

            WorkerCommand::OpenNodeContext { target, compact, reply } => {
                let _span = tracing::info_span!("worker_cmd", cmd = "open_node_context").entered();
                let result = catch_panic!("open_node_context",
                    crate::tools::open_node_context_with_store(&store, &root, &target, compact)
                        .map_err(|e| e.to_string())
                );
                let _ = reply.send(result);
            }

            WorkerCommand::BatchNodeContext { targets, compact, reply } => {
                let _span = tracing::info_span!("worker_cmd", cmd = "batch_node_context").entered();
                let result = catch_panic!("batch_node_context",
                    crate::tools::batch_open_node_context_with_store(&store, &root, targets, compact)
                        .map_err(|e| e.to_string())
                );
                let _ = reply.send(result);
            }

            WorkerCommand::ImpactRadius { changed_files, max_depth, compact, base, reply } => {
                let _span = tracing::info_span!("worker_cmd", cmd = "impact_radius").entered();
                let result = catch_panic!("impact_radius", {
                    let files = crate::tools::resolve_changed_files(changed_files, &root, &base);
                    crate::tools::get_impact_radius_with_store(&store, &root, files, max_depth, compact)
                        .map_err(|e| e.to_string())
                });
                let _ = reply.send(result);
            }

            WorkerCommand::TraceCallChain { from, to, max_depth, compact, reply } => {
                let _span = tracing::info_span!("worker_cmd", cmd = "trace_call_chain").entered();
                let result = catch_panic!("trace_call_chain",
                    crate::tools::trace_call_chain_with_store(&store, &root, &from, &to, max_depth, compact)
                        .map_err(|e| e.to_string())
                );
                let _ = reply.send(result);
            }

            WorkerCommand::HybridQuery { query, limit, compact, fusion, route, debug, result_mode, budget, reply } => {
                let _span = tracing::info_span!("worker_cmd", cmd = "hybrid_query").entered();
                let result = catch_panic!("hybrid_query", {
                    let kw_hits = kw_hits!(&query, limit * 2);
                    crate::tools::hybrid_query_with_store(
                        &store, &mut emb_store, &root, &query, limit, compact, fusion.as_deref(), kw_hits, route.as_deref(), debug, result_mode.as_deref(), None, budget.as_deref(),
                    ).map_err(|e| e.to_string())
                });
                let _ = reply.send(result);
            }

            WorkerCommand::ListStats { reply } => {
                let _span = tracing::info_span!("worker_cmd", cmd = "list_stats").entered();
                let result = catch_panic!("list_stats",
                    crate::tools::list_graph_stats_with_store(&store, &root)
                        .map_err(|e| e.to_string())
                );
                let _ = reply.send(result);
            }

            WorkerCommand::FindLargeFunctions { min_lines, kind, file_path_pattern, limit, compact, reply } => {
                let _span = tracing::info_span!("worker_cmd", cmd = "find_large_functions").entered();
                let result = catch_panic!("find_large_functions",
                    crate::tools::find_large_functions_with_store(
                        &store, &root, min_lines, kind.as_deref(), file_path_pattern.as_deref(), limit, compact,
                    ).map_err(|e| e.to_string())
                );
                let _ = reply.send(result);
            }

            WorkerCommand::GetReviewContext { changed_files, max_depth, include_source, max_lines, compact, base, reply } => {
                let _span = tracing::info_span!("worker_cmd", cmd = "get_review_context").entered();
                let result = catch_panic!("get_review_context", {
                    let files = crate::tools::resolve_changed_files(changed_files, &root, &base);
                    crate::tools::get_review_context_with_store(
                        &store, &root, files, max_depth, include_source, max_lines, compact,
                    ).map_err(|e| e.to_string())
                });
                let _ = reply.send(result);
            }

            WorkerCommand::BuildGraph { full_rebuild, base, reply } => {
                let _span = tracing::info_span!("worker_cmd", cmd = "build_graph").entered();
                let result = catch_panic!("build_graph", {
                    let root_str = root.as_str().to_owned();
                    let r = crate::tools::build_or_update_graph(full_rebuild, Some(&root_str), &base)
                        .map_err(|e| e.to_string());
                    reload_store!();
                    #[cfg(feature = "tantivy-search")]
                    rebuild_tantivy!();
                    r
                });
                let _ = reply.send(result);
            }

            WorkerCommand::EmbedGraph { reply } => {
                let _span = tracing::info_span!("worker_cmd", cmd = "embed_graph").entered();
                let result = catch_panic!("embed_graph", {
                    let root_str = root.as_str().to_owned();
                    let r = crate::tools::embed_graph(Some(&root_str))
                        .map_err(|e| e.to_string());
                    reload_emb!();
                    r
                });
                let _ = reply.send(result);
            }

            WorkerCommand::WatcherUpdate { paths } => {
                let _span = tracing::info_span!("worker_cmd", cmd = "watcher_update").entered();
                let watcher_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
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
                                // Skip dependents that match ignore patterns (e.g. target/**).
                                let dep_rel = dep_path
                                    .strip_prefix(root.as_str())
                                    .unwrap_or(dep_path)
                                    .trim_start_matches('/');
                                if crate::incremental::should_ignore_pub(dep_rel, &ignore_patterns) {
                                    tracing::debug!("Worker skipped ignored dependent: {dep_rel}");
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

                // Evict tree cache when it grows too large to bound memory.
                if evict_if_over(&mut tree_cache, MAX_TREE_CACHE) {
                    tracing::info!("tree_cache evicted (exceeded {MAX_TREE_CACHE} entries)");
                }

                if let Err(e) = store.commit() {
                    tracing::error!("Worker commit error: {e}");
                }
                // Periodic GC to prevent unbounded vector accumulation.
                if let Err(e) = emb_store.maybe_gc(&store) {
                    tracing::warn!("Worker embedding GC: {e}");
                }
                #[cfg(feature = "tantivy-search")]
                rebuild_tantivy!();
                }));
                if let Err(e) = watcher_result {
                    tracing::error!("worker panic in watcher_update: {:?}", e);
                }
            }

            WorkerCommand::WatcherRemove { paths } => {
                let _span = tracing::info_span!("worker_cmd", cmd = "watcher_remove").entered();
                let watcher_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
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
                #[cfg(feature = "tantivy-search")]
                rebuild_tantivy!();
                }));
                if let Err(e) = watcher_result {
                    tracing::error!("worker panic in watcher_remove: {:?}", e);
                }
            }

            WorkerCommand::ReloadStores => {
                let _span = tracing::info_span!("worker_cmd", cmd = "reload_stores").entered();
                tracing::info!("External store change detected — reloading graph + embeddings from disk");
                reload_store!();
                reload_emb!();
                #[cfg(feature = "tantivy-search")]
                rebuild_tantivy!();
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
    /// Controls which tools are exposed via `list_tools`.
    /// When `false`, only the core 3 tools are listed (default for MCP `serve`).
    /// When `true`, all available tools are listed (`--tools all`, or via
    /// `CodeReviewServer::new()` for library/test backward compatibility).
    expose_all_tools: bool,
}

impl std::fmt::Debug for CodeReviewServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodeReviewServer")
            .field("repo_root", &self.repo_root)
            .field("expose_all_tools", &self.expose_all_tools)
            .finish_non_exhaustive()
    }
}

impl CodeReviewServer {
    fn new_inner(
        repo_root: Option<String>,
        worker_tx: Option<std::sync::mpsc::Sender<WorkerCommand>>,
        expose_all_tools: bool,
    ) -> Self {
        Self {
            repo_root: repo_root.map(Arc::new),
            tool_router: Self::tool_router(),
            worker_tx,
            expose_all_tools,
        }
    }

    /// CLI / test constructor — no worker thread, falls back to spawn_blocking.
    /// Defaults to expose_all_tools=true for backward compat with non-MCP callers.
    pub fn new(repo_root: Option<String>) -> Self {
        Self::new_inner(repo_root, None, true)
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
                route: p.route,
                debug: p.debug,
                result_mode: p.result_mode,
                budget: p.budget,
                reply,
            }).await
        } else {
            self.spawn_blocking_fallback(move || {
                crate::tools::hybrid_query(&p.query, p.limit, repo_root.as_deref(), p.compact, p.fusion.as_deref(), p.route.as_deref(), p.debug, p.result_mode.as_deref(), p.budget.as_deref())
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

const CORE_TOOLS: &[&str] = &["hybrid_query", "open_node_context", "query_graph"];

impl ServerHandler for CodeReviewServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .build(),
        )
        .with_instructions(
            "Persistent incremental knowledge graph for token-efficient code reviews.\n\n\
             WHEN TO USE GRAPH TOOLS vs GREP:\n\
             - Grep/Read: exact filename, symbol name, or string literal lookups\n\
             - Graph tools: structural queries (who calls X?), semantic/conceptual search, cross-file tracing\n\n\
             RECOMMENDED WORKFLOW:\n\
             1. hybrid_query -- best first call for broad discovery\n\
             2. open_node_context -- after finding a function, get source + callers + callees in one call\n\
             3. query_graph(callers_of/callees_of) -- follow specific structural edges\n\
             Then switch to Read/Grep for detailed code analysis.",
        )
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, rmcp::ErrorData> {
        let all = self.tool_router.list_all();
        let tools = if self.expose_all_tools {
            all
        } else {
            all.into_iter()
                .filter(|t| CORE_TOOLS.contains(&t.name.as_ref()))
                .collect()
        };
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let tcc = ToolCallContext::new(self, request, context);
        self.tool_router.call(tcc).await
    }

    fn get_tool(&self, name: &str) -> Option<rmcp::model::Tool> {
        self.tool_router.get(name).cloned()
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
        let mut store_changed = false;

        for event in events {
            let path = event.path;
            if path.is_symlink() {
                continue;
            }
            let rel = match path.strip_prefix(repo_root.as_std_path()) {
                Ok(r) => r.to_string_lossy().replace('\\', "/"),
                Err(_) => continue,
            };

            // Detect external changes to graph/embedding store files.
            if rel == ".code-review-graph/graph.bin.zst"
                || rel == ".code-review-graph/embeddings.bin.zst"
            {
                store_changed = true;
                continue;
            }

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

        // Trigger store reload if graph/embedding files changed externally (e.g. CLI `build`).
        if store_changed {
            if cmd_tx.send(WorkerCommand::ReloadStores).is_err() {
                tracing::warn!("Watcher: worker channel closed, stopping");
                break;
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

// ---------------------------------------------------------------------------
// Per-repo instance lock — prevents multiple MCP servers for the same repo.
// ---------------------------------------------------------------------------

/// RAII guard that holds an exclusive file lock. Released on drop.
struct InstanceLock {
    _file: std::fs::File,
    path: std::path::PathBuf,
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        // File close releases the OS lock automatically.
        // Clean up the lock file (best-effort).
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Try to acquire an exclusive, non-blocking file lock.
/// Returns `true` on success, `false` if another process holds the lock.
#[cfg(windows)]
fn try_exclusive_lock(file: &std::fs::File) -> bool {
    use std::os::windows::io::AsRawHandle;
    // LockFileEx with LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY.
    // We call it via raw FFI to avoid pulling in windows-sys as a dependency.
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LockFileEx(
            hFile: *mut std::ffi::c_void,
            dwFlags: u32,
            dwReserved: u32,
            nNumberOfBytesToLockLow: u32,
            nNumberOfBytesToLockHigh: u32,
            lpOverlapped: *mut [u8; 32], // OVERLAPPED is 32 bytes on x64
        ) -> i32;
    }
    let mut overlapped = [0u8; 32];
    const LOCKFILE_EXCLUSIVE_LOCK: u32 = 0x00000002;
    const LOCKFILE_FAIL_IMMEDIATELY: u32 = 0x00000001;
    unsafe {
        LockFileEx(
            file.as_raw_handle() as *mut _,
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            1,
            0,
            &mut overlapped,
        ) != 0
    }
}

#[cfg(unix)]
fn try_exclusive_lock(file: &std::fs::File) -> bool {
    use std::os::unix::io::AsRawFd;
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

/// Try to acquire an exclusive lock for this repo root.
/// Returns `Ok(guard)` if we're the only instance, or `Err` if another is running.
fn acquire_instance_lock(root: &camino::Utf8Path) -> crate::error::Result<InstanceLock> {
    use std::io::Write;

    // Store locks in a central cache dir to avoid polluting repo roots.
    let cache_dir = dirs::cache_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("code-review-graph")
        .join("locks");
    std::fs::create_dir_all(&cache_dir)
        .map_err(|e| crate::error::CrgError::Other(format!("create lock dir: {e}")))?;

    // Hash the repo root to get a stable, filesystem-safe lock name.
    let hash = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        root.as_str().hash(&mut h);
        format!("{:016x}", h.finish())
    };
    let lock_path = cache_dir.join(format!("{hash}.lock"));

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&lock_path)
        .map_err(|e| crate::error::CrgError::Other(format!("open lock file: {e}")))?;

    // Platform-specific exclusive lock (non-blocking).
    if !try_exclusive_lock(&file) {
        return Err(crate::error::CrgError::Other(format!(
            "Another code-review-graph instance is already running for {}",
            root
        )));
    }

    // Write PID for debugging.
    let _ = write!(file, "{}", std::process::id());
    let _ = file.flush();

    tracing::info!("Instance lock acquired: {}", lock_path.display());
    Ok(InstanceLock { _file: file, path: lock_path })
}

/// Default HTTP port for shared MCP access. First instance binds this port
/// and serves HTTP; subsequent instances proxy stdio through it.
const DEFAULT_MCP_HTTP_PORT: u16 = 7432;

/// Run the MCP server. Automatically selects primary or proxy mode:
///
/// - **Primary** (first instance): binds HTTP port, spawns worker + watcher,
///   serves stdio to this session + HTTP to proxy instances.
/// - **Proxy** (port already taken): lightweight forwarder, no worker/GPU.
///   Reads stdio from Claude Code, forwards to primary via HTTP.
///
/// If the primary dies, a proxy promotes itself to primary automatically.
pub async fn run_server(repo_root: Option<String>, tool_mode: &str, port: Option<u16>) -> crate::error::Result<()> {
    let http_port = port.unwrap_or(DEFAULT_MCP_HTTP_PORT);

    // Try to bind the HTTP port — determines primary vs proxy mode.
    match tokio::net::TcpListener::bind(("127.0.0.1", http_port)).await {
        Ok(listener) => {
            tracing::info!("Primary mode — bound port {http_port}");
            run_primary_server(repo_root, tool_mode, listener).await
        }
        Err(_) => {
            tracing::info!("Port {http_port} in use — proxy mode");
            run_proxy_server(http_port, repo_root, tool_mode).await
        }
    }
}

/// Primary server: owns worker thread, watcher, GPU. Serves stdio + HTTP.
async fn run_primary_server(
    repo_root: Option<String>,
    tool_mode: &str,
    listener: tokio::net::TcpListener,
) -> crate::error::Result<()> {
    let root = resolve_root(repo_root.as_deref());

    // Acquire per-repo instance lock.
    let _lock = if root.exists() {
        match acquire_instance_lock(&root) {
            Ok(lock) => Some(lock),
            Err(e) => {
                tracing::warn!("{e} — proceeding without lock");
                None
            }
        }
    } else {
        None
    };

    // Spawn the worker thread and watcher only when we have a usable root.
    let worker_tx = if root.exists() {
        let (tx, rx) = std::sync::mpsc::channel::<WorkerCommand>();

        let worker_root = root.clone();
        std::thread::spawn(move || run_worker_thread(worker_root, rx));

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

    let expose_all = tool_mode == "all";
    let server = CodeReviewServer::new_inner(repo_root.clone(), worker_tx.clone(), expose_all);

    // HTTP task: serves proxy instances on the bound port.
    let ct = tokio_util::sync::CancellationToken::new();
    let http_ct = ct.clone();
    let http_server = server.clone();
    let http_handle = tokio::spawn(async move {
        use rmcp::transport::streamable_http_server::{
            StreamableHttpServerConfig,
            StreamableHttpService,
            session::local::LocalSessionManager,
        };

        let service = StreamableHttpService::new(
            {
                let s = http_server;
                move || Ok(s.clone())
            },
            LocalSessionManager::default().into(),
            StreamableHttpServerConfig::default()
                .with_cancellation_token(http_ct.child_token()),
        );

        let addr = listener.local_addr().unwrap();
        tracing::info!("MCP HTTP server listening on http://{addr}/mcp");
        let router = axum::Router::new().nest_service("/mcp", service);
        if let Err(e) = axum::serve(listener, router)
            .with_graceful_shutdown(async move { http_ct.cancelled().await })
            .await
        {
            tracing::error!("HTTP server error: {e}");
        }
    });

    // Stdio task: serves this session's Claude Code.
    let (stdin, stdout) = transport::io::stdio();
    serve_server(server, (stdin, stdout))
        .await
        .map_err(|e| crate::error::CrgError::Other(e.to_string()))?
        .waiting()
        .await
        .map_err(|e| crate::error::CrgError::Other(e.to_string()))?;

    // Stdio ended (this session closed). Shut down HTTP so port is freed.
    ct.cancel();
    let _ = http_handle.await;

    Ok(())
}

/// Proxy server: no worker, no GPU. Forwards stdio ↔ HTTP to the primary.
/// If the primary dies, attempts to promote itself.
async fn run_proxy_server(
    port: u16,
    repo_root: Option<String>,
    tool_mode: &str,
) -> crate::error::Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{port}/mcp");
    let mut session_id: Option<String> = None;

    let stdin = BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();
    let mut lines = stdin.lines();
    let mut pending_line: Option<String> = None;

    tracing::info!("Proxy ready — waiting for stdio input");

    loop {
        // Use buffered request from failover, or read next from stdin.
        let line = if let Some(buffered) = pending_line.take() {
            tracing::debug!("Replaying buffered request");
            buffered
        } else {
            match lines.next_line().await {
                Ok(Some(l)) => l,
                Ok(None) => { tracing::info!("Proxy: stdin closed"); break; }
                Err(e) => { tracing::error!("Proxy: stdin error: {e}"); break; }
            }
        };

        if line.trim().is_empty() {
            continue;
        }

        tracing::debug!("Proxy forwarding: {}...", &line[..line.len().min(80)]);

        let mut req = client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .body(line.clone());
        if let Some(ref sid) = session_id {
            req = req.header("mcp-session-id", sid);
        }

        match req.send().await {
            Ok(resp) => {
                if let Some(sid) = resp.headers().get("mcp-session-id") {
                    if let Ok(s) = sid.to_str() {
                        session_id = Some(s.to_string());
                    }
                }

                let is_sse = resp.headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .map_or(false, |ct| ct.contains("text/event-stream"));

                if is_sse {
                    // SSE: read full body, extract "data:" lines, relay each as a stdio line.
                    // The SSE stream ends after the server sends all events for this request.
                    let body = resp.text().await.unwrap_or_default();
                    for sse_line in body.lines() {
                        if let Some(data) = sse_line.strip_prefix("data:") {
                            let data = data.trim();
                            if !data.is_empty() {
                                let _ = stdout.write_all(data.as_bytes()).await;
                                let _ = stdout.write_all(b"\n").await;
                                let _ = stdout.flush().await;
                            }
                        }
                    }
                } else {
                    // Plain JSON response — relay as-is.
                    let body = resp.text().await.unwrap_or_default();
                    let _ = stdout.write_all(body.as_bytes()).await;
                    let _ = stdout.write_all(b"\n").await;
                    let _ = stdout.flush().await;
                }
            }
            Err(e) if e.is_connect() => {
                // Primary died. Buffer the failed request for replay.
                pending_line = Some(line);
                tracing::warn!("Primary disconnected — attempting promotion");
                match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
                    Ok(listener) => {
                        tracing::info!("Promoted to primary on port {port}");
                        return run_primary_server(repo_root, tool_mode, listener).await;
                    }
                    Err(_) => {
                        // Another proxy beat us — retry as proxy with new session.
                        tracing::info!("Another instance became primary — retrying");
                        session_id = None;
                        // pending_line still set — will be replayed next iteration.
                        continue;
                    }
                }
            }
            Err(e) => {
                tracing::error!("Proxy request failed: {e}");
                let err_resp = serde_json::json!({
                    "jsonrpc": "2.0",
                    "error": {"code": -32603, "message": format!("proxy: {e}")},
                    "id": null
                });
                let _ = stdout.write_all(err_resp.to_string().as_bytes()).await;
                let _ = stdout.write_all(b"\n").await;
                let _ = stdout.flush().await;
            }
        }
    }
    Ok(())
}

/// Evict all entries from a cache map when it exceeds `max_size`.
///
/// Extracted for testability — the worker thread applies this to `tree_cache`.
fn evict_if_over<K, V>(cache: &mut HashMap<K, V>, max_size: usize) -> bool {
    if cache.len() > max_size {
        cache.clear();
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_cache_eviction_clears_at_limit() {
        let mut cache: HashMap<String, String> = HashMap::new();

        // Fill to exactly MAX_TREE_CACHE — should NOT evict.
        for i in 0..2_000 {
            cache.insert(format!("file_{i}"), format!("tree_{i}"));
        }
        assert!(!evict_if_over(&mut cache, 2_000));
        assert_eq!(cache.len(), 2_000);

        // One more entry pushes it over — should evict.
        cache.insert("file_2000".into(), "tree_2000".into());
        assert!(evict_if_over(&mut cache, 2_000));
        assert_eq!(cache.len(), 0, "cache must be empty after eviction");
    }

    #[test]
    fn tree_cache_eviction_allows_regrowth() {
        let mut cache: HashMap<String, String> = HashMap::new();

        // Simulate 5 eviction cycles.
        for cycle in 0..5u32 {
            for i in 0..2_001 {
                cache.insert(format!("c{cycle}_f{i}"), format!("t{i}"));
            }
            assert!(evict_if_over(&mut cache, 2_000));
            assert_eq!(cache.len(), 0, "cycle {cycle}: cache must clear");
        }
    }

    #[test]
    fn tree_cache_small_repo_never_evicts() {
        let mut cache: HashMap<String, String> = HashMap::new();

        // A small repo with 500 files — should never trigger eviction.
        for i in 0..500 {
            cache.insert(format!("file_{i}"), format!("tree_{i}"));
            assert!(!evict_if_over(&mut cache, 2_000));
        }
        assert_eq!(cache.len(), 500, "small cache should be untouched");
    }
}
