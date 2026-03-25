//! Tantivy-based full-text search for graph nodes.
//!
//! Provides fuzzy, relevance-ranked search as an alternative to
//! the linear scan in `GraphStore::search_nodes`.

use tantivy::collector::TopDocs;
use tantivy::query::{FuzzyTermQuery, QueryParser};
use tantivy::schema::{OwnedValue, Schema, STORED, STRING, TEXT};
use tantivy::{doc, Index, IndexWriter, Term};

use crate::error::{CrgError, Result};
use crate::graph::GraphStore;
use crate::types::GraphNode;

/// An in-memory Tantivy index built over all nodes in a `GraphStore`.
pub struct TantivySearchIndex {
    index: Index,
    // Retained for potential future use (filtered searches, schema introspection)
    #[allow(dead_code)]
    schema: Schema,
    f_qualified_name: tantivy::schema::Field,
    f_name: tantivy::schema::Field,
    #[allow(dead_code)]
    f_kind: tantivy::schema::Field,
    #[allow(dead_code)]
    f_file_path: tantivy::schema::Field,
    f_docstring: tantivy::schema::Field,
}

impl TantivySearchIndex {
    /// Build an in-memory search index from all nodes in the graph store.
    pub fn build(store: &GraphStore) -> Result<Self> {
        let mut schema_builder = Schema::builder();
        let f_qualified_name = schema_builder.add_text_field("qualified_name", STRING | STORED);
        let f_name = schema_builder.add_text_field("name", TEXT | STORED);
        let f_kind = schema_builder.add_text_field("kind", STRING | STORED);
        let f_file_path = schema_builder.add_text_field("file_path", STRING | STORED);
        let f_docstring = schema_builder.add_text_field("docstring", TEXT);
        let schema = schema_builder.build();

        let index = Index::create_in_ram(schema.clone());
        let mut writer: IndexWriter = index
            .writer(15_000_000)
            .map_err(|e| CrgError::Other(format!("tantivy writer: {e}")))?;

        for file_path in store.get_all_files()? {
            for node in store.get_nodes_by_file(&file_path)? {
                writer
                    .add_document(doc!(
                        f_qualified_name => node.qualified_name.as_str(),
                        f_name => node.name.as_str(),
                        f_kind => node.kind.as_str(),
                        f_file_path => node.file_path.as_str(),
                        f_docstring => node.docstring.as_str(),
                    ))
                    .map_err(|e| CrgError::Other(format!("tantivy add: {e}")))?;
            }
        }
        writer
            .commit()
            .map_err(|e| CrgError::Other(format!("tantivy commit: {e}")))?;

        Ok(Self {
            index,
            schema,
            f_qualified_name,
            f_name,
            f_kind,
            f_file_path,
            f_docstring,
        })
    }

    /// Search for nodes matching a query, returning qualified names ranked by relevance.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<String>> {
        if query.trim().is_empty() {
            return Ok(vec![]);
        }

        let reader = self
            .index
            .reader()
            .map_err(|e| CrgError::Other(format!("tantivy reader: {e}")))?;
        let searcher = reader.searcher();

        let query_parser =
            QueryParser::for_index(&self.index, vec![self.f_name, self.f_docstring]);

        let parsed = query_parser.parse_query(query).unwrap_or_else(|_| {
            let term = Term::from_field_text(self.f_name, query);
            Box::new(FuzzyTermQuery::new(term, 1, true))
        });

        let top_docs = searcher
            .search(&parsed, &TopDocs::with_limit(limit))
            .map_err(|e| CrgError::Other(format!("tantivy search: {e}")))?;

        let mut results = Vec::new();
        for (_score, doc_addr) in top_docs {
            let doc: tantivy::TantivyDocument = searcher
                .doc(doc_addr)
                .map_err(|e| CrgError::Other(format!("tantivy doc: {e}")))?;
            if let Some(OwnedValue::Str(text)) = doc.get_first(self.f_qualified_name) {
                results.push(text.clone());
            }
        }
        Ok(results)
    }
}

/// Search nodes using Tantivy full-text search, returning full `GraphNode` objects.
pub fn search_nodes_tantivy(
    query: &str,
    store: &GraphStore,
    limit: usize,
) -> Result<Vec<GraphNode>> {
    let index = TantivySearchIndex::build(store)?;
    let qualified_names = index.search(query, limit)?;
    let mut nodes = Vec::new();
    for qn in qualified_names {
        if let Some(node) = store.get_node(&qn)? {
            nodes.push(node);
        }
    }
    Ok(nodes)
}

/// Search nodes using a pre-built Tantivy index.
///
/// Unlike `search_nodes_tantivy`, this function does **not** rebuild the index
/// on every call — the caller is responsible for caching the index and passing
/// it in.  Returns full `GraphNode` objects with relevance-ranked ordering
/// (exact match → prefix match → other), identical to
/// `GraphStore::search_nodes`.
pub fn search_nodes_indexed(
    index: &TantivySearchIndex,
    store: &GraphStore,
    query: &str,
    limit: usize,
) -> Result<Vec<GraphNode>> {
    if query.trim().is_empty() {
        return Ok(vec![]);
    }
    let qualified_names = index.search(query, limit * 2)?;
    let query_lower = query.to_lowercase();

    let mut results: Vec<(u8, GraphNode)> = qualified_names
        .into_iter()
        .filter_map(|qn| {
            let node = store.get_node(&qn).ok()??;
            let name_lower = node.name.to_lowercase();
            let qn_lower = node.qualified_name.to_lowercase();
            let relevance = if name_lower == query_lower || qn_lower == query_lower {
                0u8
            } else if name_lower.starts_with(&query_lower) {
                1u8
            } else {
                2u8
            };
            Some((relevance, node))
        })
        .collect();

    results.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.name.cmp(&b.1.name)));
    results.truncate(limit);
    Ok(results.into_iter().map(|(_, n)| n).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{NodeInfo, NodeKind};
    use camino::Utf8PathBuf;
    use tempfile::TempDir;

    /// Build a store containing the given nodes.
    ///
    /// Each distinct `file_path` in `nodes` gets a synthetic `NodeKind::File` node
    /// so that `GraphStore::get_all_files` (which filters by `NodeKind::File`) returns
    /// that path, allowing `TantivySearchIndex::build` to find all nodes.
    fn make_store_with_nodes(nodes: Vec<NodeInfo>) -> (GraphStore, TempDir) {
        let dir = TempDir::new().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().join("test.bin.zst")).unwrap();
        let mut store = GraphStore::new(&path).unwrap();

        let mut by_file: std::collections::HashMap<String, Vec<NodeInfo>> =
            std::collections::HashMap::new();
        for n in nodes {
            by_file.entry(n.file_path.clone()).or_default().push(n);
        }

        for (file_path, mut file_nodes) in by_file {
            let file_node = NodeInfo {
                name: file_path.clone(),
                qualified_name: file_path.clone(),
                kind: NodeKind::File,
                file_path: file_path.clone(),
                line_start: 0,
                line_end: 0,
                language: "rust".to_string(),
                is_test: false,
                docstring: String::new(),
                signature: String::new(),
                body_hash: String::new(),
            };
            file_nodes.insert(0, file_node);
            store
                .store_file_nodes_edges(&file_path, &file_nodes, &[], "testhash")
                .unwrap();
        }

        (store, dir)
    }

    fn make_fn(name: &str, qn: &str, file: &str, docstring: &str) -> NodeInfo {
        NodeInfo {
            name: name.to_string(),
            qualified_name: qn.to_string(),
            kind: NodeKind::Function,
            file_path: file.to_string(),
            line_start: 1,
            line_end: 10,
            language: "rust".to_string(),
            is_test: false,
            docstring: docstring.to_string(),
            signature: String::new(),
            body_hash: String::new(),
        }
    }

    #[test]
    fn build_and_search_known_node() {
        let nodes = vec![
            make_fn("parse_file", "mod::parse_file", "src/parser.rs", ""),
            make_fn("render_graph", "mod::render_graph", "src/viz.rs", ""),
        ];
        let (store, _dir) = make_store_with_nodes(nodes);

        let results = search_nodes_tantivy("parse_file", &store, 10).unwrap();
        assert!(!results.is_empty(), "should find parse_file");
        assert!(
            results.iter().any(|n| n.qualified_name == "mod::parse_file"),
            "result must include mod::parse_file"
        );
    }

    #[test]
    fn fuzzy_match_partial_term() {
        // "parse" should match "parse_tokens" via prefix/fuzzy
        let nodes = vec![make_fn("parse_tokens", "lib::parse_tokens", "src/lib.rs", "")];
        let (store, _dir) = make_store_with_nodes(nodes);

        let results = search_nodes_tantivy("parse", &store, 10).unwrap();
        assert!(!results.is_empty(), "should match 'parse' against 'parse_tokens'");
    }

    #[test]
    fn empty_query_returns_empty() {
        let nodes = vec![make_fn("some_func", "mod::some_func", "src/lib.rs", "")];
        let (store, _dir) = make_store_with_nodes(nodes);

        let results = search_nodes_tantivy("", &store, 10).unwrap();
        assert!(results.is_empty(), "empty query must return no results");

        let results_ws = search_nodes_tantivy("   ", &store, 10).unwrap();
        assert!(results_ws.is_empty(), "whitespace-only query must return no results");
    }

    #[test]
    fn empty_store_returns_empty() {
        let dir = TempDir::new().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().join("empty.bin.zst")).unwrap();
        let store = GraphStore::new(&path).unwrap();

        let results = search_nodes_tantivy("anything", &store, 10).unwrap();
        assert!(results.is_empty(), "empty store should return no results");
    }

    #[test]
    fn docstring_search() {
        let nodes = vec![make_fn(
            "build_index",
            "search::build_index",
            "src/search.rs",
            "Builds a full-text search index over all graph nodes",
        )];
        let (store, _dir) = make_store_with_nodes(nodes);

        let results = search_nodes_tantivy("index", &store, 10).unwrap();
        assert!(!results.is_empty(), "should find node by docstring");
        assert!(
            results.iter().any(|n| n.qualified_name == "search::build_index"),
            "must find search::build_index"
        );
    }

    #[test]
    fn search_nodes_indexed_returns_matching_nodes() {
        let nodes = vec![
            make_fn("parse_file", "src/parser.rs::parse_file", "src/parser.rs", ""),
            make_fn(
                "search_nodes",
                "src/graph.rs::search_nodes",
                "src/graph.rs",
                "",
            ),
        ];
        let (store, _dir) = make_store_with_nodes(nodes);
        let index = TantivySearchIndex::build(&store).unwrap();
        let results = search_nodes_indexed(&index, &store, "parse", 10).unwrap();
        assert!(!results.is_empty(), "should find parse_file");
        assert!(
            results.iter().any(|n| n.name == "parse_file"),
            "result must include parse_file"
        );
    }

    #[test]
    fn search_nodes_indexed_empty_query_returns_empty() {
        let nodes = vec![make_fn("some_func", "mod::some_func", "src/lib.rs", "")];
        let (store, _dir) = make_store_with_nodes(nodes);
        let index = TantivySearchIndex::build(&store).unwrap();
        let results = search_nodes_indexed(&index, &store, "", 10).unwrap();
        assert!(results.is_empty(), "empty query must return no results");
        let results_ws = search_nodes_indexed(&index, &store, "   ", 10).unwrap();
        assert!(results_ws.is_empty(), "whitespace-only query must return no results");
    }

    #[test]
    fn search_nodes_indexed_exact_match_ranked_first() {
        let nodes = vec![
            make_fn("parse", "mod::parse", "src/lib.rs", ""),
            make_fn("parse_tokens", "mod::parse_tokens", "src/lib.rs", ""),
            make_fn("parse_file", "mod::parse_file", "src/lib.rs", ""),
        ];
        let (store, _dir) = make_store_with_nodes(nodes);
        let index = TantivySearchIndex::build(&store).unwrap();
        let results = search_nodes_indexed(&index, &store, "parse", 10).unwrap();
        assert!(!results.is_empty());
        // Exact match should come first
        assert_eq!(results[0].name, "parse", "exact match must be ranked first");
    }
}
