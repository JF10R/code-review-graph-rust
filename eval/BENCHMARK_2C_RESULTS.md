# Phase 2c Benchmark Results

**Date:** 2026-03-25
**Binary:** v1.3.0 + Phase 1 (type indexing, path priors, cached lexical) + Phase 2 (auto-routing)
**Eval set:** 28 cases across 6 repos (eval/gold-eval-set.json)

## Overall Results: hybrid_query (auto route)

| Metric | Value |
|--------|-------|
| **Hit@5** | **2/28 (7.1%)** |
| **MRR** | **0.071** |
| Queries with results | 11/28 (39.3%) |
| Queries returning empty | 17/28 (60.7%) |

## Per-Query Results

| ID | Repo | Ground Truth | Route | Conf | Method | Hit@5 | Rank | Notes |
|----|------|-------------|-------|------|--------|-------|------|-------|
| httpx-001 | httpx | `_config.py` | General | 0.5 | hybrid_rrf | MISS | - | All 5 results are test files |
| httpx-002 | httpx | `_decoders.py` | General | 0.5 | hybrid_rrf | MISS | - | All 5 results are test files |
| httpx-003 | httpx | `_auth.py` | General | 0.5 | hybrid_rrf | MISS | - | All 5 results are test files |
| fastapi-001 | fastapi | `openapi/utils.py` | General | 0.5 | hybrid_rrf | MISS | - | All 5 results are test files |
| fastapi-002 | fastapi | `dependencies/utils.py` | General | 0.5 | hybrid_rrf | MISS | - | All 5 results are test files |
| fastapi-003 | fastapi | `dependencies/utils.py` | FilePath | 0.85 | keyword_path | MISS | - | Empty (executor failure) |
| nextjs-001 | next.js | `.../css/index.ts` | General | 0.5 | hybrid_rrf | MISS | - | CSS/webpack area but wrong file |
| nextjs-002 | next.js | `.../writeAppTypeDeclarations.ts` | FilePath | 0.85 | keyword_path | MISS | - | Empty (executor failure) |
| nextjs-003 | next.js | `.../manifest-loader.ts` | General | 0.5 | hybrid_rrf | **HIT** | **1** | Perfect hit |
| nextjs-004 | next.js | `.../server-patch-reducer.ts` | FilePath | 0.85 | keyword_path | MISS | - | Empty (executor failure); **HIT@1 with legacy** |
| nextjs-005 | next.js | `.../next-server.ts` | FilePath | 0.85 | keyword_path | MISS | - | Empty (executor failure) |
| nextjs-006 | next.js | `.../lru-cache.ts` | General | 0.5 | hybrid_rrf | MISS | - | lru-cache.test.ts at #3, not source |
| nextjs-007 | next.js | `.../pages-handler.ts` | FilePath | 0.85 | keyword_path | MISS | - | Empty (executor failure) |
| nextjs-008 | next.js | `.../segment-prefix-rsc.ts` | General | 0.5 | hybrid_rrf | **HIT** | **1** | Perfect hit (class name in query) |
| nextjs-009 | next.js | `.../app-page.ts` | General | 0.5 | hybrid_rrf | MISS | - | app-render area, wrong file |
| nextjs-010 | next.js | `.../entries.ts` | General | 0.5 | hybrid_rrf | MISS | - | All test files |
| nextjs-011 | next.js | `.../getTypeScriptConfiguration.ts` | FilePath | 0.85 | keyword_path | MISS | - | Empty (executor failure) |
| vscode-001 | vscode | `.../configurationModels.ts` | ConfigLookup | 0.75 | keyword_config | MISS | - | Empty |
| vscode-002 | vscode | `.../extHostLanguageFeatures.ts` | General | 0.5 | keyword_only | MISS | - | Empty |
| vscode-003 | vscode | `.../keybindingResolver.ts` | General | 0.5 | keyword_only | MISS | - | Empty |
| vscode-004 | vscode | `.../debugHover.ts` | General | 0.5 | keyword_only | MISS | - | Empty |
| k8s-001 | kubernetes | `.../noderesources/fit.go` | General | 0.5 | keyword_only | MISS | - | Empty |
| k8s-002 | kubernetes | `.../podtopologyspread/scoring.go` | General | 0.5 | keyword_only | MISS | - | Empty |
| k8s-003 | kubernetes | `.../deployment_controller.go` | General | 0.5 | keyword_only | MISS | - | Empty |
| k8s-004 | kubernetes | `.../volume_manager.go` | FilePath | 0.85 | keyword_path | MISS | - | Empty (also empty with legacy — no embeddings) |
| rust-001 | rust | `.../region_errors.rs` | General | 0.5 | keyword_only | MISS | - | Empty |
| rust-002 | rust | `.../coercion.rs` | General | 0.5 | keyword_only | MISS | - | Empty |
| rust-003 | rust | `.../inline.rs` | General | 0.5 | keyword_only | MISS | - | Empty |

## Breakdowns

### By Repo Size Tier

| Tier | Repos | Hit@5 | Empty Rate |
|------|-------|-------|------------|
| Small | httpx | 0/3 (0%) | 0% |
| Medium | fastapi | 0/3 (0%) | 33% |
| Large | next.js | 2/11 (18%) | 45% |
| Large | vscode | 0/4 (0%) | 100% |
| Mega | kubernetes | 0/4 (0%) | 100% |
| Mega | rust | 0/3 (0%) | 100% |

### By Route/Method

| Method | Queries | Hit@5 | Empty Rate | Notes |
|--------|---------|-------|------------|-------|
| hybrid_rrf | 11 | 2 (18.2%) | 0% | Always returns results |
| keyword_path_boosted | 7 | 0 (0%) | **100%** | Executor returns empty on every query |
| keyword_only | 9 | 0 (0%) | **100%** | All on repos with 0 embeddings |
| keyword_config_boosted | 1 | 0 (0%) | 100% | 1 query, no results |

Sanity check: 11 + 7 + 9 + 1 = 28 total queries. 11 non-empty, 17 empty.

### Route Classification

| Route | Queries | Classification notes |
|-------|---------|---------------------|
| General (0.5) | 20 | Low-confidence default; splits into hybrid_rrf (11) or keyword_only (9) depending on embedding availability |
| FilePath (0.85) | 7 | Queries contain file-like strings ("next-env.d.ts", "serverPatchReducer", "volume_manager"); **classification may be reasonable but executor fails closed** |
| ConfigLookup (0.75) | 1 | "Settings editor... configuration overrides" — plausible classification |

### FilePath Executor Failure: Legacy Re-test

All 7 FilePath-routed queries re-run with `route: "legacy"`:

| ID | FilePath Result | Legacy Result | Legacy Hit@5 |
|----|----------------|---------------|-------------|
| fastapi-003 | Empty | 5 results (all test files) | MISS |
| nextjs-002 | Empty | 5 results (wrong area) | MISS |
| **nextjs-004** | **Empty** | **5 results** | **HIT@1** |
| nextjs-005 | Empty | 5 results (body-streams area) | MISS |
| nextjs-007 | Empty | 5 results (response-cache area) | MISS |
| nextjs-011 | Empty | 5 results (base-path area) | MISS |
| k8s-004 | Empty | Empty (keyword_only — no embeddings) | MISS |

**Key finding:** The FilePath route problem is an **executor failure**, not purely a classifier error. The `keyword_path_boosted` method returns empty on every input. 6/7 queries produce results under legacy; 1 (k8s-004) remains empty because the underlying repo has no embeddings regardless. The confirmed regression: nextjs-004 is HIT@1 under legacy.

## semantic_search_nodes Comparison (Exploratory)

To probe the graph's underlying recall, `semantic_search_nodes` was tested with **targeted keyword queries** (e.g., function/class names extracted from ground truth). These are NOT the original benchmark queries — this tests the graph data layer, not a fair head-to-head with hybrid_query.

| ID | hybrid_query | SSN Query Used | SSN Result | SSN Rank |
|----|-------------|---------------|-----------|----------|
| httpx-001 | MISS | "SSLContext verify cert" | `_config.py::create_ssl_context` | **1** |
| vscode-001 | MISS (empty) | "configurationModels" | `configurationModels.ts::Configuration` | **2** |
| vscode-003 | MISS (empty) | "keybindingResolver" | `keybindingResolver.ts::KeybindingResolver` | **1** |
| k8s-001 | MISS (empty) | "NodeResourcesFit" | `NodeResourcesFitArgs` (config, not fit.go) | 1 (partial) |
| k8s-003 | MISS (empty) | "deployment_controller" | `deployment_controller.go` | **2** |
| rust-001 | MISS (empty) | "region_errors diagnostics" | `region_errors.rs` | **4** |

**Takeaway:** The graph's node index contains the right data. `semantic_search_nodes` keyword mode (which searches node name/qualified_name directly via SQLite) successfully locates ground-truth files that `hybrid_query`'s Tantivy-based keyword_only path cannot find. This suggests the Tantivy lexical index is either not built or not queried correctly for repos without embeddings.

## Root Cause Analysis

### Issue 1: keyword_only returns empty on unembedded repos (CRITICAL)

**What:** 9/28 queries routed to General on repos with 0 embeddings (vscode, kubernetes, rust) all get `keyword_only` method, all return empty.

**Evidence:** These repos have 184K-286K nodes. `semantic_search_nodes` keyword mode finds relevant nodes on the same repos using the same graph database. The Tantivy index and the SQLite node-name index are separate subsystems — the Tantivy index may not have been built during `build_or_update_graph`, or `keyword_only` may have a codepath bug.

**Not yet confirmed:** Whether `embed_graph` is a prerequisite for Tantivy index construction, or whether the issue is in the query execution path. Needs source investigation.

### Issue 2: keyword_path_boosted executor returns empty (CRITICAL)

**What:** All 7 queries routed to FilePath get the `keyword_path_boosted` method, which returns 0 results every time. This is an **executor failure** — the route fires but the search implementation returns nothing.

**Impact:** 1 confirmed HIT@1 lost (nextjs-004). 5 other queries returned useful (though non-hitting) results under legacy. The fix is straightforward: executor should fall back to legacy/hybrid_rrf when the specialized path returns empty.

**Classification accuracy:** Some of these queries DO mention file-like strings (e.g., "next-env.d.ts", "routes.js"), so the classification isn't necessarily wrong — the executor just fails to produce results.

### Issue 3: Test file bias in hybrid_rrf (HIGH)

**What:** Of the 11 queries where hybrid_rrf returns results, 7 have all-test-file results and 2 have mixed results. Only 2 queries (nextjs-003, nextjs-008) hit ground truth — both cases where the query contains the exact class/file name.

**Mechanism:** Test files mention bug-related terms alongside assertions, scoring highly on both keyword and semantic axes. The RRF merge amplifies this because both retrieval paths agree on test relevance.

**Note on eval bias:** The gold eval set is source-file-biased (all ground truths are source files). A flat Test-kind penalty would be incorrect for queries that genuinely target test behavior. A more nuanced approach: conditional demotion when the query does not contain test-related terms, or a bounded prior (e.g., 0.7-0.9x) rather than a hard 0.5x.

### Issue 4: Missing embeddings (MEDIUM)

**What:** vscode, kubernetes, and rust have 0 embeddings. This eliminates the semantic retrieval component.

**Dependency:** If Issue 1 is fixed (keyword_only actually works), these repos would at least have keyword recall. Embeddings would add the semantic component on top. Whether `embed_graph` is a product prerequisite or just missing eval setup needs clarification.

## Recommendations (Priority Order)

1. **Investigate and fix keyword_only empty results** — Largest concrete recall hole (9 queries). Check whether the Tantivy index is built during `build_or_update_graph` or only during `embed_graph`. Then check the keyword_only execution path.

2. **Add empty-result fallback for specialized routes** — When FilePath, ConfigLookup, or any specialized route returns empty, fall through to legacy hybrid_rrf. This is a one-line fix with clear evidence: 6/7 FilePath queries return results under legacy.

3. **Build embeddings for bench repos and re-benchmark** — Run `embed_graph` on vscode/kubernetes/rust, then re-run this benchmark. This separates "missing eval setup" from "product bug."

4. **Investigate test-file bias** — After fixes 1-2, re-measure to see how many hybrid_rrf results still miss due to test-file dominance. Then decide on a demotion strategy (bounded prior, query-conditional, or none).

## What This Report Does NOT Establish

- Whether `embed_graph` is a prerequisite for `hybrid_query` to work (unclear from black-box testing alone).
- How hybrid_query would score if all 3 bugs were fixed (depends on Tantivy recall quality, which we can't measure while it returns empty).
- Whether `semantic_search_nodes` would score well with the original benchmark queries (the SSN sample used targeted keywords, not the NL queries from the eval set).

---

## Post-Fix Re-Benchmark (same session)

**Binary:** v1.3.0 + two fixes applied (search_nodes_relaxed + empty-result fallback)

### Execution Path

All queries ran through the MCP worker path (`server.rs` worker → `hybrid_query_with_store`).
The `tantivy-search` feature is not enabled in the default build, so `kw_hits!` returns `None`
and `search_nodes_relaxed` is the keyword source for all queries. The `_debug` output shows
route and confidence but does not currently distinguish Tantivy vs relaxed as the keyword source.

### Fix 1: `search_nodes_relaxed()` in graph.rs
- OR-matching with stop-word filtering for NL queries in `hybrid_query`
- Replaces AND-logic `search_nodes()` which required ALL query words to match
- Short queries (≤3 words after filtering) still use strict AND logic
- Also applies bounded penalties: compiled-path demotion (0.5x for `/compiled/`, `/node_modules/`, `/.next/`) and test-node demotion (0.8x for Test-kind nodes)

### Fix 2: Empty-result fallback in tools.rs
- Specialized routes (FilePath, ConfigLookup, ExactSymbol) fall through to General when empty

### Results Comparison

| Metric | Before Fix | After Fix | Delta |
|--------|-----------|-----------|-------|
| **Hit@5** | 2/28 (7.1%) | **5/28 (17.9%)** | **+150%** |
| **MRR** | 0.071 | **0.137** | **+93%** |
| **Empty rate** | 17/28 (60.7%) | **0/28 (0%)** | **Eliminated** |
| Non-empty | 11/28 (39.3%) | **28/28 (100%)** | +155% |

### New Hits (3 added, all on previously-empty repos)

| ID | Repo | Ground Truth | Rank | Method | Was (before) |
|----|------|-------------|------|--------|-------------|
| vscode-002 | vscode | `extHostLanguageFeatures.ts` | **1** | keyword_only | Empty |
| k8s-002 | kubernetes | `podtopologyspread/scoring.go` | **2** | keyword_only | Empty |
| rust-002 | rust | `coercion.rs` | **3** | keyword_only | Empty |

### Retained Hits (2 unchanged)

| ID | Repo | Ground Truth | Rank | Method |
|----|------|-------------|------|--------|
| nextjs-003 | next.js | `manifest-loader.ts` | 1 | hybrid_rrf |
| nextjs-008 | next.js | `segment-prefix-rsc.ts` | 1 | hybrid_rrf |

### By Repo (After Fix)

| Repo | Hit@5 | Empty Rate | Notes |
|------|-------|------------|-------|
| httpx | 0/3 | 0% | Results now include source files alongside tests |
| fastapi | 0/3 | 0% | FilePath queries now return results |
| next.js | 2/11 (18%) | 0% | Same 2 hits; FilePath queries no longer empty |
| vscode | **1/4 (25%)** | 0% | Was 0/4 with 100% empty |
| kubernetes | **1/4 (25%)** | 0% | Was 0/4 with 100% empty |
| rust | **1/3 (33%)** | 0% | Was 0/3 with 100% empty |

### By Method (After Fix)

| Method | Queries | Hit@5 | Empty Rate |
|--------|---------|-------|------------|
| hybrid_rrf | 11 | 2 (18.2%) | 0% |
| keyword_only | 9 | **3 (33.3%)** | **0%** (was 100%) |
| keyword_path_boosted | 7 | 0 (0%) | **0%** (was 100%) |
| keyword_config_boosted | 1 | 0 (0%) | **0%** (was 100%) |

Sanity check: 11 + 9 + 7 + 1 = 28. Hits: 2 + 3 + 0 + 0 = 5.

### Attribution

The two fixes were bundled in a single re-benchmark, so per-fix attribution is not isolated.
What we can say:

- **Confirmed:** The fallback keyword path (`search_nodes` → `search_nodes_relaxed`) was the
  keyword source for all 28 queries in this run, because `tantivy-search` is not a default
  feature. The AND-to-OR logic change on this fallback path fixed the 17 previously-empty queries.
- **Not established:** Whether enabling `tantivy-search` would produce the same or better results
  via the `kw_hits!` path in `server.rs:384-388`. Tantivy uses BM25 scoring which handles long
  queries natively; it might outperform `search_nodes_relaxed` but this is untested.
- **Not isolated:** The empty-result fallback (Fix 2) and the relaxed search (Fix 1) were tested
  together. Some FilePath queries now return results from the specialized route (via relaxed
  keyword hits), others fall through to General — both fixes contribute.

The per-method comparison (keyword_only 3/9 vs hybrid_rrf 2/11) reflects different query slices,
not a controlled head-to-head. keyword_only's 33.3% rate is best on the previously-empty
unembedded-repo slice; it does not establish keyword_only as the best method overall.

### Remaining Gap Analysis

23/28 queries still miss. Breakdown:
- **8 queries**: Results are in the right area/package but wrong specific file (neighborhood hits)
- **9 queries**: Results dominated by test files over source files (note: a 0.8x test-node
  penalty is already applied by `search_nodes_relaxed`, so the remaining test bias is not
  addressable by a simple prior alone)
- **4 queries**: Results from FilePath/ConfigLookup in plausible but wrong area
- **2 queries**: Results completely off-target

Multiple levers could improve the remaining 23 misses — no single one is established as dominant:
- Stronger source/test ranking (but 0.8x is already applied; diminishing returns likely)
- Route-specific ranking fixes (FilePath/ConfigLookup results are plausible but imprecise)
- Richer edges/context (neighborhood hits suggest the right package is found, not the right file)
- Building embeddings for vscode/kubernetes/rust (adds semantic signal to keyword_only queries)
