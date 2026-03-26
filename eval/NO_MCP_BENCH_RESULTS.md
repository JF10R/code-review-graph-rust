# No-MCP Agent Bench — Full Results

**Date:** 2026-03-25
**Model:** Claude Sonnet 4.6
**Mode:** bypassPermissions (no user interaction)
**MCP:** Disabled (grep/read only)
**Eval set:** 28 gold eval cases + 2 V2 bench issues = 30 total

## Overall

| Metric | Value |
|--------|-------|
| Correct diagnoses | **30/30 (100%)** |
| Gold: match ground_truth | 22/28 |
| Gold: match relevant | 1/28 |
| Gold: mechanism_only | 5/28 |
| Gold: diagnosis excellent | 24/28 |
| Gold: diagnosis good | 4/28 |
| V2: completed | 2/2 |
| Total tokens | 2.55M |
| Avg tokens | 85K |
| Avg duration | 8.5 min |
| Median duration | 4.8 min |

## Per-Case Results

| # | Case | Repo | Diff | Tokens | Tools | Duration | Match Tier | Quality |
|---|------|------|------|--------|-------|----------|------------|---------|
| 1 | httpx-002 | httpx | simple | 39,770 | 9 | 59s | ground_truth | excellent |
| 2 | httpx-003 | httpx | simple | 37,871 | 11 | 87s | ground_truth | excellent |
| 3 | fastapi-003 | fastapi | simple | 51,944 | 23 | 102s | ground_truth | excellent |
| 4 | nextjs-004 | next.js | complex | 58,327 | 23 | 2.2m | ground_truth | excellent |
| 5 | nextjs-011 | next.js | medium | 44,483 | 25 | 2.2m | ground_truth | excellent |
| 6 | nextjs-006 | next.js | medium | 79,408 | 26 | 2.5m | mechanism | excellent |
| 7 | k8s-003 | kubernetes | complex | 65,022 | 20 | 2.6m | relevant | excellent |
| 8 | k8s-004 | kubernetes | complex | 83,740 | 40 | 3.2m | mechanism | excellent |
| 9 | nextjs-001 | next.js | simple | 59,348 | 59 | 4.3m | ground_truth | excellent |
| 10 | nextjs-005 | next.js | simple | 70,664 | 51 | 4.4m | ground_truth | excellent |
| 11 | complex-v2 | next.js | complex | 77,784 | 52 | 4.5m | v2 (no GT) | excellent |
| 12 | nextjs-010 | next.js | complex | 77,589 | 68 | 4.7m | ground_truth | excellent |
| 13 | vscode-004 | vscode | medium | 64,586 | 47 | 4.7m | ground_truth | excellent |
| 14 | fastapi-002 | fastapi | simple | 52,690 | 40 | 4.8m | ground_truth | excellent |
| 15 | nextjs-002 | next.js | medium | 92,015 | 59 | 5.0m | mechanism | excellent |
| 16 | simple-v2 | next.js | simple | 85,710 | 100 | 6.2m | v2 (no GT) | good |
| 17 | httpx-001 | httpx | simple | 65,281 | 36 | 6.5m | ground_truth | excellent |
| 18 | k8s-002 | kubernetes | complex | 77,894 | 33 | 7.6m | ground_truth | excellent |
| 19 | rust-001 | rust | medium | 70,266 | 59 | 8.2m | ground_truth | excellent |
| 20 | fastapi-001 | fastapi | medium | 94,465 | 57 | 8.3m | ground_truth | excellent |
| 21 | nextjs-007 | next.js | simple | 105,335 | 93 | 9.0m | ground_truth | excellent |
| 22 | nextjs-003 | next.js | simple | 122,562 | 140 | 13.4m | ground_truth | excellent |
| 23 | vscode-003 | vscode | complex | 96,202 | 49 | 13.2m | ground_truth | excellent |
| 24 | vscode-002 | vscode | medium | 114,571 | 79 | 15.9m | ground_truth | excellent |
| 25 | k8s-001 | kubernetes | simple | 128,913 | 115 | 17.5m | ground_truth* | good |
| 26 | rust-003 | rust | medium | 130,883 | 106 | 18.5m | ground_truth | excellent |
| 27 | vscode-001 | vscode | medium | 161,216 | 83 | 20.0m | mechanism* | good |
| 28 | nextjs-008 | next.js | simple | 62,214 | 194 | 33.2m | ground_truth | excellent |
| 29 | rust-002 | rust | complex | 85,712 | 152 | 44.3m | mechanism | excellent |
| 30 | nextjs-009 | next.js | medium | 128,673 | 336 | 54.0m | ground_truth | excellent |

\* = broken source URL in gold eval set

## By Repo Size

| Repo | Files | Cases | Avg Duration | Avg Tokens | Avg Tools |
|------|-------|-------|-------------|-----------|-----------|
| httpx | 60 | 3 | 2.9m | 48K | 19 |
| fastapi | 1,122 | 3 | 4.9m | 66K | 40 |
| next.js | 4,764 | 13 | 9.7m | 79K | 79 |
| vscode | 6,981 | 4 | 13.5m | 109K | 65 |
| kubernetes | 16,933 | 4 | 7.7m | 89K | 52 |
| rust | 36,780 | 3 | 23.7m | 96K | 106 |

## By Difficulty

| Difficulty | Cases | Avg Duration | Avg Tokens |
|------------|-------|-------------|-----------|
| simple | 13 | 7.8m | 72K |
| medium | 10 | 9.8m | 90K |
| complex | 7 | 7.5m | 78K |

## Key Observations

1. **100% correct diagnosis rate.** Sonnet with grep/read finds the root cause for every case,
   including complex concurrency bugs (k8s deadlock), type system soundness holes (rust coercion),
   and multi-system interactions (nextjs PPR+cacheComponents+standalone).

2. **Repo size correlates weakly with cost.** httpx (60 files) averages 48K tokens; rust (37K files)
   averages 96K — only 2x more despite 600x more files. The agent navigates large repos efficiently
   via targeted grep.

3. **Difficulty does NOT correlate with cost.** Complex cases (7.5m avg) are actually faster than
   medium (9.8m). The hardest cases by wall-clock time are cases with subtle bugs requiring
   exhaustive path elimination (nextjs-009: 54m, nextjs-008: 33m), not necessarily "complex" bugs.

4. **Match tier breakdown**: 22/28 gold cases found a GT file (`ground_truth`), 1 found a relevant
   file (`relevant`), 5 found the correct mechanism in a non-GT file (`mechanism_only`).
   After cross-referencing with PR diffs: 2 cases had genuinely multi-file fixes (nextjs-004,
   nextjs-010 — GT expanded). The mechanism_only cases found correct root causes at a different
   layer (e.g., vscode-001 found UI layer, GT is config service layer).

5. **2 eval cases have broken source URLs** (vscode-001, k8s-001) — the stored issue numbers
   resolve to unrelated PRs. The agents still produced correct diagnoses. k8s-001 agent correctly
   determined the stated off-by-one doesn't exist in the current code snapshot.

## Implications for MCP Comparison

The no-MCP baseline sets a very high bar:
- **Correctness** is already 100% — MCP cannot improve this, only match it.
- **Efficiency** is the comparison axis: can MCP reduce tokens, tool calls, or wall-clock time?
- The hardest cases (nextjs-008: 194 tools, nextjs-009: 336 tools) are where MCP has the most
  opportunity to help — if semantic search can shortcut the grep-exploration phase.
- Small repos (httpx, fastapi) are already fast enough that MCP overhead may not justify itself.
- The sweet spot for MCP value is likely medium-to-large repos (vscode, kubernetes, next.js)
  with complex bugs that require understanding call chains or component interactions.

## Prompt Template

```
You are investigating a bug in the codebase at {repo_root}

Bug report: "{query from gold-eval-set.json}"

Find the root cause. Identify specific files and code paths. Suggest a fix.
Do NOT edit any files — research only.
```

Full prompts saved in `eval/no-mcp-prompts.md`.
Full per-case data in `eval/no-mcp-agent-bench.json`.
