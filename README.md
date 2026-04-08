# code-review-graph (Rust)

[![MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75+-orange.svg)](https://www.rust-lang.org)
[![MCP](https://img.shields.io/badge/MCP-compatible-green.svg)](https://modelcontextprotocol.io)

Structural code knowledge graph for faster, deeper AI-assisted investigations and reviews. Parses your codebase with Tree-sitter, exposes it to Claude Code via MCP. Single binary, zero runtime dependencies, free local embeddings.

**Benchmarked on 14 real GitHub issues across 7 repos (httpx to rust compiler): agents find root causes with 16% fewer tokens and the best findings-per-minute of any tool tested (0.87 findings/min vs grep's 0.75). The edge is sharpest on large TypeScript repos (>6K files) where `hybrid_query` replaces 5-9 grep/search cycles with one routed call.**

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

Optional GPU acceleration:
```bash
cargo install ... --features gpu-cuda       # NVIDIA GPU (recommended, robust multi-process)
cargo install ... --features gpu-directml   # Any GPU on Windows (AMD, Intel, NVIDIA)
```
If GPU init fails at runtime, the server automatically falls back to CPU.

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
cargo install ... --features gpu-cuda         # GPU embeddings (CUDA, NVIDIA)
cargo install ... --features gpu-directml    # GPU embeddings (DirectML, any GPU)
cargo install ... --features hnsw-index      # HNSW vector search (10k+ nodes)
cargo install ... --features tantivy-search  # Fuzzy full-text search
cargo install ... --no-default-features      # Minimal binary (no embeddings)
```

## Benchmark Results

75 agent runs across 14 real GitHub issues and 7 repos (60-file httpx to 36.8K-file rust compiler). Five tool configurations: Grep (baseline), Scout MCP (repo-scout), Graph MCP (this tool), CodeDB MCP, and Scout+Graph. Claude Sonnet 4.6, `bypassPermissions`, sequential runs. Every variant found the correct root cause on every case (100% hit rate) — the comparison is efficiency, cost, and analysis depth.

Full methodology, per-case data, and quality scoring: [`eval/BENCHMARKS.md`](eval/BENCHMARKS.md)

### 5-way comparison (14 cases, 75 runs)

| Metric | Grep | Scout MCP | **Graph MCP** | CodeDB MCP | Scout+Graph |
|--------|------|-----------|--------------|------------|-------------|
| Avg time | 217s | 184s | **182s** | 260s | 201s |
| Avg tokens | 61K | 66K | **57K** | 66K | 65K |
| Avg tools | 38t | 27t | 32t | 35t | 30t |
| Findings/min | 0.75 | 0.84 | **0.87** | 0.58 | 0.81 |
| Findings/10K tok | 0.44 | 0.39 | **0.46** | 0.38 | 0.42 |
| Wins (fastest) | 3 | **7** | 1 | 1 | 2 |
| Wins (lowest tok) | 3 | 3 | **5** | 3 | 0 |

Repos: [httpx](https://github.com/encode/httpx) (60 files), [httpd](https://github.com/apache/httpd) (562), [FastAPI](https://github.com/fastapi/fastapi) (1.1K), [Next.js](https://github.com/vercel/next.js) (6.4K), [VS Code](https://github.com/microsoft/vscode) (7K), [Kubernetes](https://github.com/kubernetes/kubernetes) (16.9K), [Rust compiler](https://github.com/rust-lang/rust) (36.8K).

### Why Graph MCP beats Grep

Graph MCP uses **16% fewer tokens** than Grep (57K vs 61K) and produces **16% more findings per minute** (0.87 vs 0.75). The advantage comes from structural precision:

| What Graph does | What Grep requires instead | Token savings |
|----------------|---------------------------|---------------|
| `hybrid_query("OAuth2 duplication")` returns the right file at rank #1 | 3-5 grep cycles trying different terms, reading wrong files | ~30% fewer tokens on first discovery |
| `query_graph(callers_of, "ap_invoke_handler")` returns callers directly | grep for function name, read each file, grep for callers in each | ~50% fewer tokens per call-chain hop |
| `open_node_context("_findCommand")` returns source + callers + callees in one call | grep → read file → grep for callers → read those files | 1 call vs 4-6 calls |

The edge is sharpest on **large TypeScript/JS repos** (Next.js, VS Code): Graph MCP won fastest on nextjs-006 (132s vs Grep 258s, 2.0x) and lowest tokens on nextjs-004 (51K vs 69K, 26% savings). On small Python repos, Grep is competitive because symbol names are unique and files are few.

Graph MCP's weakness: **first-time setup cost**. Building the graph takes 0.2s (60 files) to 6m17s (36.8K files). Embeddings add similar overhead. After the initial build, incremental updates only re-parse changed files.

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

### Where Graph MCP helps most

| Scenario | vs Grep | Why |
|----------|---------|-----|
| Large TS repos (Next.js, 6.4K files) | **2-4x faster, 26% fewer tokens** | `hybrid_query` replaces 5-9 grep/read cycles; structural queries trace reducers/hooks directly |
| Complex multi-file bugs (vscode-003) | **2.1x faster, best quality (4/4 secondaries)** | `callers_of`/`callees_of` trace keybinding dispatch chain in 17 tool calls vs Grep's 25 |
| Cross-file call tracing | **50% fewer tokens per hop** | `query_graph(callers_of)` returns callers directly vs grep → read → grep again |
| Code reviews | **1.2x** | `get_review_context` bundles callers, callees, blast radius in one call |
| Small Python repos (<100 files) | ~same | Grep is fast enough; MCP transport adds overhead |
| Go repos (globally unique identifiers) | **Grep wins** | Go function names are perfectly greppable; Graph overhead not justified |

## Adding a language

1. Add the tree-sitter grammar crate to `Cargo.toml`
2. Create `queries/<lang>.scm` with `@definition.class`, `@definition.function`, `@reference.import`, `@reference.call` patterns
3. Wire `detect_language()` and `grammar_for()` in `parser.rs`

See [queries/](queries/) for examples.

## Credits

Rust rewrite of [code-review-graph](https://github.com/tirth8205/code-review-graph) by [Tirth Kanani](https://github.com/tirth8205). Core concept and MCP API from the original. Hybrid search, query routing, blast radius (PPR), parallel parsing, local embeddings, framework edges, and source preview are new in the Rust version.

## License

MIT. See [LICENSE](LICENSE).
