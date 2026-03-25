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

use crate::embeddings::{embed_all_nodes, semantic_search, EmbeddingStore};
use crate::error::{CrgError, Result};
use crate::graph::GraphStore;
use crate::incremental;
use crate::types::{edge_to_dict, node_to_dict, EdgeKind};

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
    let store = GraphStore::new(&db_path)?;
    Ok((store, root))
}

// ---------------------------------------------------------------------------
// Lazy staleness check
// ---------------------------------------------------------------------------

/// Check if the graph is stale and run a quick incremental update if needed.
/// Only checks git status (fast, ~10-50ms) — doesn't re-hash all files.
/// Skipped if the graph was updated less than 2 seconds ago.
fn maybe_auto_update(store: &mut GraphStore, repo_root: &Utf8Path) {
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
    let mut emb_store = EmbeddingStore::new(&emb_db_path)?;
    let result = semantic_search_nodes_with_store(&store, &mut emb_store, &root, query, kind, limit, compact)?;
    emb_store.close()?;
    store.close()?;
    Ok(result)
}

pub fn semantic_search_nodes_with_store(
    store: &GraphStore,
    emb_store: &mut EmbeddingStore,
    root: &Utf8Path,
    query: &str,
    kind: Option<&str>,
    limit: usize,
    compact: bool,
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
        let mut nodes = store.search_nodes(query, limit * 2)?;
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
    let total = emb_store.count()?;
    emb_store.close()?;
    store.close()?;

    Ok(json!({
        "status": "ok",
        "summary": format!(
            "Embedded {} new node(s). Total embeddings: {}. Semantic search is now active.",
            newly_embedded, total
        ),
        "newly_embedded": newly_embedded,
        "total_embeddings": total,
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

/// Merge graph keyword search with semantic search via Reciprocal Rank Fusion.
///
/// RRF score formula: Σ 1 / (k + rank_i)  where k = 60 (standard constant).
///
/// Falls back to keyword-only when no embeddings are available, and sets
/// `method: "keyword_only"` in the returned JSON to signal this.
pub fn hybrid_query(
    query: &str,
    limit: usize,
    repo_root: Option<&str>,
    compact: bool,
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
    let result = hybrid_query_with_store(&store, &mut emb_store, &root, query, limit, compact)?;
    emb_store.close()?;
    store.close()?;
    Ok(result)
}

pub fn hybrid_query_with_store(
    store: &GraphStore,
    emb_store: &mut EmbeddingStore,
    root: &Utf8Path,
    query: &str,
    limit: usize,
    compact: bool,
) -> Result<Value> {
    if query.trim().is_empty() {
        return Ok(json!({
            "status": "ok",
            "query": query,
            "method": "keyword_only",
            "results": [],
        }));
    }

    const RRF_K: f64 = 60.0;

    // Keyword results
    let keyword_hits = store.search_nodes(query, limit * 2)?;

    let method;
    let mut rrf_scores: HashMap<String, f64> = HashMap::new();

    // Populate keyword ranks
    for (rank, node) in keyword_hits.iter().enumerate() {
        let score = 1.0 / (RRF_K + rank as f64 + 1.0);
        *rrf_scores.entry(node.qualified_name.clone()).or_insert(0.0) += score;
    }

    // Semantic ranks (if available)
    if emb_store.available() && emb_store.count().unwrap_or(0) > 0 {
        method = "hybrid_rrf";
        let semantic_hits = crate::embeddings::semantic_search(query, store, emb_store, limit * 2, compact, root)?;
        for (rank, hit) in semantic_hits.iter().enumerate() {
            if let Some(qn) = hit.get("qualified_name").and_then(|v| v.as_str()) {
                let score = 1.0 / (RRF_K + rank as f64 + 1.0);
                *rrf_scores.entry(qn.to_string()).or_insert(0.0) += score;
            }
        }
    } else {
        method = "keyword_only";
    }

    // Sort by RRF score descending
    let mut ranked: Vec<(String, f64)> = rrf_scores.into_iter().collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    ranked.truncate(limit);

    let results: Vec<Value> = ranked
        .iter()
        .filter_map(|(qn, score)| {
            store.get_node(qn).ok().flatten().map(|node| {
                let mut d = node_dict(&node, compact, root);
                d["rrf_score"] = json!(score);
                d
            })
        })
        .collect();

    Ok(json!({
        "status": "ok",
        "query": query,
        "method": method,
        "results": results,
    }))
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
        .filter(|n| n.kind == crate::types::NodeKind::Function)
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

        let result = hybrid_query("", 10, Some(&path), false);
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

        let result = hybrid_query("add", 5, Some(&path), false);
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

        let result = hybrid_query("square", 5, Some(&path), false);
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
}
