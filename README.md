# code-review-graph (Rust)

[![MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75+-orange.svg)](https://www.rust-lang.org)
[![MCP](https://img.shields.io/badge/MCP-compatible-green.svg)](https://modelcontextprotocol.io)

Rust rewrite of [code-review-graph](https://github.com/tirth8205/code-review-graph) by Tirth Kanani. Single binary, zero runtime dependencies, free local embeddings.

---

## What this is

`code-review-graph` builds a structural knowledge graph of your codebase using Tree-sitter, then exposes it to Claude Code via MCP. Instead of scanning entire files, Claude queries the graph to find exactly which functions, classes, and tests are impacted by a change — reducing token usage by 6.8x on average (up to 49x on monorepos).

This is a complete Rust rewrite of the original Python implementation, preserving full MCP API compatibility while adding significant new capabilities. See the [original project](https://github.com/tirth8205/code-review-graph) for the core concept and token-reduction benchmarks.

---

## What's different from the Python version

| | Python (original) | Rust (this fork) |
|---|---|---|
| **Parsing** | Sequential, single-threaded | **Parallel via rayon** — all CPU cores parse concurrently |
| **Storage** | SQLite WAL + 3 tables + 7 indexes | **In-memory StableGraph**, persisted as bincode + zstd |
| **Blast radius** | Flat bidirectional BFS, all edges equal | **Direction-aware, weighted, with decay** — 30-50% fewer false positives |
| **Large graph scaling** | BFS explodes on hub nodes | **Auto PageRank** at 10k+ nodes, dampens hubs proportionally |
| **Diff seeding** | All nodes in changed files | **Node-level** — only functions whose `body_hash` changed |
| **Embeddings** | Requires `pip install [embeddings]` + API key | **Free local embeddings** (candle, all-MiniLM-L6-v2) out of the box |
| **Embedding providers** | Google Gemini only | **OpenAI + Voyage AI + Gemini** + local candle |
| **Graph freshness** | Manual `build` / hook required | **Background watcher** in MCP server + lazy stale-check per query |
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

Tree-sitter parses your code into a graph of nodes (functions, classes, imports) and edges (calls, inheritance, test coverage). When you ask Claude to review a change, it queries the graph for the blast radius — which callers, dependents, and tests are affected — then reads only those files. The graph updates incrementally on every file save.

Language-specific extraction rules are defined in declarative [`.scm` query files](queries/) — adding support for a new language is a single file addition, no Rust recompilation needed.

---

## Performance vs Python

### Real-world benchmarks (same repos as original project)

| Repo | Files | Python build | Rust build | Speedup |
|------|------:|:-------------|:-----------|--------:|
| [httpx](https://github.com/encode/httpx) | 60 | 0.96s | **0.47s** | **2.0x** |
| [FastAPI](https://github.com/fastapi/fastapi) | 1,122 | 6.61s | **0.90s** | **7.3x** |
| [Next.js](https://github.com/vercel/next.js) | 2,382 | 305s | **3.97s** | **76.8x** |

Build speedup scales with repo size thanks to **rayon parallel parsing** — all CPU cores parse files concurrently while the graph store writes sequentially. Small repos see modest gains (thread pool overhead), but large codebases see 50-80x improvements where the parallelism and bincode persistence compound.

### Micro-benchmarks (criterion, 1000-node synthetic graph)

| Operation | Python | Rust | Speedup |
|-----------|--------|------|--------:|
| Graph save (1k nodes) | 146.7 ms | **2.8 ms** | **52x** |
| Graph load + stats | 120 ms | **62 ms** | 2x |
| Impact radius (warm) | 853 us | **470 us** | 1.8x |
| Node search | 146 us | **8.3 us** | **18x** |
| Parse 50 TS functions | 6.5 ms | **3.4 ms** | 1.9x |

### Distribution

| | Python | Rust |
|---|--------|------|
| Binary size | ~150 MB (with venv) | ~40 MB |
| Runtime dependencies | Python 3.10+ | None |
| Startup | 150-300 ms | 2-5 ms |

---

## Embeddings

### Free local embeddings (default)

Embeddings work out of the box — no API key needed. The `embeddings-local` feature (enabled by default) runs `all-MiniLM-L6-v2` locally via candle.

```bash
code-review-graph build
code-review-graph embed    # Downloads model on first run (~23 MB, cached by HF Hub)
```

### API providers (optional, higher quality)

```bash
code-review-graph config set embedding-provider voyage
code-review-graph config set voyage-api-key pa-...
code-review-graph embed
```

| Provider | Model | Quality | Cost |
|----------|-------|---------|------|
| Local (candle) | all-MiniLM-L6-v2 | Good | Free |
| Voyage AI | voyage-code-3 | Best for code | ~$0.02/10k nodes |
| OpenAI | text-embedding-3-small | Good | ~$0.002/10k nodes |
| Google Gemini | text-embedding-004 | Good | Free tier available |

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

**Auto PageRank at scale**: for graphs exceeding 10,000 nodes, the algorithm switches from weighted BFS to Personalized PageRank. Hub nodes (utility files imported everywhere) are dampened proportionally to their degree, preventing the blast radius from exploding on large monorepos.

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
├── graph.rs          # StableGraph + bincode/zstd persistence
├── incremental.rs    # Git ops, full/incremental build, watch
├── tools.rs          # 9 MCP tools
├── server.rs         # rmcp MCP server (stdio) + background watcher
├── main.rs           # CLI (clap)
├── types.rs          # Shared types
├── embeddings.rs     # Vector embedding store (candle local + API providers)
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
