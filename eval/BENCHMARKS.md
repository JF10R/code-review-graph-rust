# Benchmark Results — Master Summary

Code search tool benchmarks using Claude Sonnet 4.6 in `bypassPermissions` mode, investigating real GitHub issues and reviewing PRs.

**Repos**: [httpx](https://github.com/encode/httpx) (60 files), [FastAPI](https://github.com/fastapi/fastapi) (1.1K files), [Next.js](https://github.com/vercel/next.js) (6.4K files), [VS Code](https://github.com/microsoft/vscode) (7K files), [Kubernetes](https://github.com/kubernetes/kubernetes) (16.9K files), [Rust compiler](https://github.com/rust-lang/rust) (36.8K files)

**Gold eval set**: 28 cases across 6 repos. See `gold-eval-set.json`.

For full methodology, see `BENCHMARK_METHODOLOGY.md`.
For historical round-by-round details, see `BENCHMARK_HISTORY.md`.

## Latest: Round 9 — Clean 4-Way Comparison (2026-04-02)

Clean comparison with all tools working: Grep, Scout MCP, Scout+Graph, CodeDB MCP. 3 agents max per batch for accurate timing. See `BENCHMARK_R9_CLEAN.md`.

### Performance (4 Tier 1 cases, natural v3 prompt)

| Rank | Variant | Avg Time | Avg Tokens | Avg Tools | vs Grep |
|------|---------|----------|------------|-----------|---------|
| 1 | **Scout MCP** | **60.5s** | **36.0K** | **12.8t** | **2.2x faster** |
| 2 | Scout+Graph | 69.3s | 37.3K | 12.3t | 1.9x faster |
| 3 | Grep | 130.6s | 43.3K | 22.0t | 1.0x |
| 4 | CodeDB MCP | 161.7s | 49.5K | 30.8t | 0.8x (slower) |

### Key Finding: Scout MCP is the speed king

Scout MCP is **2.2x faster** than Grep with **17% fewer tokens** and identical quality (4/4 root cause, 12/12 secondaries). Per-case speedup ranges from 1.2x (small repo) to 3.9x (Grep over-explored).

### Key Finding: CodeDB MCP is slower than Grep

CodeDB MCP is **2.7x slower** than Scout MCP and even **1.2x slower than Grep**. Despite sub-millisecond query speed, CodeDB's line-level results trigger 2.4x more tool calls than Scout MCP. Each extra round-trip costs ~3-5s of LLM inference — tool speed doesn't matter when think time is 90% of wall time.

### Key Finding: Scout+Graph is the quality leader

Scout+Graph and CodeDB MCP tied at 13 secondary findings (vs 12 for Scout MCP and Grep). Scout+Graph achieved this at only a 17% time premium over Scout MCP — best quality/speed tradeoff.

**Note**: Prior rounds (R6-R8) used broken tools. CodeDB CLI (segfaults) replaced by CodeDB MCP. R9 is the first clean comparison.

### Re-run: httpx-002 with New Builds (2026-04-06)

After CodeDB rebuild + NPP leak fixes, all MCP tools improved significantly on httpx-002:

| Variant | R9 → Rerun | Tokens | Tools |
|---------|------------|--------|-------|
| CodeDB MCP | 213s → **42s** (5.1x faster) | 53K → 41K | 44t → 9t |
| Scout+Graph | 73s → **31s** (2.4x faster) | 38K → 36K | 14t → 7t |
| Graph MCP | 82s → **34s** (2.4x faster) | 40K → 37K | 17t → 6t |
| Scout MCP | 39s → **27s** (1.4x faster) | 36K → 33K | 7t → 5t |

CodeDB's over-querying problem (44t → 9t) is resolved. All MCP tools now beat Grep (118s) by 2.8–4.4x. Full details in `BENCHMARK_R9_CLEAN.md`.

### New Case: vscode-003 — Complex, 7K files (2026-04-06)

After Tier 1 improvements + #42 conditional structural expansion (strip callers/callees at high confidence):

| Rank | Variant | Time | Tokens | Tools | Secondary (4) |
|------|---------|------|--------|-------|---------------|
| 1 | **Scout MCP** | **115s** | **52K** | **16t** | 3/4 |
| 2 | **Scout+Graph** | 133s | 57K | 21t | 3/4 |
| 3 | **Graph MCP** | 140s | 53K | 17t | **4/4** |
| 4 | CodeDB MCP | 235s | 54K | 19t | 3/4 |
| 5 | Grep (6-way) | 291s | 54K | 25t | 2/4 |

**Key finding: strip noise, keep signal.** Both Scout (4-tier confidence gating) and Graph (#42 conditional expansion) independently proved that stripping structural data at high confidence dramatically improves agent convergence. All three MCP variants now beat Grep by ~2x and cluster at 115-140s. Graph MCP achieved the best quality (4/4 secondary findings) with the fewest tokens (53K).

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
- **Scout MCP**: repo-scout MCP server (persistent)
- **Scout+Graph**: Scout CLI + code-review-graph MCP (structural queries)
- **CodeDB MCP**: CodeDB MCP server (persistent, trigram + symbol index)
- **Graph MCP**: code-review-graph Rust MCP v1.5 (semantic search + call graph, historical only)
- Prompt: `N` = natural v3, `E` = "be efficient" v2

### R9 results (clean, 2026-04-02) — all tools fixed, 3-agent batches

#### Time (seconds) — bold = fastest

| Case | Diff. | Grep | Scout MCP | Scout+Graph | CodeDB MCP |
|------|-------|------|-----------|-------------|------------|
| httpx-002 | Simple | 118 | **39** | 73 | 213 |
| httpx-003 | Medium | 56 | **47** | 67 | 182 |
| httpx-001 | Simple | 237 | **60** | 71 | 122 |
| fastapi-003 | Medium | 111 | **96** | 67 | 130 |
| nextjs-006 | Medium | 258 | **191** | 257 | — |
| nextjs-005 | Simple | **180** | 286 | 199 | — |
| nextjs-004 | Complex | 342 | **108** | 138 | — |
| k8s-003 | Complex | **116** | 428 | 434 | — |
| fastapi-002 | Simple | 267 | **262** | 361 | — |
| vscode-003 | Complex | — | — | — | — |
| rust-002 | Complex | — | — | — | — |

#### Tokens (K) — bold = most efficient

| Case | Diff. | Grep | Scout MCP | Scout+Graph | CodeDB MCP |
|------|-------|------|-----------|-------------|------------|
| httpx-002 | Simple | 42 | **36** | 38 | 53 |
| httpx-003 | Medium | 36 | **35** | 36 | 50 |
| httpx-001 | Simple | 51 | **35** | 36 | 39 |
| fastapi-003 | Medium | 44 | **38** | 39 | 56 |
| nextjs-006 | Medium | **76** | **76** | 81 | — |
| nextjs-005 | Simple | **58** | 84 | 75 | — |
| nextjs-004 | Complex | 69 | 56 | **48** | — |
| k8s-003 | Complex | 51 | **42** | 49 | — |
| fastapi-002 | Simple | **57** | 59 | 66 | — |
| vscode-003 | Complex | — | — | — | — |
| rust-002 | Complex | — | — | — | — |

#### Tool Calls — bold = fewest

| Case | Diff. | Grep | Scout MCP | Scout+Graph | CodeDB MCP |
|------|-------|------|-----------|-------------|------------|
| httpx-002 | Simple | 23 | **7** | 14 | 44 |
| httpx-003 | Medium | 9 | 8 | **10** | 27 |
| httpx-001 | Simple | 30 | 13 | **10** | 24 |
| fastapi-003 | Medium | 26 | 23 | **15** | 28 |
| nextjs-006 | Medium | 60 | **19** | 31 | — |
| nextjs-005 | Simple | 43 | 47 | **37** | — |
| nextjs-004 | Complex | 45 | **21** | 27 | — |
| k8s-003 | Complex | **20** | **18** | 22 | — |
| fastapi-002 | Simple | 45 | 50 | 54 | — |
| vscode-003 | Complex | — | — | — | — |
| rust-002 | Complex | — | — | — | — |

#### Quality: Root Cause Found

| Case | Diff. | Grep | Scout MCP | Scout+Graph | CodeDB MCP |
|------|-------|------|-----------|-------------|------------|
| httpx-002 | Simple | YES | YES | YES | YES |
| httpx-003 | Medium | YES | YES | YES | YES |
| httpx-001 | Simple | YES | YES | YES | YES |
| fastapi-003 | Medium | YES | YES | YES | YES |
| nextjs-006 | Medium | YES | YES | YES | — |
| nextjs-005 | Simple | YES | YES | YES | — |
| nextjs-004 | Complex | YES | YES | YES | — |
| k8s-003 | Complex | YES | YES | YES | — |
| fastapi-002 | Simple | YES | YES | YES | — |
| **Total** | | **9/9** | **9/9** | **9/9** | **4/4** |

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

1. **All tools find the root cause** — 100% primary accuracy (9/9) across every variant, every case
2. **Repo size determines the winner** — Scout MCP wins on small/medium repos (2-4x faster), Grep wins on large repos (>10K files, 2-4x faster)
3. **Scout+Graph is the quality leader** — 30 total secondaries vs 27 for Grep/Scout MCP across 9 cases. Structural queries find connected issues grep misses.
4. **Think time is 86% of wall time** — fewer tool calls > faster tool execution. Scout MCP averages 22 tools (T1+T2) vs Grep's 33.
5. **Large repos hurt MCP variants** — k8s-003 (16.9K files): Grep 116s vs Scout MCP 428s (3.7x slower). MCP query latency scales with repo size.
6. **Complex cases favor Scout MCP** — nextjs-004 (complex history bug): Scout MCP 108s vs Grep 342s (3.2x faster). Ranked results guide investigation more efficiently.
7. **CodeDB MCP is slowest** (Tier 1 only) — 2.7x slower than Scout MCP. Sub-ms queries don't help when line-level results trigger 2.4x more tool calls.
8. **Tool output verbosity drives agent behavior** — Scout MCP's convergence hints let agents stop sooner; verbose tools cause over-exploration
9. **No single tool wins everywhere** — optimal strategy depends on repo size and bug complexity
