# code-review-graph (Rust)

[![MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75+-orange.svg)](https://www.rust-lang.org)
[![MCP](https://img.shields.io/badge/MCP-compatible-green.svg)](https://modelcontextprotocol.io)

Rust rewrite of [code-review-graph](https://github.com/tirth8205/code-review-graph) by Tirth Kanani. Single binary, zero runtime dependencies, free local embeddings.

---

## What this is

`code-review-graph` builds a structural knowledge graph of your codebase using Tree-sitter, then exposes it to Claude Code via MCP. Instead of scanning entire files, Claude queries the graph to find exactly which functions, classes, and tests are impacted by a change — reducing token usage by 6.8x on average (up to 49x on monorepos).

This is a complete Rust rewrite of the original Python implementation, preserving full MCP API compatibility while adding significant new capabilities. See the [original project](https://github.com/tirth8205/code-review-graph) for the core concept and token-reduction benchmarks.

### TL;DR — what you get

| What | Improvement |
|------|-------------|
| Install | Single binary, no Python/pip/venv needed |
| Build speed | **7-77x faster** (rayon parallel parsing) |
| Query speed | **Sub-millisecond** — graph always in memory |
| Accuracy | **30-50% fewer false positives** — Personalized PageRank, weighted, direction-aware |
| Scale | **Personalized PageRank** for all graph sizes — no threshold, no explosion |
| Embeddings | **Jina Code v2** (768-dim, ~80% Top-5) — free, local, no API key, **GPU optional** |
| Search | **Hybrid RRF** — graph traversal + vector similarity in one query |
| Call tracing | **trace_call_chain** — shortest path between any two functions |
| Framework edges | **JSX, Express, pytest** — component/middleware/fixture edges auto-detected |
| Freshness | **Automatic** — background watcher + lazy stale-check, zero manual builds |
| Disk usage | **4-10x smaller** graph files (postcard + zstd) |

### Recent changes (v1.2)

| Change | Impact |
|--------|--------|
| **Compact response mode** | All MCP tools accept `compact: true` — strips low-value fields + repo-root path prefixes, reducing response tokens ~40%. Agents stay within context budgets on large codebases |
| **`trace_call_chain` tool** (new) | Find the shortest call path between two functions via BFS on CALLS edges. Replaces manual hop-by-hop file reading when tracing data flow |
| **Framework-aware edge inference** | JSX component usage (`<Button />` → CALLS edge), Express/Koa middleware, event emitter `.on()` handlers, and pytest fixture injection are now detected as CALLS edges. Next.js graph gained +149K edges from JSX alone |
| **GPU-accelerated embeddings** (DirectML) | Optional `gpu-directml` feature uses the GPU for embedding computation. Benchmarked **~80x faster** than CPU on 57K-node next.js (>20 min → 15 seconds on RTX 5070 Ti) |
| **Agent-optimized tool descriptions** | MCP tool descriptions now guide agents toward optimal usage patterns: semantic search for discovery, graph queries instead of grep, Read/Grep tools instead of bash |

### Previous changes (v1.1)

| Change | Impact |
|--------|--------|
| **Personalized PageRank** replaces BFS+PageRank | More accurate blast radius for all graph sizes, ~15-20% fewer false positives |
| **Jina Code v2** embeddings via fastembed (default) | ~40% better semantic search accuracy (56% → 80% Top-5) |
| **HNSW vector index** via usearch (opt-in) | 100-1000x faster similarity search at 10k+ nodes |
| **Tantivy full-text search** (opt-in) | Fuzzy, typo-tolerant, relevance-ranked node search |
| **Hybrid RRF query** (new MCP tool) | Merges structural graph + vector search in one call |
| **Token reduction metrics** (new MCP tool) | Quantifies context savings vs naive file reading |
| **Watcher re-parses dependents** | Cross-file edges stay fresh after function renames |
| **Incremental build** 30-50% less I/O | Single file read (was double), hash-skip in watch mode |

Optional features:
```bash
# GPU-accelerated embeddings (Windows, any GPU via DirectML)
cargo install ... --features gpu-directml

# HNSW vector search (requires C++ toolchain)
cargo install ... --features hnsw-index

# Tantivy full-text search
cargo install ... --features tantivy-search

# Legacy candle embeddings (instead of fastembed default)
cargo install ... --no-default-features --features embeddings-local
```

---

## What's different from the Python version

| | Python (original) | Rust (this fork) |
|---|---|---|
| **Parsing** | Sequential, single-threaded | **Parallel via rayon** — all CPU cores parse concurrently |
| **Storage** | SQLite WAL + 3 tables + 7 indexes | **In-memory StableGraph**, persisted as postcard + zstd |
| **Blast radius** | Flat bidirectional BFS, all edges equal | **Personalized PageRank** — seed-biased, weighted, direction-aware |
| **Large graph scaling** | BFS explodes on hub nodes | **PPR for all sizes** — unified algorithm, no threshold switch |
| **Diff seeding** | All nodes in changed files | **Node-level** — only functions whose `body_hash` changed |
| **Embeddings** | Requires `pip install [embeddings]` + API key | **Jina Code v2** (768-dim, ~80% accuracy) free local via fastembed |
| **Embedding providers** | Google Gemini only | **OpenAI + Voyage AI + Gemini** + local fastembed (+ candle fallback) |
| **Vector search** | N/A | **HNSW index** (opt-in) — 100-1000x faster similarity at scale |
| **Full-text search** | N/A | **Tantivy** (opt-in) — fuzzy, typo-tolerant, relevance-ranked |
| **Hybrid search** | N/A | **Reciprocal Rank Fusion** — merges graph + vector results |
| **Graph freshness** | Manual `build` / hook required | **Background watcher** in MCP server + lazy stale-check per query |
| **Watcher accuracy** | N/A | **Re-parses dependent files** — cross-file edges stay fresh after renames |
| **Config** | Environment variables only | **`config` CLI** — persistent API key storage with masked display |
| **Language rules** | Hardcoded Python HashSets | **Declarative `.scm` query files** — add a language without recompiling |
| **Progress** | Log lines every 50 files | **indicatif progress bar** with ETA |
| **Graph integrity** | SQLite journal | **CRC-32 header + magic bytes + atomic write** (tempfile + rename) |
| **Graph compression** | None (raw SQLite) | **zstd level 3** — 4-10x smaller on disk |
| **Distribution** | Python 3.10+ venv (~150 MB) | **Single 40 MB binary**, zero runtime deps |
| **Startup** | 150-300 ms (interpreter) | **2-5 ms** |

---

## Quick Start

```bash
# Install
cargo install --git https://github.com/JF10R/code-review-graph-rust

# Set up in your project
cd your-project
code-review-graph install

# Build the graph
code-review-graph build

# Restart Claude Code — done!
```

The MCP server starts automatically when Claude Code launches and keeps the graph fresh via a background watcher.

---

## How it works

Tree-sitter parses your code into a graph of nodes (functions, classes, imports) and edges (calls, inheritance, test coverage). A **framework-aware post-processing pass** adds edges for JSX component usage, Express/Koa middleware chains, event emitter registrations, and pytest fixture injection — patterns that pure AST analysis misses. When you ask Claude to review a change, it queries the graph for the blast radius — which callers, dependents, and tests are affected — then reads only those files. The graph updates incrementally on every file save.

Language-specific extraction rules are defined in declarative [`.scm` query files](queries/) — adding support for a new language is a single file addition, no Rust recompilation needed.

---

## Performance vs Python

### Real-world benchmarks (same repos as original project)

| Repo | Files | Python build | Rust build | Speedup |
|------|------:|:-------------|:-----------|--------:|
| [httpx](https://github.com/encode/httpx) | 60 | 0.96s | **0.15s** | **6.4x** |
| [FastAPI](https://github.com/fastapi/fastapi) | 1,122 | 6.61s | **0.91s** | **7.3x** |
| [Next.js](https://github.com/vercel/next.js) | 2,382 | 305s | **4.27s** | **71.4x** |

Build speedup scales with repo size thanks to **rayon parallel parsing** — all CPU cores parse files concurrently while the graph store writes sequentially. Small repos see 6x+ gains from reduced I/O (single file read path), large codebases see 70x+ improvements where parallelism and postcard persistence compound.

### Micro-benchmarks (criterion, 1000-node synthetic graph)

| Operation | Python | Rust | Speedup |
|-----------|--------|------|--------:|
| Graph save (1k nodes) | 146.7 ms | **2.1 ms** | **70x** |
| Graph load (1k nodes) | 120 ms | **1.08 ms** | **111x** |
| Impact radius (PPR, 3 files) | 853 us | **1.44 ms** | 0.6x* |
| Node search (relevance-ranked) | 146 us | **584 us** | 0.3x* |
| Get stats | — | **88 us** | — |
| Parse 50 Python functions | — | **2.2 ms** | — |
| Parse 50 TS functions | 6.5 ms | **3.0 ms** | 2.2x |

\* Impact radius and node search are now more thorough: PPR replaces simple BFS (better accuracy, slightly more compute), and search collects all matches for relevance ranking instead of taking the first N from a HashMap. The trade-off is correct — accuracy over raw speed on a 1k-node synthetic graph. Real-world codebases benefit from the improved result quality.

### Distribution

| | Python | Rust |
|---|--------|------|
| Binary size | ~150 MB (with venv) | ~40 MB |
| Runtime dependencies | Python 3.10+ | None |
| Startup | 150-300 ms | 2-5 ms |

---

## Embeddings

### Free local embeddings (default)

Embeddings work out of the box — no API key needed. The default `embeddings-fastembed` feature runs **Jina Code Embeddings v2** (768 dimensions, ~80% Top-5 accuracy on code retrieval benchmarks) locally via ONNX Runtime.

```bash
code-review-graph build
code-review-graph embed    # Downloads model on first run (~90 MB, cached locally)
```

### API providers (optional)

```bash
code-review-graph config set embedding-provider voyage
code-review-graph config set voyage-api-key pa-...
code-review-graph embed
```

| Provider | Model | Dimensions | Quality | Cost |
|----------|-------|-----------|---------|------|
| **Local (fastembed)** | Jina Code v2 | 768 | **Best local** (~80% Top-5) | Free |
| Local (candle) | all-MiniLM-L6-v2 | 384 | Good (~56% Top-5) | Free |
| Voyage AI | voyage-code-3 | 1024 | Best overall | ~$0.02/10k nodes |
| OpenAI | text-embedding-3-small | 1536 | Good | ~$0.002/10k nodes |
| Google Gemini | text-embedding-004 | 768 | Good | Free tier available |

### HNSW vector index (optional)

For large codebases (10k+ nodes), enable the HNSW index for 100-1000x faster similarity search:

```bash
cargo install --git https://github.com/JF10R/code-review-graph-rust --features hnsw-index
```

Requires a C++ toolchain (MSVC on Windows, GCC/Clang on Linux). The index is rebuilt in memory from stored embeddings on startup.

### When to use embeddings

- **Without embeddings**: `semantic_search` falls back to keyword matching (exact name substring). Sufficient for most projects.
- **With embeddings**: `semantic_search` uses vector similarity. Finds functions by meaning, not just name. Worth enabling for large codebases (>500 files) where exact matching misses relevant results.

### Minimal binary (no embeddings)

```bash
cargo install --git https://github.com/JF10R/code-review-graph-rust --no-default-features
```

---

## CLI Reference

```
code-review-graph install     # Register MCP server (creates .mcp.json)
code-review-graph build       # Full parse of all files
code-review-graph update      # Incremental update (changed files only)
code-review-graph embed       # Compute vector embeddings for semantic search
code-review-graph status      # Graph statistics
code-review-graph watch       # Auto-update on file changes
code-review-graph visualize   # Generate interactive HTML visualization
code-review-graph serve       # Start MCP server (stdio)
code-review-graph config set <key> <value>
code-review-graph config get <key>
code-review-graph config list
code-review-graph config reset
```

Global flags (most commands):

- `--repo PATH` — specify project directory (defaults to git root or cwd)
- `--quiet` / `-q` — suppress output (for PostToolUse hooks)
- `--base REF` — git ref for incremental diff (default: `HEAD~1`, `update` only)
- `--dry-run` — preview without writing (`install` only)

---

## Configuration

### Config keys

| Key | Description |
|-----|-------------|
| `embedding-provider` | `openai`, `voyage`, `gemini`, or `none` |
| `openai-api-key` | OpenAI API key |
| `voyage-api-key` | Voyage AI API key |
| `gemini-api-key` | Google Gemini API key |
| `embedding-model` | Override the provider's default model |

Config is stored at `~/.config/code-review-graph/config.json` (Linux/Mac) or `%APPDATA%/code-review-graph/config.json` (Windows). API keys are masked in `config list` output.

Environment variables (`EMBEDDING_PROVIDER`, `OPENAI_API_KEY`, `VOYAGE_API_KEY`, `GEMINI_API_KEY`, `EMBEDDING_MODEL`) take priority over the config file.

### Exclude files

Create `.code-review-graphignore` at the project root (same syntax as `.gitignore`):

```
generated/**
*.generated.ts
vendor/**
```

### PostToolUse hook (always-fresh graph)

Add to `.claude/settings.json` to update the graph after every file edit:

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

> **Important**: Only match `Edit|Write`, never `Bash`. Including `Bash` causes the graph to rebuild after every shell command (including read-only `ls`, `grep`, `cargo test`), adding significant overhead — benchmarked at 126 unnecessary rebuilds per code investigation session.

---

## Supported languages

Python, TypeScript, JavaScript, Vue, Go, Rust, Java, C, C++, C#, Ruby, PHP, Swift, Kotlin

Vue uses regex-based `<script>` block extraction with automatic 2-pass parsing (outer SFC → inner TS/JS). Kotlin uses `tree-sitter-kotlin-ng` (the actively maintained fork).

Each language's extraction rules are defined in a [`.scm` query file](queries/) — see [Adding a new language](#adding-a-new-language) below.

---

## Blast Radius Algorithm

The impact analysis is direction-aware, weighted, and node-level — significantly more precise than the original Python BFS.

**Direction-aware traversal**: only reverse dependency edges propagate impact. Callers of the changed function are affected; callees are not (they didn't change).

**Weighted edges**: impact decays by 0.7x per hop, and edge weights reflect semantic risk:

| Edge type | Weight | Rationale |
|-----------|--------|-----------|
| INHERITS | 1.2 | Liskov substitution risk |
| CALLS | 1.0 | Direct dependency |
| IMPLEMENTS | 1.0 | Interface contract |
| TESTED_BY | 0.8 | Tests should be re-run |
| IMPORTS_FROM | 0.5 | May be types/constants only |
| CONTAINS | 0.1 | Structural, rarely semantic |

**Node-level diff seeding**: only functions whose `body_hash` actually changed are used as seeds — not all nodes in the modified file. Reduces false positives when a file has many functions but only one changed.

**Personalized PageRank for all graph sizes**: the algorithm uses Personalized PageRank (PPR) with teleportation biased toward the changed seed nodes. This replaces the previous BFS + threshold-based PageRank switch. PPR naturally dampens hub nodes (utility files imported everywhere) proportionally to their degree, while focusing scores on nodes structurally close to the actual change. The weighted out-degree normalization ensures edge weights are consistent between numerator and denominator.

---

## Adding a new language

Adding language support requires 3 changes:

### 1. Add the grammar crate

Find the tree-sitter grammar on [crates.io](https://crates.io/search?q=tree-sitter-) and add it to `Cargo.toml`:

```toml
tree-sitter-elixir = "0.3"
```

### 2. Create a query file

Create `queries/<lang>.scm` with four tagged patterns. Use `@definition.class`, `@definition.function`, `@reference.import`, and `@reference.call` as capture names:

```scheme
; queries/elixir.scm

; Classes / modules
(call target: (identifier) @_name
  (#match? @_name "^defmodule$")
  (arguments (alias) @name)) @definition.class

; Functions
(call target: (identifier) @_name
  (#match? @_name "^(def|defp)$")
  (arguments (call target: (identifier) @name))) @definition.function

; Imports
(call target: (identifier) @_name
  (#match? @_name "^(import|alias|use|require)$")) @reference.import

; Calls
(call target: (identifier) @callee) @reference.call
(call target: (dot_call remote: (_) operator: (identifier) @callee)) @reference.call
```

Check your patterns against real code using `tree-sitter parse` or the [tree-sitter playground](https://tree-sitter.github.io/tree-sitter/playground).

### 3. Wire the grammar in parser.rs

Add two entries:

```rust
// In detect_language():
"ex" | "exs" => Some("elixir"),

// In grammar_for():
"elixir" => Some(tree_sitter_elixir::LANGUAGE.into()),
```

The query file is loaded automatically — `SCM_SOURCES` maps language names to `.scm` contents via `include_str!()`. Add the entry:

```rust
("elixir", include_str!("../queries/elixir.scm")),
```

That's it. Run `cargo test` to verify extraction works, then `cargo bench` to check parse performance.

---

## Architecture

```
src/
├── parser.rs         # Multi-language tree-sitter parser (14 grammars)
├── graph.rs          # StableGraph + postcard/zstd persistence
├── incremental.rs    # Git ops, full/incremental build, watch
├── tools.rs          # 12 MCP tools (incl. trace_call_chain, hybrid RRF, compact mode)
├── server.rs         # rmcp MCP server (stdio) + background watcher
├── persistence.rs    # Generic postcard+zstd save/load with CRC integrity
├── search.rs         # Tantivy full-text search (opt-in feature)
├── main.rs           # CLI (clap)
├── types.rs          # Shared types (type-safe enums for patterns, algorithms)
├── embeddings.rs     # Vector embeddings (fastembed Jina Code v2 + HNSW + API providers)
├── config.rs         # Persistent config (API keys, provider)
├── visualization.rs  # Interactive D3.js HTML export
├── tsconfig.rs       # TypeScript path alias resolver
├── error.rs          # Error types
└── lib.rs            # Crate root

queries/              # Declarative .scm extraction rules per language
├── python.scm
├── javascript.scm    # Shared by TypeScript/TSX
├── rust.scm
├── go.scm
├── java.scm
├── c.scm             # Shared by C++
├── csharp.scm
├── ruby.scm
├── kotlin.scm
├── swift.scm
└── php.scm

benches/              # Criterion benchmarks (dev-only)
└── benchmarks.rs
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

Rust rewrite of [code-review-graph](https://github.com/tirth8205/code-review-graph) by [Tirth Kanani](https://github.com/tirth8205). The core concept, MCP tool API, and token-reduction methodology originate from the original project. The blast radius algorithm, parallel parsing, embedding system, and persistence layer are new in this fork.

## License

MIT. See [LICENSE](LICENSE).

The original project is also MIT-licensed.
