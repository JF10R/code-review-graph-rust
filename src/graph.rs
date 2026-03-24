//! SQLite-backed graph store with petgraph for traversal.
//!
//! Stores nodes and edges in SQLite with WAL mode.
//! Builds an in-memory petgraph DiGraph for impact radius analysis.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use petgraph::graph::{DiGraph, NodeIndex};
use rusqlite::{params, Connection};

use crate::error::Result;
use crate::types::{
    EdgeInfo, EdgeKind, GraphEdge, GraphNode, GraphStats, ImpactResult, NodeInfo, NodeKind,
};

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

const SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS nodes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    kind TEXT NOT NULL,
    name TEXT NOT NULL,
    qualified_name TEXT NOT NULL UNIQUE,
    file_path TEXT NOT NULL,
    line_start INTEGER NOT NULL DEFAULT 0,
    line_end INTEGER NOT NULL DEFAULT 0,
    language TEXT NOT NULL DEFAULT '',
    is_test INTEGER NOT NULL DEFAULT 0,
    docstring TEXT NOT NULL DEFAULT '',
    signature TEXT NOT NULL DEFAULT '',
    body_hash TEXT NOT NULL DEFAULT '',
    file_hash TEXT NOT NULL DEFAULT '',
    updated_at REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS edges (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    kind TEXT NOT NULL,
    source_qualified TEXT NOT NULL,
    target_qualified TEXT NOT NULL,
    file_path TEXT NOT NULL,
    line INTEGER NOT NULL DEFAULT 0,
    updated_at REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS metadata (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_nodes_file ON nodes(file_path);
CREATE INDEX IF NOT EXISTS idx_nodes_kind ON nodes(kind);
CREATE INDEX IF NOT EXISTS idx_nodes_qualified ON nodes(qualified_name);
CREATE INDEX IF NOT EXISTS idx_edges_source ON edges(source_qualified);
CREATE INDEX IF NOT EXISTS idx_edges_target ON edges(target_qualified);
CREATE INDEX IF NOT EXISTS idx_edges_kind ON edges(kind);
CREATE INDEX IF NOT EXISTS idx_edges_file ON edges(file_path);
";

// Column list constants — avoids repeating SELECT projections across queries.
const NODE_COLS: &str =
    "kind, name, qualified_name, file_path, line_start, line_end, \
     language, is_test, docstring, signature, body_hash, file_hash";

const EDGE_COLS: &str =
    "kind, source_qualified, target_qualified, file_path, line";

// ---------------------------------------------------------------------------
// In-memory graph cache
// ---------------------------------------------------------------------------

/// Insert `name` into the petgraph (and index map) only when not already present.
/// Returns the NodeIndex without a second clone of `name`.
fn get_or_insert_node(
    graph: &mut DiGraph<String, EdgeKind>,
    index: &mut HashMap<String, NodeIndex>,
    name: String,
) -> NodeIndex {
    use std::collections::hash_map::Entry;
    match index.entry(name) {
        Entry::Occupied(e) => *e.get(),
        Entry::Vacant(e) => {
            let idx = graph.add_node(e.key().clone());
            e.insert(idx);
            idx
        }
    }
}

struct GraphCache {
    graph: DiGraph<String, EdgeKind>,
    node_index: HashMap<String, NodeIndex>,
}

impl GraphCache {
    fn build(conn: &Connection) -> Result<Self> {
        let mut graph: DiGraph<String, EdgeKind> = DiGraph::new();
        let mut node_index: HashMap<String, NodeIndex> = HashMap::new();

        let mut stmt =
            conn.prepare("SELECT source_qualified, target_qualified, kind FROM edges")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;

        for row in rows {
            let (src, tgt, kind_str) = row?;
            let edge_kind = EdgeKind::from_str(&kind_str).unwrap_or(EdgeKind::Calls);
            let src_idx = get_or_insert_node(&mut graph, &mut node_index, src);
            let tgt_idx = get_or_insert_node(&mut graph, &mut node_index, tgt);
            graph.add_edge(src_idx, tgt_idx, edge_kind);
        }

        Ok(Self { graph, node_index })
    }
}

// ---------------------------------------------------------------------------
// GraphStore
// ---------------------------------------------------------------------------

/// Persistent graph store backed by SQLite.
pub struct GraphStore {
    conn: Connection,
    cache: Mutex<Option<GraphCache>>,
}

impl GraphStore {
    /// Open (or create) the graph database at the given path.
    pub fn new(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(db_path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")?;
        conn.execute_batch(SCHEMA_SQL)?;

        Ok(Self {
            conn,
            cache: Mutex::new(None),
        })
    }

    // -- Write operations --

    /// Store all nodes and edges for a file (replaces previous data for that file).
    pub fn store_file_nodes_edges(
        &self,
        file_path: &str,
        nodes: &[NodeInfo],
        edges: &[EdgeInfo],
        file_hash: &str,
    ) -> Result<()> {
        self.remove_file_data_inner(file_path)?;
        for node in nodes {
            self.upsert_node_inner(node, file_hash)?;
        }
        for edge in edges {
            self.upsert_edge_inner(edge)?;
        }
        self.invalidate_cache();
        Ok(())
    }

    /// Remove all data associated with a file.
    pub fn remove_file_data(&self, file_path: &str) -> Result<()> {
        self.remove_file_data_inner(file_path)?;
        self.invalidate_cache();
        Ok(())
    }

    /// No-op: rusqlite auto-commits unless in an explicit transaction.
    /// Kept for API compatibility with incremental.rs callers.
    pub fn commit(&self) -> Result<()> {
        Ok(())
    }

    // -- Read operations --

    /// Get a node by qualified name.
    pub fn get_node(&self, qualified_name: &str) -> Result<Option<GraphNode>> {
        const SQL: &str = concat!(
            "SELECT kind, name, qualified_name, file_path, line_start, line_end, \
             language, is_test, docstring, signature, body_hash, file_hash \
             FROM nodes WHERE qualified_name = ?"
        );
        let mut stmt = self.conn.prepare_cached(SQL)?;
        let result = stmt.query_row(params![qualified_name], row_to_node);
        match result {
            Ok(n) => Ok(Some(n)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Get all nodes in a file.
    pub fn get_nodes_by_file(&self, file_path: &str) -> Result<Vec<GraphNode>> {
        const SQL: &str = concat!(
            "SELECT kind, name, qualified_name, file_path, line_start, line_end, \
             language, is_test, docstring, signature, body_hash, file_hash \
             FROM nodes WHERE file_path = ?"
        );
        let mut stmt = self.conn.prepare_cached(SQL)?;
        let rows = stmt.query_map(params![file_path], row_to_node)?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Get all file paths in the store.
    pub fn get_all_files(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn
            .prepare_cached("SELECT DISTINCT file_path FROM nodes WHERE kind = 'File'")?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Search nodes by name substring (multi-word AND logic).
    ///
    /// Each word must independently match `name` or `qualified_name`
    /// (case-insensitive).
    pub fn search_nodes(&self, query: &str, limit: usize) -> Result<Vec<GraphNode>> {
        let words: Vec<String> = query
            .split_whitespace()
            .map(|w| w.to_lowercase())
            .collect();
        if words.is_empty() {
            return Ok(vec![]);
        }

        let conditions: String = words
            .iter()
            .map(|_| "(LOWER(name) LIKE ? OR LOWER(qualified_name) LIKE ?)")
            .collect::<Vec<_>>()
            .join(" AND ");
        let sql = format!(
            "SELECT {NODE_COLS} FROM nodes WHERE {conditions} LIMIT {limit}"
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let param_values: Vec<String> = words
            .iter()
            .flat_map(|w| [format!("%{w}%"), format!("%{w}%")])
            .collect();
        let params_refs: Vec<&dyn rusqlite::types::ToSql> = param_values
            .iter()
            .map(|s| s as &dyn rusqlite::types::ToSql)
            .collect();

        let rows = stmt.query_map(params_refs.as_slice(), row_to_node)?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Get nodes exceeding a line count threshold, ordered by size descending.
    pub fn get_nodes_by_size(
        &self,
        min_lines: usize,
        kind: Option<&str>,
        file_path_pattern: Option<&str>,
        limit: usize,
    ) -> Result<Vec<GraphNode>> {
        let mut conditions = vec!["(line_end - line_start + 1) >= ?".to_string()];
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> =
            vec![Box::new(min_lines as i64)];

        if let Some(k) = kind {
            conditions.push("kind = ?".to_string());
            param_values.push(Box::new(k.to_string()));
        }
        if let Some(pat) = file_path_pattern {
            conditions.push("file_path LIKE ?".to_string());
            param_values.push(Box::new(format!("%{pat}%")));
        }

        let where_clause = conditions.join(" AND ");
        let sql = format!(
            "SELECT {NODE_COLS} FROM nodes WHERE {where_clause} \
             ORDER BY (line_end - line_start + 1) DESC LIMIT {limit}"
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let params_refs: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|b| b.as_ref()).collect();
        let rows = stmt.query_map(params_refs.as_slice(), row_to_node)?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    // -- Edge operations --

    /// Get edges originating from a qualified name.
    pub fn get_edges_by_source(&self, qualified_name: &str) -> Result<Vec<GraphEdge>> {
        const SQL: &str =
            "SELECT kind, source_qualified, target_qualified, file_path, line \
             FROM edges WHERE source_qualified = ?";
        let mut stmt = self.conn.prepare_cached(SQL)?;
        let rows = stmt.query_map(params![qualified_name], row_to_edge)?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Get edges targeting a qualified name.
    pub fn get_edges_by_target(&self, qualified_name: &str) -> Result<Vec<GraphEdge>> {
        const SQL: &str =
            "SELECT kind, source_qualified, target_qualified, file_path, line \
             FROM edges WHERE target_qualified = ?";
        let mut stmt = self.conn.prepare_cached(SQL)?;
        let rows = stmt.query_map(params![qualified_name], row_to_edge)?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Search edges by unqualified target name (CALLS edges only).
    ///
    /// CALLS edges often store unqualified target names (e.g. `foo` rather than
    /// `file.ts::foo`). Use this to find callers even when qualified lookup fails.
    pub fn search_edges_by_target_name(&self, name: &str) -> Result<Vec<GraphEdge>> {
        const SQL: &str =
            "SELECT kind, source_qualified, target_qualified, file_path, line \
             FROM edges WHERE target_qualified = ? AND kind = 'CALLS'";
        let mut stmt = self.conn.prepare_cached(SQL)?;
        let rows = stmt.query_map(params![name], row_to_edge)?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    // -- Impact analysis (petgraph) --

    /// Compute the blast radius of changed files.
    pub fn get_impact_radius(
        &self,
        changed_files: &[String],
        max_depth: usize,
        max_nodes: usize,
    ) -> Result<ImpactResult> {
        let mut seeds: HashSet<String> = HashSet::new();
        for f in changed_files {
            for node in self.get_nodes_by_file(f)? {
                seeds.insert(node.qualified_name.clone());
            }
        }

        let impacted = self.with_cache(|cache| {
            Ok(bfs_impact(&seeds, cache, max_depth, max_nodes))
        })?;

        let changed_nodes: Vec<GraphNode> = seeds
            .iter()
            .filter_map(|qn| self.get_node(qn).ok().flatten())
            .collect();

        let mut impacted_nodes: Vec<GraphNode> = impacted
            .iter()
            .filter(|qn| !seeds.contains(*qn))
            .filter_map(|qn| self.get_node(qn).ok().flatten())
            .collect();

        let total_impacted = impacted_nodes.len();
        let truncated = total_impacted > max_nodes;
        if truncated {
            impacted_nodes.truncate(max_nodes);
        }

        let impacted_files: Vec<String> = impacted_nodes
            .iter()
            .map(|n| n.file_path.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();

        let all_qns: HashSet<String> = seeds
            .iter()
            .cloned()
            .chain(impacted_nodes.iter().map(|n| n.qualified_name.clone()))
            .collect();
        let edges = self.get_edges_among(&all_qns)?;

        Ok(ImpactResult {
            changed_nodes,
            impacted_nodes,
            impacted_files,
            edges,
            truncated,
            total_impacted,
        })
    }

    // -- Metadata --

    /// Get aggregate statistics.
    pub fn get_stats(&self) -> Result<GraphStats> {
        let total_nodes = self
            .conn
            .query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get::<_, i64>(0))?
            as usize;

        let total_edges = self
            .conn
            .query_row("SELECT COUNT(*) FROM edges", [], |r| r.get::<_, i64>(0))?
            as usize;

        let nodes_by_kind = {
            let mut stmt = self
                .conn
                .prepare("SELECT kind, COUNT(*) FROM nodes GROUP BY kind")?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)? as usize,
                ))
            })?;
            rows.collect::<rusqlite::Result<HashMap<_, _>>>()?
        };

        let edges_by_kind = {
            let mut stmt = self
                .conn
                .prepare("SELECT kind, COUNT(*) FROM edges GROUP BY kind")?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)? as usize,
                ))
            })?;
            rows.collect::<rusqlite::Result<HashMap<_, _>>>()?
        };

        let languages = {
            let mut stmt = self.conn.prepare(
                "SELECT DISTINCT language FROM nodes \
                 WHERE language IS NOT NULL AND language != ''",
            )?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            rows.map(|r| r.map_err(Into::into)).collect::<Result<Vec<_>>>()?
        };

        let files_count = self.conn.query_row(
            "SELECT COUNT(*) FROM nodes WHERE kind = 'File'",
            [],
            |r| r.get::<_, i64>(0),
        )? as usize;

        let last_updated = self.get_metadata("last_updated")?;

        Ok(GraphStats {
            total_nodes,
            total_edges,
            nodes_by_kind,
            edges_by_kind,
            languages,
            files_count,
            last_updated,
        })
    }

    /// Set a metadata key-value pair.
    pub fn set_metadata(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO metadata (key, value) VALUES (?, ?)",
            params![key, value],
        )?;
        Ok(())
    }

    /// Get a metadata value.
    pub fn get_metadata(&self, key: &str) -> Result<Option<String>> {
        let result = self.conn.query_row(
            "SELECT value FROM metadata WHERE key = ?",
            params![key],
            |row| row.get(0),
        );
        match result {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Close the database connection.
    pub fn close(self) -> Result<()> {
        drop(self.conn);
        Ok(())
    }

    // -- Internal helpers --

    fn invalidate_cache(&self) {
        if let Ok(mut guard) = self.cache.lock() {
            *guard = None;
        }
    }

    fn with_cache<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&GraphCache) -> Result<T>,
    {
        let mut guard = self.cache.lock().unwrap();
        if guard.is_none() {
            *guard = Some(GraphCache::build(&self.conn)?);
        }
        f(guard.as_ref().unwrap())
    }

    fn remove_file_data_inner(&self, file_path: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM nodes WHERE file_path = ?", params![file_path])?;
        self.conn
            .execute("DELETE FROM edges WHERE file_path = ?", params![file_path])?;
        Ok(())
    }

    fn upsert_node_inner(&self, node: &NodeInfo, file_hash: &str) -> Result<()> {
        let now = unix_now();
        self.conn.execute(
            "INSERT INTO nodes
               (kind, name, qualified_name, file_path, line_start, line_end,
                language, is_test, docstring, signature, body_hash, file_hash, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(qualified_name) DO UPDATE SET
               kind=excluded.kind, name=excluded.name,
               file_path=excluded.file_path, line_start=excluded.line_start,
               line_end=excluded.line_end, language=excluded.language,
               is_test=excluded.is_test, docstring=excluded.docstring,
               signature=excluded.signature, body_hash=excluded.body_hash,
               file_hash=excluded.file_hash, updated_at=excluded.updated_at",
            params![
                node.kind.as_str(),
                node.name,
                node.qualified_name,
                node.file_path,
                node.line_start as i64,
                node.line_end as i64,
                node.language,
                node.is_test as i64,
                node.docstring,
                node.signature,
                node.body_hash,
                file_hash,
                now,
            ],
        )?;
        Ok(())
    }

    fn upsert_edge_inner(&self, edge: &EdgeInfo) -> Result<()> {
        let now = unix_now();

        // SELECT-then-INSERT-or-UPDATE preserves distinct call sites at different
        // source lines (a single UPSERT on (kind,src,tgt) would collapse them).
        let existing_id: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM edges
                 WHERE kind = ?1 AND source_qualified = ?2 AND target_qualified = ?3
                       AND file_path = ?4 AND line = ?5",
                params![
                    edge.kind.as_str(),
                    edge.source_qualified,
                    edge.target_qualified,
                    edge.file_path,
                    edge.line as i64,
                ],
                |row| row.get(0),
            )
            .ok();

        if let Some(id) = existing_id {
            self.conn.execute(
                "UPDATE edges SET updated_at = ?1 WHERE id = ?2",
                params![now, id],
            )?;
        } else {
            self.conn.execute(
                "INSERT INTO edges
                   (kind, source_qualified, target_qualified, file_path, line, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    edge.kind.as_str(),
                    edge.source_qualified,
                    edge.target_qualified,
                    edge.file_path,
                    edge.line as i64,
                    now,
                ],
            )?;
        }
        Ok(())
    }

    /// Return edges where both source and target are in the given set.
    ///
    /// Batches the source-side IN clause to stay under SQLite's 999-variable limit,
    /// then filters targets in Rust.
    fn get_edges_among(&self, qualified_names: &HashSet<String>) -> Result<Vec<GraphEdge>> {
        if qualified_names.is_empty() {
            return Ok(vec![]);
        }
        const BATCH_SIZE: usize = 450;
        let qns: Vec<&String> = qualified_names.iter().collect();
        let mut results = Vec::new();

        for batch in qns.chunks(BATCH_SIZE) {
            let placeholders = batch.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
            let sql = format!(
                "SELECT {EDGE_COLS} FROM edges WHERE source_qualified IN ({placeholders})"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let params_refs: Vec<&dyn rusqlite::types::ToSql> =
                batch.iter().map(|s| *s as &dyn rusqlite::types::ToSql).collect();
            let rows = stmt.query_map(params_refs.as_slice(), row_to_edge)?;
            for row in rows {
                let edge = row?;
                if qualified_names.contains(&edge.target_qualified) {
                    results.push(edge);
                }
            }
        }
        Ok(results)
    }
}

// ---------------------------------------------------------------------------
// BFS traversal (pure function, no DB access)
// ---------------------------------------------------------------------------

fn bfs_impact(
    seeds: &HashSet<String>,
    cache: &GraphCache,
    max_depth: usize,
    max_nodes: usize,
) -> HashSet<String> {
    use petgraph::Direction;

    let mut visited: HashSet<String> = HashSet::new();
    let mut frontier: VecDeque<String> = seeds.iter().cloned().collect();
    let mut impacted: HashSet<String> = HashSet::new();

    for _ in 0..max_depth {
        if frontier.is_empty() {
            break;
        }
        let mut next_frontier: Vec<String> = Vec::new();
        while let Some(qn) = frontier.pop_front() {
            if visited.contains(&qn) {
                continue;
            }
            visited.insert(qn.clone());

            if let Some(&idx) = cache.node_index.get(&qn) {
                for nb_idx in cache.graph.neighbors(idx) {
                    let nb = &cache.graph[nb_idx];
                    if !visited.contains(nb) {
                        impacted.insert(nb.clone());
                        next_frontier.push(nb.clone());
                    }
                }
                for pred_idx in cache.graph.neighbors_directed(idx, Direction::Incoming) {
                    let pred = &cache.graph[pred_idx];
                    if !visited.contains(pred) {
                        impacted.insert(pred.clone());
                        next_frontier.push(pred.clone());
                    }
                }
            }
            if visited.len() + next_frontier.len() > max_nodes {
                return impacted;
            }
        }
        frontier.extend(next_frontier);
    }
    impacted
}

// ---------------------------------------------------------------------------
// Row mapping helpers
// ---------------------------------------------------------------------------

fn row_to_node(row: &rusqlite::Row<'_>) -> rusqlite::Result<GraphNode> {
    let kind_str: String = row.get(0)?;
    let kind = NodeKind::from_str(&kind_str).unwrap_or(NodeKind::Function);
    Ok(GraphNode {
        kind,
        name: row.get(1)?,
        qualified_name: row.get(2)?,
        file_path: row.get(3)?,
        line_start: row.get::<_, i64>(4)? as usize,
        line_end: row.get::<_, i64>(5)? as usize,
        language: row.get(6)?,
        is_test: row.get::<_, i64>(7)? != 0,
        docstring: row.get(8)?,
        signature: row.get(9)?,
        body_hash: row.get(10)?,
        file_hash: row.get(11)?,
    })
}

fn row_to_edge(row: &rusqlite::Row<'_>) -> rusqlite::Result<GraphEdge> {
    let kind_str: String = row.get(0)?;
    let kind = EdgeKind::from_str(&kind_str).unwrap_or(EdgeKind::Calls);
    Ok(GraphEdge {
        kind,
        source_qualified: row.get(1)?,
        target_qualified: row.get(2)?,
        file_path: row.get(3)?,
        line: row.get::<_, i64>(4)? as usize,
    })
}

fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}
