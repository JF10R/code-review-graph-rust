# 4-Way Benchmark Prompts (Standardized)

## Prompt Template

All 4 variants use the same base prompt. Tool instructions are appended per variant.

### Base Prompt v2 — "Standardized" (used for standardized round)

```
You are investigating a bug in {REPO_PATH}.

Bug report: "{BUG_DESCRIPTION}"

Find the root cause. Identify specific files and code paths. Suggest a fix.
Do NOT edit any files — research only. Be efficient — find the answer and stop.
```

### Base Prompt v3 — "Natural" (recommended for final benchmarks)

```
There's a bug in the codebase at {REPO_PATH}.

Bug report: "{BUG_DESCRIPTION}"

Find the issue and suggest a fix. Do NOT edit any files — research only.
```

### Tool Appendix: Scout (append for Scout and Scout+MCP variants)

```
For code search, use `repo-scout query --repo {REPO_PATH} --query "TERM" --json` via Bash instead of Grep. Use Read to examine files after finding them.
```

### Tool Appendix: MCP (append for MCP and Scout+MCP variants)

```
You have access to MCP code-review-graph tools. Use hybrid_query, open_node_context, query_graph with repo_root="{REPO_PATH}" and compact: true for structural and semantic code discovery.
```

### Variant Matrix

| Variant | Base | +Scout | +MCP |
|---------|------|--------|------|
| Grep | yes | no | no |
| Scout | yes | yes | no |
| MCP | yes | no | yes |
| Scout+MCP | yes | yes | yes |

---

## Cases

### httpx-002
- **Repo**: D:/GitHub/bench-repos/httpx
- **Bug**: "Empty zstd-encoded response body causes decode error instead of returning empty bytes"
- **Ground truth**: `httpx/_decoders.py` — `ZStandardDecoder.decode()` missing empty-input guard
- **Source**: [encode/httpx#3412](https://github.com/encode/httpx/pull/3412)

### httpx-003
- **Repo**: D:/GitHub/bench-repos/httpx
- **Bug**: "RFC 2069 digest authentication fails — where is the qop field omitted for legacy digest auth compatibility?"
- **Ground truth**: `httpx/_auth.py` — `_resolve_qop` / `_build_auth_header` qop omission for RFC 2069
- **Source**: [encode/httpx#3045](https://github.com/encode/httpx/pull/3045)

### fastapi-003
- **Repo**: D:/GitHub/bench-repos/fastapi
- **Bug**: "Form fields with empty string values are incorrectly treated as None/missing instead of empty string in FastAPI dependency injection"
- **Ground truth**: `fastapi/dependencies/utils.py` — `_get_multidict_value` treats `value == ""` same as `None`
- **Source**: [fastapi/fastapi#13533](https://github.com/fastapi/fastapi/issues/13533) / [fastapi/fastapi#13537](https://github.com/fastapi/fastapi/pull/13537)

### vscode-003
- **Repo**: D:/GitHub/bench-repos/vscode
- **Bug**: "Keybinding resolver picks the wrong command when two keybindings have the same chord and one has a when clause with a negated context key"
- **Ground truth**: `keybindingResolver.ts` — `whenIsEntirelyIncluded` + `_findCommand` last-wins ordering
- **Source**: [microsoft/vscode#229761](https://github.com/microsoft/vscode/issues/229761)

### nextjs-006
- **Repo**: D:/GitHub/bench-repos/next.js
- **Bug**: "LRU cache memory leak — null route handler cache entries have size zero so they are never evicted causing unbounded memory growth"
- **Ground truth**: `filesystem.ts` — `calculateSize` returns 0 for null; `require.ts` falsy check on cached null
- **Source**: [vercel/next.js#89033](https://github.com/vercel/next.js/issues/89033) / [vercel/next.js#89040](https://github.com/vercel/next.js/pull/89040)

### fastapi-001 (Phase 1 — medium)
- **Repo**: D:/GitHub/bench-repos/fastapi
- **Bug**: "OAuth2 security schemes appear duplicated in OpenAPI spec with and without scopes when declared at APIRouter level"
- **Ground truth**: `fastapi/openapi/utils.py` — `get_openapi_security_definitions` list vs dict merge
- **Source**: [fastapi/fastapi#14454](https://github.com/fastapi/fastapi/issues/14454) / [fastapi/fastapi#14455](https://github.com/fastapi/fastapi/pull/14455)

### k8s-003 (Phase 1 — large, Go)
- **Repo**: D:/GitHub/bench-repos/kubernetes
- **Bug**: "Deployment rollout stalls indefinitely when maxUnavailable and maxSurge are both set — reconcileOldReplicaSets in the rolling update controller gets stuck in a loop without making progress"
- **Ground truth**: `pkg/controller/deployment/deployment_controller.go` / `rolling.go:reconcileOldReplicaSets`
- **Source**: [kubernetes/kubernetes#124847](https://github.com/kubernetes/kubernetes/issues/124847)

### k8s-004 (Phase 1 — large, Go)
- **Repo**: D:/GitHub/bench-repos/kubernetes
- **Bug**: "kubelet volume manager deadlocks when reconciler detaches a volume while attach/detach controller holds the global volume lock"
- **Ground truth**: `pkg/kubelet/volumemanager/volume_manager.go` — deadlock chain across 5 files, 2 lock types
- **Source**: [kubernetes/kubernetes#122718](https://github.com/kubernetes/kubernetes/issues/122718)

### rust-001 (Phase 1 — largest, Rust)
- **Repo**: D:/GitHub/bench-repos/rust
- **Bug**: "Borrow checker emits confusing error message about lifetime mismatch when returning a reference from a closure that captures a local"
- **Ground truth**: 5-crate compiler trace (borrowck → diagnostics → region_name → nice_region_error → session_diagnostics)
- **Source**: [rust-lang/rust#130528](https://github.com/rust-lang/rust/issues/130528)

---

## Agent Parameters

- **Model**: Sonnet 4.6
- **Mode**: bypassPermissions
- **Max time**: no artificial cap (measure natural completion)
- **Repo-scout**: must be indexed before run
- **MCP graph**: must be built + embedded before run
