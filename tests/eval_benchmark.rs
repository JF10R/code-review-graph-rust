//! Automated eval runner for the gold eval set.
//!
//! Run with:
//!   cargo test --ignored eval_benchmark_node_mode -- --nocapture
//!   cargo test --ignored eval_benchmark_file_mode -- --nocapture
//!   cargo test --ignored -- --nocapture   (runs both)
//!
//! Requires pre-built graphs in D:\GitHub\bench-repos\ for each repo.
//! Cases whose repo_root does not exist on disk are skipped with a warning.
//!
//! Ablation: pass result_mode = None (node) or Some("file") to compare routing
//! strategies without changing the eval harness.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use code_review_graph::embeddings::EmbeddingStore;
use code_review_graph::graph::GraphStore;
use code_review_graph::incremental;
use code_review_graph::paths::normalize_path;
use code_review_graph::tools::AblationConfig;

// ---------------------------------------------------------------------------
// Gold eval case schema
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct EvalCase {
    id: String,
    repo: String,
    repo_root: String,
    query: String,
    category: String,
    ground_truth_files: Vec<String>,
    #[allow(dead_code)]
    relevant_files: Vec<String>,
    difficulty: String,
}

// ---------------------------------------------------------------------------
// Path matching helpers
// ---------------------------------------------------------------------------

/// Returns true when `result_path` ends with `ground_truth` (or vice-versa),
/// after normalizing separators and case.
fn paths_match(result_path: &str, ground_truth: &str) -> bool {
    let r = normalize_path(result_path).to_lowercase();
    let g = normalize_path(ground_truth).to_lowercase();
    r.ends_with(&g) || g.ends_with(&r)
}

// ---------------------------------------------------------------------------
// Core eval runner
// ---------------------------------------------------------------------------

fn run_eval(result_mode: Option<&str>) {
    let json_path = "eval/gold-eval-set.json";
    let json = std::fs::read_to_string(json_path)
        .unwrap_or_else(|e| panic!("Could not read {json_path}: {e}"));
    let cases: Vec<EvalCase> =
        serde_json::from_str(&json).expect("Could not parse eval/gold-eval-set.json");

    let mode_label = result_mode.unwrap_or("node");

    println!("\n=== Eval Benchmark (result_mode: {mode_label}) ===\n");
    println!(
        "| {:>2} | {:<16} | {:<18} | {:<10} | {:<10} | {:>5} | {} |",
        "#", "ID", "Repo", "Category", "Difficulty", "Hit@5", "Rank | Ground Truth"
    );
    println!("|{:-<4}|{:-<18}|{:-<20}|{:-<12}|{:-<12}|{:-<7}|{:-<40}|",
        "", "", "", "", "", "", "");

    let mut hits: usize = 0;
    let mut mrr_sum: f64 = 0.0;
    let mut skipped: usize = 0;

    // Per-repo accumulators: (hits, total)
    let mut by_repo: HashMap<String, (usize, usize)> = HashMap::new();
    // Per-difficulty accumulators
    let mut by_diff: HashMap<String, (usize, usize)> = HashMap::new();
    // Per-category accumulators
    let mut by_cat: HashMap<String, (usize, usize)> = HashMap::new();

    for (i, case) in cases.iter().enumerate() {
        // Skip repos that haven't been built yet.
        if !Path::new(&case.repo_root).exists() {
            println!(
                "| {:>2} | {:<16} | {:<18} | {:<10} | {:<10} | {:<5} | SKIPPED (repo_root not found) |",
                i + 1,
                truncate(&case.id, 16),
                truncate(&case.repo, 18),
                truncate(&case.category, 10),
                truncate(&case.difficulty, 10),
                "-",
            );
            skipped += 1;
            continue;
        }

        let result = code_review_graph::tools::hybrid_query(
            &case.query,
            10,
            Some(&case.repo_root),
            true,
            None,
            None,
            None,
            result_mode,
            Some("thorough"),
        );

        match result {
            Err(e) => {
                println!(
                    "| {:>2} | {:<16} | {:<18} | {:<10} | {:<10} | ERROR | {e} |",
                    i + 1,
                    truncate(&case.id, 16),
                    truncate(&case.repo, 18),
                    truncate(&case.category, 10),
                    truncate(&case.difficulty, 10),
                );
            }
            Ok(val) => {
                let results: &[serde_json::Value] = val["results"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);

                // Find the first ground-truth hit within the top-5 results.
                let rank = find_hit_rank(results, &case.ground_truth_files, 5);

                let hit = rank.is_some();
                if let Some(r) = rank {
                    hits += 1;
                    mrr_sum += 1.0 / r as f64;
                }

                // Accumulate breakdown stats (only non-skipped cases).
                let repo_entry = by_repo.entry(case.repo.clone()).or_default();
                repo_entry.1 += 1;
                if hit { repo_entry.0 += 1; }

                let diff_entry = by_diff.entry(case.difficulty.clone()).or_default();
                diff_entry.1 += 1;
                if hit { diff_entry.0 += 1; }

                let cat_entry = by_cat.entry(case.category.clone()).or_default();
                cat_entry.1 += 1;
                if hit { cat_entry.0 += 1; }

                let rank_str = rank.map_or("-".to_string(), |r| r.to_string());
                let hit_label = if hit { "HIT  " } else { "MISS " };
                let gt = best_gt_filename(&case.ground_truth_files);

                println!(
                    "| {:>2} | {:<16} | {:<18} | {:<10} | {:<10} | {} | {:>4} | {} |",
                    i + 1,
                    truncate(&case.id, 16),
                    truncate(&case.repo, 18),
                    truncate(&case.category, 10),
                    truncate(&case.difficulty, 10),
                    hit_label,
                    rank_str,
                    gt,
                );
            }
        }
    }

    let evaluated = cases.len() - skipped;
    let hit_rate = if evaluated > 0 {
        hits as f64 / evaluated as f64 * 100.0
    } else {
        0.0
    };
    let mrr = if evaluated > 0 {
        mrr_sum / evaluated as f64
    } else {
        0.0
    };

    println!("\n=== Summary (mode: {mode_label}) ===");
    println!("Hit@5: {hits}/{evaluated} ({hit_rate:.1}%)");
    println!("MRR:   {mrr:.3}");
    println!("Cases: {} total, {} evaluated, {} skipped", cases.len(), evaluated, skipped);

    if !by_repo.is_empty() {
        println!("\n--- Per-repo breakdown ---");
        let mut repos: Vec<&String> = by_repo.keys().collect();
        repos.sort();
        for repo in repos {
            let (h, t) = by_repo[repo];
            println!("  {:<30} {h}/{t} ({:.0}%)", repo, h as f64 / t as f64 * 100.0);
        }
    }

    if !by_diff.is_empty() {
        println!("\n--- Per-difficulty breakdown ---");
        let mut diffs: Vec<&String> = by_diff.keys().collect();
        diffs.sort();
        for diff in diffs {
            let (h, t) = by_diff[diff];
            println!("  {:<12} {h}/{t} ({:.0}%)", diff, h as f64 / t as f64 * 100.0);
        }
    }

    if !by_cat.is_empty() {
        println!("\n--- Per-category breakdown ---");
        let mut cats: Vec<&String> = by_cat.keys().collect();
        cats.sort();
        for cat in cats {
            let (h, t) = by_cat[cat];
            println!("  {:<14} {h}/{t} ({:.0}%)", cat, h as f64 / t as f64 * 100.0);
        }
    }
}

// ---------------------------------------------------------------------------
// Ablation runner (leave-one-out)
// ---------------------------------------------------------------------------

/// Result from a single ablation config run.
struct AblationResult {
    label: String,
    hits: usize,
    mrr: f64,
    evaluated: usize,
    /// Per-case hit rank (None = miss, Some(rank) = hit at rank).
    per_case: Vec<(String, Option<usize>)>,
}

/// Run the 28-case gold eval with a specific ablation config using
/// `hybrid_query_with_store` directly.
fn run_eval_ablation(ablation: &AblationConfig) -> AblationResult {
    let json_path = "eval/gold-eval-set.json";
    let json = std::fs::read_to_string(json_path)
        .unwrap_or_else(|e| panic!("Could not read {json_path}: {e}"));
    let cases: Vec<EvalCase> =
        serde_json::from_str(&json).expect("Could not parse eval/gold-eval-set.json");

    let label = ablation.label();
    let mut hits: usize = 0;
    let mut mrr_sum: f64 = 0.0;
    let mut skipped: usize = 0;
    let mut per_case: Vec<(String, Option<usize>)> = Vec::new();

    // Cache open stores per repo_root to avoid re-opening for every case.
    let mut stores: HashMap<String, (GraphStore, EmbeddingStore)> = HashMap::new();

    for case in &cases {
        if !Path::new(&case.repo_root).exists() {
            skipped += 1;
            continue;
        }

        // Open or reuse stores for this repo.
        if !stores.contains_key(&case.repo_root) {
            let root = camino::Utf8Path::new(&case.repo_root);
            let db_path = incremental::get_db_path(root);
            let store = GraphStore::new(&db_path).expect("open graph store");
            let emb_db_path = incremental::get_embeddings_db_path(root);
            let emb_store = EmbeddingStore::new(&emb_db_path).expect("open embedding store");
            stores.insert(case.repo_root.clone(), (store, emb_store));
        }
        let (store, emb_store) = stores.get_mut(&case.repo_root).unwrap();
        let root = camino::Utf8Path::new(&case.repo_root);

        let result = code_review_graph::tools::hybrid_query_with_store(
            store,
            emb_store,
            root,
            &case.query,
            10,
            true,            // compact
            None,            // fusion
            None,            // keyword_hits
            None,            // route
            None,            // debug
            Some("file"),    // result_mode
            Some(ablation),  // ablation config
            Some("thorough"), // budget
        );

        match result {
            Err(_) => {
                per_case.push((case.id.clone(), None));
            }
            Ok(val) => {
                let results: &[serde_json::Value] = val["results"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let rank = find_hit_rank(results, &case.ground_truth_files, 5);
                if let Some(r) = rank {
                    hits += 1;
                    mrr_sum += 1.0 / r as f64;
                }
                per_case.push((case.id.clone(), rank));
            }
        }
    }

    let evaluated = cases.len() - skipped;
    let mrr = if evaluated > 0 { mrr_sum / evaluated as f64 } else { 0.0 };

    // Close all stores.
    for (_, (store, emb_store)) in stores {
        let _ = emb_store.close();
        let _ = store.close();
    }

    AblationResult { label, hits, mrr, evaluated, per_case }
}

/// Print a comparison table across all ablation configs.
fn print_ablation_comparison(results: &[AblationResult]) {
    println!("\n{:=<80}", "");
    println!("ABLATION STUDY — Leave-One-Out Analysis (file mode)");
    println!("{:=<80}\n", "");

    // Summary table.
    println!("| {:<18} | {:>7} | {:>7} | {:>6} |", "Config", "Hit@5", "MRR", "Delta");
    println!("|{:-<20}|{:-<9}|{:-<9}|{:-<8}|", "", "", "", "");

    let baseline_hits = results[0].hits;
    let _baseline_mrr = results[0].mrr;
    for r in results {
        let delta_hits = r.hits as i32 - baseline_hits as i32;
        let delta_str = if r.label == "full" {
            "—".to_string()
        } else {
            format!("{:+}", delta_hits)
        };
        let evaluated = r.evaluated;
        println!(
            "| {:<18} | {:>2}/{:<2} ({:>4.1}%) | {:>7.3} | {:>6} |",
            r.label,
            r.hits,
            evaluated,
            r.hits as f64 / evaluated as f64 * 100.0,
            r.mrr,
            delta_str,
        );
    }

    // Per-case diff: show cases where removing a component changed the outcome.
    println!("\n--- Per-case diffs (changes vs full) ---\n");
    println!("| {:<16} | {:<18} | {} |", "Case", "Full", "Config → Changed");
    println!("|{:-<18}|{:-<20}|{:-<50}|", "", "", "");

    let full = &results[0];
    for (idx, (case_id, full_rank)) in full.per_case.iter().enumerate() {
        let mut diffs: Vec<String> = Vec::new();
        for r in &results[1..] {
            let ablated_rank = &r.per_case[idx].1;
            if ablated_rank != full_rank {
                let f = full_rank.map_or("MISS".to_string(), |r| format!("HIT@{r}"));
                let a = ablated_rank.map_or("MISS".to_string(), |r| format!("HIT@{r}"));
                diffs.push(format!("{}: {}→{}", r.label, f, a));
            }
        }
        if !diffs.is_empty() {
            let full_str = full_rank.map_or("MISS".to_string(), |r| format!("HIT@{r}"));
            println!("| {:<16} | {:<18} | {} |", case_id, full_str, diffs.join(", "));
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Find the 1-based rank (within `top_n`) of the first result whose
/// `file_path` matches any of the ground-truth paths.
///
/// Works for both node mode (each result has `file_path`) and file mode
/// (each result also has `file_path` at the top level).
fn find_hit_rank(
    results: &[serde_json::Value],
    ground_truth_files: &[String],
    top_n: usize,
) -> Option<usize> {
    for (idx, result) in results.iter().take(top_n).enumerate() {
        let file_path = result["file_path"].as_str().unwrap_or("");
        if ground_truth_files.iter().any(|gt| paths_match(file_path, gt)) {
            return Some(idx + 1);
        }
    }
    None
}

/// Return the filename portion of the first ground-truth path (for display).
fn best_gt_filename(ground_truth_files: &[String]) -> String {
    ground_truth_files
        .first()
        .map(|f| {
            let norm = normalize_path(f);
            norm.split('/').next_back().unwrap_or(&norm).to_string()
        })
        .unwrap_or_else(|| "?".to_string())
}

/// Truncate a string to `max_len` characters for table display.
fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}…", &s[..max_len.saturating_sub(1)])
    }
}

// ---------------------------------------------------------------------------
// Test entry points
// ---------------------------------------------------------------------------

/// Node-mode eval: results are individual code nodes ranked by relevance.
/// The Hit@5 check looks for a ground-truth file in the `file_path` field of
/// the top-5 nodes.
#[test]
#[ignore]
fn eval_benchmark_node_mode() {
    run_eval(None);
}

/// File-mode eval: results are aggregated at the file level via fanout+rerank.
/// The Hit@5 check looks for a ground-truth file in the `file_path` field of
/// the top-5 file results.
#[test]
#[ignore]
fn eval_benchmark_file_mode() {
    run_eval(Some("file"));
}

/// Ablation: run both modes back-to-back and print a side-by-side diff of
/// Hit@5 / MRR to surface which routing strategy works better per case.
#[test]
#[ignore]
fn eval_benchmark_ablation() {
    println!("\n### Node mode ###");
    run_eval(None);
    println!("\n### File mode ###");
    run_eval(Some("file"));
}

/// Leave-one-out ablation study: run file mode with each component disabled
/// individually. Produces a comparison table showing each component's
/// marginal contribution to Hit@5 and MRR.
///
/// Run with:
///   cargo test --release --ignored eval_ablation_leave_one_out -- --nocapture
#[test]
#[ignore]
fn eval_ablation_leave_one_out() {
    let components = ["fanout", "expansion", "priors", "scorer", "decomposition", "semantic"];

    // Baseline: all components enabled (not production default).
    println!("Running: all_enabled (baseline)");
    let mut results = vec![run_eval_ablation(&AblationConfig::all_enabled())];

    // Leave-one-out for each component.
    for component in &components {
        println!("Running: -{component}");
        results.push(run_eval_ablation(&AblationConfig::without(component)));
    }

    print_ablation_comparison(&results);
}

/// Regression: vscode-003 must stay Hit@5 with the production default (fanout OFF).
/// This query was a MISS under config D (fanout ON + decomposition ON).
/// Skips gracefully if bench repo is not on disk.
#[test]
fn eval_regression_vscode_003() {
    let repo = "D:\\GitHub\\bench-repos\\vscode";
    if !Path::new(repo).exists() {
        eprintln!("SKIP: vscode bench repo not found at {repo}");
        return;
    }
    let result = code_review_graph::tools::hybrid_query(
        "Keybinding resolver picks the wrong command when two keybindings have the same chord and one has a when clause with a negated context key",
        10, Some(repo), true, None, None, None, Some("file"), Some("thorough"),
    ).expect("hybrid_query should succeed");

    let results = result["results"].as_array().expect("results array");
    let rank = find_hit_rank(results, &["src/vs/platform/keybinding/common/keybindingResolver.ts".to_string()], 5);
    assert!(rank.is_some(), "vscode-003 must be Hit@5; got MISS. Top 5: {:?}",
        results.iter().take(5).map(|r| r["file_path"].as_str().unwrap_or("?")).collect::<Vec<_>>());
    println!("vscode-003: HIT@{}", rank.unwrap());
}

/// Regression: kubernetes-004 must stay Hit@5 with the production default (fanout OFF).
/// This query was a MISS under config D (fanout ON + decomposition ON).
/// Skips gracefully if bench repo is not on disk.
#[test]
fn eval_regression_kubernetes_004() {
    let repo = "D:\\GitHub\\bench-repos\\kubernetes";
    if !Path::new(repo).exists() {
        eprintln!("SKIP: kubernetes bench repo not found at {repo}");
        return;
    }
    let result = code_review_graph::tools::hybrid_query(
        "kubelet volume manager deadlocks when reconciler detaches a volume while attach/detach controller holds the global volume lock",
        10, Some(repo), true, None, None, None, Some("file"), Some("thorough"),
    ).expect("hybrid_query should succeed");

    let results = result["results"].as_array().expect("results array");
    let rank = find_hit_rank(results, &["pkg/kubelet/volumemanager/volume_manager.go".to_string()], 5);
    assert!(rank.is_some(), "kubernetes-004 must be Hit@5; got MISS. Top 5: {:?}",
        results.iter().take(5).map(|r| r["file_path"].as_str().unwrap_or("?")).collect::<Vec<_>>());
    println!("kubernetes-004: HIT@{}", rank.unwrap());
}

/// 2×2 interaction test: fanout × decomposition with everything else fixed.
/// Tests whether the gains from removing fanout and decomposition are
/// independent or overlapping.
///
/// Run with:
///   cargo test --release --ignored eval_ablation_interaction -- --nocapture
#[test]
#[ignore]
fn eval_ablation_interaction() {
    // A: minimal — no fanout, no decomposition (semantic + kw_relaxed + scorer + priors + expansion)
    let a = AblationConfig { fanout: false, decomposition: false, ..AblationConfig::all_enabled() };
    // B: + decomposition only (= current production default)
    let b = AblationConfig { fanout: false, decomposition: true, ..AblationConfig::all_enabled() };
    // C: + fanout only
    let c = AblationConfig { fanout: true, decomposition: false, ..AblationConfig::all_enabled() };
    // D: all enabled (both fanout and decomposition ON)
    let d = AblationConfig::all_enabled();

    let configs: Vec<(&str, AblationConfig)> = vec![
        ("A: minimal", a),
        ("B: +decomp", b),
        ("C: +fanout", c),
        ("D: full", d),
    ];

    let mut results = Vec::new();
    for (label, cfg) in &configs {
        println!("Running: {label}");
        let mut r = run_eval_ablation(cfg);
        r.label = label.to_string();
        results.push(r);
    }

    print_ablation_comparison(&results);

    // Print the 2×2 matrix for quick reading.
    println!("\n--- 2x2 Interaction Matrix (Hit@5 / MRR) ---\n");
    println!("                    | decomp OFF          | decomp ON           |");
    println!("|--------------------|---------------------|---------------------|");
    println!(
        "| fanout OFF          | A: {:>2}/28 / {:.3}     | B: {:>2}/28 / {:.3}     |",
        results[0].hits, results[0].mrr, results[1].hits, results[1].mrr,
    );
    println!(
        "| fanout ON           | C: {:>2}/28 / {:.3}     | D: {:>2}/28 / {:.3}     |",
        results[2].hits, results[2].mrr, results[3].hits, results[3].mrr,
    );
}
