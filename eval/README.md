# Eval & Benchmark Suite

Evaluation and benchmarking infrastructure for code-review-graph MCP tools.

## Gold Eval Set

`gold-eval-set.json` contains hand-curated test cases from real GitHub issues across 6 repositories at different scales:

| Repo | Language | Scale | Cases |
|------|----------|-------|-------|
| httpx | Python | small (~60 files) | 3 |
| fastapi | Python | medium (~1.1K files) | 3 |
| next.js | TS/JS | large (~2.4K files) | 11 |
| vscode | TS | large (~8K files) | TBD |
| kubernetes | Go | mega (~25K files) | TBD |
| rust | Rust | mega (~30K files) | TBD |

Each case has:
- A natural language query (what an agent would search for)
- Ground truth files (hand-curated root-cause files, not raw PR diffs)
- Category, difficulty, and source issue/PR

### Label tiers
- **Ground truth files**: Hand-curated root-cause files only (tests, docs, snapshots excluded)
- **Relevant files**: Broader set of related files (weak labels from PR diffs)

## Benchmark Repos

Located at `D:\GitHub\bench-repos\`:

```
bench-repos/
  httpx/          # encode/httpx (Python, small)
  fastapi/        # fastapi/fastapi (Python, medium)
  next.js/        # vercel/next.js (TS/JS, large)
  vscode/         # microsoft/vscode (TS, large)
  kubernetes/     # kubernetes/kubernetes (Go, mega)
  rust/           # rust-lang/rust (Rust, mega)
```

Each repo needs a built graph before benchmarking:
```bash
cd D:/GitHub/bench-repos/<repo> && code-review-graph build
```

## Running Benchmarks

### Metrics
- **Hit@5**: Does the correct file appear in top 5 search results?
- **MRR**: Mean reciprocal rank of first correct result
- **Fallback rate**: % of queries where agent fell back to grep
- **Time-to-first-correct-file**: Tool calls before reaching root cause file
- **Total tokens**: Full agent token consumption
- **Tool call count**: MCP calls vs grep/read fallbacks

### Conditions
- **No MCP**: Agent uses only grep/glob/read (baseline)
- **With MCP**: Agent uses code-review-graph MCP tools
- **Python MCP**: Agent uses the Python version (`D:\GitHub\code-review-graph\`)

### Prior Results
- `D:\GitHub\bench-repos\BENCHMARK_RESULTS.md` — v1.0.0 latency/quality comparison
- `D:\GitHub\bench-repos\BENCHMARK_V2_RESULTS.md` — No MCP vs Old MCP vs New MCP on NextJS issues
