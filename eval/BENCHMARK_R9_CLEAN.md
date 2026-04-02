# Round 9: Clean 5-Way Comparison (2026-04-01)

**Date**: 2026-04-01
**Hardware**: Windows 11, RTX 5070 Ti 16GB (DirectML), AMD CPU
**Model**: Claude Sonnet 4.6 for all agents
**Mode**: bypassPermissions (no user interaction)
**Prompt**: Natural v3 (no coaching)
**Concurrency**: 3 agents max per batch (clean timing, <5% overhead)

## Variants

| Variant | Tool | Persistent? | Notes |
|---------|------|-------------|-------|
| **Grep** | Native Grep + Read | N/A | Baseline — no external tools |
| **Scout MCP** | `mcp__repo-scout__search_code` | Yes (MCP server) | repo-scout v0.1.0 (2026-04-01 build) |
| **Scout CLI** | `repo-scout query --json` via Bash | No (process per call) | Same binary, CLI invocation |
| **Scout+Graph** | Scout CLI + code-review-graph MCP | Partial | Scout CLI + graph MCP hybrid |
| **CodeDB CLI** | `codedb.exe` via Bash | No (re-indexes per call) | Zig build, Windows-patched |

### Tool Characteristics

| Tool | Index time (httpx/fastapi) | Query time | Per-call overhead |
|------|---------------------------|------------|-------------------|
| Grep | N/A | ~1ms | ~200ms (Bash spawn) |
| Scout MCP | 0 (persistent) | ~5ms | ~50ms (IPC) |
| Scout CLI | 0 (reads cache) | ~5ms | ~300ms (process spawn + JSON) |
| CodeDB CLI | **270ms / 3.6s** | **0.6ms / 3.9ms** | **270ms-3.6s (re-index!)** |
| Graph MCP | 0 (persistent) | ~10ms | ~100ms (IPC + embedding) |

## Results

### Time (seconds) — bold = fastest

| Case | Diff. | Grep | Scout MCP | Scout CLI | Scout+Graph | CodeDB |
|------|-------|------|-----------|-----------|-------------|--------|
| httpx-002 | Simple | 118.3 | 39.2 | **33.6** | 55.7 | 164.6 |
| httpx-003 | Medium | 55.7 | 47.1 | 74.8 | 77.9 | **41.6** |
| httpx-001 | Simple | 237.3 | 60.1 | **40.3** | 67.9 | 77.0 |
| fastapi-003 | Medium | 111.2 | 95.6 | 113.4 | **81.2** | 213.8 |
| **Average** | | **130.6** | **60.5** | **65.5** | **70.7** | **124.3** |

### Tokens (K) — bold = most efficient

| Case | Diff. | Grep | Scout MCP | Scout CLI | Scout+Graph | CodeDB |
|------|-------|------|-----------|-----------|-------------|--------|
| httpx-002 | Simple | 42 | 36 | 37 | 37 | 48 |
| httpx-003 | Medium | 36 | **33** | 38 | 38 | **33** |
| httpx-001 | Simple | 51 | 35 | **33** | 35 | 39 |
| fastapi-003 | Medium | 44 | **38** | 48 | 42 | 40 |
| **Average** | | **43.3** | **35.5** | **39.0** | **38.0** | **40.0** |

### Tool Calls — bold = fewest

| Case | Diff. | Grep | Scout MCP | Scout CLI | Scout+Graph | CodeDB |
|------|-------|------|-----------|-----------|-------------|--------|
| httpx-002 | Simple | 23 | 7 | **5** | 14 | 28 |
| httpx-003 | Medium | 9 | 8 | **5** | 13 | **6** |
| httpx-001 | Simple | 30 | 13 | **6** | 13 | 14 |
| fastapi-003 | Medium | 26 | 23 | 30 | **20** | 18 |
| **Average** | | **22.0** | **12.8** | **11.5** | **15.0** | **16.5** |

### Quality: Root Cause Found

| Case | Diff. | Grep | Scout MCP | Scout CLI | Scout+Graph | CodeDB |
|------|-------|------|-----------|-----------|-------------|--------|
| httpx-002 | Simple | YES | YES | YES | YES | YES |
| httpx-003 | Medium | YES | YES | YES | YES | YES |
| httpx-001 | Simple | YES | YES | YES | YES | YES |
| fastapi-003 | Medium | YES | YES | YES | YES | YES |
| **Total** | | **4/4** | **4/4** | **4/4** | **4/4** | **4/4** |

### Quality: Secondary Findings

| Case | Grep | Scout MCP | Scout CLI | Scout+Graph | CodeDB |
|------|------|-----------|-----------|-------------|--------|
| httpx-002 | 3 (deepest: ByteChunker masking, MultiDecoder.flush) | 3 (test gap, flush unreachable, seen_data) | 3 (test gap, flush, BrotliDecoder) | 4 (MultiDecoder.flush) | 3 (flush, ByteChunker, streaming) |
| httpx-003 | 3 (nonce drift, test gap, algorithm) | 3 (hardcoded b"auth", KeyError, nonce count) | 4 (nonce reset, test gap, quoting, preemptive) | 2 (hardcoded b"auth", regex) | 3 (algorithm RFC, nonce drift, _resolve_qop) |
| httpx-001 | 3 (verify=str skip, pragma:nocover, sync+async) | 3 (verify=str skip, pragma:nocover, deprecation msg) | 3 (cert deprecated, pragma:nocover, verify=str) | 3 (verify=str, pragma:nocover, deprecation) | 3 (verify=str, pragma:nocover, CHANGELOG regression) |
| fastapi-003 | 3 (PR #13537, _extract_form_body, test gap) | 3 (Form-only guard, sequence branch, test gap) | 3 (_extract_form_body, test gap, annotation) | 4 (cleanest fix: remove branch) | 3 (sequence branch, test codifies bug, test gap) |
| **Total** | **12** | **12** | **13** | **13** | **12** |

## Analysis

### Scout MCP vs Grep

| Metric | Scout MCP | Grep | Scout MCP advantage |
|--------|-----------|------|---------------------|
| Avg time | **60.5s** | 130.6s | **2.2x faster** |
| Avg tokens | **35.5K** | 43.3K | **18% fewer** |
| Avg tools | **12.8** | 22.0 | **42% fewer** |
| Root cause | 4/4 | 4/4 | Tied |
| Secondaries | 12 | 12 | Tied |

Scout MCP is **2.2x faster** than Grep with 18% fewer tokens and identical quality. The speedup comes from fewer round-trips (12.8 vs 22.0 tool calls) — each eliminated round-trip saves ~3-5s of Sonnet inference time.

Per-case speedup:
- httpx-001: **3.9x faster** (60s vs 237s — Grep ran Python SSL experiments)
- httpx-002: **3.0x faster** (39s vs 118s)
- httpx-003: **1.2x faster** (47s vs 56s — both fast, small repo)
- fastapi-003: **1.2x faster** (96s vs 111s — both used supplementary Bash)

### Scout MCP vs Scout CLI

| Metric | Scout MCP | Scout CLI | Notes |
|--------|-----------|-----------|-------|
| Avg time | **60.5s** | 65.5s | MCP 8% faster |
| Avg tokens | **35.5K** | 39.0K | MCP 9% fewer |
| Avg tools | 12.8 | **11.5** | CLI 10% fewer calls |

Roughly equivalent on small/medium repos. MCP has lower per-call overhead (no process spawn), CLI agents make fewer calls. The advantage should grow on larger repos where MCP's persistent connection avoids repeated startup costs.

### Scout MCP vs CodeDB

| Metric | Scout MCP | CodeDB | Notes |
|--------|-----------|--------|-------|
| Avg time | **60.5s** | 124.3s | Scout MCP 2.1x faster |
| Avg tokens | **35.5K** | 40.0K | Scout MCP 11% fewer |
| Avg tools | **12.8** | 16.5 | Scout MCP 22% fewer |

CodeDB's re-indexing overhead (270ms-3.6s per call) compounds across multiple invocations. On httpx (small), CodeDB can match or beat Scout MCP when the agent is targeted (httpx-003: CodeDB 41.6s vs Scout MCP 47.1s). On fastapi (medium), CodeDB is 2.2x slower. **Prediction**: gap will widen dramatically on larger repos (next.js, vscode, kubernetes) where re-indexing costs 10-30s per call.

### CodeDB's Unique Advantage

CodeDB won httpx-003 (41.6s, fastest across all variants) despite re-indexing. Why:
- Only 1 CodeDB search call + 5 Read calls — dead-on targeted
- httpx re-index is 270ms (negligible on small repos)
- Sub-millisecond query speed (590µs) is unmatched

**If CodeDB had a persistent MCP server**, it would likely be competitive with Scout MCP across all repo sizes. The Zig compiler bug that blocks MCP mode on Windows is the key limitation.

### Scout+Graph: Structural Queries Add Value

Scout+Graph (Scout CLI + code-review-graph MCP) had the highest secondary finding count (13, tied with Scout CLI) and produced the cleanest fix suggestions. The graph's structural queries ("who calls this?", "what are the callers?") enable deeper impact analysis.

However, it's the slowest Scout variant (70.7s avg) due to the additional MCP round-trips for graph queries. The value proposition is **quality over speed** — expect it to shine on complex multi-file cases in Tier 2+.

## Pending Cases

The following cases need to be run with the R9 variant matrix:

### Tier 2: Medium (next batch)

| Case | Grep | Scout MCP | Scout+Graph | CodeDB |
|------|------|-----------|-------------|--------|
| nextjs-006 | pending | pending | pending | pending |
| nextjs-005 | pending | pending | pending | pending |
| nextjs-004 | pending | pending | pending | pending |
| k8s-003 | pending | pending | pending | pending |
| fastapi-002 | pending | pending | pending | pending |

### Tier 3: Complex

| Case | Grep | Scout MCP | Scout+Graph | CodeDB |
|------|------|-----------|-------------|--------|
| nextjs-003 | pending | pending | pending | pending |
| nextjs-007 | pending | pending | pending | pending |
| nextjs-008 | pending | pending | pending | pending |
| vscode-003 | pending | pending | pending | pending |

### Tier 4: Monster

| Case | Grep | Scout MCP | Scout+Graph | CodeDB |
|------|------|-----------|-------------|--------|
| rust-002 | pending | pending | pending | pending |

## Environment Notes

- **Scout cache corruption**: All bench repo indexes had stale rkyv manifest.bin files from a previous repo-scout build (Mar 27 vs Apr 1). Fixed by rebuilding CLI from same source as MCP server and re-indexing all 6 repos.
- **CodeDB segfaults**: Previous CodeDB build segfaulted on every invocation due to missing Windows compatibility patches (stack overflow from 98KB path buffers, 512KB arrays on stack, missing flock/ftruncate guards). Fixed by rebuilding with all 6 patches applied.
- **Scout MCP auto-heal**: repo-scout commit 63b4faf adds automatic re-indexing when manifest.bin is corrupt or incompatible — future version mismatches will self-heal.
