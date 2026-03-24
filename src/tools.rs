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

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::embeddings::{embed_all_nodes, semantic_search, EmbeddingStore};
use crate::error::{CrgError, Result};
use crate::graph::GraphStore;
use crate::incremental;
use crate::types::{edge_to_dict, node_to_dict, EdgeKind};

/// Common JS/TS builtin method names filtered from callers_of results.
/// "Who calls .map()?" returns hundreds of hits and is never useful.
static BUILTIN_CALL_NAMES: &[&str] = &[
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
];

fn is_builtin_call(name: &str) -> bool {
    BUILTIN_CALL_NAMES.contains(&name)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Validate that a path is a plausible project root.
fn validate_repo_root(path: &Path) -> Result<PathBuf> {
    let resolved = path.canonicalize().map_err(|e| {
        CrgError::InvalidRepoRoot(format!("{}: {}", path.display(), e))
    })?;
    if !resolved.is_dir() {
        return Err(CrgError::InvalidRepoRoot(format!(
            "not a directory: {}",
            resolved.display()
        )));
    }
    if !resolved.join(".git").exists() && !resolved.join(".code-review-graph").exists() {
        return Err(CrgError::InvalidRepoRoot(format!(
            "no .git or .code-review-graph found: {}",
            resolved.display()
        )));
    }
    Ok(resolved)
}

/// Resolve repo root and open the graph store.
fn get_store(repo_root: Option<&str>) -> Result<(GraphStore, PathBuf)> {
    let root = match repo_root {
        Some(p) => validate_repo_root(Path::new(p))?,
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
fn maybe_auto_update(store: &mut GraphStore, repo_root: &Path) {
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
    // Only update if there are actually changed files not yet in the graph
    let _ = crate::incremental::incremental_update(
        repo_root, store, "HEAD", Some(changed),
    );
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
) -> Result<Value> {
    const MAX_RESULTS: usize = 500;
    let (mut store, root) = get_store(repo_root)?;
    maybe_auto_update(&mut store, &root);

    let files = resolve_changed_files(changed_files, &root, base);

    if files.is_empty() {
        store.close()?;
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
        .map(|f| root.join(f).to_string_lossy().into_owned())
        .collect();

    let impact = store.get_impact_radius(&abs_files, max_depth, MAX_RESULTS, None)?;

    let changed_dicts: Vec<Value> = impact.changed_nodes.iter().map(node_to_dict).collect();
    let impacted_dicts: Vec<Value> = impact.impacted_nodes.iter().map(node_to_dict).collect();
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

    store.close()?;
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

const QUERY_PATTERNS: &[(&str, &str)] = &[
    ("callers_of",   "Find all functions that call a given function"),
    ("callees_of",   "Find all functions called by a given function"),
    ("imports_of",   "Find all imports of a given file or module"),
    ("importers_of", "Find all files that import a given file or module"),
    ("children_of",  "Find all nodes contained in a file or class"),
    ("tests_for",    "Find all tests for a given function or class"),
    ("inheritors_of","Find all classes that inherit from a given class"),
    ("file_summary", "Get a summary of all nodes in a file"),
];

fn pattern_description(pattern: &str) -> Option<&'static str> {
    QUERY_PATTERNS.iter().find(|(p, _)| *p == pattern).map(|(_, d)| *d)
}

pub fn query_graph(
    pattern: &str,
    target: &str,
    repo_root: Option<&str>,
) -> Result<Value> {
    let (mut store, root) = get_store(repo_root)?;
    maybe_auto_update(&mut store, &root);

    let description = match pattern_description(pattern) {
        Some(d) => d,
        None => {
            store.close()?;
            let available: Vec<&str> = QUERY_PATTERNS.iter().map(|(p, _)| *p).collect();
            return Ok(json!({
                "status": "error",
                "error": format!("Unknown pattern '{}'. Available: {:?}", pattern, available),
            }));
        }
    };

    // Filter common builtins for callers_of
    if pattern == "callers_of" && is_builtin_call(target) && !target.contains("::") {
        store.close()?;
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

    // Resolve the target node
    let mut target_name = target.to_string();
    let node_opt = resolve_target_node(&store, target, &root)?;

    let node = match node_opt {
        ResolveResult::Found(n) => {
            target_name = n.qualified_name.clone();
            Some(n)
        }
        ResolveResult::Ambiguous(candidates) => {
            store.close()?;
            return Ok(json!({
                "status": "ambiguous",
                "summary": format!("Multiple matches for '{}'. Please use a qualified name.", target),
                "candidates": candidates,
            }));
        }
        ResolveResult::NotFound => None,
    };

    if node.is_none() && pattern != "file_summary" {
        store.close()?;
        return Ok(json!({
            "status": "not_found",
            "summary": format!("No node found matching '{}'.", target),
        }));
    }

    let qn: String = node.as_ref().map(|n| n.qualified_name.clone()).unwrap_or_else(|| target_name.clone());

    let mut results: Vec<Value> = vec![];
    let mut edges_out: Vec<Value> = vec![];

    match pattern {
        "callers_of" => {
            for e in store.get_edges_by_target(&qn)? {
                if e.kind == EdgeKind::Calls {
                    if let Some(caller) = store.get_node(&e.source_qualified)? {
                        results.push(node_to_dict(&caller));
                    }
                    edges_out.push(edge_to_dict(&e));
                }
            }
            // Fallback: search by plain name when qualified lookup found nothing
            if results.is_empty() {
                if let Some(ref n) = node {
                    for e in store.search_edges_by_target_name(&n.name)? {
                        if let Some(caller) = store.get_node(&e.source_qualified)? {
                            results.push(node_to_dict(&caller));
                        }
                        edges_out.push(edge_to_dict(&e));
                    }
                }
            }
        }
        "callees_of" => {
            for e in store.get_edges_by_source(&qn)? {
                if e.kind == EdgeKind::Calls {
                    if let Some(callee) = store.get_node(&e.target_qualified)? {
                        results.push(node_to_dict(&callee));
                    }
                    edges_out.push(edge_to_dict(&e));
                }
            }
        }
        "imports_of" => {
            for e in store.get_edges_by_source(&qn)? {
                if e.kind == EdgeKind::ImportsFrom {
                    results.push(json!({ "import_target": e.target_qualified }));
                    edges_out.push(edge_to_dict(&e));
                }
            }
        }
        "importers_of" => {
            let abs_target = node.as_ref()
                .map(|n| n.file_path.clone())
                .unwrap_or_else(|| root.join(target).to_string_lossy().into_owned());
            for e in store.get_edges_by_target(&abs_target)? {
                if e.kind == EdgeKind::ImportsFrom {
                    results.push(json!({ "importer": e.source_qualified, "file": e.file_path }));
                    edges_out.push(edge_to_dict(&e));
                }
            }
        }
        "children_of" => {
            for e in store.get_edges_by_source(&qn)? {
                if e.kind == EdgeKind::Contains {
                    if let Some(child) = store.get_node(&e.target_qualified)? {
                        results.push(node_to_dict(&child));
                    }
                }
            }
        }
        "tests_for" => {
            for e in store.get_edges_by_target(&qn)? {
                if e.kind == EdgeKind::TestedBy {
                    if let Some(t) = store.get_node(&e.source_qualified)? {
                        results.push(node_to_dict(&t));
                    }
                }
            }
            // Naming convention fallback
            let name = node.as_ref().map(|n| n.name.as_str()).unwrap_or(target);
            let seen: std::collections::HashSet<String> = results.iter()
                .filter_map(|r| r.get("qualified_name").and_then(|v| v.as_str()).map(|s| s.to_string()))
                .collect();
            for prefix in &[format!("test_{name}"), format!("Test{name}")] {
                for t in store.search_nodes(prefix, 10)? {
                    if !seen.contains(&t.qualified_name) && t.is_test {
                        results.push(node_to_dict(&t));
                    }
                }
            }
        }
        "inheritors_of" => {
            for e in store.get_edges_by_target(&qn)? {
                if matches!(e.kind, EdgeKind::Inherits | EdgeKind::Implements) {
                    if let Some(child) = store.get_node(&e.source_qualified)? {
                        results.push(node_to_dict(&child));
                    }
                    edges_out.push(edge_to_dict(&e));
                }
            }
        }
        "file_summary" => {
            let abs_path = root.join(target).to_string_lossy().into_owned();
            for n in store.get_nodes_by_file(&abs_path)? {
                results.push(node_to_dict(&n));
            }
        }
        _ => unreachable!("pattern already validated above"),
    }

    store.close()?;
    Ok(json!({
        "status": "ok",
        "pattern": pattern,
        "target": target_name,
        "description": description,
        "summary": format!("Found {} result(s) for {}('{}')", results.len(), pattern, target_name),
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
) -> Result<Value> {
    let (mut store, root) = get_store(repo_root)?;
    maybe_auto_update(&mut store, &root);

    let files = resolve_changed_files(changed_files, &root, base);

    if files.is_empty() {
        store.close()?;
        return Ok(json!({
            "status": "ok",
            "summary": "No changes detected. Nothing to review.",
            "context": {},
        }));
    }

    let abs_files: Vec<String> = files.iter()
        .map(|f| root.join(f).to_string_lossy().into_owned())
        .collect();

    let impact = store.get_impact_radius(&abs_files, max_depth, 500, None)?;

    let mut context = json!({
        "changed_files": files,
        "impacted_files": impact.impacted_files,
        "graph": {
            "changed_nodes": impact.changed_nodes.iter().map(node_to_dict).collect::<Vec<_>>(),
            "impacted_nodes": impact.impacted_nodes.iter().map(node_to_dict).collect::<Vec<_>>(),
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
                extract_relevant_lines(&lines, &impact.changed_nodes, &full_path.to_string_lossy())
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

    let summary_parts = vec![
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

    store.close()?;
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
) -> Result<Value> {
    let (mut store, root) = get_store(repo_root)?;
    maybe_auto_update(&mut store, &root);
    let emb_db_path = incremental::get_embeddings_db_path(&root);
    let mut emb_store = EmbeddingStore::new(&emb_db_path)?;
    let search_mode;

    let results: Vec<Value> = if emb_store.available() && emb_store.count()? > 0 {
        search_mode = "semantic";
        let mut raw = semantic_search(query, &store, &mut emb_store, limit * 2)?;
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
        nodes.iter().map(node_to_dict).collect()
    };

    let kind_suffix = kind.map(|k| format!(" (kind={k})")).unwrap_or_default();
    emb_store.close()?;
    store.close()?;
    Ok(json!({
        "status": "ok",
        "query": query,
        "search_mode": search_mode,
        "summary": format!("Found {} node(s) matching '{}'{}", results.len(), query, kind_suffix),
        "results": results,
    }))
}

// ---------------------------------------------------------------------------
// Tool 6: list_graph_stats
// ---------------------------------------------------------------------------

pub fn list_graph_stats(repo_root: Option<&str>) -> Result<Value> {
    let (mut store, root) = get_store(repo_root)?;
    maybe_auto_update(&mut store, &root);
    let stats = store.get_stats()?;

    let root_name = root.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.to_string_lossy().into_owned());

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
    let emb_db_path = incremental::get_embeddings_db_path(&root);
    let emb_count = EmbeddingStore::new(&emb_db_path)
        .and_then(|es| es.count())
        .unwrap_or(0);
    summary_parts.push(String::new());
    summary_parts.push(format!("Embeddings: {emb_count} nodes embedded"));

    store.close()?;
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
    let mut search_roots: Vec<PathBuf> = vec![];

    if let Some(p) = repo_root {
        search_roots.push(PathBuf::from(p));
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

pub fn find_large_functions(
    min_lines: usize,
    kind: Option<&str>,
    file_path_pattern: Option<&str>,
    limit: usize,
    repo_root: Option<&str>,
) -> Result<Value> {
    let (mut store, root) = get_store(repo_root)?;
    maybe_auto_update(&mut store, &root);

    let nodes = store.get_nodes_by_size(min_lines, kind, file_path_pattern, limit)?;

    let results: Vec<Value> = nodes.iter().map(|n| {
        let line_count = if n.line_end >= n.line_start {
            n.line_end - n.line_start + 1
        } else {
            0
        };
        let relative_path = Path::new(&n.file_path)
            .strip_prefix(&root)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| n.file_path.clone());
        let mut d = node_to_dict(n);
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

    store.close()?;
    Ok(json!({
        "status": "ok",
        "summary": summary_parts.join("\n"),
        "total_found": results.len(),
        "min_lines": min_lines,
        "results": results,
    }))
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Resolve changed files from the provided list or from git diff.
fn resolve_changed_files(
    changed_files: Option<Vec<String>>,
    root: &Path,
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
    root: &Path,
) -> Result<ResolveResult> {
    if let Some(node) = store.get_node(target)? {
        return Ok(ResolveResult::Found(node));
    }
    let abs_target = root.join(target).to_string_lossy().into_owned();
    if let Some(node) = store.get_node(&abs_target)? {
        return Ok(ResolveResult::Found(node));
    }
    let candidates = store.search_nodes(target, 5)?;
    match candidates.len() {
        0 => Ok(ResolveResult::NotFound),
        1 => Ok(ResolveResult::Found(candidates.into_iter().next().unwrap())),
        _ => Ok(ResolveResult::Ambiguous(
            candidates.iter().map(node_to_dict).collect(),
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
        for i in start..end.min(lines.len()) {
            parts.push(format!("{}: {}", i + 1, lines[i]));
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
        let path = dir.path().to_string_lossy().into_owned();
        (dir, path)
    }

    // -----------------------------------------------------------------------
    // validate_repo_root
    // -----------------------------------------------------------------------

    #[test]
    fn validate_repo_root_rejects_nonexistent() {
        let result = validate_repo_root(Path::new("/nonexistent/path/that/does/not/exist"));
        assert!(result.is_err());
    }

    #[test]
    fn validate_repo_root_rejects_dir_without_git() {
        let dir = TempDir::new().unwrap();
        // No .git, no .code-review-graph
        let result = validate_repo_root(dir.path());
        assert!(result.is_err(), "dir without .git should be rejected");
    }

    #[test]
    fn validate_repo_root_accepts_git_repo() {
        let (dir, _) = make_git_repo();
        let result = validate_repo_root(dir.path());
        assert!(result.is_ok(), "dir with .git should be accepted");
    }

    #[test]
    fn validate_repo_root_accepts_code_review_graph_dir() {
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join(".code-review-graph")).unwrap();
        let result = validate_repo_root(dir.path());
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

        let result = query_graph("totally_unknown_pattern", "some_target", Some(&path));
        assert!(result.is_ok());
        let val = result.unwrap();
        assert_eq!(val["status"], "error");
        assert!(val["error"].as_str().unwrap().contains("totally_unknown_pattern"));
    }

    #[test]
    fn query_graph_known_pattern_not_found_returns_not_found() {
        let (_dir, path) = make_git_repo();
        build_or_update_graph(true, Some(&path), "HEAD").unwrap();

        let result = query_graph("callers_of", "definitely_not_a_real_function_xyz", Some(&path));
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
}
