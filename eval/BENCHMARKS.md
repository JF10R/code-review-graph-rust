# Benchmark Results — Master Summary

Code search tool benchmarks using Claude Sonnet 4.6 in `bypassPermissions` mode, investigating real GitHub issues and reviewing PRs.

**Repos**: [httpx](https://github.com/encode/httpx) (60 files), [FastAPI](https://github.com/fastapi/fastapi) (1.1K files), [Next.js](https://github.com/vercel/next.js) (6.4K files), [VS Code](https://github.com/microsoft/vscode) (7K files), [Kubernetes](https://github.com/kubernetes/kubernetes) (16.9K files), [Rust compiler](https://github.com/rust-lang/rust) (36.8K files)

**Gold eval set**: 28 cases across 6 repos. See `gold-eval-set.json`.

For full methodology, see `BENCHMARK_METHODOLOGY.md`.
For historical round-by-round details, see `BENCHMARK_HISTORY.md`.

## Latest: Round 9 — Clean 5-Way, Fixed Tools (2026-04-01)

Clean re-run with fixed tools: Scout MCP (persistent server), CodeDB CLI (Windows patches applied), Grep baseline, Scout+Graph (Scout CLI + code-review-graph MCP). 3 agents max per batch for accurate timing. See `BENCHMARK_R9_CLEAN.md`.

### Performance (4 Tier 1 cases, natural v3 prompt)

| Variant | Avg Time | Avg Tokens | Avg Tools | vs Grep |
|---------|----------|------------|-----------|---------|
| **Scout MCP** | **60.5s** | **35.5K** | **12.8t** | **2.2x faster** |
| Scout CLI | 65.5s | 39.0K | 11.5t | 2.0x faster |
| Scout+Graph | 70.7s | 38.0K | 15.0t | 1.8x faster |
| CodeDB | 124.3s | 40.0K | 16.5t | 1.1x faster |
| Grep | 130.6s | 43.3K | 22.0t | 1.0x |

### Key Finding: Scout MCP vs Grep

Scout MCP is **2.2x faster** with **18% fewer tokens** and identical quality (4/4 root cause, 12/12 secondaries). Per-case speedup ranges from 1.2x (small repo, simple bug) to 3.9x (Grep over-explored with Python experiments).

### Key Finding: CodeDB Re-Index Tax

CodeDB has sub-millisecond query speed but re-indexes on every CLI call (no persistent server on Windows). On httpx (60 files, 270ms re-index), CodeDB won httpx-003 outright (41.6s, fastest). On fastapi (1.1K files, 3.6s re-index), CodeDB was 2.2x slower than Scout MCP. Gap will widen on larger repos.

### Key Finding: Scout MCP ≈ Scout CLI on Small/Medium Repos

Scout MCP is 8% faster than Scout CLI on average — marginal on small repos. The persistent MCP server avoids process-spawn overhead (~300ms/call) but agents compensate by making fewer CLI calls. Expect MCP advantage to grow on larger repos.

**Note**: Prior rounds (R6-R8) used broken tool builds. R6-R7 Scout was v1 (pre-confidence-gating). R8 had 20-agent concurrency contamination. R9 is the first clean comparison with all tools working correctly.

## Previous: Round 7 — Natural Prompt, New Cases (2026-04-01)

5 new cases with natural v3 prompt (no coaching). See `BENCHMARK_R7_NATURAL.md`.

**Warning**: R7 Scout MCP used v1 (no confidence gating) and R7 timing is contaminated by 20-agent concurrency. Token/tool counts are reliable; wall times are not.

| Variant | Avg Time* | Avg Tokens | Avg Tools |
|---------|----------|------------|-----------|
| CodeDB | 209s | 57K | 34t |
| Grep | 241s | 59K | 39t |
| Scout MCP v1 | 752s* | 69K | 34t |

*Times unreliable due to concurrency.

## Previous: Round 6 — Fair "Be Efficient" (2026-04-01)

Identical v2 "be efficient" prompt. 5 original cases. See `BENCHMARK_R6_EFFICIENT.md`.

| Variant | Avg Time | Quality % |
|---------|----------|-----------|
| Grep | 170s | **78%** |
| Scout MCP | 170s | 74% |
| CodeDB | 204s | 61% |

Scout MCP wins on complex logic (vscode-003: 4/4 in 368s vs Grep 2/4 in 571s). Grep wins on cross-file breadth and overall quality.

## Comprehensive Results Grid

All per-case results across tools. Format: `time / tokens / tools`. Bold = fastest on that case.

### Legend

- **Grep**: Native Grep + Read (baseline)
- **Scout MCP**: repo-scout MCP server (persistent, 2026-04-01 build)
- **Scout CLI**: repo-scout CLI via Bash (same binary, process-per-call)
- **Scout+Graph**: Scout CLI + code-review-graph MCP (structural queries)
- **CodeDB**: codedb CLI (Windows patched build, re-indexes per invocation)
- **Graph MCP**: code-review-graph Rust MCP v1.5 (semantic search + call graph)
- Prompt: `N` = natural v3, `E` = "be efficient" v2

### R9 results (clean, 2026-04-01) — all tools fixed, 3-agent batches

#### Time (seconds) — bold = fastest

| Case | Diff. | Grep | Scout MCP | Scout CLI | Scout+Graph | CodeDB |
|------|-------|------|-----------|-----------|-------------|--------|
| httpx-002 | Simple | 118 | 39 | **34** | 56 | 165 |
| httpx-003 | Medium | 56 | 47 | 75 | 78 | **42** |
| httpx-001 | Simple | 237 | 60 | **40** | 68 | 77 |
| fastapi-003 | Medium | 111 | 96 | 113 | **81** | 214 |
| nextjs-006 | Medium | — | — | — | — | — |
| nextjs-005 | Simple | — | — | — | — | — |
| nextjs-004 | Complex | — | — | — | — | — |
| k8s-003 | Complex | — | — | — | — | — |
| vscode-003 | Complex | — | — | — | — | — |
| rust-002 | Complex | — | — | — | — | — |

#### Tokens (K) — bold = most efficient

| Case | Diff. | Grep | Scout MCP | Scout CLI | Scout+Graph | CodeDB |
|------|-------|------|-----------|-----------|-------------|--------|
| httpx-002 | Simple | 42 | 36 | 37 | 37 | 48 |
| httpx-003 | Medium | 36 | **33** | 38 | 38 | **33** |
| httpx-001 | Simple | 51 | 35 | **33** | 35 | 39 |
| fastapi-003 | Medium | 44 | **38** | 48 | 42 | 40 |
| nextjs-006 | Medium | — | — | — | — | — |
| nextjs-005 | Simple | — | — | — | — | — |
| nextjs-004 | Complex | — | — | — | — | — |
| k8s-003 | Complex | — | — | — | — | — |
| vscode-003 | Complex | — | — | — | — | — |
| rust-002 | Complex | — | — | — | — | — |

#### Tool Calls — bold = fewest

| Case | Diff. | Grep | Scout MCP | Scout CLI | Scout+Graph | CodeDB |
|------|-------|------|-----------|-----------|-------------|--------|
| httpx-002 | Simple | 23 | 7 | **5** | 14 | 28 |
| httpx-003 | Medium | 9 | 8 | **5** | 13 | **6** |
| httpx-001 | Simple | 30 | 13 | **6** | 13 | 14 |
| fastapi-003 | Medium | 26 | 23 | 30 | **20** | 18 |
| nextjs-006 | Medium | — | — | — | — | — |
| nextjs-005 | Simple | — | — | — | — | — |
| nextjs-004 | Complex | — | — | — | — | — |
| k8s-003 | Complex | — | — | — | — | — |
| vscode-003 | Complex | — | — | — | — | — |
| rust-002 | Complex | — | — | — | — | — |

#### Quality: Root Cause Found

| Case | Diff. | Grep | Scout MCP | Scout CLI | Scout+Graph | CodeDB |
|------|-------|------|-----------|-----------|-------------|--------|
| httpx-002 | Simple | YES | YES | YES | YES | YES |
| httpx-003 | Medium | YES | YES | YES | YES | YES |
| httpx-001 | Simple | YES | YES | YES | YES | YES |
| fastapi-003 | Medium | YES | YES | YES | YES | YES |
| **Total** | | **4/4** | **4/4** | **4/4** | **4/4** | **4/4** |

### Historical R6-R7 results (older tools, mixed prompts)

**Warning**: R6-R7 used Scout MCP v1 (no confidence gating), broken CodeDB (segfault on Windows), and R8 had 20-agent concurrency contamination. Retained for reference but not directly comparable to R9.

| Case | Diff. | Prompt | Grep | Scout MCP v1 | CodeDB (broken) |
|------|-------|--------|------|--------------|-----------------|
| httpx-001 | Simple | N | 149 | 52 (v2) | 101 |
| httpx-002 | Simple | E | 46 | 56 | 46 |
| httpx-003 | Medium | E | 47 | 88 | 48 |
| fastapi-002 | Simple | N | 490 | 294 (v2) | 328 |
| fastapi-003 | Medium | E | 38 | 72 | 74 |
| vscode-003 | Complex | E | 571 | 368 | 706 |
| nextjs-004 | Complex | N | 121 | 1319† | 261 |
| nextjs-006 | Medium | E | 150 | 267† | 147 |
| k8s-003 | Complex | N | 204 | 770† | 144 |
| rust-002 | Complex | N | 1471 | 984 (v2) | killed |

† Contaminated by concurrency. Token/tool counts reliable, times are not.

### Historical results (R1-R2, 2026-03-26/27) — Graph MCP available

These results used earlier prompts (not directly comparable to R6-R7) but are the only runs with Graph MCP (code-review-graph).

| Case | Diff. | Grep (no-MCP) | Graph MCP (Rust v1.5) | Speedup |
|------|-------|--------------|----------------------|---------|
| httpx-002 | Simple | 40K/59s/9t | 40K/84s/14t | 0.7x |
| httpx-003 | Medium | 38K/87s/11t | 39K/72s/12t | **1.2x** |
| fastapi-001 | Medium | 94K/8.3m/57t | **76K/3.9m/33t** | **2.1x** |
| fastapi-003 | Medium | 52K/102s/23t | **43K/73s/13t** | **1.4x** |
| vscode-003 | Complex | 96K/13.2m/49t | **60K/2.7m/24t** | **4.9x** |
| k8s-002 | Medium | 78K/7.6m/33t | **59K/3.2m/25t** | **2.4x** |
| k8s-004 | Complex | 84K/3.2m/40t | 85K/4.1m/38t | 0.78x |
| rust-001 | Complex | 70K/8.2m/59t | **89K/5.6m/50t** | **1.5x** |

Graph MCP faster in **6/8 cases**, avg **2.0x speedup**. Strongest on large repos (vscode 4.9x, k8s-002 2.4x).

### PR Reviews (R2, Graph MCP only)

| Case | No-MCP | Graph MCP | Real findings |
|------|--------|-----------|---------------|
| vscode-003 | 56K/1.9m/~4 findings | 59K/1.9m/**6 findings** | +50% |
| fastapi-001 | 63K/2.0m | 86K/4.6m/**6 findings** | — |
| nextjs-001 | 57K/3.7m | 62K/3.0m/**7 findings** | — |
| nextjs-006 | 43K/1.9m | 58K/2.4m/**5 findings** | — |

### Scout MCP v2 Impact (confidence-gated metadata)

| Case | Scout v1 | Scout v2 | Speedup | vs Grep |
|------|---------|---------|---------|---------|
| httpx-001 | 298s/35K/12t | **52s/38K/10t** | **5.7x** | **2.9x faster** |
| fastapi-002 | 619s/58K/33t | **294s/66K/45t** | **2.1x** | **1.7x faster** |
| rust-002 | killed | **984s/120K/109t** | survived | **1.5x faster** |

### What's Missing (R9 clean runs)

| Case | Diff. | Grep | Scout MCP | Scout CLI | Scout+Graph | CodeDB |
|------|-------|------|-----------|-----------|-------------|--------|
| httpx-002 | Simple | R9 ✓ | R9 ✓ | R9 ✓ | R9 ✓ | R9 ✓ |
| httpx-003 | Medium | R9 ✓ | R9 ✓ | R9 ✓ | R9 ✓ | R9 ✓ |
| httpx-001 | Simple | R9 ✓ | R9 ✓ | R9 ✓ | R9 ✓ | R9 ✓ |
| fastapi-003 | Medium | R9 ✓ | R9 ✓ | R9 ✓ | R9 ✓ | R9 ✓ |
| nextjs-006 | Medium | pending | pending | — | pending | pending |
| nextjs-005 | Simple | pending | pending | — | pending | pending |
| nextjs-004 | Complex | pending | pending | — | pending | pending |
| k8s-003 | Complex | pending | pending | — | pending | pending |
| fastapi-002 | Simple | pending | pending | — | pending | pending |
| nextjs-003 | Simple | pending | pending | — | pending | pending |
| nextjs-007 | Simple | pending | pending | — | pending | pending |
| nextjs-008 | Simple | pending | pending | — | pending | pending |
| vscode-003 | Complex | pending | pending | — | pending | pending |
| rust-002 | Complex | pending | pending | — | pending | pending |

**4/14 cases complete.** Scout CLI column will not be continued (Scout MCP replaces it). Priority: Tier 2 cases (nextjs-006 through k8s-003), then Tier 3 (complex), then rust-002 (monster).

## PR Review Results

| Case | No-MCP | MCP Findings | Notes |
|------|--------|-------------|-------|
| vscode-003 | ~4 | 5 (Python), 6 (Rust) | 3-way comparison |
| fastapi-001 | — | 6 | Cache mutation, decorator gap |
| nextjs-001 | — | 7 | PostCSS singleton, regex gaps |
| nextjs-006 | — | 5 | NaN infinite loop |

MCP reviews produce ~2x more real findings at higher token cost.

## Key Findings

1. **All tools find the root cause** — 100% primary accuracy across every variant, every round
2. **Think time is 86% of wall time** — fewer tool calls > faster tool execution
3. **Scout MCP is 2.2x faster than Grep** (R9) — with 18% fewer tokens and identical quality
4. **CodeDB is repo-size-sensitive** — fastest on small repos (httpx-003: 42s), slowest on medium+ due to re-indexing tax (3.6s/call on fastapi)
5. **Scout MCP ≈ Scout CLI on small repos** — MCP's persistent connection saves ~8% on average; gap expected to grow on larger repos
6. **Scout+Graph produces the cleanest fixes** — structural queries enable deeper impact analysis, but at 17% time premium over Scout MCP
7. **Over-exploration is the #1 failure mode** — more tool calls often = worse quality (CodeDB httpx-002: 28 tools, 165s vs Scout MCP: 7 tools, 39s)
8. **Prompt matters as much as tools** — v2 "be efficient" eliminates 80% of speed variance (R6-R7 finding, still relevant)
9. **Prior rounds had broken tools** — R6-R8 CodeDB segfaulted, Scout cache was corrupt, R8 had 20-agent concurrency. R9 is the first clean comparison.
