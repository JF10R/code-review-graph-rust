# Benchmark Results

All benchmarks use Claude Sonnet 4.6 in `bypassPermissions` mode, investigating real GitHub issues. Each case runs three times: no MCP (grep/read only), Python MCP v1.8.4, and Rust MCP v1.5.

**Benchmark repos**: [httpx](https://github.com/encode/httpx) (60 files), [FastAPI](https://github.com/fastapi/fastapi) (1,122 files), [Next.js](https://github.com/vercel/next.js) (6,396 files), [VS Code](https://github.com/microsoft/vscode) (6,981 files), [Kubernetes](https://github.com/kubernetes/kubernetes) (16,933 files), [Rust compiler](https://github.com/rust-lang/rust) (36,780 files)

## Investigation benchmarks

### Tested with all 3 configurations (2026-03-27)

| Case | Bug source | No MCP | Python MCP | Rust MCP | Best |
|------|-----------|--------|-----------|---------|------|
| httpx-002 | zstd empty body decode error | 40K / 59s | 49K / 52s | 40K / 84s | No-MCP (tok), Python (time) |
| fastapi-001 | [#14454](https://github.com/fastapi/fastapi/issues/14454) OAuth2 duplication | 94K / 8.3m | 85K / 6.7m | **76K / 3.9m** | **Rust** |
| k8s-002 | PodTopologySpread scoring + minDomains | 78K / 7.6m | 60K / 1.9m | 59K / 3.2m | Rust (tok), Python (time) |
| vscode-003 | Keybinding resolver negated when clause | 96K / 13.2m | 109K / 11.6m | **60K / 2.7m** | **Rust** |

**Averages:**

| | No MCP | Python MCP | Rust MCP |
|---|--------|-----------|---------|
| Tokens | 77K | 76K | **59K** |
| Time | 7.4m | 5.3m | **2.6m** |
| Tool calls | 37 | 38 | **24** |

### Tested with Rust MCP + no-MCP only (2026-03-27)

| Case | Bug source | No MCP | Rust MCP | Speedup |
|------|-----------|--------|---------|---------|
| httpx-003 | RFC 2069 digest auth qop field | 38K / 87s | 39K / 72s | **1.2x** |
| fastapi-003 | Form empty string treated as None | 52K / 102s | **43K / 73s** | **1.4x** |
| k8s-004 | Volume manager deadlock (2 lock types) | 84K / 3.2m | 85K / 4.1m | 0.78x |
| rust-001 | Borrow checker closure lifetime messages | 70K / 8.2m | 89K / 5.6m | **1.5x** |
| nextjs-complex | [#89252](https://github.com/vercel/next.js/issues/89252) CSS shared chunks | 78K / 4.5m | 82K / 4.1m | **1.1x** |
| nextjs-simple | [#91862](https://github.com/vercel/next.js/issues/91862) SASS precision | 86K / 6.2m* | 143K / 16.2m | 1.8x* |

*nextjs-simple no-MCP on full repo was 29.7m; 6.2m was sparse checkout.

### Needs retesting with Python MCP

These cases were tested with Rust MCP but not yet with the current Python MCP (graphs were broken during first attempt). Priority candidates for next benchmark round:

- [ ] httpx-003 — small repo, tests ExactText route potential
- [ ] fastapi-003 — medium repo, form validation bug
- [ ] k8s-004 — large repo, complex deadlock (Rust MCP lost on time)
- [ ] rust-001 — largest repo, deep compiler investigation
- [ ] nextjs-complex — medium repo, cross-file CSS tracing
- [ ] nextjs-simple — medium repo, cross-language JS/Rust investigation

### Needs retesting with all 3 configurations

These cases exist in the gold eval set but have not been benchmarked with any MCP:

- [ ] vscode-001, vscode-002, vscode-004 — VS Code (184K nodes)
- [ ] k8s-001, k8s-003 — Kubernetes (210K nodes)
- [ ] nextjs-001 through nextjs-011 (excluding 004, 010) — Next.js (90K nodes)
- [ ] rust-002, rust-003 — Rust compiler (286K nodes)
- [ ] httpx-001 — httpx (1.3K nodes)
- [ ] fastapi-002 — FastAPI (6.4K nodes)

## Quality comparison

All agents found correct root causes on all cases (100% accuracy across all configurations). The differentiation is **analysis depth** — how well the agent traces the full causal chain vs finding just the fix.

| Bug | No MCP | Python MCP | Rust MCP |
|-----|--------|-----------|---------|
| FastAPI OAuth2 | Found dict-merge fix | Traced 4-step chain (cache_key → flat_dependant → security_deps → duplication) | Same 4-step chain + cache_key divergence mechanism |
| VS Code keybinding | Found _findCommand ordering | Found _map leak + prefix removal + softDispatch state | Found _map leak + shadow variable + chord ordering + prefix removal |
| k8s-002 scoring | Found scoring/filtering asymmetry | Same + test comment explaining design decision | Same + topologyNormalizingWeight calibration issue |
| httpx-002 zstd | Found missing empty guard | Same, also identified seen_data flag | Same, also traced iter_bytes → decode → flush chain |

**Why depth matters**: Finding "change this dict" is correct but shallow. Understanding "the duplication happens because cache_key includes scopes, so the same scheme appears twice with different keys" tells you where else the same pattern could break, what the fix's blast radius is, and whether the fix is complete.

## PR review benchmarks

### Tested with all 3 configurations (2026-03-27, vscode-003)

| | No MCP | Python MCP | Rust MCP |
|---|--------|-----------|---------|
| Tokens | 56K | 64K | **59K** |
| Time | 1.9m | 2.1m | **1.9m** |
| Real findings | ~4 est | 5 | **6** |

Shared findings (both MCPs): `_map` memory leak, `_isTargetedForRemoval` prefix semantics, `handleRemovals` isDefault guard, `lookupPrimaryKeybinding` context skip.
Rust-only: shadow variable in `_addKeyPress`, `MoreChordsNeeded` vs exact-match ordering.
Python-only: `softDispatch` chord state management.

### Tested with Rust MCP + no-MCP only (2026-03-27)

| Case | No MCP | Rust MCP | Real findings |
|------|--------|---------|---------------|
| fastapi-001 review | 63K / 2.0m | 86K / 4.6m | 6 (cache mutation, decorator gap, OpenAPI collision, ...) |
| nextjs-001 review | 57K / 3.7m | 62K / 3.0m | 7 (PostCSS singleton, empty Event, regex gaps, ...) |
| nextjs-006 review | 43K / 1.9m | 58K / 2.4m | 5 (NaN infinite loop, silent disk cache failure, ...) |

Note: Rust MCP reviews produce ~2x more real findings but at higher cost. The richer context enables deeper exploration. Review-specific budget controls are planned.

## Methodology

- **Model**: Claude Sonnet 4.6 for all agents
- **Mode**: `bypassPermissions` (no user interaction)
- **MCP prompt**: "Use the code-review-graph MCP tools for discovery. Always pass compact: true."
- **No-MCP prompt**: Same bug report, no MCP instruction
- **Metrics**: tokens (from API usage), wall-clock time (from task duration), tool call count
- **Quality**: manual assessment against ground truth files from gold eval set
- **Gold eval set**: 28 cases with ground truth files and root cause descriptions. See `eval/gold-eval-set.json`
- **Hardware**: Windows 11, RTX 5070 Ti 16GB (DirectML), AMD CPU
