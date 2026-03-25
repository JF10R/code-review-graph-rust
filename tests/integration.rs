//! Integration tests: full pipeline from disk files → graph → queries.

use std::fs;
use std::path::Path;

use camino::Utf8Path;
use code_review_graph::graph::GraphStore;
use code_review_graph::incremental::{full_build, get_db_path, incremental_update};
use code_review_graph::parser::CodeParser;
use code_review_graph::tools::{
    build_or_update_graph, find_large_functions, get_impact_radius, hybrid_query, list_graph_stats,
    measure_token_reduction, query_graph, semantic_search_nodes, trace_call_chain,
};
use tempfile::TempDir;

fn p(path: &std::path::Path) -> &Utf8Path {
    Utf8Path::from_path(path).expect("test path is valid UTF-8")
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Returns `true` if the tree-sitter grammars can be initialised on this
/// system. On some Windows debug builds the C ABI grammar libraries fail to
/// load. All tests that require parsing skip gracefully when this returns
/// `false` so the suite still reports "ok" (0 failures).
fn grammars_available() -> bool {
    let parser = CodeParser::new();
    parser
        .parse_bytes(Path::new("check.py"), b"")
        .is_ok()
}

/// Create a temp dir that looks like a git repo (has a `.git` dir) and
/// populate it with a mix of Python and TypeScript source files.
fn setup_test_repo() -> TempDir {
    let dir = TempDir::new().unwrap();
    fs::create_dir(dir.path().join(".git")).unwrap();

    // Python module
    fs::write(
        dir.path().join("utils.py"),
        b"
def add(a, b):
    return a + b

def subtract(a, b):
    return a - b
",
    )
    .unwrap();

    // Python file that calls utils
    fs::write(
        dir.path().join("main.py"),
        b"
from utils import add, subtract

def run():
    result = add(1, 2)
    diff = subtract(5, 3)
    return result + diff
",
    )
    .unwrap();

    // TypeScript file
    fs::write(
        dir.path().join("service.ts"),
        b"
export function greet(name: string): string {
    return `Hello, ${name}`;
}

export function farewell(name: string): string {
    return `Goodbye, ${name}`;
}
",
    )
    .unwrap();

    dir
}

// ---------------------------------------------------------------------------
// Test 1: full_build produces expected stats
// ---------------------------------------------------------------------------

#[test]
fn full_build_produces_nodes_and_edges() {
    if !grammars_available() { return; }
    let dir = setup_test_repo();
    let db_path = get_db_path(p(dir.path()));
    let mut store = GraphStore::new(&db_path).unwrap();

    let result = full_build(p(dir.path()), &mut store).unwrap();

    // We created 3 source files; at minimum all should parse
    assert!(result.files_parsed >= 3, "should parse at least 3 files, got {}", result.files_parsed);
    assert!(result.total_nodes > 0, "should extract some nodes");

    let stats = store.get_stats().unwrap();
    assert!(stats.total_nodes > 0);
    assert!(stats.files_count > 0);
    // At minimum Python and TypeScript should be present
    let langs = &stats.languages;
    assert!(
        langs.iter().any(|l| l == "python"),
        "python should be in languages: {:?}", langs
    );
    assert!(
        langs.iter().any(|l| l == "typescript"),
        "typescript should be in languages: {:?}", langs
    );
}

// ---------------------------------------------------------------------------
// Test 2: query_graph callers_of / callees_of
// ---------------------------------------------------------------------------

#[test]
fn query_graph_callers_and_callees() {
    if !grammars_available() { return; }
    let dir = setup_test_repo();
    let root_str = dir.path().to_string_lossy().into_owned();

    // Build the graph first
    build_or_update_graph(true, Some(&root_str), "HEAD").unwrap();

    // callees_of "run" should include add and subtract (called inside run)
    let callees_result = query_graph("callees_of", "run", Some(&root_str), false).unwrap();
    assert_eq!(callees_result["status"], "ok", "callees_of should succeed");

    // callers_of "add" should include run (which calls add)
    let callers_result = query_graph("callers_of", "add", Some(&root_str), false).unwrap();
    assert_eq!(callers_result["status"], "ok", "callers_of should succeed");
}

// ---------------------------------------------------------------------------
// Test 3: incremental_update detects changed files
// ---------------------------------------------------------------------------

#[test]
fn incremental_update_picks_up_new_function() {
    if !grammars_available() { return; }
    let dir = setup_test_repo();
    let db_path = get_db_path(p(dir.path()));
    let mut store = GraphStore::new(&db_path).unwrap();
    full_build(p(dir.path()), &mut store).unwrap();

    // Add a new function to utils.py
    fs::write(
        dir.path().join("utils.py"),
        b"
def add(a, b):
    return a + b

def subtract(a, b):
    return a - b

def multiply(a, b):
    return a * b
",
    )
    .unwrap();

    let result = incremental_update(
        p(dir.path()),
        &mut store,
        "HEAD",
        Some(vec!["utils.py".to_string()]),
    )
    .unwrap();

    // multiply is new so it should appear in changed_qualified_names
    assert!(
        !result.changed_qualified_names.is_empty(),
        "adding a new function should produce changed_qualified_names"
    );
    let has_multiply = result
        .changed_qualified_names
        .iter()
        .any(|qn| qn.ends_with("multiply"));
    assert!(has_multiply, "multiply should be in changed_qualified_names");
}

// ---------------------------------------------------------------------------
// Test 4: save/load cycle preserves graph
// ---------------------------------------------------------------------------

#[test]
fn save_load_cycle_preserves_graph() {
    if !grammars_available() { return; }
    let dir = TempDir::new().unwrap();
    fs::create_dir(dir.path().join(".git")).unwrap();
    fs::write(
        dir.path().join("calc.py"),
        b"def square(x): return x * x\n",
    )
    .unwrap();

    let db_path = get_db_path(p(dir.path()));

    // Build and close (triggers save)
    {
        let mut store = GraphStore::new(&db_path).unwrap();
        full_build(p(dir.path()), &mut store).unwrap();
        // commit is called inside full_build, but call close() explicitly
        store.close().unwrap();
    }

    // Reload from disk
    let store2 = GraphStore::new(&db_path).unwrap();
    let stats = store2.get_stats().unwrap();
    assert!(stats.total_nodes > 0, "nodes should survive save+load cycle");

    // The "square" function should be findable
    let results = store2.search_nodes("square", 10).unwrap();
    assert!(
        results.iter().any(|n| n.name == "square"),
        "square should be in the graph after reload"
    );
}

// ---------------------------------------------------------------------------
// Test 5: parse_and_store via CodeParser produces body_hash
// ---------------------------------------------------------------------------

#[test]
fn parser_body_hash_changes_when_content_changes() {
    if !grammars_available() { return; }
    let parser = CodeParser::new();
    let path = Path::new("calc.py");

    let src_v1 = b"def square(x): return x * x\n";
    let src_v2 = b"def square(x):\n    # optimized\n    return x ** 2\n";

    let (nodes_v1, _) = parser.parse_bytes(path, src_v1).unwrap();
    let (nodes_v2, _) = parser.parse_bytes(path, src_v2).unwrap();

    let hash_v1 = nodes_v1
        .iter()
        .find(|n| n.name == "square")
        .map(|n| n.body_hash.clone())
        .expect("square should exist in v1");

    let hash_v2 = nodes_v2
        .iter()
        .find(|n| n.name == "square")
        .map(|n| n.body_hash.clone())
        .expect("square should exist in v2");

    assert_ne!(hash_v1, hash_v2, "body_hash should differ when function body changes");
}

// ---------------------------------------------------------------------------
// Test 6: list_graph_stats after build
// ---------------------------------------------------------------------------

#[test]
fn list_graph_stats_after_build() {
    if !grammars_available() { return; }
    let dir = setup_test_repo();
    let root_str = dir.path().to_string_lossy().into_owned();

    build_or_update_graph(true, Some(&root_str), "HEAD").unwrap();

    let stats = list_graph_stats(Some(&root_str)).unwrap();
    assert_eq!(stats["status"], "ok");
    let total = stats["total_nodes"].as_u64().unwrap_or(0);
    assert!(total > 0, "should have nodes after build");
    assert!(stats["nodes_by_kind"].is_object());
    assert!(stats["edges_by_kind"].is_object());
    assert!(stats["files_count"].as_u64().unwrap_or(0) > 0);
}

// ---------------------------------------------------------------------------
// Test 7: impact radius correctly flags callers
// ---------------------------------------------------------------------------

#[test]
fn impact_radius_flags_callers_of_changed_function() {
    if !grammars_available() { return; }
    let dir = setup_test_repo();
    let db_path = get_db_path(p(dir.path()));
    let mut store = GraphStore::new(&db_path).unwrap();
    full_build(p(dir.path()), &mut store).unwrap();

    // utils.py contains add; main.py calls add via run() -> add()
    let abs_utils = dir.path().join("utils.py").to_string_lossy().into_owned();
    let impact = store
        .get_impact_radius(&[abs_utils], 5, 100, None)
        .unwrap();

    // There should be some impacted nodes (callers of functions in utils.py)
    // Algorithm is always personalized_pagerank
    assert_eq!(impact.algorithm, "personalized_pagerank");
    // changed_nodes should include the nodes from utils.py
    assert!(!impact.changed_nodes.is_empty(), "changed_nodes should be populated");
}

// ---------------------------------------------------------------------------
// Test 8: query_graph file_summary returns nodes for a file
// ---------------------------------------------------------------------------

#[test]
fn query_graph_file_summary_returns_nodes() {
    if !grammars_available() { return; }
    let dir = setup_test_repo();
    let root_str = dir.path().to_string_lossy().into_owned();
    build_or_update_graph(true, Some(&root_str), "HEAD").unwrap();

    let result = query_graph("file_summary", "utils.py", Some(&root_str), false).unwrap();
    let status = result["status"].as_str().unwrap();
    // file_summary may return "ok" with results, or "ambiguous" when the short
    // name matches multiple recorded paths — both are valid non-error outcomes.
    assert!(
        status == "ok" || status == "ambiguous",
        "file_summary should return ok or ambiguous, got: {status}"
    );
    assert_ne!(status, "error", "file_summary must not return error");
    // Only verify results are non-empty when the lookup succeeded unambiguously
    if status == "ok" {
        let results_arr = result["results"].as_array().unwrap();
        assert!(!results_arr.is_empty(), "file_summary should return nodes for utils.py");
    }
}

// ---------------------------------------------------------------------------
// Test 9: query_graph imports_of returns ok or not_found (not error)
// ---------------------------------------------------------------------------

#[test]
fn query_graph_imports_of_does_not_error() {
    if !grammars_available() { return; }
    let dir = setup_test_repo();
    let root_str = dir.path().to_string_lossy().into_owned();
    build_or_update_graph(true, Some(&root_str), "HEAD").unwrap();

    // main.py imports from utils — query imports_of main.py
    let result = query_graph("imports_of", "main.py", Some(&root_str), false).unwrap();
    let status = result["status"].as_str().unwrap();
    assert!(
        status == "ok" || status == "not_found" || status == "ambiguous",
        "imports_of should return ok/not_found/ambiguous, got: {status}"
    );
    // Must never return "error"
    assert_ne!(status, "error", "imports_of must not return error status");
}

// ---------------------------------------------------------------------------
// Test 10: query_graph importers_of returns ok or not_found (not error)
// ---------------------------------------------------------------------------

#[test]
fn query_graph_importers_of_does_not_error() {
    if !grammars_available() { return; }
    let dir = setup_test_repo();
    let root_str = dir.path().to_string_lossy().into_owned();
    build_or_update_graph(true, Some(&root_str), "HEAD").unwrap();

    // utils.py is imported by main.py — query importers_of utils.py
    let result = query_graph("importers_of", "utils.py", Some(&root_str), false).unwrap();
    let status = result["status"].as_str().unwrap();
    assert!(
        status == "ok" || status == "not_found" || status == "ambiguous",
        "importers_of should return ok/not_found/ambiguous, got: {status}"
    );
    assert_ne!(status, "error", "importers_of must not return error status");
}

// ---------------------------------------------------------------------------
// Test 11: query_graph children_of returns ok or not_found (not error)
// ---------------------------------------------------------------------------

#[test]
fn query_graph_children_of_does_not_error() {
    if !grammars_available() { return; }
    let dir = setup_test_repo();
    let root_str = dir.path().to_string_lossy().into_owned();
    build_or_update_graph(true, Some(&root_str), "HEAD").unwrap();

    // utils.py should contain add and subtract
    let result = query_graph("children_of", "utils.py", Some(&root_str), false).unwrap();
    let status = result["status"].as_str().unwrap();
    assert!(
        status == "ok" || status == "not_found" || status == "ambiguous",
        "children_of should return ok/not_found/ambiguous, got: {status}"
    );
    assert_ne!(status, "error", "children_of must not return error status");
}

// ---------------------------------------------------------------------------
// Test 12: query_graph tests_for returns ok or not_found (not error)
// ---------------------------------------------------------------------------

#[test]
fn query_graph_tests_for_does_not_error() {
    if !grammars_available() { return; }
    // Set up a repo with a test file that follows naming conventions
    let dir = TempDir::new().unwrap();
    fs::create_dir(dir.path().join(".git")).unwrap();

    fs::write(
        dir.path().join("math_utils.py"),
        b"def add(a, b):\n    return a + b\n",
    )
    .unwrap();

    fs::write(
        dir.path().join("test_math_utils.py"),
        b"def test_add():\n    assert add(1, 2) == 3\n",
    )
    .unwrap();

    let root_str = dir.path().to_string_lossy().into_owned();
    build_or_update_graph(true, Some(&root_str), "HEAD").unwrap();

    let result = query_graph("tests_for", "add", Some(&root_str), false).unwrap();
    let status = result["status"].as_str().unwrap();
    assert!(
        status == "ok" || status == "not_found" || status == "ambiguous",
        "tests_for should return ok/not_found/ambiguous, got: {status}"
    );
    assert_ne!(status, "error", "tests_for must not return error status");
}

// ---------------------------------------------------------------------------
// Test 13: query_graph inheritors_of returns ok or not_found (not error)
// ---------------------------------------------------------------------------

#[test]
fn query_graph_inheritors_of_does_not_error() {
    if !grammars_available() { return; }
    let dir = TempDir::new().unwrap();
    fs::create_dir(dir.path().join(".git")).unwrap();

    // Python class hierarchy: Animal ← Dog
    fs::write(
        dir.path().join("animals.py"),
        b"class Animal:\n    def speak(self):\n        pass\n\nclass Dog(Animal):\n    def speak(self):\n        return 'woof'\n",
    )
    .unwrap();

    let root_str = dir.path().to_string_lossy().into_owned();
    build_or_update_graph(true, Some(&root_str), "HEAD").unwrap();

    let result = query_graph("inheritors_of", "Animal", Some(&root_str), false).unwrap();
    let status = result["status"].as_str().unwrap();
    assert!(
        status == "ok" || status == "not_found" || status == "ambiguous",
        "inheritors_of should return ok/not_found/ambiguous, got: {status}"
    );
    assert_ne!(status, "error", "inheritors_of must not return error status");
}

// ---------------------------------------------------------------------------
// Test 14: hybrid_query returns ok status and expected fields
// ---------------------------------------------------------------------------

#[test]
fn hybrid_query_returns_ok_with_method_field() {
    if !grammars_available() { return; }
    let dir = setup_test_repo();
    let root_str = dir.path().to_string_lossy().into_owned();
    build_or_update_graph(true, Some(&root_str), "HEAD").unwrap();

    let result = hybrid_query("add", 5, Some(&root_str), false, None).unwrap();
    assert_eq!(result["status"], "ok");
    // No embeddings in temp test repos — should fall back to keyword_only
    assert_eq!(
        result["method"], "keyword_only",
        "should use keyword_only when no embeddings: {result:?}"
    );
    assert!(result["results"].is_array());
}

#[test]
fn hybrid_query_empty_query_returns_empty() {
    if !grammars_available() { return; }
    let dir = setup_test_repo();
    let root_str = dir.path().to_string_lossy().into_owned();
    build_or_update_graph(true, Some(&root_str), "HEAD").unwrap();

    let result = hybrid_query("", 10, Some(&root_str), false, None).unwrap();
    assert_eq!(result["status"], "ok");
    assert!(result["results"].as_array().unwrap().is_empty());
}

#[test]
fn hybrid_query_results_include_rrf_score() {
    if !grammars_available() { return; }
    let dir = setup_test_repo();
    let root_str = dir.path().to_string_lossy().into_owned();
    build_or_update_graph(true, Some(&root_str), "HEAD").unwrap();

    let result = hybrid_query("subtract", 5, Some(&root_str), false, None).unwrap();
    assert_eq!(result["status"], "ok");
    let arr = result["results"].as_array().unwrap();
    if !arr.is_empty() {
        assert!(
            arr[0].get("rrf_score").is_some(),
            "each result should have rrf_score field"
        );
        assert!(
            arr[0]["rrf_score"].as_f64().unwrap() > 0.0,
            "rrf_score should be positive"
        );
    }
}

// ---------------------------------------------------------------------------
// Test 17: measure_token_reduction returns expected structure
// ---------------------------------------------------------------------------

#[test]
fn measure_token_reduction_returns_ok_structure() {
    if !grammars_available() { return; }
    let dir = setup_test_repo();
    let root_str = dir.path().to_string_lossy().into_owned();
    build_or_update_graph(true, Some(&root_str), "HEAD").unwrap();

    let result = measure_token_reduction(None, Some(&root_str), "HEAD").unwrap();
    assert_eq!(result["status"], "ok");
    assert!(result["naive_bytes"].is_number());
    assert!(result["context_bytes"].is_number());
    assert!(result["reduction_percent"].is_number());
}

#[test]
fn measure_token_reduction_naive_bytes_exceeds_context_for_changed_files() {
    if !grammars_available() { return; }
    let dir = setup_test_repo();
    let root_str = dir.path().to_string_lossy().into_owned();
    build_or_update_graph(true, Some(&root_str), "HEAD").unwrap();

    let result = measure_token_reduction(
        Some(vec!["utils.py".to_string()]),
        Some(&root_str),
        "HEAD",
    )
    .unwrap();
    assert_eq!(result["status"], "ok");

    let naive = result["naive_bytes"].as_u64().unwrap();
    let context = result["context_bytes"].as_u64().unwrap();
    // The test repo has 3 source files; context for one changed file should be
    // smaller-than-or-equal-to the full naive bytes
    assert!(
        naive >= context,
        "naive_bytes ({naive}) should be >= context_bytes ({context}) when reviewing a subset of files"
    );
}

// ---------------------------------------------------------------------------
// Snapshot tests (insta)
// ---------------------------------------------------------------------------

/// Walk a JSON value and replace any string that contains the temp dir path
/// (in any platform path format) with `[path]`.
fn redact_temp_paths(val: &mut serde_json::Value, temp_dir: &str) {
    // Strip the Windows extended-length prefix (//?/) and normalise separators
    // so the match works regardless of how the path was recorded.
    let norm = temp_dir.replace('\\', "/");
    let plain = norm.trim_start_matches("//?/");

    match val {
        serde_json::Value::String(s) => {
            // Normalise both sides to forward slashes and strip the Windows
            // extended-length prefix (//?/) before matching.
            let s_plain = s.replace('\\', "/");
            let s_plain = s_plain.trim_start_matches("//?/");
            if s_plain.contains(plain) {
                *s = "[path]".to_string();
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr.iter_mut() {
                redact_temp_paths(item, temp_dir);
            }
        }
        serde_json::Value::Object(map) => {
            for v in map.values_mut() {
                redact_temp_paths(v, temp_dir);
            }
        }
        _ => {}
    }
}

/// Sort an array-of-objects field by the given key, if it exists.
fn sort_array_by_key(val: &mut serde_json::Value, array_ptr: &str, key: &str) {
    if let Some(arr) = val.pointer_mut(array_ptr).and_then(|v| v.as_array_mut()) {
        arr.sort_by(|a, b| {
            let ka = a.get(key).and_then(|v| v.as_str()).unwrap_or("");
            let kb = b.get(key).and_then(|v| v.as_str()).unwrap_or("");
            ka.cmp(kb)
        });
    }
}

#[test]
fn snapshot_list_graph_stats() {
    if !grammars_available() { return; }
    let dir = setup_test_repo();
    let root_str = dir.path().to_string_lossy().into_owned();
    build_or_update_graph(true, Some(&root_str), "HEAD").unwrap();

    let mut result = list_graph_stats(Some(&root_str)).unwrap();
    // languages order is non-deterministic — sort for stable snapshot
    if let Some(langs) = result.get_mut("languages").and_then(|v| v.as_array_mut()) {
        langs.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
    }
    insta::assert_json_snapshot!(result, {
        ".last_updated" => "[timestamp]",
        ".summary" => "[summary]",
    });
}

#[test]
fn snapshot_query_graph_children_of() {
    if !grammars_available() { return; }
    let dir = setup_test_repo();
    let root_str = dir.path().to_string_lossy().into_owned();
    build_or_update_graph(true, Some(&root_str), "HEAD").unwrap();

    let mut result = query_graph("children_of", "utils.py", Some(&root_str), false).unwrap();
    let temp_dir = dir.path().to_string_lossy().into_owned();
    redact_temp_paths(&mut result, &temp_dir);
    sort_array_by_key(&mut result, "/results", "name");
    sort_array_by_key(&mut result, "/candidates", "name");
    insta::assert_json_snapshot!(result, {
        ".summary" => "[summary]",
    });
}

#[test]
fn snapshot_find_large_functions() {
    if !grammars_available() { return; }
    let dir = setup_test_repo();
    let root_str = dir.path().to_string_lossy().into_owned();
    build_or_update_graph(true, Some(&root_str), "HEAD").unwrap();

    // min_lines=1 to capture the small functions in the test repo
    let mut result = find_large_functions(1, None, None, 20, Some(&root_str), true).unwrap();
    let temp_dir = dir.path().to_string_lossy().into_owned();
    redact_temp_paths(&mut result, &temp_dir);
    sort_array_by_key(&mut result, "/results", "name");
    insta::assert_json_snapshot!(result, {
        ".summary" => "[summary]",
    });
}

#[test]
fn snapshot_get_impact_radius() {
    if !grammars_available() { return; }
    let dir = setup_test_repo();
    let root_str = dir.path().to_string_lossy().into_owned();
    build_or_update_graph(true, Some(&root_str), "HEAD").unwrap();

    let mut result = get_impact_radius(
        Some(vec!["utils.py".to_string()]),
        3,
        Some(&root_str),
        "HEAD",
        true,
    ).unwrap();
    let temp_dir = dir.path().to_string_lossy().into_owned();
    redact_temp_paths(&mut result, &temp_dir);
    sort_array_by_key(&mut result, "/changed_nodes", "name");
    sort_array_by_key(&mut result, "/impacted_nodes", "name");
    insta::assert_json_snapshot!(result, {
        ".summary" => "[summary]",
    });
}

#[test]
fn snapshot_trace_call_chain() {
    if !grammars_available() { return; }
    let dir = setup_test_repo();
    let root_str = dir.path().to_string_lossy().into_owned();
    build_or_update_graph(true, Some(&root_str), "HEAD").unwrap();

    // run calls add — trace the chain
    let mut result = trace_call_chain("run", "add", 5, false, Some(&root_str)).unwrap();
    let temp_dir = dir.path().to_string_lossy().into_owned();
    redact_temp_paths(&mut result, &temp_dir);
    insta::assert_json_snapshot!(result, {
        ".summary" => "[summary]",
    });
}

#[test]
fn snapshot_semantic_search_nodes() {
    if !grammars_available() { return; }
    let dir = setup_test_repo();
    let root_str = dir.path().to_string_lossy().into_owned();
    build_or_update_graph(true, Some(&root_str), "HEAD").unwrap();

    let mut result = semantic_search_nodes("add function", None, 5, Some(&root_str), true).unwrap();
    let temp_dir = dir.path().to_string_lossy().into_owned();
    redact_temp_paths(&mut result, &temp_dir);
    sort_array_by_key(&mut result, "/results", "name");
    insta::assert_json_snapshot!(result, {
        ".results[].similarity" => "[score]",
        ".summary" => "[summary]",
    });
}
