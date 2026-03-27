# code-review-graph (Rust)

[![MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75+-orange.svg)](https://www.rust-lang.org)
[![MCP](https://img.shields.io/badge/MCP-compatible-green.svg)](https://modelcontextprotocol.io)

Structural code knowledge graph for faster, deeper AI-assisted investigations and reviews. Parses your codebase with Tree-sitter, exposes it to Claude Code via MCP. Single binary, zero runtime dependencies, free local embeddings.

**Benchmarked on 10 real GitHub issues across 6 repos (httpx to rust compiler): agents find root causes 2x faster than grep-only and 2x faster than the [Python MCP](https://github.com/tirth8205/code-review-graph), with deeper causal analysis on complex bugs. The edge is sharpest on large codebases (>6K files) where `hybrid_query` replaces 5-9 grep/search cycles with one routed call.**

Rust rewrite of [code-review-graph](https://github.com/tirth8205/code-review-graph) by [Tirth Kanani](https://github.com/tirth8205).

## Why this exists

Claude Code uses grep and file reads to investigate bugs. On large codebases, agents spend most of their time on discovery — grepping for function names, reading files to understand callers, then grepping again to trace the next hop. This tool builds a graph of functions, classes, and call relationships so the agent can ask structural questions directly: "who calls this function?", "what's the blast radius of this change?", "find code related to this concept." One graph query replaces 3-5 grep/read cycles.

## Install

### Prebuilt binaries (recommended)

Download the latest release for your platform from [Releases](https://github.com/JF10R/code-review-graph-rust/releases/latest):

| Platform | Binary |
|----------|--------|
| Linux x86_64 | `code-review-graph-linux-amd64` |
| macOS x86_64 | `code-review-graph-macos-amd64` |
| macOS ARM64 | `code-review-graph-macos-arm64` |
| Windows x86_64 | `code-review-graph-windows-amd64.exe` |

Place the binary somewhere on your `PATH` and rename it to `code-review-graph` (or `code-review-graph.exe` on Windows).

### From source

```bash
cargo install --git https://github.com/JF10R/code-review-graph-rust
```

## Quick Start

```bash
cd your-project
code-review-graph install   # Creates .mcp.json
code-review-graph build     # Parses codebase
# Restart Claude Code — done
```

The MCP server starts automatically and keeps the graph fresh via a background watcher.

## How it works

```
                    Agent query: "OAuth2 schemes duplicated in OpenAPI"
                                        |
                                  hybrid_query()
                                        |
                    +-------------------+-------------------+
                    |                   |                   |
            classify_query()     keyword search      semantic search
            route: General       (Tantivy/relaxed)   (Jina Code v2)
                    |                   |                   |
                    +-------------------+-------------------+
                                        |
                              Reciprocal Rank Fusion
                              + file-level aggregation
                                        |
                              Top 3 files + evidence nodes
                              + source preview (5 lines)
                              + match_type, confidence
```

**Build phase**: Tree-sitter parses your codebase in parallel (rayon), extracting functions, classes, imports, and call edges into a SQLite graph. Incremental updates only re-parse changed files (content-hash skip).

**Query phase**: `hybrid_query` classifies each query and routes it:
- **ExactSymbol** (`getUserById`) — keyword-only, no embedding overhead
- **FilePath** (`src/auth/middleware.ts`) — keyword with path boosting
- **ExactText** (`"TypeError: Cannot read..."`) — literal search for error messages
- **Causal** (`"why does cache invalidation fail"`) — full hybrid (keyword + semantic + fusion)
- **General** — multi-channel fanout with Reciprocal Rank Fusion

Results are aggregated to file-level, scored with evidence (which query terms matched, which search channels contributed), and returned with source previews so the agent often doesn't need a follow-up Read.

**Structural queries**: `query_graph(callers_of)`, `trace_call_chain(A, B)`, and `open_node_context` answer graph questions directly — "who calls this function?", "how does A reach B?", "give me source + callers + callees in one call." These replace the grep-for-callers → read-file → grep-again cycle that accounts for most agent overhead on large repos.

**Blast radius**: `get_impact_radius` uses Personalized PageRank (direction-aware, weighted by edge type) to find transitively affected code. Unlike BFS, PPR naturally decays with distance and weights CALLS edges higher than CONTAINS, reducing false positives 30-50%.

## Key features

| Feature | Details |
|---------|---------|
| **14 languages** | Python, TypeScript, JavaScript, Vue, Go, Rust, Java, C, C++, C#, Ruby, PHP, Swift, Kotlin |
| **5-62x faster builds** | Parallel Tree-sitter parsing via rayon |
| **Personalized PageRank** | Direction-aware, weighted blast radius |
| **Semantic search** | Jina Code v2 embeddings (768-dim, free local) with optional GPU |
| **Hybrid search** | Automatic query classification + multi-channel fusion |
| **Call chain tracing** | Shortest path between any two functions |
| **Framework edges** | JSX components, Express middleware, pytest fixtures auto-detected |
| **Source preview** | High-confidence results include 5-line previews (saves a Read call) |
| **Always fresh** | Background watcher + content-hash skip + lazy stale-check |

## vs Python MCP

| | Python MCP | Rust MCP |
|---|--------|------|
| **Agent benchmark** (4 cases) | 76K tok / 5.3m avg | **59K tok / 2.6m avg** (2x faster, 22% cheaper) |
| **Agent analysis depth** | Deeper than grep on 2/4 | **Deeper than grep on 3/4** |
| Build (Next.js, 6396 files) | ~300s | **5.0s** (60x) |
| Binary size | ~150 MB (venv) | ~40 MB |
| Startup | 150-300 ms | 2-5 ms |
| Blast radius | BFS, all edges equal | PPR, weighted, direction-aware |
| Embeddings | API key or MiniLM (384-dim) | Free local Jina Code v2 (768-dim) |
| Search | `semantic_search_nodes` only | **`hybrid_query`** (keyword + semantic + routing) |
| Result granularity | Node only | **Node + file-level** with evidence + source preview |
| Query handling | Raw string match | **Query decomposition** + automatic route classification |
| Graph freshness | Manual rebuild | Auto watcher + stale-check |

The biggest efficiency gap: Python agents issue 5-9 overlapping `semantic_search_nodes` calls to explore a bug. Rust's `hybrid_query` consolidates keyword + semantic + path search with automatic query routing into 2-3 calls.

## Embeddings

Works out of the box — no API key needed:

```bash
code-review-graph embed   # Downloads model on first run (~90 MB)
```

Optional providers: OpenAI, Voyage AI, Google Gemini. See `code-review-graph config --help`.

Optional GPU acceleration (Windows):
```bash
cargo install ... --features gpu-directml   # ~80x faster on large codebases
```

## Configuration

```bash
code-review-graph config set embedding-provider voyage
code-review-graph config set voyage-api-key pa-...
```

Exclude files via `.code-review-graphignore` (gitignore syntax).

### PostToolUse hook (always-fresh graph)

```json
{
  "hooks": {
    "PostToolUse": [{
      "matcher": "Edit|Write",
      "hooks": [{"type": "command", "command": "code-review-graph update -q"}]
    }]
  }
}
```

> Only match `Edit|Write`, never `Bash`. Including `Bash` triggers rebuilds on every shell command.

### Recommended CLAUDE.md snippet

Add this to your project's `CLAUDE.md`:

```markdown
## Code Review Graph (MCP)

**When to use graph tools vs Grep:**
- **Grep/Read**: exact filename, symbol name, or string literal lookups
- **Graph tools**: structural queries (who calls X?), semantic/conceptual search, cross-file tracing

| Tool | When to use |
|------|-------------|
| `hybrid_query` | Best first call — combined keyword + semantic + structural search |
| `open_node_context` | After finding a function — get source + callers + callees in one call |
| `trace_call_chain(from, to)` | Trace how function A reaches function B |
| `query_graph(callers_of)` | Who calls this function? |
| `get_review_context` | Token-efficient review bundle for changed files |
| `get_impact_radius` | Blast radius analysis for a set of changed files |
| `semantic_search_nodes` | Find code by concept when keyword search fails |

Always pass `compact: true`. Use 3-5 MCP calls for discovery, then `Read`/`Grep` for details.
```

## CLI

```
code-review-graph install     # Register MCP server
code-review-graph build       # Full parse
code-review-graph update      # Incremental update
code-review-graph embed       # Compute embeddings
code-review-graph status      # Graph stats
code-review-graph watch       # Auto-update on changes
code-review-graph visualize   # Interactive HTML graph
code-review-graph serve       # MCP server (stdio)
code-review-graph config      # API keys, provider settings
```

## Optional features

```bash
cargo install ... --features gpu-directml    # GPU embeddings (DirectML)
cargo install ... --features hnsw-index      # HNSW vector search (10k+ nodes)
cargo install ... --features tantivy-search  # Fuzzy full-text search
cargo install ... --no-default-features      # Minimal binary (no embeddings)
```

## Benchmark Results

14-case agent benchmark: Sonnet 4.6 investigates real GitHub issues and reviews code, with and without MCP. Three configurations tested: no MCP (grep/read only), [Python MCP](https://github.com/tirth8205/code-review-graph) v1.8.4, and this Rust MCP v1.5. All agents found correct root causes — the comparison is speed, cost, and analysis depth.

Full methodology, per-case data, and retesting checklist: [`eval/BENCHMARKS.md`](eval/BENCHMARKS.md)

### Investigation: 3-way comparison (4 shared cases)

| Metric | No MCP | Python MCP | **Rust MCP** |
|--------|--------|-----------|-------------|
| Avg tokens | 77K | 76K | **59K** (-22%) |
| Avg time | 7.4m | 5.3m | **2.6m** (2.8x vs no-MCP) |
| Avg tool calls | 37 | 38 | **24** (-37%) |
| Deeper analysis | 0/4 | 2/4 | **3/4** |

Repos: [httpx](https://github.com/encode/httpx) (1.3K nodes), [FastAPI](https://github.com/fastapi/fastapi) (6.4K), [Kubernetes](https://github.com/kubernetes/kubernetes) (210K), [VS Code](https://github.com/microsoft/vscode) (184K).

### Investigation: all 10 Rust MCP cases

| Case | Issue source | No-MCP | **Rust MCP** | Speedup |
|------|-------------|--------|-------------|---------|
| vscode-003 | [#keybinding resolver](https://github.com/microsoft/vscode) | 96K / 13.2m | **60K / 2.7m** | **4.9x** |
| k8s-002 | [#PodTopologySpread](https://github.com/kubernetes/kubernetes) | 78K / 7.6m | **59K / 3.2m** | **2.4x** |
| fastapi-001 | [#14454](https://github.com/fastapi/fastapi/issues/14454) | 94K / 8.3m | **76K / 3.9m** | **2.1x** |
| nextjs-simple | [#91862](https://github.com/vercel/next.js/issues/91862) | 86K / 6.2m | 143K / 16.2m | 1.8x* |
| rust-001 | [#borrow checker](https://github.com/rust-lang/rust) | 70K / 8.2m | 89K / 5.6m | **1.5x** |
| fastapi-003 | [#form fields](https://github.com/fastapi/fastapi) | 52K / 102s | **43K / 73s** | **1.4x** |
| httpx-003 | [#digest auth](https://github.com/encode/httpx) | 38K / 87s | 39K / 72s | **1.2x** |
| nextjs-complex | [#89252](https://github.com/vercel/next.js/issues/89252) | 78K / 4.5m | 82K / 4.1m | **1.1x** |
| httpx-002 | [#zstd decoder](https://github.com/encode/httpx) | 40K / 59s | 40K / 84s | 0.7x |
| k8s-004 | [#volume deadlock](https://github.com/kubernetes/kubernetes) | 84K / 3.2m | 85K / 4.1m | 0.78x |

Faster in **8/10 cases**, average **2.0x speedup**. *nextjs-simple compared against full-repo no-MCP baseline (29.7m).

### Why quality differs, not just speed

On 6/10 cases, the Rust MCP agent produced **deeper root-cause analysis** than grep-only. The graph enables structural questions that grep can't answer efficiently:

| Bug | No-MCP finding | Rust MCP finding (additional) |
|-----|---------------|-------------------------------|
| FastAPI OAuth2 duplication | Found the dict-merge fix | Also traced the 4-step chain: cache_key divergence → get_flat_dependant → _security_dependencies → duplication mechanism |
| VS Code keybinding resolver | Found _findCommand ordering | Also found _map memory leak (never pruned), shadow variable, and chord-length vs context priority conflict |
| Next.js CSS shared chunks | Found the chunking plugin | Traced the full 3-stage chain: next-app-loader injection → FlightClientEntryPlugin propagation → CssChunkingPlugin chunk.split() |
| Rust borrow checker errors | Found region_errors.rs | Traced through 5 compiler crates to identify DefaultReturn → CannotMatchHirTy fallback path |

This matters because deeper causal chains lead to more targeted fixes. Finding "change this dict" is correct but shallow — understanding "the duplication happens because cache_key includes scopes, so the same scheme appears twice with different keys" tells you WHERE ELSE the same pattern could break.

### PR review: 3-way comparison (vscode-003)

| Metric | No MCP | Python MCP | **Rust MCP** |
|--------|--------|-----------|-------------|
| Tokens | 56K | 64K | **59K** |
| Time | 1.9m | 2.1m | **1.9m** |
| Real findings | ~4 | 5 | **6** |

Both MCPs found the `_map` memory leak and `_isTargetedForRemoval` prefix issue. Rust uniquely found a shadow variable bug and chord-ordering edge case. Python uniquely found the `softDispatch` state issue.

### Where Rust MCP helps most

| Scenario | Speedup | Why |
|----------|---------|-----|
| Large repos (>100K nodes) | **2-5x** | `hybrid_query` + `open_node_context` replace 10+ grep/read cycles |
| Cross-file bug tracing | **1.5-2x** | `query_graph(callers_of)` and `trace_call_chain` follow call graphs directly |
| Code reviews | **1.2x** | `get_review_context` bundles callers, callees, blast radius in one call |
| Small repos (<2K nodes) | ~same or slower | Grep is fast enough; MCP transport adds overhead |

## Adding a language

1. Add the tree-sitter grammar crate to `Cargo.toml`
2. Create `queries/<lang>.scm` with `@definition.class`, `@definition.function`, `@reference.import`, `@reference.call` patterns
3. Wire `detect_language()` and `grammar_for()` in `parser.rs`

See [queries/](queries/) for examples.

## Credits

Rust rewrite of [code-review-graph](https://github.com/tirth8205/code-review-graph) by [Tirth Kanani](https://github.com/tirth8205). Core concept and MCP API from the original. Hybrid search, query routing, blast radius (PPR), parallel parsing, local embeddings, framework edges, and source preview are new in the Rust version.

## License

MIT. See [LICENSE](LICENSE).
