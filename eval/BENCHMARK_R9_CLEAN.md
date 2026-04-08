# Round 9: Clean 4-Way Comparison (2026-04-02)

**Date**: 2026-04-01 (Grep/Scout MCP/Scout+Graph), 2026-04-02 (CodeDB MCP)
**Hardware**: Windows 11, RTX 5070 Ti 16GB (CUDA), AMD CPU
**Model**: Claude Sonnet 4.6 for all agents
**Mode**: bypassPermissions (no user interaction)
**Prompt**: Natural v3 (no coaching)
**Concurrency**: 3 agents max per batch (clean timing, <5% overhead)

## Variants

| Variant | Tool | Persistent? | Notes |
|---------|------|-------------|-------|
| **Grep** | Native Grep + Read | N/A | Baseline — no external tools |
| **Scout MCP** | `mcp__repo-scout__search_code` | Yes (MCP server) | repo-scout v0.1.0 |
| **Scout+Graph** | Scout CLI + code-review-graph MCP | Partial | Scout CLI + graph MCP hybrid |
| **CodeDB MCP** | `mcp__codedb__codedb_*` | Yes (MCP server) | CodeDB with persistent index |

### Tool Characteristics

| Tool | Index time (httpx/fastapi) | Query time | Per-call overhead |
|------|---------------------------|------------|-------------------|
| Grep | N/A | ~1ms | ~200ms (Bash spawn) |
| Scout MCP | 0 (persistent) | ~5ms | ~50ms (IPC) |
| CodeDB MCP | 2.5s / 48s (first index) | **0.6ms / 3.9ms** | ~50ms (IPC, persistent after index) |
| Graph MCP | 0 (persistent) | ~10ms | ~100ms (IPC + embedding) |

## Results

### Tier 1: Time (seconds) — bold = fastest

| Case | Diff. | Grep | Scout MCP | Scout+Graph | CodeDB MCP |
|------|-------|------|-----------|-------------|------------|
| httpx-002 | Simple | 118 | **39** | 73 | 213 |
| httpx-003 | Medium | 56 | **47** | 67 | 182 |
| httpx-001 | Simple | 237 | **60** | 71 | 122 |
| fastapi-003 | Medium | 111 | **96** | 67 | 130 |
| **T1 Average** | | **131** | **61** | **69** | **162** |

### Tier 2: Time (seconds) — bold = fastest

| Case | Diff. | Repo | Grep | Scout MCP | Scout+Graph |
|------|-------|------|------|-----------|-------------|
| nextjs-006 | Medium | next.js (6.4K) | 258 | **191** | 257 |
| nextjs-005 | Simple | next.js (6.4K) | **180** | 286 | 199 |
| nextjs-004 | Complex | next.js (6.4K) | 342 | **108** | 138 |
| k8s-003 | Complex | kubernetes (16.9K) | **116** | 428 | 434 |
| fastapi-002 | Simple | fastapi (1.1K) | 267 | **262** | 361 |
| **T2 Average** | | | **233** | **255** | **278** |

### Tier 1: Tokens (K) — bold = most efficient

| Case | Diff. | Grep | Scout MCP | Scout+Graph | CodeDB MCP |
|------|-------|------|-----------|-------------|------------|
| httpx-002 | Simple | 42 | **36** | 38 | 53 |
| httpx-003 | Medium | 36 | **35** | 36 | 50 |
| httpx-001 | Simple | 51 | **35** | 36 | 39 |
| fastapi-003 | Medium | 44 | **38** | 39 | 56 |
| **T1 Average** | | **43** | **36** | **37** | **50** |

### Tier 2: Tokens (K) — bold = most efficient

| Case | Diff. | Grep | Scout MCP | Scout+Graph |
|------|-------|------|-----------|-------------|
| nextjs-006 | Medium | **76** | **76** | 81 |
| nextjs-005 | Simple | **58** | 84 | 75 |
| nextjs-004 | Complex | 69 | **56** | **48** |
| k8s-003 | Complex | **51** | **42** | 49 |
| fastapi-002 | Simple | **57** | 59 | 66 |
| **T2 Average** | | **62** | **63** | **64** |

### Tier 1: Tool Calls — bold = fewest

| Case | Diff. | Grep | Scout MCP | Scout+Graph | CodeDB MCP |
|------|-------|------|-----------|-------------|------------|
| httpx-002 | Simple | 23 | **7** | 14 | 44 |
| httpx-003 | Medium | 9 | 8 | **10** | 27 |
| httpx-001 | Simple | 30 | 13 | **10** | 24 |
| fastapi-003 | Medium | 26 | 23 | **15** | 28 |
| **T1 Average** | | **22** | **13** | **12** | **31** |

### Tier 2: Tool Calls — bold = fewest

| Case | Diff. | Grep | Scout MCP | Scout+Graph |
|------|-------|------|-----------|-------------|
| nextjs-006 | Medium | 60 | **19** | 31 |
| nextjs-005 | Simple | 43 | 47 | **37** |
| nextjs-004 | Complex | 45 | **21** | 27 |
| k8s-003 | Complex | **20** | **18** | 22 |
| fastapi-002 | Simple | 45 | 50 | 54 |
| **T2 Average** | | **43** | **31** | **34** |

### Quality: Root Cause Found

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

### Quality: Secondary Findings

#### Tier 1

| Case | Grep | Scout MCP | Scout+Graph | CodeDB MCP |
|------|------|-----------|-------------|------------|
| httpx-002 | 3 | 3 | 4 | 3 |
| httpx-003 | 3 | 3 | 2 | 4 |
| httpx-001 | 3 | 3 | 3 | 3 |
| fastapi-003 | 3 | 3 | 4 | 3 |
| **T1 Total** | **12** | **12** | **13** | **13** |

#### Tier 2

| Case | Grep | Scout MCP | Scout+Graph |
|------|------|-----------|-------------|
| nextjs-006 | 3 (truthy check, lru guard, use-cache-wrapper byte hack) | 3 (truthy check, lru guard, prod-only) | 3 (truthy check, require.ts, response-cache) |
| nextjs-005 | 3 (replaceRequestBody fragility, size-limit truncation, finalize timing) | 3 (config naming, replaceRequestBody, finalize drain) | 3 (router-server bypass, edge sandbox, endPromise) |
| nextjs-004 | 2 (refreshReducer fallback, previousNavigationDidMismatch) | 3 (TODO comment, reducer plumbing, completeSoftNavigation) | 4 (refreshReducer, useInsertionEffect, TODO, previousNavigationDidMismatch) |
| k8s-003 | 3 (error swallowing, ResolveFenceposts, line 138) | 3 (error swallowing, ResolveFenceposts, recompute) | 3 (error swallowing, ResolveFenceposts, rolloutRolling) |
| fastapi-002 | 4 (Json expansion, second guard, Query/Header/Cookie, fix status) | 3 (non-Annotated safe, validation error, fix PR) | 4 (non-Annotated path, missing tests, copy_field_info, Query/Header) |
| **T2 Total** | **15** | **15** | **17** |

## Analysis

### Tier 1: Scout MCP dominates small/medium repos

| Metric | Scout MCP | Scout+Graph | Grep | CodeDB MCP |
|--------|-----------|-------------|------|------------|
| Avg time | **61s** | 69s | 131s | 162s |
| Avg tokens | **36K** | 37K | 43K | 50K |
| Avg tools | 13 | **12** | 22 | 31 |
| Root cause | 4/4 | 4/4 | 4/4 | 4/4 |
| Secondaries | 12 | **13** | 12 | 13 |

Scout MCP is **2.2x faster than Grep** on Tier 1 (httpx 60 files, fastapi 1.1K files). The speedup comes from fewer round-trips — each eliminated tool call saves ~3-5s of Sonnet inference. Scout+Graph adds structural queries at a modest 13% time premium, producing the best fix quality.

### Tier 2: Grep closes the gap on large repos

| Metric | Grep | Scout MCP | Scout+Graph |
|--------|------|-----------|-------------|
| Avg time | **233s** | 255s | 278s |
| Avg tokens | **62K** | 63K | 64K |
| Avg tools | 43 | **31** | **34** |
| Root cause | 5/5 | 5/5 | 5/5 |
| Secondaries | **15** | 15 | **17** |

**Grep is fastest on Tier 2** — 1.1x faster than Scout MCP on average. The reversal is driven by **kubernetes-003** (16.9K files) where Grep was 3.7x faster than both MCP variants (116s vs 428-434s).

Per-case winners:
- nextjs-006 (medium): **Scout MCP** 191s vs Grep 258s (1.4x)
- nextjs-005 (simple): **Grep** 180s vs Scout MCP 286s (1.6x)
- nextjs-004 (complex): **Scout MCP** 108s vs Grep 342s (**3.2x** — biggest MCP win)
- k8s-003 (complex, Go): **Grep** 116s vs Scout MCP 428s (**3.7x** — biggest Grep win)
- fastapi-002 (simple): **Scout MCP** 262s vs Grep 267s (~same)

### Why Grep wins on large repos

On kubernetes (16.9K files, Go), Scout MCP was 3.7x slower despite fewer tool calls (18 vs 20). Two factors:
1. **MCP query latency scales with repo size** — each Scout/Graph query processes a larger index
2. **Grep's native integration** — no IPC overhead, no JSON serialization, immediate results

On next.js (6.4K files, TS), Scout MCP won 2/3 cases. The crossover point appears to be around 10K+ files where MCP overhead exceeds Grep's higher tool count.

### Scout+Graph: Quality leader across both tiers

| Tier | Grep sec. | Scout MCP sec. | Scout+Graph sec. |
|------|-----------|----------------|------------------|
| T1 | 12 | 12 | **13** |
| T2 | 15 | 15 | **17** |
| **Total** | **27** | **27** | **30** |

Scout+Graph consistently finds more secondary issues (+11% over Grep/Scout MCP). On nextjs-004, it found 4 secondaries vs Grep's 2. The graph's `callers_of`/`callees_of` queries surface connected code that grep-based exploration misses.

### CodeDB MCP: Tier 1 only (not tested on Tier 2)

CodeDB MCP was 2.7x slower than Scout MCP on Tier 1. Not run on Tier 2 due to MCP stability issues on larger repos.

### Combined Rankings (T1+T2, 9 cases)

| Rank | Variant | T1 Avg | T2 Avg | Overall Avg | Total Tokens | Total Secondaries |
|------|---------|--------|--------|-------------|--------------|-------------------|
| 1 | **Scout MCP** | **61s** | 255s | 158s | **50K avg** | 27 |
| 2 | **Scout+Graph** | 69s | 278s | 174s | 51K avg | **30** |
| 3 | **Grep** | 131s | **233s** | 182s | 53K avg | 27 |
| 4 | CodeDB MCP | 162s | — | (T1 only) | 50K avg | 13 (T1) |

**Scout MCP is the fastest overall** but Grep is closing the gap on larger repos. **Scout+Graph is the quality leader** at a 10% time premium over Scout MCP. All variants achieve 100% root cause accuracy.

### Key Insight: Repo Size Determines the Winner

| Repo size | Winner | Why |
|-----------|--------|-----|
| Small (<100 files) | **Scout MCP** 2-4x faster | Fewer round-trips, minimal overhead |
| Medium (1K-6K files) | **Scout MCP** ~1.3x faster | MCP overhead manageable, still fewer tools |
| Large (>10K files) | **Grep** 2-4x faster | MCP query latency scales with repo size |

## Pending Cases

### Tier 3: Complex (next batch)

| Case | Grep | Scout MCP | Scout+Graph |
|------|------|-----------|-------------|
| nextjs-003 | pending | pending | pending |
| nextjs-007 | pending | pending | pending |
| nextjs-008 | pending | pending | pending |
| vscode-003 | — | done (2026-04-06) | done (2026-04-06) |

### Tier 4: Monster

| Case | Grep | Scout MCP | Scout+Graph |
|------|------|-----------|-------------|
| rust-002 | pending | pending | pending |

## Re-run: httpx-002 with New Builds (2026-04-06)

Re-benchmarked httpx-002 after CodeDB rebuild + NPP leak fixes. Sequential runs (no concurrency).

### Results

| Variant | Time | Tokens | Tools | Root Cause | Secondaries (3) |
|---------|------|--------|-------|------------|-----------------|
| **Scout MCP** | **27s** | **33K** | **5t** | YES | 2/3 |
| **Scout+Graph** | 31s | 36K | 7t | YES | 2/3 |
| **Graph MCP** | 34s | 37K | 6t | YES | 2/3 |
| **CodeDB MCP** | 42s | 41K | 9t | YES | 2/3 |
| Grep (R9 baseline) | 118s | 42K | 23t | YES | 3/3 |

### Improvement vs R9 Baselines

| Variant | Time (R9 → now) | Tokens | Tools |
|---------|-----------------|--------|-------|
| CodeDB MCP | 213s → 42s (**5.1x faster**) | 53K → 41K (-23%) | 44t → 9t (**-80%**) |
| Scout+Graph | 73s → 31s (**2.4x faster**) | 38K → 36K (-5%) | 14t → 7t (-50%) |
| Scout MCP | 39s → 27s (1.4x faster) | 36K → 33K (-8%) | 7t → 5t (-29%) |
| Graph MCP | 82s → 34s (2.4x faster) | 40K → 37K (-8%) | 17t → 6t (-65%) |

Graph MCP R9 baseline from 6-way run (MCP-only variant, `BENCHMARK_6WAY_SCOUT_MCP.md`).

### Secondary Findings Detail

| Finding | Grep (R9) | CodeDB | Graph | Scout | S+Graph |
|---------|-----------|--------|-------|-------|---------|
| `seen_data` flag poisoned before guard | Y | Y | Y | Y | Y |
| BrotliDecoder has correct pattern | Y | Y | Y | Y | Y |
| `test_zstd_empty` doesn't exercise streaming | Y | ~ | ~ | - | ~ |

### Key Takeaway

CodeDB is the biggest winner — 5.1x faster than its R9 self with 80% fewer tool calls. The new build eliminated the over-querying problem. All MCP tools now beat Grep by 2.8–4.4x on this case. Only Grep's exhaustive crawl catches the subtle test coverage gap (finding #3).

## New Case: vscode-003 — 5-Way Comparison (2026-04-06)

First benchmark of vscode-003 (Complex, 7K files). Keybinding resolver bug: wrong command selected when two keybindings share a chord and one has a negated `when` clause. Ground truth: `keybindingResolver.ts` — `whenIsEntirelyIncluded` + `_findCommand` last-wins ordering.

Sequential runs (no concurrency). Grep baseline from 6-way run (`BENCHMARK_6WAY_SCOUT_MCP.md`).

### Results (latest runs — after Tier 1 + #42 conditional expansion)

| Rank | Variant | Time | Tokens | Tools | Root Cause | Secondary (4) |
|------|---------|------|--------|-------|------------|---------------|
| 1 | **Scout MCP** | **115s** | **52K** | **16t** | YES | 3/4 |
| 2 | **Scout+Graph** | 133s | 57K | 21t | YES | 3/4 |
| 3 | **Graph MCP** | 140s | 53K | 17t | YES | **4/4** |
| 4 | CodeDB MCP | 235s | 54K | 19t | YES | 3/4 |
| 5 | Grep (6-way) | 291s | 54K | 25t | YES | 2/4 |

### vs 6-Way Baselines (vscode-003)

| Variant | 6-way → now | Tokens | Tools |
|---------|-------------|--------|-------|
| Grep | 291s (baseline) | 54K | 25t |
| Scout MCP | 902s → **115s** (7.8x faster) | 110K → 52K (-53%) | 56t → 16t (-71%) |
| Graph MCP | 548s → **140s** (3.9x faster) | 101K → 53K (-47%) | 35t → 17t (-51%) |
| Scout+Graph | 314s → **133s** (2.4x faster) | 70K → 57K (-19%) | 31t → 21t (-32%) |

### Quality: Secondary Findings Detail

Ground truth: 4 secondary findings for vscode-003.

| Finding | Grep (6-way) | Scout MCP | Graph MCP | Scout+Graph | CodeDB MCP |
|---------|-------------|-----------|-----------|-------------|------------|
| `_addKeyPress` removes from `_lookupMap` | Y | Y | Y | Y | Y |
| `implies()` no complement/negation awareness | Y | Y | Y | Y | ~ |
| `_findCommand` reverse iteration (last-wins) | Y | Y | Y | Y | Y |
| `MoreChordsNeeded` with negated binding | - | - | Y | ~ | Y |
| **Total** | **2/4** | **3/4** | **4/4** | **3/4** | **3/4** |

Graph MCP found all 4 secondaries including the subtle `MoreChordsNeeded` chord-length interaction — the deepest analysis despite being the most token-efficient (53K). Scout MCP was fastest (115s) but missed the chord-length edge case.

### Analysis: #42 Conditional Expansion Solved the Blowup

Graph MCP's journey on vscode-003:

| Run | Change | Time | Tokens | Tools |
|-----|--------|------|--------|-------|
| 6-way (original) | None | 548s | 101K | 35t |
| Pre-Tier 1 (outlier) | NPP fixes only | 197s | 51K | 31t |
| Post-Tier 1 run 1 | +action_hint, -confidence | 672s | 98K | 49t |
| Post-Tier 1 run 2 | +confidence restored | 1097s | 123K | 54t |
| **Post-#42** | **+conditional expansion** | **140s** | **53K** | **17t** |

The critical fix was #42 (conditional structural expansion): stripping callers/callees from `open_node_context` on exact-match + compact mode. This cut tool calls from 31-54 → 17 — the agent stopped chasing structural leads on every lookup.

Scout+Graph (133s) also recovered from its previous 864s blowup. With Graph returning sparse results, the two-tool combo no longer amplifies exploration. All three MCP variants now cluster at 115-140s.

### Key Insight: Strip Noise, Keep Signal

| Approach | What it strips | Effect |
|----------|---------------|--------|
| Scout Tier 1 | Callers, related_files at high confidence | **7.8x faster** |
| Graph #42 | Callers/callees from open_node_context on exact match | **3.9x faster** |
| Graph Tier 1 (initial mistake) | Confidence score itself | **Hurt** — lost stopping signal |

**Principle**: Strip structural noise (callers, callees, related files) at high confidence. Keep decision signals (confidence score, action_hint) visible. Both Scout and Graph independently validated this pattern.

## New Case: k8s-004 — Complex Structural Deadlock, 16.9K files (2026-04-06)

First benchmark of k8s-004 (Complex, Go, 16.9K files). Kubelet volume manager deadlock: reconciler detaches a volume while attach/detach controller holds the global volume lock. Ground truth: deadlock chain across 5 files, 2 lock types.

Sequential runs (no concurrency).

### Results

| Variant | Time | Tokens | Tools | Root Cause |
|---------|------|--------|-------|------------|
| **Grep** | **203s** | 99K | 37t | YES |
| Scout MCP | 261s | **84K** | **30t** | YES |
| Scout MCP (Opus) | 342s | 120K | 58t | YES |
| Scout+Graph | 401s | 85K | 46t | YES |
| Graph MCP | 478s | 109K | 67t | YES |

### Quality: Three Different Deadlock Analyses

All three found the root cause but identified **different deadlock mechanisms**:

| Aspect | Grep (Sonnet) | Scout (Sonnet) | Scout (Opus) |
|--------|---------------|----------------|--------------|
| Deadlock type | Mutex (pm.mutex + prober.mutex) | Mutex (pm.mutex + pendingOps.lock) | **Distributed liveness** |
| Key insight | FlexVolume prober 3-goroutine cycle | Attach/detach closure asymmetry | Informer lag + state-based circular wait |
| Actionability | Good (use RLock) | **Best** (2-line fix: move lookup inside closure) | Good (don't delete from ASW until confirmed) |
| Secondary findings | Data race in GetNodesToUpdateStatusFor | — | — |
| Architectural depth | 6 files | 5 files | **8 files, cross-component** |

Opus found the most sophisticated analysis (distributed liveness deadlock between kubelet and controller via stale informer state) at 2x the tool cost of Sonnet. Scout Sonnet found the most actionable fix (attach/detach asymmetry = 2-line closure move).

### Overall Quality Ranking

| Rank | Variant | Quality | Unique Insight |
|------|---------|---------|---------------|
| 1 | Scout MCP (Opus) | **A+** | Distributed liveness deadlock, 8-file cross-component trace |
| 2 | Scout MCP (Sonnet) | **A** | Attach/detach closure asymmetry (most actionable 2-line fix) |
| 3 | Grep (Sonnet) | **A-** | FlexVolume prober 3-goroutine cycle + data race secondary |
| 4 | Scout+Graph | **B+** | UpdateNodeStatusForNode inside-loop lock hazard |
| 5 | Graph MCP | **B+** | Starvation angle (write lock for reads), but less focused |

Every variant found a real deadlock mechanism. No two found the same one — k8s-004 has multiple interacting lock hazards. Opus justified its 2x tool cost with the deepest architecture-level analysis.

### k8s-003 Scout Re-run Attempt (INVALID — wrong prompt)

Attempted to re-run k8s-003 with Scout MCP (Sonnet + Opus) but used an incorrect bug description ("Scheduler scoring plugin returns wrong node scores when multiple plugins use the same state key"). The actual k8s-003 bug is a **deployment controller stall** (GT: `deployment_controller.go` / `rolling.go:reconcileOldReplicaSets`).

All agents spiraled (Sonnet: 1673s/179K/116t, Opus: killed at 80t) searching for a non-existent state key collision. Results discarded — not a tool or model failure, but a prompt error.

**Correct k8s-003 prompt** (for future runs): "Deployment rollout stalls indefinitely when maxUnavailable and maxSurge are both set — reconcileOldReplicaSets in the rolling update controller gets stuck in a loop without making progress"

### k8s-003 Re-run with Corrected Prompt (2026-04-07)

| Rank | Variant | Time | Tokens | Tools | Root Cause | Secondary (3) |
|------|---------|------|--------|-------|------------|---------------|
| 1 | **Grep** | **49s** | **33K** | **6t** | YES | 1/3 |
| 2 | **Graph MCP** | 84s | 43K | 12t | YES | **3/3** |
| 3 | **Scout MCP** | 88s | 42K | 8t | YES | 2/3 |
| 4 | Scout+Graph | 166s | 47K | 18t | YES | 2/3 |
| 5 | CodeDB MCP | 239s | 67K | 20t | YES | 2/3 |

All found root cause (`rolling.go` error swallowing: `return false, nil` instead of `return false, err`). Graph MCP had best quality (3/3: error swallowing + stale allRSs + negative clamp). Grep fastest (49s) — Go identifiers perfectly greppable. Prompt quality was the decisive factor: wrong prompt → 100% failure; correct prompt → 100% success.

## New Case: rust-001 — 5-Crate Compiler Trace, 36.8K files (2026-04-07)

First benchmark of rust-001 (Complex, Rust, 36.8K files). Borrow checker confusing error for closure lifetime mismatch. Ground truth: `region_errors.rs` — `is_closure_fn_mut` guard only matches FnMut, not Fn closures. Source: [rust-lang/rust#130528](https://github.com/rust-lang/rust/issues/130528).

All variants run with Opus (except Scout Sonnet comparison). Sequential runs.

### Results

| Rank | Variant | Model | Time | Tokens | Tools | Root Cause |
|------|---------|-------|------|--------|-------|------------|
| 1 | **Scout+Graph** | Opus | **345s** | 90K | **40t** | YES |
| 2 | **Scout MCP** | Sonnet | 366s | **80K** | 36t | YES |
| 3 | Scout MCP | Opus | 539s | 137K | 67t | YES |
| 4 | Grep | Opus | 552s | 93K | 104t | YES |
| 5 | Graph MCP | Opus | 564s | 85K | 68t | YES |

### Key Findings

- **Scout+Graph Opus fastest** (345s) — combo worked well on this case, no blowup
- **Scout Sonnet nearly as fast as Opus** (366s vs 539s) at lower cost — Opus didn't add quality
- **Grep slowest** (552s, 104 tools) — 36.8K files means many grep calls needed to navigate the compiler
- All variants found the same root cause: `is_closure_fn_mut` at `region_errors.rs:475` gates the clear error message to FnMut only; Fn closures fall through to `report_general_error` with confusing synthetic lifetime names

### Sonnet vs Opus on rust-001

| Model | Scout Time | Scout Tokens | Scout Tools |
|-------|-----------|-------------|-------------|
| Sonnet | **366s** | **80K** | **36t** |
| Opus | 539s | 137K | 67t |

Sonnet was 1.5x faster with 42% fewer tokens. On this case, Opus's deeper reasoning didn't yield better results — both found the same root cause and similar secondaries.

### Why Grep Beats Scout on Kubernetes



Grep was 1.3x faster than Scout MCP (203s vs 261s), consistent with R9 k8s-003 (Grep 3.7x faster). Gap narrowed thanks to Scout's Tier 1 confidence gating, but:

1. **16.9K files past the MCP crossover point** (~10K) — per-call IPC + enrichment overhead exceeds Grep's higher tool count
2. **Go's exported identifiers are perfectly greppable** — `FindPluginBySpec`, `DetachVolume` are unique strings; Scout's semantic search adds overhead without discovery value
3. **Go package structure amplifies enrichment noise** — dense cross-package imports generate more tangential leads per Scout result

## Environment Notes

- **GPU**: CUDA 13.2 + cuDNN 9.20 on RTX 5070 Ti (NVIDIA). Graph MCP uses CUDA EP for embedding inference with automatic CPU fallback.
- **Scout MCP auto-heal**: repo-scout commit 63b4faf adds automatic re-indexing when manifest.bin is corrupt — future version mismatches self-heal.
- **CodeDB MCP**: Persistent MCP server with snapshot-based indexing. First index takes 2.5s (httpx) to 48s (fastapi); subsequent queries use cached snapshot.
- **Shared MCP**: code-review-graph server auto-shares via HTTP port 7432 — first instance is primary, subsequent instances proxy through it.
