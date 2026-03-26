# Eval & Benchmark Suite

Evaluation and benchmarking infrastructure for code-review-graph MCP tools.

## Gold Eval Set

`gold-eval-set.json` contains 28 hand-curated test cases from real GitHub issues across 6 repositories at different scales:

| Repo | Language | Scale | Cases | Notes |
|------|----------|-------|-------|-------|
| httpx | Python | small (~60 files) | 3 | |
| fastapi | Python | medium (~1.1K files) | 3 | |
| next.js | TS/JS | large (~4.8K files) | 11 | |
| vscode | TS | large (~7K files) | 4 | 1 has broken source URL |
| kubernetes | Go | mega (~17K files) | 4 | 1 has broken source URL |
| rust | Rust | mega (~37K files) | 3 | |

Each case has:
- A natural language query (what an agent would search for)
- Ground truth files (hand-curated root-cause files from PR diffs; some multi-file)
- Relevant files (broader set of related files)
- Category, difficulty, and source issue/PR

### Label tiers
- **Ground truth files**: Files changed in the fix PR (tests, docs, snapshots excluded)
- **Relevant files**: Broader set of related files (weak labels)

### Known issues
- `vscode-001`: Source URL (#236578) points to unrelated PR. GT file unverified.
- `kubernetes-001`: Source URL (#128855) points to unrelated PR. GT file unverified.

## No-MCP Agent Bench

`no-mcp-agent-bench.json` — 30 agent investigations (28 gold + 2 V2 bench issues) using Sonnet with grep/read only (no MCP).

Key results:
- 28/28 gold cases: correct root cause diagnosis
- 22/28 found a GT file (`ground_truth`), 1 found a relevant file, 5 found the mechanism in a different file
- Avg: 85K tokens, 65 tool uses, 8.5 min per case
- Full analysis in `NO_MCP_BENCH_RESULTS.md`

### Schema

`match_tier` values:
- `ground_truth`: Agent found at least one file from `gt_files`
- `relevant`: Agent found a file from `relevant_files` but not `gt_files`
- `mechanism_only`: Agent found the correct bug mechanism but in a file not in GT or relevant
- `none`: Agent did not find the root cause

## Benchmark Repos

Located at `bench-repos/`:

```
bench-repos/
  httpx/          # encode/httpx (Python, small)
  fastapi/        # fastapi/fastapi (Python, medium)
  next.js/        # vercel/next.js (TS/JS, large)
  vscode/         # microsoft/vscode (TS, large)
  kubernetes/     # kubernetes/kubernetes (Go, mega)
  rust/           # rust-lang/rust (Rust, mega)
```

Each repo needs a built graph + embeddings before MCP benchmarking:
```bash
code-review-graph build --repo bench-repos/<repo>
# Then via MCP: embed_graph(repo_root="bench-repos/<repo>")
```

## Running Benchmarks

### Retrieval Eval (lite, automated)
```bash
cargo test --release --features gpu-directml --locked eval_benchmark_file_mode -- --ignored --nocapture
```

### Ablation Study
```bash
cargo test --release --features gpu-directml --locked eval_ablation_leave_one_out -- --ignored --nocapture
cargo test --release --features gpu-directml --locked eval_ablation_interaction -- --ignored --nocapture
```

### Regression Tests (run by default)
```bash
cargo test --release --features gpu-directml --locked eval_regression
```

### Metrics
- **Hit@5**: Does the correct file appear in top 5 search results?
- **MRR**: Mean reciprocal rank of first correct result
- **match_tier**: Did the agent find a GT file, relevant file, or mechanism-only?
- **diagnosis_quality**: excellent / good
- **Total tokens**: Full agent token consumption
- **Tool call count**: MCP calls vs grep/read

### Conditions
- **No MCP**: Agent uses only grep/glob/read (baseline) — completed, results in `no-mcp-agent-bench.json`
- **Rust MCP**: Agent uses code-review-graph MCP tools — pending
- **Python MCP**: Agent uses the Python version — pending

### Prior Results
- `BENCHMARK_2C_RESULTS.md` — Phase 2c retrieval eval + post-fix re-benchmark + Phase 3 results
- `NO_MCP_BENCH_RESULTS.md` — Full no-MCP agent bench analysis
- `bench-repos/BENCHMARK_V2_RESULTS.md` — No MCP vs Old MCP vs New MCP on NextJS issues
