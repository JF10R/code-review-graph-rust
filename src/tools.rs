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

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::error::{CrgError, Result};
use crate::graph::GraphStore;
use crate::incremental;
use crate::types::{edge_to_dict, node_to_dict};

/// Common JS/TS builtin method names filtered from callers_of results.
static BUILTIN_CALL_NAMES: &[&str] = &[
    "map", "filter", "reduce", "forEach", "find", "findIndex",
    "some", "every", "includes", "indexOf", "push", "pop",
    "shift", "unshift", "splice", "slice", "concat", "join",
    "flat", "flatMap", "sort", "reverse", "fill", "keys",
    "values", "entries", "from", "isArray", "trim", "split",
    "replace", "replaceAll", "match", "search", "substring",
    "toLowerCase", "toUpperCase", "startsWith", "endsWith",
    "assign", "freeze", "defineProperty", "hasOwnProperty",
    "create", "fromEntries", "log", "warn", "error", "info",
    "debug", "then", "catch", "finally", "resolve", "reject",
    "all", "race", "parse", "stringify", "floor", "ceil",
    "round", "random", "max", "min", "abs", "pow", "sqrt",
    "addEventListener", "querySelector", "getElementById",
    "createElement", "appendChild", "preventDefault",
    "setTimeout", "clearTimeout", "setInterval", "clearInterval",
    "toString", "valueOf", "toJSON", "getTime", "now",
    "call", "apply", "bind", "next", "emit", "on", "off",
    "pipe", "write", "read", "end", "close", "send", "status",
    "json", "set", "get", "delete", "has", "describe", "it",
    "test", "expect", "beforeEach", "afterEach", "mock",
    "require", "fetch",
];

fn is_builtin_call(name: &str) -> bool {
    BUILTIN_CALL_NAMES.contains(&name)
}

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

// Each tool returns a serde_json::Value (MCP response dict).

pub fn build_or_update_graph(
    full_rebuild: bool,
    repo_root: Option<&str>,
    base: &str,
) -> Result<Value> {
    let _ = (full_rebuild, repo_root, base);
    todo!()
}

pub fn get_impact_radius(
    changed_files: Option<Vec<String>>,
    max_depth: usize,
    repo_root: Option<&str>,
    base: &str,
) -> Result<Value> {
    let _ = (changed_files, max_depth, repo_root, base);
    todo!()
}

pub fn query_graph(
    pattern: &str,
    target: &str,
    repo_root: Option<&str>,
) -> Result<Value> {
    let _ = (pattern, target, repo_root);
    todo!()
}

pub fn get_review_context(
    changed_files: Option<Vec<String>>,
    max_depth: usize,
    include_source: bool,
    max_lines_per_file: usize,
    repo_root: Option<&str>,
    base: &str,
) -> Result<Value> {
    let _ = (changed_files, max_depth, include_source, max_lines_per_file, repo_root, base);
    todo!()
}

pub fn semantic_search_nodes(
    query: &str,
    kind: Option<&str>,
    limit: usize,
    repo_root: Option<&str>,
) -> Result<Value> {
    let _ = (query, kind, limit, repo_root);
    todo!()
}

pub fn list_graph_stats(repo_root: Option<&str>) -> Result<Value> {
    let _ = repo_root;
    todo!()
}

pub fn embed_graph(repo_root: Option<&str>) -> Result<Value> {
    let _ = repo_root;
    todo!()
}

pub fn get_docs_section(section_name: &str, repo_root: Option<&str>) -> Result<Value> {
    let _ = (section_name, repo_root);
    todo!()
}

pub fn find_large_functions(
    min_lines: usize,
    kind: Option<&str>,
    file_path_pattern: Option<&str>,
    limit: usize,
    repo_root: Option<&str>,
) -> Result<Value> {
    let _ = (min_lines, kind, file_path_pattern, limit, repo_root);
    todo!()
}
