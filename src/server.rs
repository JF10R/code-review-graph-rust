//! MCP server wiring via rmcp.
//!
//! Registers all 9 tools and runs the server over stdio transport.
//! All tool handlers delegate to `tokio::task::spawn_blocking` to avoid
//! blocking the async event loop during heavy operations (SQLite writes,
//! tree-sitter parsing, embedding).
//!
//! When started via `code-review-graph serve`, a background OS thread watches
//! the repository for file changes and incrementally updates the graph stored
//! on disk. Tool handlers continue to open the store per-call (Option B), so
//! they always read the latest on-disk snapshot produced by the watcher.

use rmcp::{
    ServerHandler,
    handler::server::router::tool::ToolRouter,
    handler::server::wrapper::Parameters,
    model::{ServerCapabilities, ServerInfo},
    schemars, serve_server, tool, tool_handler, tool_router,
    transport,
};
use serde::Deserialize;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
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
    #[schemars(description = "If true, return slim node objects. Reduces response tokens ~40%.")]
    #[serde(default)]
    compact: bool,
    #[schemars(description = "Repository root path. Auto-detected if omitted.")]
    #[serde(default)]
    repo_root: Option<String>,
}

// Default value helpers
fn default_base() -> String { "HEAD~1".to_string() }
fn default_max_depth() -> usize { 2 }
fn default_true() -> bool { true }
fn default_max_lines() -> usize { 200 }
fn default_search_limit() -> usize { 20 }
fn default_min_lines() -> usize { 50 }
fn default_large_limit() -> usize { 50 }
fn default_chain_depth() -> usize { 10 }

// ---------------------------------------------------------------------------
// MCP server struct
// ---------------------------------------------------------------------------

/// The MCP server for code-review-graph. Holds the optional default repo root
/// and the generated tool router.
#[derive(Clone)]
pub struct CodeReviewServer {
    /// Default repo root passed via the CLI `--repo` flag.
    repo_root: Option<Arc<String>>,
    tool_router: ToolRouter<Self>,
}

impl std::fmt::Debug for CodeReviewServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodeReviewServer")
            .field("repo_root", &self.repo_root)
            .finish_non_exhaustive()
    }
}

impl CodeReviewServer {
    pub fn new(repo_root: Option<String>) -> Self {
        Self {
            repo_root: repo_root.map(Arc::new),
            tool_router: Self::tool_router(),
        }
    }

    /// Resolve the effective repo root: prefer the per-call value, fall back to
    /// the server-level default set via `--repo`.
    fn resolve_repo_root(&self, per_call: Option<String>) -> Option<String> {
        per_call.or_else(|| self.repo_root.as_deref().map(|s| s.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Tool implementations (thin async wrappers → spawn_blocking → sync tools)
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
        tokio::task::spawn_blocking(move || {
            crate::tools::build_or_update_graph(
                p.full_rebuild,
                repo_root.as_deref(),
                &p.base,
            )
            .map(|v| v.to_string())
            .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    /// Analyze the blast radius of changed files in the codebase.
    ///
    /// Shows which functions, classes, and files are impacted by changes.
    /// Auto-detects changed files from git if not specified.
    #[tool(name = "get_impact_radius")]
    async fn get_impact_radius_tool(
        &self,
        Parameters(p): Parameters<ImpactRadiusParams>,
    ) -> std::result::Result<String, String> {
        let repo_root = self.resolve_repo_root(p.repo_root);
        tokio::task::spawn_blocking(move || {
            crate::tools::get_impact_radius(
                p.changed_files,
                p.max_depth,
                repo_root.as_deref(),
                &p.base,
            )
            .map(|v| v.to_string())
            .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    /// Run a predefined graph query to explore code relationships.
    ///
    /// Available patterns: callers_of, callees_of, imports_of, importers_of,
    /// children_of, tests_for, inheritors_of, file_summary.
    #[tool(name = "query_graph")]
    async fn query_graph_tool(
        &self,
        Parameters(p): Parameters<QueryGraphParams>,
    ) -> std::result::Result<String, String> {
        let repo_root = self.resolve_repo_root(p.repo_root);
        tokio::task::spawn_blocking(move || {
            crate::tools::query_graph(
                &p.pattern,
                &p.target,
                repo_root.as_deref(),
            )
            .map(|v| v.to_string())
            .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
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
        tokio::task::spawn_blocking(move || {
            crate::tools::get_review_context(
                p.changed_files,
                p.max_depth,
                p.include_source,
                p.max_lines_per_file,
                repo_root.as_deref(),
                &p.base,
            )
            .map(|v| v.to_string())
            .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    /// Search for code entities by name, keyword, or semantic similarity.
    ///
    /// Uses vector embeddings for semantic search when available (run
    /// embed_graph_tool first). Falls back to keyword matching otherwise.
    #[tool(name = "semantic_search_nodes")]
    async fn semantic_search_nodes_tool(
        &self,
        Parameters(p): Parameters<SemanticSearchParams>,
    ) -> std::result::Result<String, String> {
        let repo_root = self.resolve_repo_root(p.repo_root);
        tokio::task::spawn_blocking(move || {
            crate::tools::semantic_search_nodes(
                &p.query,
                p.kind.as_deref(),
                p.limit,
                repo_root.as_deref(),
            )
            .map(|v| v.to_string())
            .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
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
        tokio::task::spawn_blocking(move || {
            crate::tools::list_graph_stats(repo_root.as_deref())
                .map(|v| v.to_string())
                .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    /// Compute vector embeddings for all graph nodes to enable semantic search.
    ///
    /// Uses the all-MiniLM-L6-v2 model (384-dim vectors).
    /// Only computes embeddings for nodes that don't already have them.
    /// After running this, semantic_search_nodes_tool uses vector similarity.
    #[tool(name = "embed_graph")]
    async fn embed_graph_tool(
        &self,
        Parameters(p): Parameters<EmbedParams>,
    ) -> std::result::Result<String, String> {
        let repo_root = self.resolve_repo_root(p.repo_root);
        tokio::task::spawn_blocking(move || {
            crate::tools::embed_graph(repo_root.as_deref())
                .map(|v| v.to_string())
                .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
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
        let repo_root = self.resolve_repo_root(None);
        tokio::task::spawn_blocking(move || {
            crate::tools::get_docs_section(&p.section_name, repo_root.as_deref())
                .map(|v| v.to_string())
                .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    /// Find functions, classes, or files exceeding a line-count threshold.
    ///
    /// Useful for decomposition audits, code quality checks, and enforcing
    /// size limits during code review. Results are ordered by line count.
    #[tool(name = "find_large_functions")]
    async fn find_large_functions_tool(
        &self,
        Parameters(p): Parameters<LargeFunctionsParams>,
    ) -> std::result::Result<String, String> {
        let repo_root = self.resolve_repo_root(p.repo_root);
        tokio::task::spawn_blocking(move || {
            crate::tools::find_large_functions(
                p.min_lines,
                p.kind.as_deref(),
                p.file_path_pattern.as_deref(),
                p.limit,
                repo_root.as_deref(),
            )
            .map(|v| v.to_string())
            .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    /// Find the shortest call chain between two functions.
    /// Traverses CALLS edges to show how function A connects to function B
    /// through intermediate calls. Useful for understanding data flow,
    /// tracing bug propagation, and mapping dependency chains.
    /// Try outgoing (callee) direction first, then incoming (caller) direction.
    #[tool(name = "trace_call_chain")]
    async fn trace_call_chain_tool(
        &self,
        Parameters(p): Parameters<TraceCallChainParams>,
    ) -> std::result::Result<String, String> {
        let repo_root = self.resolve_repo_root(p.repo_root);
        tokio::task::spawn_blocking(move || {
            crate::tools::trace_call_chain(
                &p.from,
                &p.to,
                p.max_depth,
                p.compact,
                repo_root.as_deref(),
            )
            .map(|v| v.to_string())
            .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
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
            "Persistent incremental knowledge graph for token-efficient, \
             context-aware code reviews. Parses your codebase with Tree-sitter, \
             builds a structural graph, and provides smart impact analysis.",
        )
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Resolve a server-level repo root string to a `PathBuf`, or fall back to
/// the project root auto-detection logic used by `tools.rs`.
fn resolve_root(repo_root: Option<&str>) -> PathBuf {
    match repo_root {
        Some(p) => PathBuf::from(p),
        None => crate::incremental::find_project_root(None),
    }
}

// ---------------------------------------------------------------------------
// Background file watcher (Option B: saves to disk, tools reload per call)
// ---------------------------------------------------------------------------

/// Parse `path` from disk and store its nodes/edges into `store`.
/// Returns `(node_count, edge_count)` on success, or an error string on failure.
fn watcher_parse_and_store(
    parser: &crate::parser::CodeParser,
    store: &mut crate::graph::GraphStore,
    path: &std::path::Path,
) -> Result<(usize, usize), String> {
    use crate::incremental::{is_binary_pub, sha256_bytes_pub};
    if is_binary_pub(path) {
        return Err(format!("{}: binary file skipped", path.display()));
    }
    let source = std::fs::read(path).map_err(|e| format!("{}: {}", path.display(), e))?;
    let fhash = sha256_bytes_pub(&source);
    let abs_str = path.to_string_lossy();
    let (nodes, edges) = parser
        .parse_bytes(path, &source)
        .map_err(|e| format!("{}: {}", path.display(), e))?;
    let n = nodes.len();
    let e = edges.len();
    store
        .store_file_nodes_edges(&abs_str, &nodes, &edges, &fhash)
        .map_err(|e| format!("{}: {}", path.display(), e))?;
    Ok((n, e))
}

/// Spawn a background OS thread that watches `repo_root` for source-file
/// changes and incrementally updates `graph.bin.zst` on disk.
///
/// Uses the same notify debouncer logic as `incremental::watch()` but runs
/// independently of any `Arc<Mutex<GraphStore>>` — it opens a fresh store,
/// processes the batch, saves to disk, then drops the store. This keeps the
/// locking surface minimal and is safe with the per-call `get_store()` pattern
/// used by the tool handlers.
fn run_background_watcher(repo_root: PathBuf) -> crate::error::Result<()> {
    use notify::RecursiveMode;
    use notify_debouncer_mini::{new_debouncer, DebounceEventResult};
    use crate::incremental::{get_db_path, load_ignore_patterns_pub, find_dependents};
    use crate::graph::GraphStore;
    use crate::parser::CodeParser;

    let ignore_patterns = load_ignore_patterns_pub(&repo_root);
    let parser = CodeParser::new();

    let (tx, rx) = std::sync::mpsc::channel::<DebounceEventResult>();
    let mut debouncer = new_debouncer(Duration::from_millis(300), tx)
        .map_err(|e| crate::error::CrgError::Other(e.to_string()))?;
    debouncer
        .watcher()
        .watch(&repo_root, RecursiveMode::Recursive)
        .map_err(|e| crate::error::CrgError::Other(e.to_string()))?;

    log::info!(
        "Background watcher active — watching {}",
        repo_root.display()
    );

    for result in rx {
        let events = match result {
            Ok(evts) => evts,
            Err(e) => {
                log::error!("Watcher error: {:?}", e);
                continue;
            }
        };

        let mut paths_to_update: HashSet<PathBuf> = HashSet::new();
        let mut paths_to_remove: HashSet<PathBuf> = HashSet::new();

        for event in events {
            let path = event.path;
            if path.is_symlink() {
                continue;
            }
            let rel = match path.strip_prefix(&repo_root) {
                Ok(r) => r.to_string_lossy().replace('\\', "/"),
                Err(_) => continue,
            };
            if crate::incremental::should_ignore_pub(&rel, &ignore_patterns) {
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

        // Open the store once per batch, apply all changes, save, close.
        let db_path = get_db_path(&repo_root);
        let mut store = match GraphStore::new(&db_path) {
            Ok(s) => s,
            Err(e) => {
                log::error!("Background watcher: could not open store: {}", e);
                continue;
            }
        };

        for path in &paths_to_remove {
            let abs_str = path.to_string_lossy().into_owned();
            if let Err(e) = store.remove_file_data(&abs_str) {
                log::error!("Watcher remove {}: {}", abs_str, e);
            } else {
                let rel = path
                    .strip_prefix(&repo_root)
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| abs_str.clone());
                log::info!("Watcher removed: {}", rel);
            }
        }

        // Track processed paths to guard against circular import cycles.
        let mut processed: HashSet<String> = paths_to_update
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();

        for path in &paths_to_update {
            let abs_str = path.to_string_lossy().into_owned();
            match watcher_parse_and_store(&parser, &mut store, path) {
                Ok((n, e)) => {
                    let _ = store.set_metadata(
                        "last_updated",
                        &chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S").to_string(),
                    );
                    let rel = path
                        .strip_prefix(&repo_root)
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|_| abs_str.clone());
                    log::info!("Watcher updated: {} ({} nodes, {} edges)", rel, n, e);

                    // Re-parse dependents so cross-file edges stay fresh.
                    let deps = find_dependents(&store, &abs_str).unwrap_or_default();
                    for dep_path in &deps {
                        if processed.contains(dep_path.as_str()) {
                            continue;
                        }
                        processed.insert(dep_path.clone());
                        let dep = std::path::PathBuf::from(dep_path);
                        match watcher_parse_and_store(&parser, &mut store, &dep) {
                            Ok((dn, de)) => log::debug!(
                                "Watcher re-parsed dependent: {} ({} nodes, {} edges)",
                                dep_path, dn, de
                            ),
                            Err(e) => log::warn!("Watcher dependent {}: {}", dep_path, e),
                        }
                    }
                }
                Err(err) => log::error!("Watcher {}", err),
            }
        }

        if let Err(e) = store.commit() {
            log::error!("Watcher commit error: {}", e);
        }
    }

    log::info!("Background watcher stopped.");
    Ok(())
}

/// Run the MCP server over stdio. Blocks until the client disconnects.
pub async fn run_server(repo_root: Option<String>) -> crate::error::Result<()> {
    // Resolve the repository root once so we can start the background watcher.
    let root = resolve_root(repo_root.as_deref());

    // Only start the watcher when we have a usable root directory.
    if root.exists() {
        let watcher_root = root.clone();
        std::thread::spawn(move || {
            if let Err(e) = run_background_watcher(watcher_root) {
                log::error!("Background watcher error: {}", e);
            }
        });
    } else {
        log::info!("No repo root detected, background watcher disabled");
    }

    let server = CodeReviewServer::new(repo_root);
    let (stdin, stdout) = transport::io::stdio();
    serve_server(server, (stdin, stdout))
        .await
        .map_err(|e| crate::error::CrgError::Other(e.to_string()))?
        .waiting()
        .await
        .map_err(|e| crate::error::CrgError::Other(e.to_string()))?;
    Ok(())
}
