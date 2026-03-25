# code-review-graph (Rust)

[![MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75+-orange.svg)](https://www.rust-lang.org)
[![MCP](https://img.shields.io/badge/MCP-compatible-green.svg)](https://modelcontextprotocol.io)

Structural knowledge graph for token-efficient code reviews. Parses your codebase with Tree-sitter, exposes it to Claude Code via MCP. Single binary, zero runtime dependencies, free local embeddings.

Rust rewrite of [code-review-graph](https://github.com/tirth8205/code-review-graph) by [Tirth Kanani](https://github.com/tirth8205).

## Why this exists

Claude Code reads entire files to review changes. On large codebases, that burns context on irrelevant code. This tool builds a graph of functions, classes, and their relationships — Claude queries the graph to find exactly what's impacted, then reads only those files. **6.8x average token reduction** (up to 49x on monorepos).

## Quick Start

```bash
cargo install --git https://github.com/JF10R/code-review-graph-rust
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
| **7-77x faster builds** | Parallel parsing via rayon |
| **Personalized PageRank** | Direction-aware, weighted blast radius — 30-50% fewer false positives than BFS |
| **Semantic search** | Jina Code v2 embeddings (768-dim, free local) with optional GPU acceleration |
| **Call chain tracing** | `trace_call_chain` finds shortest path between any two functions |
| **Framework edges** | JSX components, Express middleware, event emitters, pytest fixtures auto-detected |
| **Compact mode** | `compact: true` strips paths + low-value fields, reducing response tokens ~40% |
| **Always fresh** | Background watcher + hash-skip + lazy stale-check per query |

## vs Python version

| | Python | Rust |
|---|--------|------|
| Build (Next.js, 2382 files) | 305s | **4.3s** (71x) |
| Graph save/load | 147/120 ms | **2.1/1.1 ms** (70-111x) |
| Binary size | ~150 MB (venv) | ~40 MB |
| Startup | 150-300 ms | 2-5 ms |
| Blast radius | BFS, all edges equal | PPR, weighted, direction-aware |
| Embeddings | API key required | Free local (Jina Code v2) |
| Graph freshness | Manual rebuild | Auto watcher + stale-check |

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
