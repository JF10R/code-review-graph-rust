# code-review-graph (Rust)

[![MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75+-orange.svg)](https://www.rust-lang.org)
[![MCP](https://img.shields.io/badge/MCP-compatible-green.svg)](https://modelcontextprotocol.io)

Structural knowledge graph for token-efficient code reviews. Parses your codebase with Tree-sitter, exposes it to Claude Code via MCP. Single binary, zero runtime dependencies, free local embeddings.

Rust rewrite of [code-review-graph](https://github.com/tirth8205/code-review-graph) by [Tirth Kanani](https://github.com/tirth8205).

## Why this exists

Claude Code reads entire files to review changes. On large codebases, that burns context on irrelevant code. This tool builds a graph of functions, classes, and their relationships — Claude queries the graph to find exactly what's impacted, then reads only those files. **6.8x average token reduction** (up to 49x on monorepos).

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

## Key Features

| Feature | Details |
|---------|---------|
| **14 languages** | Python, TypeScript, JavaScript, Vue, Go, Rust, Java, C, C++, C#, Ruby, PHP, Swift, Kotlin |
| **5-62x faster builds** | Parallel parsing via rayon |
| **Personalized PageRank** | Direction-aware, weighted blast radius — 30-50% fewer false positives than BFS |
| **Semantic search** | Jina Code v2 embeddings (768-dim, free local) with optional GPU acceleration |
| **Call chain tracing** | `trace_call_chain` finds shortest path between any two functions |
| **Framework edges** | JSX components, Express middleware, event emitters, pytest fixtures auto-detected |
| **Compact mode** | `compact: true` strips paths + low-value fields, reducing response tokens ~40% |
| **Hybrid search** | Reciprocal Rank Fusion of keyword + semantic results |
| **File-level retrieval** | `result_mode: "file"` — multi-channel fanout, file aggregation, evidence-rich results |
| **Query decomposition** | Extracts symbols, path fragments, domain terms from NL queries for targeted search |
| **Always fresh** | Background watcher + hash-skip + lazy stale-check per query |

## vs Python version

| | Python | Rust |
|---|--------|------|
| Build (Next.js, 2382 files) | 305s | **5.0s** (61x) |
| Graph save/load | 147/120 ms | **2.1/1.1 ms** (70-111x) |
| Binary size | ~150 MB (venv) | ~40 MB |
| Startup | 150-300 ms | 2-5 ms |
| Blast radius | BFS, all edges equal | PPR, weighted, direction-aware |
| Embeddings | API key required | Free local (Jina Code v2) |
| Graph freshness | Manual rebuild | Auto watcher + stale-check |
| Search | Keyword only | **Multi-channel fanout** (keyword + semantic + path + config) |
| Result granularity | Node only | **Node + file-level** with evidence |
| Query handling | Raw string match | **Query decomposition** (symbols, paths, domain terms) |
| Eval suite | None | **28-case gold set**, 6 repos, Hit@5 tracked |

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

## Agent Benchmark Results

8-case agent-level benchmark across 6 repos (httpx, FastAPI, Next.js, VS Code, Kubernetes, Rust compiler). Agents investigate real GitHub issues with and without MCP tools. Full results in [`eval/BENCHMARK_MCP_V1.3_RESULTS.md`](eval/BENCHMARK_MCP_V1.3_RESULTS.md).

| Repo size | MCP speedup | Cases |
|-----------|-------------|-------|
| >100K nodes (vscode, k8s, rust) | **2.9x faster** | Discovery-heavy investigations |
| 6-90K nodes (fastapi, next.js) | **1.6x faster** | Cross-file tracing |
| <2K nodes (httpx) | ~same | Grep is sufficient |

**MCP is faster in 5/8 cases, averaging 1.8x speedup.** Value comes from structural queries (`query_graph`, `trace_call_chain`) on large repos where grep is slow. Tokens are similar or slightly higher; the win is wall-clock time.

### PR Review Benchmark

4-case review benchmark: agent reviews a changed file for bugs, risks, and affected callers.

| Metric | No-MCP | MCP | Delta |
|--------|--------|-----|-------|
| Time | 2.4m | **2.0m** | **1.2x faster** |
| Tool calls | 30 | **21** | **-30%** |
| Tokens | 54,865 | 59,269 | +8% |

**MCP faster in all 4 review cases with zero losses.** `get_review_context` provides callers, callees, and blast radius in one call — the designed use case for this tool.

## Performance (v1.3.0)

| Operation | Before | After |
|-----------|--------|-------|
| Impact radius edge collection | O(E) full edge scan | O(degree sum) — 10-100x faster |
| Incremental file hash check | O(nodes_in_file) clones | O(1) hash lookup |
| Incremental parse (multi-file) | Sequential | Parallel via rayon — 4-8x faster |
| Keyword search per-query cost | N `to_lowercase()` allocs | Precomputed cache — 0 allocs |

## Adding a language

1. Add the tree-sitter grammar crate to `Cargo.toml`
2. Create `queries/<lang>.scm` with `@definition.class`, `@definition.function`, `@reference.import`, `@reference.call` patterns
3. Wire `detect_language()` and `grammar_for()` in `parser.rs`

See [queries/](queries/) for examples.

## Supported languages

Python, TypeScript, JavaScript, Vue, Go, Rust, Java, C, C++, C#, Ruby, PHP, Swift, Kotlin

## Credits

Rust rewrite of [code-review-graph](https://github.com/tirth8205/code-review-graph) by [Tirth Kanani](https://github.com/tirth8205). Core concept and MCP API from the original. Blast radius algorithm, parallel parsing, embeddings, framework edges, and persistence are new.

## License

MIT. See [LICENSE](LICENSE).
