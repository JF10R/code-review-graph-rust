# Changelog

## v1.3.0

### Performance

- **`get_edges_among` rewrite** — Impact radius and review context queries now iterate only edges incident to the subgraph nodes (O(degree sum)) instead of scanning all edges in the graph (O(E)). **10-100x faster** for typical impact queries where the subgraph is small relative to the full graph.
- **`file_hash_unchanged` O(1) lookup** — Incremental update hash check uses direct `HashMap::get` instead of cloning all nodes in a file. Eliminates O(n) allocations per file during change detection.
- **Parallel incremental parsing** — `incremental_update` now uses `rayon::par_iter` for the tree-sitter parse phase, matching the parallelism already used by `full_build`. **4-8x faster** incremental updates when multiple files change (e.g., branch switches).
- **Precomputed lowercase search cache** — `search_nodes` no longer calls `to_lowercase()` on every node name per query. A `lowercase_cache` in `GraphData` is maintained incrementally on insert/remove, eliminating N string allocations per keyword search.

### Bug Fixes

- **`repo_root` parameter respected** — MCP tool calls with an explicit `repo_root` now correctly query that repo's graph instead of always using the server's startup directory.

## v1.2.0

- **Release binaries** — Prebuilt binaries for Linux, macOS (x86_64 + ARM64), and Windows published to GitHub Releases on each version tag. No Rust toolchain needed.
- **Release workflow fix** — Release notes are now generated once (not per-platform), eliminating duplicated descriptions.
- **Compact response mode** — All MCP tools accept `compact: true`, stripping low-value fields and repo-root path prefixes. Reduces response tokens ~40%.
- **`trace_call_chain` tool** — Find the shortest call path between two functions via BFS on CALLS edges.
- **Framework-aware edge inference** — JSX component usage, Express/Koa middleware, event emitters, and pytest fixtures detected as CALLS edges. Next.js gained +149K edges from JSX alone.
- **GPU-accelerated embeddings** — Optional `gpu-directml` feature for DirectML GPU embedding computation (~80x faster than CPU on large codebases).
- **Agent-optimized tool descriptions** — MCP descriptions guide agents toward semantic search for discovery, graph queries instead of grep.
- **Rust `#[test]` attribute detection** — Parser now detects `#[test]`, `#[tokio::test]` attributes, not just naming conventions.
- **Stale embedding GC** — Dead vectors pruned at search time when >20% stale, not only during embed runs.
- **`file_summary` Windows fix** — Uses resolved `node.file_path` directly instead of rebuilding with `root.join()`.
- **Watcher hash-skip** — Background watcher skips files whose content hash hasn't changed.
- **Auto-update source filter** — Non-source files (.json, .md, .lock) no longer trigger incremental graph updates.
- **Hook best practice** — PostToolUse matcher changed from `Write|Edit|Bash` to `Write|Edit` (126 unnecessary rebuilds eliminated per session).

## v1.1.0

- **Personalized PageRank** replaces BFS+PageRank for blast radius analysis. ~15-20% fewer false positives.
- **Jina Code v2 embeddings** via fastembed (default). ~40% better semantic search accuracy (56% -> 80% Top-5).
- **HNSW vector index** via usearch (opt-in). 100-1000x faster similarity search at 10k+ nodes.
- **Tantivy full-text search** (opt-in). Fuzzy, typo-tolerant, relevance-ranked node search.
- **Hybrid RRF query** merges structural graph + vector search in one call.
- **Token reduction metrics** tool quantifies context savings vs naive file reading.
- **Watcher re-parses dependents** — cross-file edges stay fresh after function renames.
- **Incremental build** 30-50% less I/O — single file read, hash-skip in watch mode.

## v1.0.0

- Initial Rust rewrite of [code-review-graph](https://github.com/tirth8205/code-review-graph) (Python).
- 14-language Tree-sitter parsing with declarative `.scm` query files.
- Parallel parsing via rayon (7-77x faster than Python).
- In-memory StableGraph with postcard+zstd persistence (4-10x smaller).
- Direction-aware weighted blast radius with node-level diff seeding.
- MCP server (stdio) with background file watcher.
- CLI: build, update, embed, status, watch, visualize, config, install.
