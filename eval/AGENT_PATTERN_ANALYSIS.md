# Agent Pattern Analysis: MCP Tool Usage in Code Investigation & Review

**Date**: 2026-03-26
**Data**: 8 investigation benchmarks (6 repos) + 4 review benchmarks (3 repos) + 20 agent transcripts analyzed
**Research**: 17 papers surveyed (2025-2026)

## Part 1: Observed Agent Patterns

### The Winning Pattern (k8s-002, 3x speedup)

```
Call 1: hybrid_query("PodTopologySpread scoring minDomains")  → 3 candidate files
Call 2: Read(scoring.go)                                      → validate candidate
Call 3: semantic_search("minDomains filtering")               → find counterpart
Call 4: Read(filtering.go)                                    → confirm asymmetry
Call 5: query_graph(callers_of, "Score")                      → understand call site
Calls 6-26: Read/Grep only                                   → deep analysis
```

**Properties:**
- 5 MCP calls (19% of total), immediately validated with Read
- Balanced tool mix: 2 hybrid_query + 2 semantic_search + 1 query_graph
- Clean transition from discovery → comprehension at call ~9
- No backtracking, no duplicate searches

### The Losing Pattern (nextjs-simple, 2.8x slower)

```
Calls 1-3: hybrid_query + semantic_search + semantic_search   → overlapping results
Calls 4-8: Read files from MCP results                        → validate
Calls 9-15: More MCP searches with keyword variations          → 70% duplicate nodes
Calls 16-80: grep/read spiral through compiled bundles         → over-exploration
Calls 80-148: Still searching after root cause found           → no stopping signal
```

**Properties:**
- 14+ MCP calls (many redundant)
- Same nodes returned by multiple searches (no cross-search dedup)
- No transition point — MCP and grep interleaved throughout
- 81% of calls happened AFTER the answer was found

### The Review Pattern (MCP consistently wins)

```
Call 1: get_review_context(changed_files)    → callers, callees, source preview
Call 2: get_impact_radius(changed_files)     → full blast radius in one call
Calls 3-5: query_graph / Read               → validate specific risks
Calls 6-10: Read/Grep                       → detailed code analysis
```

**vs No-MCP review:**
```
Calls 1-5: Bash/Grep to find the file, understand structure
Calls 6-15: Grep for callers (5-16 cycles)
Calls 16-25: Read files to understand impact
```

**MCP review wins because:**
- `get_impact_radius` replaces 5-16 grep-for-callers cycles
- `get_review_context` eliminates the "lost" directory exploration phase
- Bounded task = no over-exploration risk

## Part 2: Root Causes of MCP Losses

### 1. Semantic Search Fragmentation
Agent issues 3-5 searches with slightly different keywords. Results overlap 70-80%.
No server-side deduplication across calls. Agent wastes cycles mentally deduplicating.

**Evidence:** httpx-002 MCP agent searched "zstd decode", "zstd decompression encoding",
"content decoder" — each returning mostly the same ZStandardDecoder nodes.

### 2. Results Too Granular, Too Many
Each MCP call returns 10-20 individual function nodes. Only 20-40% relevant.
Agent reads each result description, increasing token cost without proportional value.

**Evidence:** k8s-004 MCP: 11 semantic_search calls, each returning ~15 nodes.
Total: ~165 node descriptions consumed. Useful: ~30. Waste ratio: 82%.

### 3. No Stopping Signal
MCP returns results as long as the agent asks. No indication of "diminishing returns"
or "you've already seen most relevant code." Agent interprets results as "there's more
to find" and continues searching.

**Evidence:** nextjs-simple: root cause found at call ~15. Agent made 133 more calls.
No MCP response ever said "confidence: high" or "coverage: 80% of relevant code seen."

### 4. MCP Doesn't Replace Grep — It Adds To It
Expected: MCP discovery replaces grep cycles.
Actual: Agent does MCP discovery AND grep/read cycles (additive, not substitutive).

**Evidence:** Across all loss cases, MCP agents made 110-170% of no-MCP call counts.
MCP calls were additive overhead, not grep replacements.

### 5. Tools Never Used
Across ALL investigation transcripts analyzed (wins and losses):
- `trace_call_chain`: 0 uses (agents don't know two endpoints upfront)
- `open_node_context`: 0 uses (agents don't know about it)
- `get_review_context`: 0 uses in investigation (review-only tool)
- `get_impact_radius`: 0 uses in investigation (review-only tool)

The most valuable structural tools are invisible to the agent during investigation.

## Part 3: Research-Backed Improvement Proposals

### Proposal 1: Confidence + Coverage Signals (LOW effort, HIGH impact)

**Source:** BATS (arxiv 2511.17006) — budget signals cut search calls 40%.
SeekBench (arxiv 2509.22391) — calibrated stopping via evidence sufficiency.

**Implementation:** Every MCP response includes:
```json
{
  "results": [...],
  "meta": {
    "confidence": 0.87,        // how well results match the query
    "coverage": 0.72,          // fraction of relevant code space covered
    "overlap_with_previous": 0.65,  // dedup signal vs session history
    "suggestion": "high-confidence match found; consider reading top file"
  }
}
```

**How it helps:**
- Agent sees `overlap_with_previous: 0.65` → stops issuing redundant searches
- Agent sees `confidence: 0.87` → transitions to Read instead of more MCP calls
- Agent sees `suggestion` → guided toward next action

**Estimated impact:** -30-40% redundant MCP calls based on BATS paper data.

### Proposal 2: Compound Hierarchical Localization Tool (MEDIUM effort, HIGH impact)

**Source:** Agentless (arxiv 2407.01489) — 27% solve rate at $0.34/issue.
BugCerberus (arxiv 2502.15292) — hierarchical file→function→statement.
Meta-RAG (arxiv 2508.02611) — NL summaries condense codebases 80%.

**Implementation:** New tool `localize(query, depth="file"|"function"|"line")`:
- Internally does: keyword search → semantic search → file aggregation → rank
- Returns 3 files with function-level drill-down and 5-line preview
- One call replaces: hybrid_query + semantic_search + 2 Read calls

```json
{
  "candidates": [
    {
      "file": "scoring.go",
      "confidence": 0.91,
      "relevant_functions": [
        {"name": "Score", "line": 199, "preview": "func (pl *PodTopologySpread) Score(..."},
        {"name": "NormalizeScore", "line": 229, "preview": "func (pl *PodTopologySpread) NormalizeScore(..."}
      ],
      "why": "Contains scoring logic; minDomains field present but unused in Score()"
    }
  ]
}
```

**How it helps:**
- Replaces 3-5 discovery calls with 1 compound call
- Preview eliminates 2-3 Read calls
- "why" field gives the agent reasoning context, reducing false-path exploration
- Hard cap at 3 candidates prevents result bloat

**Estimated impact:** -50-60% discovery calls based on Agentless pattern.

### Proposal 3: Session-Aware Cross-Search Deduplication (LOW effort, MEDIUM impact)

**Source:** Observed pattern — 70-80% result overlap across sequential searches.

**Implementation:** MCP server maintains per-session seen-nodes set:
- Track which qualified_names have been returned in previous calls
- On subsequent calls, filter out already-seen nodes
- Return `new_results` + `previously_seen_count` metadata

```json
{
  "results": [/* only NEW nodes */],
  "meta": {
    "new_results": 3,
    "filtered_duplicates": 12,
    "total_unique_seen": 47
  }
}
```

**How it helps:**
- Agent sees "12 duplicates filtered" → knows further searching is saturated
- Only new information presented → higher signal-to-noise ratio
- Natural stopping signal when `new_results` drops to 0-1

**Estimated impact:** -20-30% redundant MCP calls. Simple hash set in server state.

### Proposal 4: Improved Tool Descriptions with Boundaries (LOW effort, MEDIUM impact)

**Source:** "MCP Tool Descriptions Are Smelly" (arxiv 2602.14878) — 90% of tools lack
"when NOT to use" guidance. Our own data: agents never use open_node_context or
trace_call_chain because descriptions don't explain WHEN they're better than search.

**Implementation:** Update each tool description:
```
semantic_search_nodes:
  "Find code by concept. Returns top-K nodes ranked by similarity.
   WHEN TO USE: conceptual queries without exact identifiers.
   WHEN NOT TO USE: exact symbol/filename lookups (use Grep instead).
   STOPPING HINT: if top result score > 0.8, it's a strong match.
   BETTER ALTERNATIVE: use hybrid_query for first discovery call;
   use open_node_context AFTER finding a function to get callers+source in one call."

open_node_context:
  "Get source code + callers + callees for a specific function in ONE call.
   WHEN TO USE: AFTER search finds a function name — replaces query_graph + Read.
   COST: 1 call replaces 3 calls (search → query_graph → Read)."
```

**How it helps:**
- Agents discover `open_node_context` (currently 0% usage in investigation)
- "STOPPING HINT" gives concrete threshold for transitioning out of discovery
- "BETTER ALTERNATIVE" guides tool selection

**Estimated impact:** Hard to quantify, but agents using 0% of structural tools is a fixable gap.

### Proposal 5: Structured Error Recovery (LOW effort, LOW-MEDIUM impact)

**Source:** SERF from "Bridging Protocol and Production" (arxiv 2603.13417).

**Implementation:** When search returns 0 or low-quality results:
```json
{
  "results": [],
  "meta": {
    "suggestion": "No matches for exact query. Try: (1) broader terms, (2) hybrid_query instead of semantic_search, (3) Grep for exact string match.",
    "alternative_queries": ["volume manager", "reconciler detach"]
  }
}
```

**How it helps:** Agent doesn't repeat failed queries with minor variations. Gets
actionable recovery guidance server-side.

### Proposal 6: Review-First Investigation Mode (MEDIUM effort, HIGH impact)

**Source:** Our own data — review tools (`get_review_context`, `get_impact_radius`) are
the most effective MCP tools but are NEVER used during investigation. 100% adoption in
review benchmarks, 0% in investigation benchmarks.

**Implementation:** Make `get_review_context` and `get_impact_radius` work for
investigation, not just review:
- Accept a query (not just changed files) as input
- Internally: localize query → top 3 files → get callers/callees/blast radius
- Return the "review context" for the most likely buggy files

This is essentially Proposal 2 (compound tool) but reusing existing review infrastructure.

**How it helps:**
- Leverages the tool that already wins (review: 100% adoption, 33% fewer calls)
- Agent gets callers+callees+source in one call during investigation
- No new tool to build — extends existing get_review_context

## Part 4: Priority Matrix

| # | Proposal | Effort | Impact | Risk |
|---|----------|--------|--------|------|
| 4 | Tool description boundaries | LOW | MEDIUM | None |
| 1 | Confidence + coverage signals | LOW | HIGH | None |
| 3 | Cross-search deduplication | LOW | MEDIUM | None |
| 5 | Structured error recovery | LOW | LOW-MED | None |
| 2 | Compound localization tool | MEDIUM | HIGH | Behavior change |
| 6 | Review-first investigation | MEDIUM | HIGH | Needs testing |

**Recommended execution order:** 4 → 1 → 3 → 5 → 2 → 6

Start with description fixes (free, immediate), add signals (cheap, high ROI),
then build compound tools once the signal infrastructure is validated.

## Part 5: Key Papers Referenced

| Paper | Key Insight | Arxiv |
|-------|-------------|-------|
| BATS | Budget signals in responses cut calls 40% | 2511.17006 |
| SeekBench | Calibration = answer only when evidence sufficient | 2509.22391 |
| Agentless | File→function→line cascade in 3 prompts | 2407.01489 |
| BugCerberus | Hierarchical localization +16.5% Top-1 | 2502.15292 |
| Meta-RAG | NL summaries condense 80%, SOTA localization | 2508.02611 |
| CodeScout | RL agent with Unix terminal matches graph tools | 2603.17829 |
| MCP Smelly | 90% of tool descriptions lack boundaries | 2602.14878 |
| CABP/ATBA/SERF | Budget allocation + structured error recovery | 2603.13417 |
| BRaIn | LLM relevance feedback +87.6% MAP | 2501.10542 |
| LocAgent | Graph navigation by LLM = 92.7% file accuracy | 2503.09089 |
