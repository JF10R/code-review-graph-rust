# code-review-graph (Rust)

> Rust rewrite of [code-review-graph](https://github.com/tirth8205/code-review-graph) by Tirth Kanani. Single binary, zero dependencies, 50-200x faster queries.

**Stop burning tokens. Start reviewing smarter.**

Claude Code re-reads your entire codebase on every task. `code-review-graph` fixes that. It builds a structural map of your code with [Tree-sitter](https://tree-sitter.github.io/tree-sitter/), tracks changes incrementally, and gives Claude precise context so it reads only what matters.

This is a complete rewrite in Rust of the original Python implementation, preserving full API compatibility while delivering dramatically better performance and distribution.

---

## Why this exists

### Without code-review-graph

Claude Code reads **every changed file plus context** on each task. On a 1,000-file project, that's thousands of tokens burned just finding the right 5 files. On a monorepo, it's catastrophic.

### With code-review-graph (Python)

The original Python version solves this with a structural knowledge graph — Tree-sitter parses your code into nodes (functions, classes, imports) and edges (calls, inheritance, test coverage). Claude queries the graph instead of scanning files. **6.8x fewer tokens on average, up to 49x on monorepos.**

### With code-review-graph (Rust) — this version

Same solution, fundamentally better execution:

| | Without code-review-graph | Python (original) | **Rust (this version)** |
|---|---|---|---|
| Token usage | Full scan every time | **6.8x reduction** | **6.8x reduction** (same graph) |
| Installation | N/A | Python 3.10+ venv, ~150 MB | **Single 28 MB binary** |
| Startup | N/A | ~150-300 ms | **~2-5 ms** |
| `list_graph_stats` | N/A | ~5 ms (SQL) | **<0.1 ms** (O(1) in-memory) |
| `get_impact_radius` first call | N/A | ~200 ms (build cache) | **<1 ms** (always in memory) |
| `query_graph callers_of` | N/A | ~10 ms (SQL + cache) | **<0.5 ms** (direct HashMap) |
| Incremental update (5 files) | N/A | ~200 ms | **~50 ms** |
| Graph on disk (1k files) | N/A | ~2 MB (SQLite) | **~200 KB** (bincode+zstd) |
| Graph on disk (10k files) | N/A | ~20 MB | **~2 MB** |
| Runtime dependencies | N/A | Python + pip + venv | **None** |
| Auto-update graph | N/A | Manual hook | **Background watcher + lazy stale-check** |

---

## Quick Start

### Download the binary

```bash
# From GitHub releases (coming soon)
# or build from source:
cargo install --path .
```

### Initialize

```bash
cd your-project
code-review-graph install   # Creates .mcp.json for Claude Code
code-review-graph build     # Parses the codebase (~10s for 500 files)
```

Restart Claude Code after installation.

### Use

The graph updates **automatically**:
- The MCP server starts a **background watcher** that re-indexes modified files in real time
- Each tool request checks **graph freshness** and runs an incremental update if needed
- The `--quiet` flag enables integration with Claude Code PostToolUse hooks

```
# Ask Claude:
Review my recent changes using the code graph
```

---

## Advantages vs the Python version

### Performance

| Operation | Python | Rust | Speedup |
|---|---|---|---|
| MCP server startup | 150-300 ms (interpreter + imports) | 2-5 ms | **30-60x** |
| First query (cold start) | 200+ ms (build NetworkX cache from SQLite) | <1 ms (graph already in memory) | **200x** |
| Graph stats | 5 ms (COUNT SQL queries) | <0.1 ms (O(1) `.node_count()`) | **50x** |
| Node search | 10 ms (SQL LIKE) | <0.5 ms (HashMap iteration) | **20x** |
| Save after build | 100 ms (INSERT SQLite) | 20 ms (serialize+zstd+write) | **5x** |
| Full build (tree-sitter) | ~3s | ~2.5s | 1.2x (tree-sitter dominates) |

### Architecture

| Aspect | Python | Rust |
|---|---|---|
| Storage | SQLite WAL + 3 tables + 7 indexes + lazy petgraph cache | **In-memory StableGraph**, persisted as bincode+zstd |
| On-disk format | `.code-review-graph/graph.db` (SQLite) | `.code-review-graph/graph.bin.zst` (4-10x smaller) |
| Graph cache | Lazy — rebuilt on first `get_impact_radius()` | **Always in memory** — zero cold start |
| Concurrency | SQLite WAL (multiple readers) | Arc\<Mutex\> in memory (single-writer, zero I/O for reads) |
| Integrity | SQLite journal | CRC-32 header + magic bytes + atomic write (tempfile+rename) |
| Embeddings | SQLite table | bincode/zstd (same format as graph) |

### Distribution

| Aspect | Python | Rust |
|---|---|---|
| Install size | ~150 MB (Python + venv + tree-sitter + SQLite) | **28 MB** (single static binary) |
| Dependencies | Python 3.10+, pip, venv, tree-sitter C libs | **None** |
| Installation | `pip install` + `code-review-graph install` | Download binary + `code-review-graph install` |
| Cross-platform | Platform-specific wheels | Compiled binary per target |
| Build time | ~50s (C compilation of SQLite + tree-sitter) | **~20s** (no SQLite) |

### Added features

| Feature | Python | Rust |
|---|---|---|
| Background watcher in MCP server | No (separate command) | **Yes** — auto-starts on `serve` |
| Lazy stale-check per request | No | **Yes** — `git status` before each query |
| `--quiet` flag for hooks | No | **Yes** — `code-review-graph update -q` |
| Graph compression | No | **zstd level 3** — 4-5x ratio |
| Integrity checksum | No | **CRC-32** + magic bytes + auto-rebuild on corruption |
| Atomic writes | No | **tempfile + rename** — no corruption on crash |

---

## Advantages vs Claude Code without code-review-graph

Without the graph, Claude Code must:
1. Read **all changed files** plus their likely dependencies
2. Guess the blast radius of a change
3. Scan thousands of tokens to find the 5 relevant files

With code-review-graph:
- **6.8x fewer tokens on average** (up to 49x on monorepos)
- **Precise blast radius** — knows exactly which functions/classes/tests are impacted
- **Higher review quality** — scored 8.8/10 vs 7.2/10 on benchmarks
- **14 languages supported** with full extraction of functions, classes, imports, calls, inheritance, and tests

Benchmarks from the original project (reproduced with permission):

| Repo | Size | Standard tokens | Tokens with graph | Reduction | Quality |
|---|---:|---:|---:|---:|---|
| httpx | 125 files | 12,507 | 458 | **26.2x** | 9.0 vs 7.0 |
| FastAPI | 2,915 files | 5,495 | 871 | **8.1x** | 8.5 vs 7.5 |
| Next.js | 27,732 files | 21,614 | 4,457 | **6.0x** | 9.0 vs 7.0 |

---

## Supported languages

Python, TypeScript, JavaScript, Go, Rust, Java, C, C++, C#, Ruby, PHP, Swift, Vue

> Kotlin: AST tables are ready in the code, waiting for a compatible `tree-sitter-kotlin` release (0.3 depends on tree-sitter 0.20, incompatible with our 0.24).

---

## CLI

```
code-review-graph install     # Register MCP server (.mcp.json)
code-review-graph build       # Full codebase parse
code-review-graph update      # Incremental update (changed files only)
code-review-graph status      # Graph statistics
code-review-graph watch       # Auto-update on file changes
code-review-graph visualize   # Generate interactive HTML visualization
code-review-graph serve       # Start MCP server (stdio)
```

Useful options:
- `--repo PATH` — specify project directory
- `--quiet` / `-q` — silent mode (for hooks)
- `--base REF` — git ref for incremental diff (default: `HEAD~1`)

---

## MCP Tools

Claude uses these tools automatically once the graph is built.

| Tool | Description |
|---|---|
| `build_or_update_graph` | Build or update the graph |
| `get_impact_radius` | Blast radius of changed files |
| `get_review_context` | Token-optimized review context |
| `query_graph` | Callers, callees, imports, inheritance, tests |
| `semantic_search_nodes` | Search by name or similarity |
| `embed_graph` | Compute vector embeddings |
| `list_graph_stats` | Graph size and health |
| `get_docs_section` | Documentation sections |
| `find_large_functions` | Functions/classes exceeding a line-count threshold |

---

## Configuration

### Exclude files

Create `.code-review-graphignore` at the project root:

```
generated/**
*.generated.ts
vendor/**
```

### PostToolUse hook (always-fresh graph)

Add to `.claude/settings.json`:

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

---

## Architecture

```
.code-review-graph/
├── graph.bin.zst         # StableGraph + indexes (bincode + zstd)
├── embeddings.bin.zst    # Vector embeddings (same format)
└── .gitignore            # Auto-generated
```

```
src/
├── parser.rs         # Multi-language tree-sitter parser (13 grammars)
├── graph.rs          # StableGraph + bincode/zstd persistence
├── incremental.rs    # Git ops, full/incremental build, watch
├── tools.rs          # 9 MCP tools
├── server.rs         # rmcp MCP server (stdio) + background watcher
├── main.rs           # CLI (clap)
├── types.rs          # Shared types
├── embeddings.rs     # Vector embedding store
├── visualization.rs  # Interactive D3.js HTML export
├── tsconfig.rs       # TypeScript path alias resolver
├── error.rs          # Error types
└── lib.rs            # Crate root
```

---

## Building from source

```bash
git clone https://github.com/JF10R/code-review-graph-rust.git
cd code-review-graph-rust
cargo build --release
# Binary: target/release/code-review-graph
```

---

## Credits

Rust rewrite of [code-review-graph](https://github.com/tirth8205/code-review-graph) by [Tirth Kanani](https://github.com/tirth8205). The architecture, blast-radius algorithms, and benchmarks originate from the original project.

## License

MIT. See [LICENSE](LICENSE).

The original project is also MIT-licensed.
