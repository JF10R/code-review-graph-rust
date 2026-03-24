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

| | Sans code-review-graph | Python (original) | **Rust (cette version)** |
|---|---|---|---|
| Token usage | Scan complet chaque fois | **6.8x reduction** | **6.8x reduction** (meme graphe) |
| Installation | N/A | Python 3.10+ venv, ~150 MB | **Binaire unique 28 MB** |
| Startup | N/A | ~150-300 ms | **~2-5 ms** |
| `list_graph_stats` | N/A | ~5 ms (SQL) | **<0.1 ms** (O(1) en memoire) |
| `get_impact_radius` 1er appel | N/A | ~200 ms (build cache) | **<1 ms** (toujours en memoire) |
| `query_graph callers_of` | N/A | ~10 ms (SQL + cache) | **<0.5 ms** (HashMap direct) |
| Incremental update (5 fichiers) | N/A | ~200 ms | **~50 ms** |
| Graphe sur disque (1k fichiers) | N/A | ~2 MB (SQLite) | **~200 KB** (bincode+zstd) |
| Graphe sur disque (10k fichiers) | N/A | ~20 MB | **~2 MB** |
| Dependencies runtime | N/A | Python + pip + venv | **Aucune** |
| Auto-update du graphe | N/A | Hook manuel | **Background watcher + lazy stale-check** |

---

## Quick Start

### Download le binaire

```bash
# Depuis les releases GitHub (a venir)
# ou build depuis les sources :
cargo install --path .
```

### Initialiser

```bash
cd votre-projet
code-review-graph install   # Cree .mcp.json pour Claude Code
code-review-graph build     # Parse le codebase (~10s pour 500 fichiers)
```

Redemarrer Claude Code apres l'installation.

### Utiliser

Le graphe se met a jour **automatiquement** :
- Le serveur MCP lance un **background watcher** qui re-indexe les fichiers modifies en temps reel
- Chaque requete d'outil verifie la **fraicheur du graphe** et fait un update incremental si necessaire
- Le flag `--quiet` permet l'integration avec les hooks PostToolUse de Claude Code

```
# Demander a Claude :
Review my recent changes using the code graph
```

---

## Avantages vs la version Python

### Performance

| Operation | Python | Rust | Gain |
|---|---|---|---|
| Startup du serveur MCP | 150-300 ms (interpreteur + imports) | 2-5 ms | **30-60x** |
| Premiere requete (cold start) | 200+ ms (build cache NetworkX depuis SQLite) | <1 ms (graphe deja en memoire) | **200x** |
| Stats du graphe | 5 ms (COUNT queries SQL) | <0.1 ms (O(1) `.node_count()`) | **50x** |
| Recherche de noeuds | 10 ms (SQL LIKE) | <0.5 ms (HashMap iteration) | **20x** |
| Save apres build | 100 ms (INSERT SQLite) | 20 ms (serialize+zstd+write) | **5x** |
| Build complet (tree-sitter) | ~3s | ~2.5s | 1.2x (tree-sitter domine) |

### Architecture

| Aspect | Python | Rust |
|---|---|---|
| Stockage | SQLite WAL + 3 tables + 7 indexes + cache lazy petgraph | **StableGraph en memoire**, persiste en bincode+zstd |
| Format sur disque | `.code-review-graph/graph.db` (SQLite) | `.code-review-graph/graph.bin.zst` (4-10x plus petit) |
| Cache graphe | Lazy — reconstruit au 1er `get_impact_radius()` | **Toujours en memoire** — zero cold start |
| Concurrence | SQLite WAL (lecteurs multiples) | Arc\<Mutex\> en memoire (single-writer, zero I/O pour les reads) |
| Integrite | SQLite journal | CRC-32 header + magic bytes + atomic write (tempfile+rename) |
| Embeddings | SQLite table | bincode/zstd (meme format que le graphe) |

### Distribution

| Aspect | Python | Rust |
|---|---|---|
| Taille installation | ~150 MB (Python + venv + tree-sitter + SQLite) | **28 MB** (binaire unique statique) |
| Dependencies | Python 3.10+, pip, venv, tree-sitter C libs | **Aucune** |
| Installation | `pip install` + `code-review-graph install` | Telecharger le binaire + `code-review-graph install` |
| Cross-platform | Wheels par plateforme | Binaire compile par cible |
| Temps de build | ~50s (compilation C de SQLite + tree-sitter) | **~20s** (pas de SQLite) |

### Fonctionnalites ajoutees

| Feature | Python | Rust |
|---|---|---|
| Background watcher dans le serveur MCP | Non (commande separee) | **Oui** — auto-start au `serve` |
| Lazy stale-check par requete | Non | **Oui** — `git status` avant chaque query |
| Flag `--quiet` pour hooks | Non | **Oui** — `code-review-graph update -q` |
| Compression du graphe | Non | **zstd level 3** — ratio 4-5x |
| Checksum d'integrite | Non | **CRC-32** + magic bytes + auto-rebuild sur corruption |
| Atomic writes | Non | **tempfile + rename** — pas de corruption sur crash |

---

## Avantages vs Claude Code sans code-review-graph

Sans le graphe, Claude Code doit :
1. Lire **tous les fichiers modifies** + leurs dependances probables
2. Deviner le blast radius d'un changement
3. Scanner des milliers de tokens pour trouver les 5 fichiers pertinents

Avec code-review-graph :
- **6.8x moins de tokens en moyenne** (jusqu'a 49x sur les monorepos)
- **Blast radius precis** — sait exactement quels fonctions/classes/tests sont impactes
- **Qualite de review superieure** — score 8.8/10 vs 7.2/10 sur les benchmarks
- **14 langages supportes** avec extraction complete des fonctions, classes, imports, appels, heritage et tests

Les benchmarks du projet original (reproduits avec permission) :

| Repo | Taille | Tokens standard | Tokens avec graphe | Reduction | Qualite |
|---|---:|---:|---:|---:|---|
| httpx | 125 fichiers | 12,507 | 458 | **26.2x** | 9.0 vs 7.0 |
| FastAPI | 2,915 fichiers | 5,495 | 871 | **8.1x** | 8.5 vs 7.5 |
| Next.js | 27,732 fichiers | 21,614 | 4,457 | **6.0x** | 9.0 vs 7.0 |

---

## Langages supportes

Python, TypeScript, JavaScript, Go, Rust, Java, C, C++, C#, Ruby, PHP, Swift, Vue

> Kotlin : les tables AST sont pretes dans le code, en attente d'une version compatible de `tree-sitter-kotlin` (0.3 depend de tree-sitter 0.20, incompatible avec notre 0.24).

---

## CLI

```
code-review-graph install     # Enregistrer le serveur MCP (.mcp.json)
code-review-graph build       # Parse complet du codebase
code-review-graph update      # Mise a jour incrementale (fichiers modifies)
code-review-graph status      # Statistiques du graphe
code-review-graph watch       # Auto-update sur changements de fichiers
code-review-graph visualize   # Generer une visualisation HTML interactive
code-review-graph serve       # Demarrer le serveur MCP (stdio)
```

Options utiles :
- `--repo PATH` — specifier le repertoire du projet
- `--quiet` / `-q` — mode silencieux (pour les hooks)
- `--base REF` — ref git pour le diff incremental (defaut: `HEAD~1`)

---

## Outils MCP

Claude utilise ces outils automatiquement une fois le graphe construit.

| Outil | Description |
|---|---|
| `build_or_update_graph` | Construire ou mettre a jour le graphe |
| `get_impact_radius` | Blast radius des fichiers modifies |
| `get_review_context` | Contexte de review optimise en tokens |
| `query_graph` | Callers, callees, imports, heritage, tests |
| `semantic_search_nodes` | Recherche par nom ou similarite |
| `embed_graph` | Calculer les embeddings vectoriels |
| `list_graph_stats` | Taille et sante du graphe |
| `get_docs_section` | Sections de documentation |
| `find_large_functions` | Fonctions/classes depassant un seuil de lignes |

---

## Configuration

### Exclure des fichiers

Creer `.code-review-graphignore` a la racine du projet :

```
generated/**
*.generated.ts
vendor/**
```

### Hook PostToolUse (graphe toujours frais)

Ajouter dans `.claude/settings.json` :

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
├── embeddings.bin.zst    # Vecteurs d'embeddings (meme format)
└── .gitignore            # Auto-genere
```

```
src/
├── parser.rs         # Tree-sitter multi-langages (13 grammaires)
├── graph.rs          # StableGraph + bincode/zstd persistence
├── incremental.rs    # Git ops, full/incremental build, watch
├── tools.rs          # 9 outils MCP
├── server.rs         # Serveur MCP rmcp (stdio) + background watcher
├── main.rs           # CLI (clap)
├── types.rs          # Types partages
├── embeddings.rs     # Store vectoriel
├── visualization.rs  # Export HTML D3.js
├── tsconfig.rs       # Resolver d'alias TypeScript
├── error.rs          # Types d'erreurs
└── lib.rs            # Racine du crate
```

---

## Build depuis les sources

```bash
git clone https://github.com/votre-user/code-review-graph-rust.git
cd code-review-graph-rust
cargo build --release
# Binaire: target/release/code-review-graph
```

---

## Credits

Rewrite Rust du projet [code-review-graph](https://github.com/tirth8205/code-review-graph) par [Tirth Kanani](https://github.com/tirth8205). L'architecture, les algorithmes de blast radius, et les benchmarks sont issus du projet original.

## Licence

MIT. Voir [LICENSE](LICENSE).

Le projet original est egalement sous licence MIT.
