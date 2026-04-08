# Eval & Benchmark Suite

Evaluation and benchmarking infrastructure for code search tools (code-review-graph MCP, repo-scout, CodeDB, grep).

## Structure

| File | Purpose |
|------|---------|
| **BENCHMARKS.md** | Master summary — start here |
| **BENCHMARK_METHODOLOGY.md** | How benchmarks work: setup, prompts, metrics, biases |
| **BENCHMARK_HISTORY.md** | Consolidated results from all rounds (R1-R6) |
| **BENCHMARK_R6_EFFICIENT.md** | Latest round: fair 3-way with "be efficient" prompt |
| **gold-eval-set.json** | 28-case gold eval set with ground truth |
| **benchmark-prompts-4way.md** | Prompt templates and case definitions |

### Historical Round Files (archived, referenced from BENCHMARK_HISTORY.md)

| File | Round | Date |
|------|-------|------|
| BENCHMARK_2C_RESULTS.md | Pre-1: retrieval eval | 2026-03-25 |
| NO_MCP_BENCH_RESULTS.md | Pre-1: no-MCP baseline | 2026-03-25 |
| BENCHMARK_MCP_V1.3_RESULTS.md | R1: MCP 3-way | 2026-03-26 |
| BENCHMARK_V1.5_FULL_RESULTS.md | R2: MCP v1.5 + PR reviews | 2026-03-27 |
| BENCHMARK_4WAY_RESULTS.md | R3: 4-way best runs | 2026-03-28 |
| BENCHMARK_4WAY_STANDARDIZED.md | R3a: standardized prompt | 2026-03-28 |
| BENCHMARK_4WAY_NATURAL.md | R3b: natural prompt | 2026-03-28 |
| BENCHMARK_4WAY_QUALITY.md | R3c: Opus quality scoring | 2026-03-28 |
| BENCHMARK_5WAY_CODEDB.md | R4: 5-way + CodeDB | 2026-04-01 |
| BENCHMARK_6WAY_SCOUT_MCP.md | R5: 6-way Scout MCP server | 2026-04-01 |

### Research & Design Docs

| File | Contents |
|------|----------|
| AGENT_PATTERN_ANALYSIS.md | Agent tool usage pattern research |
| V2_ANALYSIS_*.md | v2 design analysis for MCP and Scout |
| V2_IMPL_SPEC_*.md | v2 implementation specs |
| V2_PRIORITIES.md | v2 feature priorities |
| V2_RESEARCH_LOG.md | v2 research log |

## Gold Eval Set

28 hand-curated cases from real GitHub issues across 6 repos:

| Repo | Language | Scale | Cases |
|------|----------|-------|-------|
| httpx | Python | small (60 files) | 3 |
| fastapi | Python | medium (1.1K files) | 3 |
| next.js | TS/JS | large (6.4K files) | 11 |
| vscode | TS | large (7K files) | 4 |
| kubernetes | Go | mega (16.9K files) | 4 |
| rust | Rust | mega (36.8K files) | 3 |

Difficulty: 10 simple, 11 medium, 7 complex.

### Known Issues
- `vscode-001`: Source URL (#236578) points to unrelated PR. GT file unverified.
- `kubernetes-001`: Source URL (#128855) points to unrelated PR. GT file unverified.
