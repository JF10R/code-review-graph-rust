use std::path::Path;

use code_review_graph::{
    graph::GraphStore,
    parser::CodeParser,
    types::{EdgeInfo, EdgeKind, NodeInfo, NodeKind},
};
use criterion::{criterion_group, criterion_main, Criterion};
use std::hint::black_box;
use tempfile::TempDir;

fn grammar_check_ok(parser: &CodeParser, path: &Path, label: &str) -> bool {
    match parser.parse_bytes(path, b"") {
        Ok(_) => true,
        Err(e) => {
            eprintln!("{label}: grammar unavailable, skipping ({e})");
            false
        }
    }
}

fn generate_python_source(num_functions: usize) -> String {
    let mut src = String::with_capacity(num_functions * 90);
    src.push_str("import os\nimport sys\n\n");
    for i in 0..num_functions {
        src.push_str(&format!(
            "def function_{i}(x, y):\n    \"\"\"Docstring for function {i}.\"\"\"\n    result = x + y\n    return result\n\n"
        ));
    }
    src.push_str("def main():\n");
    for i in 0..num_functions {
        src.push_str(&format!("    function_{i}(1, 2)\n"));
    }
    src
}

fn generate_typescript_source(num_functions: usize) -> String {
    let mut src = String::with_capacity(num_functions * 100);
    src.push_str("import { readFileSync } from 'fs';\nimport path from 'path';\n\n");
    src.push_str("export class Processor {\n");
    for i in 0..num_functions {
        src.push_str(&format!(
            "  process_{i}(x: number, y: number): number {{\n    return x + y;\n  }}\n\n"
        ));
    }
    src.push_str("}\n\n");
    for i in 0..num_functions {
        src.push_str(&format!(
            "export function standalone_{i}(a: string): string {{\n  return a.trim();\n}}\n\n"
        ));
    }
    src
}

fn populate_graph(store: &mut GraphStore, num_files: usize, nodes_per_file: usize) {
    for f in 0..num_files {
        let file_path = format!("/src/file_{f}.ts");
        let mut nodes = Vec::with_capacity(nodes_per_file);
        let mut edges = Vec::with_capacity(nodes_per_file.saturating_sub(1));
        for n in 0..nodes_per_file {
            nodes.push(NodeInfo {
                name: format!("func_{n}"),
                qualified_name: format!("{file_path}::func_{n}"),
                kind: NodeKind::Function,
                file_path: file_path.clone(),
                line_start: n * 10,
                line_end: n * 10 + 8,
                language: "typescript".to_string(),
                is_test: false,
                docstring: String::new(),
                signature: "(x: number): number".to_string(),
                body_hash: format!("hash_{f}_{n}"),
            });
            if n > 0 {
                edges.push(EdgeInfo {
                    source_qualified: format!("{file_path}::func_{n}"),
                    target_qualified: format!("{file_path}::func_{}", n - 1),
                    kind: EdgeKind::Calls,
                    file_path: file_path.clone(),
                    line: n * 10 + 3,
                });
            }
        }
        let file_hash = format!("hash_{f}");
        store
            .store_file_nodes_edges(&file_path, &nodes, &edges, &file_hash)
            .unwrap();
    }
}

fn build_graph_in_tempdir() -> (TempDir, GraphStore) {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("graph.bin.zst");
    let mut store = GraphStore::new(&db_path).unwrap();
    // 10 files × 100 nodes = 1000 nodes; 99 intra-file call edges per file → ~990 total.
    populate_graph(&mut store, 10, 100);
    (dir, store)
}

fn bench_parse_python(c: &mut Criterion) {
    let parser = CodeParser::new();
    let path = Path::new("module.py");
    if !grammar_check_ok(&parser, path, "parse_python") {
        return;
    }
    let source = generate_python_source(50);

    c.bench_function("parse_python/50_functions", |b| {
        b.iter(|| black_box(parser.parse_bytes(path, source.as_bytes()).unwrap()))
    });
}

fn bench_parse_typescript(c: &mut Criterion) {
    let parser = CodeParser::new();
    let path = Path::new("module.ts");
    if !grammar_check_ok(&parser, path, "parse_typescript") {
        return;
    }
    let source = generate_typescript_source(50);

    c.bench_function("parse_typescript/50_functions", |b| {
        b.iter(|| black_box(parser.parse_bytes(path, source.as_bytes()).unwrap()))
    });
}

fn bench_graph_save(c: &mut Criterion) {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("graph.bin.zst");
    let mut store = GraphStore::new(&db_path).unwrap();
    populate_graph(&mut store, 10, 100);

    c.bench_function("graph/commit_1000_nodes", |b| {
        b.iter(|| black_box(store.commit().unwrap()))
    });
}

fn bench_graph_load(c: &mut Criterion) {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("graph.bin.zst");
    {
        let mut store = GraphStore::new(&db_path).unwrap();
        populate_graph(&mut store, 10, 100);
        store.commit().unwrap();
    }

    c.bench_function("graph/load_1000_nodes", |b| {
        b.iter(|| black_box(GraphStore::new(&db_path).unwrap()))
    });
}

fn bench_impact_radius(c: &mut Criterion) {
    let (_dir, store) = build_graph_in_tempdir();
    let changed_files: Vec<String> = vec![
        "/src/file_0.ts".to_string(),
        "/src/file_3.ts".to_string(),
        "/src/file_7.ts".to_string(),
    ];

    c.bench_function("graph/impact_radius_3_files", |b| {
        b.iter(|| {
            black_box(
                store
                    .get_impact_radius(black_box(&changed_files), 5, 200, None)
                    .unwrap(),
            )
        })
    });
}

fn bench_search_nodes(c: &mut Criterion) {
    let (_dir, store) = build_graph_in_tempdir();

    c.bench_function("graph/search_nodes_limit20", |b| {
        b.iter(|| black_box(store.search_nodes(black_box("func"), 20).unwrap()))
    });
}

fn bench_graph_stats(c: &mut Criterion) {
    let (_dir, store) = build_graph_in_tempdir();

    c.bench_function("graph/get_stats", |b| {
        b.iter(|| black_box(store.get_stats().unwrap()))
    });
}

criterion_group!(parser_benches, bench_parse_python, bench_parse_typescript);
criterion_group!(
    graph_benches,
    bench_graph_save,
    bench_graph_load,
    bench_impact_radius,
    bench_search_nodes,
    bench_graph_stats
);
criterion_main!(parser_benches, graph_benches);
