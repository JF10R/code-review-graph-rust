# Eval & Benchmark Suite

Evaluation and benchmarking infrastructure for code search tools (code-review-graph MCP, repo-scout, CodeDB, grep).

## Structure

| File | Purpose |
|------|---------|
| **BENCHMARKS.md** | Master summary — start here |
| **BENCHMARK_METHODOLOGY.md** | How benchmarks work: setup, prompts, metrics, biases |
| **BENCHMARK_R9_CLEAN.md** | Round 9: detailed 5-way comparison with per-case analysis |
| **cases.json** | 14-case eval set with ground truth |
| **results.json** | All benchmark runs (75 entries) with time, tokens, tools, quality |
| **eval-report.py** | Script to generate summary tables from the JSON database |
| **benchmark-prompts-4way.md** | Prompt templates and case definitions |

### Research & Design Docs

| File | Contents |
|------|----------|
| V2_ANALYSIS_*.md | v2 design analysis for MCP and Scout |
| V2_IMPL_SPEC_*.md | v2 implementation specs |
| V2_PRIORITIES.md | v2 feature priorities |
| V2_RESEARCH_LOG.md | v2 research log |

## Eval Set

14 hand-curated cases from real GitHub issues across 7 repos:

| Repo | Language | Scale | Cases |
|------|----------|-------|-------|
| httpx | Python | small (60 files) | 3 |
| fastapi | Python | medium (1.1K files) | 3 |
| httpd | C | medium (562 files) | 1 |
| next.js | TS/JS | large (6.4K files) | 3 |
| vscode | TS | large (7K files) | 1 |
| kubernetes | Go | mega (16.9K files) | 2 |
| rust | Rust | mega (36.8K files) | 1 |

Difficulty: 4 simple, 5 medium, 5 complex.

## Running Benchmarks

Generate summary tables:
```bash
python eval/eval-report.py                    # full summary
python eval/eval-report.py --case httpx-002   # single case
python eval/eval-report.py --variant graph_mcp # single variant
python eval/eval-report.py --round R10        # single round
```

Run a single eval case as a Claude Code subagent:
```
Agent(
  model: "sonnet",
  mode: "bypassPermissions",
  prompt: "<base_prompt> + <tool_appendix>"
)
```

Metrics come from the agent result metadata: `duration_ms`, `total_tokens`, `tool_uses`.

Rules:
- **Sequential only** — never run eval agents in parallel (API 529s skew timing)
- **general-purpose agent type** — not Explore (no MCP access)
- Store results in results.json, summarize in BENCHMARKS.md
