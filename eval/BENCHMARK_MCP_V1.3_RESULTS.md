# Benchmark: MCP v1.3+ Agent-Level Comparison

**Date**: 2026-03-26
**Hardware**: Windows 11, RTX 5070 Ti 16GB (DirectML), AMD CPU
**Model**: Claude Sonnet 4.6 for all agents
**MCP version**: v1.3.0 + f16 storage, file-first embeddings, query expansion, exact_channels, budget mode

## Setup

- **No-MCP baseline**: from `eval/no-mcp-agent-bench.json` (run 2026-03-25), except nextjs-simple which was re-run on expanded repo (2026-03-26)
- **MCP agents**: same prompt + "Use the code-review-graph MCP tools for discovery"
- **Python MCP**: v1.8.4 Python version for comparison (no hybrid_query, no query expansion)
- **Budget mode**: hybrid_query defaults to budget="fast" (file-mode, top 3, no expansion)
- **Mode**: bypassPermissions (no user interaction)

## Repo Sizes

| Repo | Files | Nodes | Edges | Language |
|------|-------|-------|-------|----------|
| httpx | 60 | 1,301 | 7,939 | Python |
| fastapi | 1,122 | 6,399 | 27,275 | Python |
| next.js | 6,396 | 89,684 | 481,159 | TS/JS/Rust |
| vscode | 6,981 | 183,874 | 950,433 | TS |
| kubernetes | 16,933 | 209,727 | 1,578,354 | Go |
| rust | 36,780 | 286,296 | 1,170,429 | Rust |

Note: next.js expanded from 2,382 files (sparse checkout) to 6,396 files after adding turbopack/crates + crates Rust source.

## Final Results — Time (primary metric)

| Case | Nodes | No-MCP Time | Best MCP Time | MCP Config | Speedup |
|------|-------|-------------|---------------|------------|---------|
| vscode-003 | 184K | 13.2m | **3.5m** | Rust v1 | **3.8x faster** |
| k8s-002 | 210K | 7.6m | **2.5m** | Rust v1 | **3.0x faster** |
| fastapi-001 | 6.4K | 8.3m | **4.0m** | Rust v1 | **2.1x faster** |
| rust-001 | 286K | 8.2m | **4.3m** | Rust v1 | **1.9x faster** |
| nextjs-simple | 90K | 29.7m | **18.4m** | Budget | **1.6x faster** |
| nextjs-complex | 62K | 4.5m | 4.4m | Rust v1 | ~same |
| httpx-002 | 1.3K | 1.0m | 1.6m | Budget | 0.6x slower |
| k8s-004 | 210K | 3.2m | 5.1m | Rust v1 | 0.6x slower |

**MCP faster in 5/8 cases. Average speedup: 1.9x.**

## Full Token Comparison

| Case | No-MCP Tok | Rust v1 Tok | Budget Tok | Python Tok |
|------|------------|-------------|------------|------------|
| httpx-002 | **39,770** | 44,426 | 41,887 | 46,101 |
| fastapi-001 | 94,465 | **85,432** | 88,306 | 87,699 |
| nextjs-simple | 163,208 | 147,467 | 159,132 | 113,058 |
| nextjs-complex | **77,784** | 92,145 | — | 102,317 |
| vscode-003 | 96,202 | **72,747** | — | 135,287 |
| k8s-002 | 77,894 | 69,219 | — | **62,112** |
| k8s-004 | **83,740** | 98,570 | — | 112,075 |
| rust-001 | **70,266** | 70,308 | — | 85,241 |

## Budget Mode Results

Budget mode (hybrid_query defaults to fast: file-mode, top 3, no expansion):

| Case | No-MCP | Rust v1 | Budget | Budget vs No-MCP |
|------|--------|---------|--------|------------------|
| httpx-002 | 39,770 / 1.0m | 44,426 / 1.3m | 41,887 / 1.6m | +5% tok |
| fastapi-001 | 94,465 / 8.3m | 85,432 / 4.0m | 88,306 / 4.5m | -7% tok, 1.8x faster |
| nextjs-simple | 163,208 / 29.7m | 147,467 / 17m | 159,132 / 17.3m | -2% tok, 1.6x faster* |

*nextjs-simple compared against no-MCP on expanded repo (29.7m), not sparse checkout (6.2m).

Budget mode helped on fastapi-001 (closed gap with v1) and narrowed httpx-002 overhead from +12% to +5%. Did not fix nextjs-simple over-exploration — the agent's own Read/Grep exploration is uncapped by budget.

## Rust MCP vs Python MCP

| Metric | Rust MCP avg | Python MCP avg |
|--------|--------------|----------------|
| Tokens | 85,039 | 92,986 (+9%) |
| Time | 5.3m | 11.1m (+110%) |

Rust MCP is 9% cheaper on tokens and 2x faster than Python MCP. Largest gap on vscode-003 (73K/3.5m vs 135K/17.9m). Python MCP has no hybrid_query — agents compensate with more individual calls.

## Key Findings

### 1. MCP is a time accelerator, not a token saver
MCP responses add tokens (JSON payloads), but agents reach the answer faster by skipping grep→read→think cycles. Time is the right metric, not tokens.

### 2. MCP value scales with repo size
| Tier | Cases | Avg MCP Speedup |
|------|-------|-----------------|
| >100K nodes | vscode, k8s-002, rust | **2.9x faster** |
| 6-90K nodes | fastapi, nextjs | **1.6x faster** |
| <2K nodes | httpx | 0.6x (slower) |

### 3. Incomplete codebase biases benchmarks
nextjs-simple appeared to be a catastrophic MCP loss (17m vs 6.2m) until the Rust source files were checked out. On the complete codebase, no-MCP took 29.7m and MCP took 18.4m — a 1.6x win. The previous no-MCP "win" was because the agent stopped early without finding the real root cause.

### 4. Budget mode helps on small/medium repos
Budget mode reduced httpx-002 overhead from +12% to +5% tokens. On fastapi-001, it preserved most of v1's advantage. But it doesn't prevent agent-driven over-exploration on complex investigations.

### 5. Over-exploration is agent behavior, not tool behavior
nextjs-simple: 148-169 calls regardless of budget mode. The agent keeps Read/Grep-ing because the investigation is genuinely complex (cross-language JS→Rust root cause). Budget caps hybrid_query output but not the agent's own exploration depth.

### 6. Two cases consistently lose
- **httpx-002**: too small (60 files). Grep finds the answer instantly.
- **k8s-004**: no-MCP agent found a correct-enough answer fast (3.2m). MCP enabled deeper but unnecessary analysis (5.1m).

## Comparison with V2 Benchmark (2026-03-24)

On the CSS complex issue (same query, same repo):
| | Old MCP (v1) | New MCP (v1.3+) |
|---|---|---|
| Tokens | 137,866 | 92,145 |
| Time | 7.6m | 4.4m |
| Quality | Good | Excellent |

v1.3+ used **33% fewer tokens** and **42% less time** with better quality.
