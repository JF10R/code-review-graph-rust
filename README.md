# code-review-graph (Rust)

[![MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75+-orange.svg)](https://www.rust-lang.org)
[![MCP](https://img.shields.io/badge/MCP-compatible-green.svg)](https://modelcontextprotocol.io)

Rust rewrite of [code-review-graph](https://github.com/tirth8205/code-review-graph) by Tirth Kanani. Single binary, zero runtime dependencies.

---

## What this is

`code-review-graph` builds a structural knowledge graph of your codebase using Tree-sitter, then exposes it to Claude Code via MCP. Instead of scanning entire files, Claude queries the graph to find exactly which functions, classes, and tests are impacted by a change — reducing token usage by 6.8x on average (up to 49x on monorepos).

This is a complete Rust rewrite of the original Python implementation, preserving full API compatibility. See the [original project](https://github.com/tirth8205/code-review-graph) for architecture details and benchmarks.

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

---

## Performance vs Python

| Metric | Python | Rust |
|--------|--------|------|
| Startup | 150-300 ms | 2-5 ms |
| First query (cold start) | ~200 ms | <1 ms |
| Graph on disk (1k files) | ~2 MB (SQLite) | ~200 KB (bincode+zstd) |
| Binary size | ~150 MB (with venv) | 28 MB |
| Runtime dependencies | Python 3.10+ | None |

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

Python, TypeScript, JavaScript, Go, Rust, Java, C, C++, C#, Ruby, PHP, Swift, Vue, Kotlin

Kotlin support uses `tree-sitter-kotlin-ng` (the actively maintained fork).

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
├── embeddings.rs     # Vector embedding store (candle + API providers)
├── config.rs         # Persistent config (API keys, provider)
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
