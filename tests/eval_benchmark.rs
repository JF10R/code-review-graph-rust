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

use code_review_graph::paths::normalize_path;

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
