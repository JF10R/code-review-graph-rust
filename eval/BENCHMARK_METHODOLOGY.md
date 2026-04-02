# Benchmark Methodology: Code Search Tool Evaluation

## Overview

We benchmark code search tools by measuring their ability to **investigate real bugs** and **review pull requests** in open-source repositories. An LLM agent receives a bug report (or PR diff) and must find the root cause, identify related issues, and suggest a fix — using only the search tool under test.

This is not a retrieval benchmark (find file X). It measures **end-to-end investigation quality**: can the tool guide an agent from a vague bug report to a precise, actionable diagnosis?

## Test Environment

- **Hardware**: Windows 11, RTX 5070 Ti 16GB (DirectML), AMD CPU
- **Model**: Claude Sonnet 4.6 for all agents (identical model across all variants)
- **Mode**: `bypassPermissions` — zero human interaction during the run
- **Measurement**: wall-clock time, total tokens, tool call count, quality scoring

## Gold Eval Set

We maintain a **28-case gold evaluation set** (`eval/gold-eval-set.json`) with documented ground truth for each case. Cases are sourced from real GitHub issues (22), PRs (4), and manual curation (2).

### Repositories

| Repo | Language | Nodes | Files | Cases |
|------|----------|-------|-------|-------|
| [httpx](https://github.com/encode/httpx) | Python | 2.6K | 60 | 3 |
| [fastapi](https://github.com/fastapi/fastapi) | Python | 12.8K | 1.1K | 3 |
| [next.js](https://github.com/vercel/next.js) | TypeScript/Rust | 147K | 6.4K | 11 |
| [vscode](https://github.com/microsoft/vscode) | TypeScript | 183K | 7.0K | 4 |
| [kubernetes](https://github.com/kubernetes/kubernetes) | Go | 210K | 16.9K | 4 |
| [rust](https://github.com/rust-lang/rust) (compiler) | Rust | 286K | 36.8K | 3 |

### Difficulty Distribution

| Difficulty | Count | Examples |
|------------|-------|---------|
| **Simple** | 10 | 1-file guard, missing null check, wrong path separator |
| **Medium** | 11 | Bug propagating through 3+ functions, cache invariant, config interaction |
| **Complex** | 7 | Multi-file deadlock, 5-crate compiler trace, resolver ordering with 4 interacting subsystems |

### Bug Categories

API (6), runtime (4), compiler (3), config (2), bundling (2), routing (2), error-handling (2), editor (2), scheduler (2), css (1), controller (1), kubelet (1).

### Ground Truth Structure

Each case documents:
- **Source**: GitHub issue/PR URL
- **Ground truth files**: exact file(s) containing the root cause
- **Relevant files**: broader set of files involved
- **Root cause description**: precise 1-2 sentence explanation
- **Difficulty**: simple / medium / complex

For the actively benchmarked subset (8-10 cases), we also document **secondary findings** — related issues beyond the primary root cause that a thorough investigation would discover.

## Benchmark Types

### 1. Bug Investigation (primary)

The agent receives a bug report and must find the root cause, trace the causal chain, and suggest a fix. This is the main benchmark type, used across all rounds.

**Actively benchmarked cases** (used in 4-way, 5-way, and 6-way comparisons):

| Case | Repo | Bug | Difficulty |
|------|------|-----|------------|
| httpx-002 | httpx | Empty zstd-encoded response body causes decode error | Simple |
| httpx-003 | httpx | RFC 2069 digest auth fails — qop field handling | Medium |
| fastapi-001 | fastapi | OAuth2 security schemes duplicated in OpenAPI spec | Medium |
| fastapi-003 | fastapi | Form fields with empty string values treated as None | Medium |
| vscode-003 | vscode | Keybinding resolver picks wrong command with negated when clause | Complex |
| nextjs-006 | next.js | LRU cache memory leak — null entries size zero, never evicted | Medium |
| k8s-004 | kubernetes | Volume manager deadlock — lock inversion, 5 files, 2 lock types | Complex |
| rust-001 | rust | Borrow checker confusing error for closure lifetime mismatch | Complex |

**Extended cases** (tested in specific rounds):

| Case | Repo | Bug | Difficulty |
|------|------|-----|------------|
| k8s-002 | kubernetes | PodTopologySpread scoring ignores minDomains | Medium |
| nextjs-complex | next.js | [#89252](https://github.com/vercel/next.js/issues/89252) CSS shared chunk bundling | Medium |
| nextjs-simple | next.js | [#91862](https://github.com/vercel/next.js/issues/91862) SASS precision loss | Medium |

### 2. PR Review

The agent receives a PR diff and must identify real issues — logic bugs, missing edge cases, security concerns, performance problems. This benchmarks review depth and precision.

**Benchmarked PR reviews:**

| Case | Repo | Findings (no-MCP / MCP) | Notes |
|------|------|------------------------|-------|
| vscode-003 review | vscode | ~4 / 5-6 | 3-way comparison (no-MCP, Python MCP, Rust MCP) |
| fastapi-001 review | fastapi | — / 6 | Cache mutation, decorator gap, OpenAPI collision |
| nextjs-001 review | next.js | — / 7 | PostCSS singleton bug, empty Event, regex gaps |
| nextjs-006 review | next.js | — / 5 | NaN infinite loop, silent disk cache failure |

Key finding: MCP-assisted reviews produce ~2x more real findings but at higher token cost. The graph's structural queries ("who calls this changed function?") enable deeper impact analysis.

## Prompt Design

### Base Prompts

Two prompt variants have been used across benchmark rounds:

**Standardized (v2)** — coaching prompt:
```
You are investigating a bug in {REPO_PATH}.

Bug report: "{BUG_DESCRIPTION}"

Find the root cause. Identify specific files and code paths. Suggest a fix.
Do NOT edit any files — research only. Be efficient — find the answer and stop.
```

**Natural (v3)** — minimal prompt:
```
There's a bug in the codebase at {REPO_PATH}.

Bug report: "{BUG_DESCRIPTION}"

Find the issue and suggest a fix. Do NOT edit any files — research only.
```

The v2 "be efficient" instruction measurably reduces exploration time across all variants (~2-3x faster on average). The v3 natural prompt produces more thorough investigations but at higher cost. **All variants in a given round use the same base prompt** — only the tool appendix differs.

### Tool Appendices

Each variant appends tool-specific instructions to the base prompt:

**Grep** (baseline): no appendix — agent uses native Grep + Read tools.

**Scout CLI**:
```
For code search, use `repo-scout query --repo {REPO_PATH} --query "TERM" --json`
via Bash instead of Grep. Use Read to examine files after finding them.
```

**Scout MCP**:
```
For code search, use the MCP tool `mcp__repo-scout__search_code` with
repo="{REPO_PATH}" instead of Grep. Use Read to examine files after finding them.
```

**Code-Review-Graph MCP**:
```
You have access to MCP code-review-graph tools. Use hybrid_query,
open_node_context, query_graph with repo_root="{REPO_PATH}" and compact: true
for structural and semantic code discovery.
```

**CodeDB CLI**:
```
For code search, use CodeDB CLI via Bash instead of Grep:
- Search: codedb.exe "{REPO_PATH}" search "TERM"
- Find symbol: codedb.exe "{REPO_PATH}" find "SymbolName"
- Word lookup: codedb.exe "{REPO_PATH}" word "identifier"
- Outline file: codedb.exe "{REPO_PATH}" outline "path/to/file"
Use Read to examine files after finding them.
```

### Variant Matrix

| Variant | Base prompt | +Scout | +Code-Review-Graph | +CodeDB |
|---------|------------|--------|-------------------|---------|
| Grep | yes | — | — | — |
| Scout CLI | yes | CLI | — | — |
| Scout MCP | yes | MCP | — | — |
| MCP only | yes | — | yes | — |
| Scout+MCP CLI | yes | CLI | yes | — |
| Scout+MCP MCP | yes | MCP | yes | — |
| CodeDB | yes | — | — | yes |

## Quality Evaluation

### Primary Root Cause

Binary: did the agent identify the correct file and root cause? Across all benchmark rounds, **every variant scores 100% on primary root cause** — the bugs are findable with any tool.

The discriminating factors are **speed**, **cost**, and **depth**.

### Secondary Findings

Each case has a set of documented secondary findings — related issues beyond the primary root cause that a thorough investigation would discover. These are the real quality discriminator.

Examples of secondary findings:
- A test that exists but doesn't exercise the buggy path
- A second function with the same bug pattern
- A related subsystem interaction that compounds the issue
- A defensive guard that was added but is bypassed by a different code path

### Opus Quality Scoring (8-case round)

For the full 8-case investigation round, each analysis was scored by Claude Opus on four dimensions (1-5):

| Dimension | What it measures |
|-----------|-----------------|
| **Correctness (C)** | Did it find the right file and root cause? |
| **Depth (D)** | How many secondary issues beyond the primary? |
| **Fix Quality (F)** | Is the suggested fix correct and actionable? |
| **Precision (P)** | Did it avoid false positives and wrong tangents? |

Aggregate results (8 cases, 4 variants):

| Variant | Avg C | Avg D | Avg F | Avg P | Avg Total (/20) |
|---------|-------|-------|-------|-------|------------------|
| Scout | 4.88 | 4.50 | 4.88 | **4.88** | **19.12** |
| MCP | 4.88 | **4.88** | 4.88 | 4.38 | 19.00 |
| Grep | 4.88 | 4.75 | 4.75 | 4.12 | 18.50 |
| Scout+MCP | 4.75 | 4.62 | 4.50 | 4.12 | 18.00 |

Key insight: Scout leads on **precision** (fewest wrong tangents). MCP leads on **depth** (most secondary findings). Grep is strong overall but slower. Scout+MCP scored lowest — the combination caused over-exploration without proportional quality gains.

### PR Review Quality

PR reviews are scored by **real findings count** — issues that represent genuine bugs, missing edge cases, or security concerns (not style nits). Findings are cross-validated against the known ground truth and independent manual review.

## Metrics

### Performance Metrics

| Metric | What it measures |
|--------|-----------------|
| **Wall-clock time** | Total seconds from agent start to completion |
| **Total tokens** | Input + output tokens consumed |
| **Tool calls** | Number of tool invocations (search, read, etc.) |

### Derived Metrics

| Metric | Formula | What it measures |
|--------|---------|-----------------|
| **vs Grep speedup** | grep_time / variant_time | Speed relative to baseline |
| **Findings/min** | (primary + secondary) / (time/60) | Quality throughput |
| **Findings/10K tokens** | (primary + secondary) / (tokens/10K) | Cost efficiency |

### Key Insight: Think Time Dominates

Tool execution is only 10-17% of wall time. The speedup from better search tools comes from **fewer round-trips** — each saved round-trip eliminates ~3.5s of model inference time.

| Component | Grep | Scout CLI |
|-----------|------|-----------|
| Avg tool calls | 45 | 9 |
| Avg tool time | 17s (10%) | 5s (14%) |
| Avg think time | 156s (90%) | 29s (86%) |
| **Total** | **173s** | **34s** |

The 80% reduction in tool calls translates to an 80% reduction in think time — this is the primary driver of Scout's speed advantage, not faster tool execution.

## Benchmark Evolution

### Round 1: MCP v1.3 (2026-03-26)

**Goal**: Validate that our Rust MCP improves over no-MCP and Python MCP.
- 8 cases, 3 variants (no-MCP, Python MCP, Rust MCP)
- Result: Rust MCP 2.0x faster than no-MCP, 2.0x faster than Python MCP
- MCP faster in 8/10 investigation cases

### Round 2: MCP v1.5 (2026-03-27)

**Goal**: Full 3-way comparison with ExactText route, metadata, budget mode.
- 10 investigation cases + 4 PR reviews
- Result: Rust MCP 2.8x faster on shared 4-case set, deeper analysis in 6/10 cases
- PR reviews: MCP produces ~2x more real findings

### Round 3: 4-Way (2026-03-28)

**Goal**: Add Scout (repo-scout) as a fourth variant.
- 8 cases, 4 variants (Grep, MCP, Scout CLI, Scout+MCP CLI)
- Two prompt versions: Standardized (v2) and Natural (v3)
- Result: Scout CLI 4.7x faster than Grep; Scout+MCP best cost-efficiency (5.0 findings/10K tokens)
- Key finding: tool execution is 10-17% of wall time; round-trip reduction is the real driver

### Round 4: 5-Way + CodeDB (2026-04-01)

**Goal**: Add CodeDB as a fifth variant.
- 5 cases, 5 variants (Grep, MCP, Scout CLI, Scout+MCP CLI, CodeDB)
- CodeDB compiled from source for Windows (6 patches, CLI-only — MCP blocked by Zig compiler bug)
- Result: CodeDB slowest (0.6x Grep) due to CLI re-indexing overhead; 61% quality

### Round 5: 6-Way + Scout MCP (2026-04-01)

**Goal**: Test Scout as MCP server vs CLI.
- 5 cases, 7 variants (added Scout MCP, Scout+MCP MCP)
- Result: Scout MCP 7.3x slower than Scout CLI (over-exploration from rich responses)
- Lesson: terse tool output = faster convergence for LLM agents

### Round 6: Standardized "Be Efficient" (2026-04-01)

**Goal**: Fair comparison with identical prompt across Grep, Scout MCP, CodeDB.
- 5 cases, 3 variants, all using v2 "be efficient" prompt
- Eliminates prompt bias from prior cross-round comparisons

### Rounds 7-8: New Cases + Concurrency (2026-04-01)

**Goal**: Expand eval set with new cases; test concurrency limits.
- R7: 5 new cases (nextjs-003/005/007/008, k8s-003), natural v3 prompt
- R8: 20-agent concurrent run — proved >5 agents invalidates timing data
- Key finding: MCP server queueing (30-120s stalls) and API rate-limiting (15-113s gaps) at 20 concurrent agents

### Round 9: Clean 5-Way, Fixed Tools (2026-04-01)

**Goal**: First clean comparison with all tools working correctly.
- 4 Tier 1 cases, 5 variants (Grep, Scout MCP, Scout CLI, Scout+Graph, CodeDB)
- Fixed: Scout cache corruption (rkyv version mismatch), CodeDB segfaults (Windows patches), concurrency (3 agents max)
- Result: Scout MCP 2.2x faster than Grep, 18% fewer tokens, identical quality
- CodeDB fastest on small repo (httpx-003: 42s) but 2.2x slower on medium repo (fastapi re-index tax)
- Ongoing: Tier 2-4 cases pending

## Known Biases and Limitations

### Prompt Sensitivity

The "be efficient — find the answer and stop" instruction in the v2 prompt measurably reduces exploration across all variants (~2-3x). Benchmark rounds should be compared **only within the same prompt version**. Prior cross-round comparisons (e.g., Scout CLI v2 vs Scout MCP v3) are biased.

### Tool Output Verbosity

Tools that return richer per-result context (callers, test files, hints) cause agents to explore more deeply. This can improve quality but often at disproportionate cost. In our 6-way benchmark, Scout MCP (rich output) was 7.3x slower than Scout CLI (terse output) for the same quality level — the agent followed every hint.

**Lesson**: for search tools paired with LLM agents, less context per result often means faster convergence. The agent's reasoning is the bottleneck (86% of wall time), so minimizing round-trips matters more than maximizing per-result context.

### Agent Concurrency — CRITICAL

Running too many agents in parallel inflates wall-clock times from **two independent sources**:

1. **Sonnet/Opus API queueing**: The Anthropic inference API rate-limits concurrent requests. At 20 concurrent agents, even Grep-only agents (no MCP) showed 15-113s inference gaps between tool calls. Normal gap is 2-5s.

2. **MCP server queueing**: repo-scout and code-review-graph are single-process or limited-instance servers. At 20 concurrent agents, MCP tool calls stalled 30-120s each, with total stall times of 200-774s per agent (up to 85% of wall time).

**Evidence from R8 (20 concurrent, 2026-04-01)**:
- Grep nj-003 (no MCP): 917s wall time, but had 260s of API inference gaps → real ~650s
- Scout v2 k8s-003: 558s wall time, 477s MCP stalls (85%) → real ~81s
- Scout nj-005: 955s wall time, 608s MCP stalls → real ~347s

**Concurrency rules for accurate timing**:

| Max concurrent agents | Timing accuracy | When to use |
|-----------------------|-----------------|-------------|
| **3** | Excellent (<5% overhead) | **Standard benchmark runs** |
| **4** | Good (~10% overhead) | 1 case × 4 variants (same repo, acceptable) |
| **5** | Risky (~20%+ overhead) | Only if all Grep/no-MCP variants |
| **>5** | **Unreliable** (30-85% overhead) | Never. Token/tool data still valid but times are garbage |

**Execution protocol**:
1. **Batch by case**: run 1 case × 3 variants (Grep + Scout + CodeDB), wait for completion, then run the 4th (Scout+Graph)
2. **Default to 3 concurrent agents** — this is safe for all variant combinations including MCP-heavy ones
3. **Never exceed 4 concurrent agents** for timing-sensitive runs
4. **Note concurrency** in results: record how many agents ran simultaneously
5. **When contention is suspected**: check agent JSONL timestamps for >30s gaps between tool calls. Sum all gaps >30s = estimated contention overhead. Report both raw wall time and estimated real time.

**Contention-independent metrics**: Token counts and tool call counts are always reliable regardless of concurrency. Use these for quality and efficiency comparisons when timing is contaminated.

### Single-Run Variance

Each case is run once per variant (not averaged over multiple runs). LLM agent behavior is non-deterministic — a different random seed could change tool call counts by 20-30%. Trends across 5+ cases are meaningful; individual case comparisons should be interpreted cautiously.

### CodeDB Windows Limitation

CodeDB has no official Windows support. Our benchmarks use a patched CLI build (6 source patches for Windows compatibility). CodeDB's MCP server mode is unavailable on Windows due to a Zig compiler bug (128MB+ stack frames for mcp.zig). This means CodeDB re-indexes the repo on every CLI invocation — a significant disadvantage vs persistent MCP servers. CodeDB's sub-millisecond query speed (verified: 315us search) would likely perform in the Scout range with a working MCP server.

### Ground Truth Completeness

Secondary findings are documented to the best of our ability, but the ground truth set may not be exhaustive. An agent finding a valid secondary issue not in our ground truth would be scored as 0 — this slightly penalizes deeper investigations.

### Over-Exploration Penalty

More exploration is not always better. In the k8s-004 case (deadlock), Scout+MCP produced the longest analysis (17K chars) but **never converged** on the actual deadlock — ending on a race condition instead. More tool calls and tokens led to worse quality. This is a recurring pattern: the optimal agent behavior is targeted exploration, not exhaustive exploration.

### Model Selection: Sonnet vs Opus for Complex Cases

All benchmarks use Sonnet 4.6 for consistency. However, some cases (notably rust-002: unsound higher-ranked lifetime coercion in the Rust compiler) exceed Sonnet's effective reasoning depth. On rust-002:
- Grep/Sonnet: 24.5 min, 149K tokens, 111 tools — found the right file early but spent 20+ minutes circularly re-deriving type theory conclusions
- Scout MCP/Sonnet: killed after >20 min (same circular reasoning pattern)
- CodeDB/Sonnet: killed after >20 min

The bottleneck is **reasoning depth, not search**. All tools locate `coercion.rs` quickly, but understanding the binder/variance/leak-check interaction requires holding a complex multi-step logical chain that Sonnet cannot sustain without looping.

**Recommendation**: For complex cases (difficulty=complex, especially compiler/type-theory bugs), consider running an additional Opus variant. Opus is more likely to:
- Hold multi-step logical chains without losing the thread
- Recognize when a line of reasoning is circular and pivot
- Reason about code semantics rather than manually simulating execution

A structural search tool (code-review-graph MCP with call graph) could also help by answering "who calls X?" and "what does this function do?" structurally, reducing the reasoning burden. The combination of **Opus + code-review-graph MCP** on rust-002 has never been tested and is the most promising untried configuration.

## Reproducing

### Prerequisites

1. Clone benchmark repos into `bench-repos/` (httpx, fastapi, vscode, next.js, kubernetes, rust)
2. Build and index tools under test (repo-scout, code-review-graph, codedb)
3. Ensure MCP servers are running (for MCP variants)

### Running a Single Case

Each case is run as a Claude Code subagent:

```
Agent(
  model: "sonnet",
  mode: "bypassPermissions",
  prompt: "<base_prompt> + <tool_appendix>",
  run_in_background: true
)
```

The agent's output includes a `BENCHMARK_SUMMARY` block with self-reported tool counts, root cause, fix, files examined, and secondary findings.

Timing and token counts come from the agent task notification metadata (`duration_ms`, `total_tokens`, `tool_uses`).

### Running a Full Round

Launch all cases x variants in parallel (e.g., 5 cases x 3 variants = 15 agents). Ensure sufficient system resources — large repo cases (vscode, next.js) can consume significant memory during indexing.

## File Index

| File | Contents |
|------|----------|
| `gold-eval-set.json` | 28-case gold eval set with ground truth |
| `BENCHMARKS.md` | Master results summary across all rounds |
| `BENCHMARK_MCP_V1.3_RESULTS.md` | Round 1: MCP v1.3 agent comparison |
| `BENCHMARK_V1.5_FULL_RESULTS.md` | Round 2: MCP v1.5 full 3-way + PR reviews |
| `BENCHMARK_4WAY_RESULTS.md` | Round 3: 4-way with Scout |
| `BENCHMARK_4WAY_QUALITY.md` | Round 3: Opus quality scoring (8 cases x 4 variants) |
| `BENCHMARK_4WAY_STANDARDIZED.md` | Round 3: standardized prompt results |
| `BENCHMARK_4WAY_NATURAL.md` | Round 3: natural prompt results |
| `BENCHMARK_5WAY_CODEDB.md` | Round 4: 5-way with CodeDB |
| `BENCHMARK_6WAY_SCOUT_MCP.md` | Round 5: 6-way Scout MCP vs CLI |
| `BENCHMARK_R8_NEWCASES.md` | Round 8: new cases + concurrency analysis |
| `BENCHMARK_R9_CLEAN.md` | Round 9: clean 5-way with fixed tools |
| `benchmark-prompts-4way.md` | Prompt templates and case definitions |
| `BENCHMARK_METHODOLOGY.md` | This file |
