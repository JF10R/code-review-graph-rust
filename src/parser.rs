//! Multi-language source code parser using tree-sitter.
//!
//! Extracts functions, classes, imports, calls, and inheritance from ASTs.
//! Supports 15 languages (+ Vue SFC) via native tree-sitter grammar crates.
//! Node-type classification is driven by per-language `.scm` query files
//! embedded at compile time via `include_str!()`.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::OnceLock;

use sha2::{Digest, Sha256};
use tree_sitter::{Language, Node, Parser};

use crate::error::{CrgError, Result};
use crate::types::{EdgeInfo, EdgeKind, NodeInfo, NodeKind};

// ---------------------------------------------------------------------------
// Language detection
// ---------------------------------------------------------------------------

/// Detect the programming language from a file extension.
pub fn detect_language(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?;
    match ext.to_ascii_lowercase().as_str() {
        "py" => Some("python"),
        "js" | "mjs" | "cjs" | "jsx" => Some("javascript"),
        "ts" | "mts" | "cts" => Some("typescript"),
        "tsx" => Some("tsx"),
        "rs" => Some("rust"),
        "go" => Some("go"),
        "java" => Some("java"),
        "c" | "h" => Some("c"),
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" | "hh" => Some("cpp"),
        "cs" => Some("csharp"),
        "rb" => Some("ruby"),
        "php" => Some("php"),
        "kt" | "kts" => Some("kotlin"),
        "swift" => Some("swift"),
        "zig" | "zon" => Some("zig"),
        "vue" => Some("vue"),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Language grammar lookup
// ---------------------------------------------------------------------------

fn grammar_for(language: &str) -> Option<Language> {
    match language {
        "python" => Some(tree_sitter_python::LANGUAGE.into()),
        "javascript" => Some(tree_sitter_javascript::LANGUAGE.into()),
        "typescript" => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        "tsx" => Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
        "rust" => Some(tree_sitter_rust::LANGUAGE.into()),
        "go" => Some(tree_sitter_go::LANGUAGE.into()),
        "java" => Some(tree_sitter_java::LANGUAGE.into()),
        "c" => Some(tree_sitter_c::LANGUAGE.into()),
        "cpp" => Some(tree_sitter_cpp::LANGUAGE.into()),
        "csharp" => Some(tree_sitter_c_sharp::LANGUAGE.into()),
        "ruby" => Some(tree_sitter_ruby::LANGUAGE.into()),
        "php" => Some(tree_sitter_php::LANGUAGE_PHP.into()),
        "kotlin" => Some(tree_sitter_kotlin_ng::LANGUAGE.into()),
        "swift" => Some(tree_sitter_swift::LANGUAGE.into()),
        "zig" => Some(tree_sitter_zig::LANGUAGE.into()),
        _ => None,
    }
}

fn make_parser(language: &str) -> Option<Parser> {
    let grammar = grammar_for(language)?;
    let mut parser = Parser::new();
    parser.set_language(&grammar).ok()?;
    Some(parser)
}

// ---------------------------------------------------------------------------
// Node-type classification via embedded .scm query files
// ---------------------------------------------------------------------------
//
// Each language has a `queries/<lang>.scm` file embedded at compile time.
// The file contains S-expression patterns tagged with one of four categories:
//   @definition.class    — class-like constructs
//   @definition.function — callable definitions
//   @reference.import    — import/include statements
//   @reference.call      — call sites
//
// `CompiledQueries` parses those files once and exposes O(1) HashSet lookups
// identical in interface to the old hardcoded slices, keeping all walk logic
// unchanged while making the `.scm` files the single source of truth.

/// Embedded `.scm` sources: (language_key, scm_text).
/// Languages sharing a grammar (typescript→javascript, tsx→javascript,
/// cpp→c) are listed with their own key but point to the shared .scm file.
const SCM_SOURCES: &[(&str, &str)] = &[
    ("python",     include_str!("../queries/python.scm")),
    ("javascript", include_str!("../queries/javascript.scm")),
    ("typescript", include_str!("../queries/javascript.scm")),
    ("tsx",        include_str!("../queries/javascript.scm")),
    ("rust",       include_str!("../queries/rust.scm")),
    ("go",         include_str!("../queries/go.scm")),
    ("java",       include_str!("../queries/java.scm")),
    ("c",          include_str!("../queries/c.scm")),
    ("cpp",        include_str!("../queries/c.scm")),
    ("csharp",     include_str!("../queries/csharp.scm")),
    ("ruby",       include_str!("../queries/ruby.scm")),
    ("kotlin",     include_str!("../queries/kotlin.scm")),
    ("swift",      include_str!("../queries/swift.scm")),
    ("php",        include_str!("../queries/php.scm")),
    ("zig",        include_str!("../queries/zig.scm")),
];

type ScmSets = (HashSet<String>, HashSet<String>, HashSet<String>, HashSet<String>, HashSet<String>);

/// Parse a `.scm` source and collect node-kind names per tag category into
/// five `HashSet`s: (classes, functions, imports, calls, types).
///
/// Only the outermost node kind (first bare word inside the leading `(`) of
/// each S-expression line is collected; child field constraints are ignored.
fn parse_scm(scm: &str) -> ScmSets {
    let mut cls:  HashSet<String> = HashSet::new();
    let mut func: HashSet<String> = HashSet::new();
    let mut imp:  HashSet<String> = HashSet::new();
    let mut call: HashSet<String> = HashSet::new();
    let mut typ:  HashSet<String> = HashSet::new();

    let mut current_kind: Option<String> = None;

    for raw_line in scm.lines() {
        let line = raw_line.trim();

        if line.is_empty() || line.starts_with(';') {
            continue;
        }

        // Extract the outermost node kind when a new S-expression opens.
        if let Some(inner) = line.strip_prefix('(') {
            let kind_end = inner
                .find([')', ' '])
                .unwrap_or(inner.len());
            let kind = inner[..kind_end].trim();
            if !kind.is_empty() {
                current_kind = Some(kind.to_owned());
            }
        }

        // Route the pending kind to the matching set when a tag is found.
        if let Some(kind) = current_kind.take() {
            if line.contains("@definition.class") {
                cls.insert(kind);
            } else if line.contains("@definition.function") {
                func.insert(kind);
            } else if line.contains("@reference.import") {
                imp.insert(kind);
            } else if line.contains("@reference.call") {
                call.insert(kind);
            } else if line.contains("@definition.type") {
                typ.insert(kind);
            } else {
                // Tag not found on this line yet — put the kind back.
                current_kind = Some(kind);
            }
        }
    }

    (cls, func, imp, call, typ)
}

/// Pre-computed node-kind sets for a single language, derived from the
/// language's embedded `.scm` query file.  Passed by reference into the
/// recursive walk to avoid per-call allocations.
struct CompiledQueries {
    cls:  HashSet<String>,
    func: HashSet<String>,
    imp:  HashSet<String>,
    call: HashSet<String>,
    typ:  HashSet<String>,
}

impl CompiledQueries {
    fn from_scm(scm: &str) -> Self {
        let (cls, func, imp, call, typ) = parse_scm(scm);
        Self { cls, func, imp, call, typ }
    }

    #[inline] fn is_class(&self, kind: &str)    -> bool { self.cls.contains(kind)  }
    #[inline] fn is_func(&self, kind: &str)     -> bool { self.func.contains(kind) }
    #[inline] fn is_import(&self, kind: &str)   -> bool { self.imp.contains(kind)  }
    #[inline] fn is_call(&self, kind: &str)     -> bool { self.call.contains(kind) }
    #[inline] fn is_type(&self, kind: &str)     -> bool { self.typ.contains(kind)  }
}

// Lazily-built cache: one CompiledQueries per language, constructed at most once.
static LANG_TYPES_CACHE: OnceLock<HashMap<&'static str, CompiledQueries>> = OnceLock::new();

fn get_lang_types(language: &str) -> Option<&'static CompiledQueries> {
    let cache = LANG_TYPES_CACHE.get_or_init(|| {
        let mut m = HashMap::new();
        for &(lang, scm) in SCM_SOURCES {
            m.insert(lang, CompiledQueries::from_scm(scm));
        }
        m
    });
    cache.get(language)
}

// Cached Ruby import regex — compiled once, reused on every Ruby parse.
static RUBY_IMPORT_RE: OnceLock<regex::Regex> = OnceLock::new();

// ---------------------------------------------------------------------------
// Test detection patterns
// ---------------------------------------------------------------------------

const TEST_RUNNER_NAMES: &[&str] = &[
    "describe",
    "it",
    "test",
    "beforeEach",
    "afterEach",
    "beforeAll",
    "afterAll",
];

fn is_test_file(path: &str) -> bool {
    let p = path.replace('\\', "/");
    p.contains("test_")
        || p.contains("_test.")
        || p.contains(".test.")
        || p.contains(".spec.")
        || p.contains("_spec.")
        || p.contains("/tests/")
        || p.contains("/test/")
        || p.ends_with("_test.py")
        || p.ends_with("_test.go")
}

fn is_test_function(name: &str, file_path: &str) -> bool {
    if name.starts_with("test_") || name.starts_with("Test") {
        return true;
    }
    if is_test_file(file_path) && TEST_RUNNER_NAMES.contains(&name) {
        return true;
    }
    false
}

/// Returns `true` when a Rust function node has a `#[test]` or
/// `#[tokio::test]` attribute among its preceding siblings in the parent.
///
/// In tree-sitter-rust, `#[test]` is represented as an `attribute_item` node
/// that appears as a sibling just before the `function_item`.  We walk the
/// parent's children looking for `attribute_item` nodes that precede `fn_node`
/// and whose text contains "test".
fn rust_fn_has_test_attr(parent: &Node, fn_node: &Node, source: &[u8]) -> bool {
    let mut cur = parent.walk();
    let mut found_test_attr = false;
    for sibling in parent.children(&mut cur) {
        if sibling.id() == fn_node.id() {
            // We've reached the function itself — return whatever we found so far.
            return found_test_attr;
        }
        if sibling.kind() == "attribute_item" {
            let text = node_text(&sibling, source);
            // Match #[test] or #[tokio::test] or #[async_std::test] etc.
            if text.contains("test") {
                found_test_attr = true;
            } else {
                // Reset: a non-test attribute between the last test attr and
                // the fn means the test attr belongs to something else.
                found_test_attr = false;
            }
        } else if !sibling.is_extra() && sibling.kind() != "line_comment" && sibling.kind() != "block_comment" {
            // Any non-attribute, non-comment node resets the attribute window.
            found_test_attr = false;
        }
    }
    false
}

/// C# test framework attribute names that mark a method as a test.
const CSHARP_TEST_ATTRS: &[&str] = &[
    "Test", "Fact", "Theory", "TestMethod", "TestCase",
];

/// Returns `true` when a C# method node has a test framework attribute
/// (`[Test]`, `[Fact]`, `[Theory]`, `[TestMethod]`, `[TestCase]`).
///
/// In tree-sitter-c-sharp, `attribute_list` nodes are direct children of
/// `method_declaration`, containing `attribute` nodes with identifier names.
fn csharp_fn_has_test_attr(node: &Node, source: &[u8]) -> bool {
    let mut cur = node.walk();
    for child in node.children(&mut cur) {
        if child.kind() == "attribute_list" {
            let mut c2 = child.walk();
            for attr in child.children(&mut c2) {
                if attr.kind() == "attribute" {
                    let mut c3 = attr.walk();
                    for name_child in attr.children(&mut c3) {
                        if name_child.kind() == "identifier" {
                            let name = node_text(&name_child, source);
                            if CSHARP_TEST_ATTRS.contains(&name) {
                                return true;
                            }
                        }
                    }
                }
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------
// SHA-256 body hash
// ---------------------------------------------------------------------------
// Zig helpers
// ---------------------------------------------------------------------------

/// Zig: unwrap `const Foo = struct { ... }` patterns.
/// Returns `(name, inner_type_node, NodeKind)` if the variable_declaration
/// contains an anonymous type expression (struct/enum/union/opaque/error_set).
fn zig_unwrap_type_binding<'t>(
    var_decl: &Node<'t>,
    source: &[u8],
    lt: &CompiledQueries,
) -> Option<(String, Node<'t>, NodeKind)> {
    let mut name: Option<String> = None;
    let mut inner_type: Option<(Node<'t>, NodeKind)> = None;

    let mut cur = var_decl.walk();
    for child in var_decl.children(&mut cur) {
        if child.kind() == "identifier" && name.is_none() {
            name = Some(node_text(&child, source).to_owned());
        }
        if lt.is_class(child.kind()) {
            inner_type = Some((child, NodeKind::Class));
        } else if lt.is_type(child.kind()) {
            inner_type = Some((child, NodeKind::Type));
        }
    }
    let n = name?;
    let (inner, kind) = inner_type?;
    Some((n, inner, kind))
}

/// Zig: extract the name of a `test "description" { ... }` declaration.
fn zig_test_name(test_decl: &Node, source: &[u8]) -> String {
    let mut cur = test_decl.walk();
    for child in test_decl.children(&mut cur) {
        if child.kind() == "string" {
            let raw = node_text(&child, source);
            return raw.trim_matches('"').to_owned();
        }
        if child.kind() == "identifier" {
            return node_text(&child, source).to_owned();
        }
    }
    "<anonymous test>".to_owned()
}

/// Zig: extract `@import("path")` calls from variable_declarations.
/// `const std = @import("std");` → maps "std" to "std".
fn zig_collect_import(
    node: &Node,
    source: &[u8],
    import_map: &mut HashMap<String, String>,
) {
    if node.kind() != "variable_declaration" {
        return;
    }
    let mut var_name: Option<String> = None;
    let mut import_path: Option<String> = None;

    let mut cur = node.walk();
    for child in node.children(&mut cur) {
        if child.kind() == "identifier" && var_name.is_none() {
            var_name = Some(node_text(&child, source).to_owned());
        }
        if child.kind() == "builtin_function" || child.kind() == "call_expression" {
            let text = node_text(&child, source);
            if text.starts_with("@import") {
                // Extract the path from @import("path")
                if let Some(start) = text.find('"') {
                    if let Some(end) = text[start + 1..].find('"') {
                        import_path = Some(text[start + 1..start + 1 + end].to_owned());
                    }
                }
            }
        }
    }
    if let (Some(name), Some(path)) = (var_name, import_path) {
        import_map.insert(name, path);
    }
}

// ---------------------------------------------------------------------------

fn body_hash(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

// ---------------------------------------------------------------------------
// Helpers to extract text from AST nodes
// ---------------------------------------------------------------------------

fn node_text<'s>(node: &Node, source: &'s [u8]) -> &'s str {
    std::str::from_utf8(&source[node.byte_range()]).unwrap_or("")
}

// ---------------------------------------------------------------------------
// Name extraction
// ---------------------------------------------------------------------------

fn get_name(node: &Node, language: &str, kind: &str, source: &[u8]) -> Option<String> {
    // C/C++ functions: name is inside function_declarator or pointer_declarator
    if matches!(language, "c" | "cpp") && kind == "function" {
        let mut cur = node.walk();
        for child in node.children(&mut cur) {
            if matches!(child.kind(), "function_declarator" | "pointer_declarator") {
                if let Some(n) = get_name(&child, language, kind, source) {
                    return Some(n);
                }
            }
        }
    }

    // Rust impl_item: `impl Trait for Type` → name should be `Type`, not `Trait`.
    // For plain `impl Type`, name is `Type` (first type_identifier, no `for`).
    if language == "rust" && node.kind() == "impl_item" {
        return rust_impl_name(node, source);
    }

    // Most languages: first identifier child
    // field_identifier is used by tree-sitter-go 0.25 for method names in method_declaration
    let name_kinds = &[
        "identifier",
        "name",
        "type_identifier",
        "property_identifier",
        "simple_identifier",
        "constant",
        "field_identifier",
    ];
    let mut cur = node.walk();
    for child in node.children(&mut cur) {
        if name_kinds.contains(&child.kind()) {
            return Some(node_text(&child, source).to_owned());
        }
    }

    // Go type declarations: look for type_spec inside
    if language == "go" && node.kind() == "type_declaration" {
        let mut cur = node.walk();
        for child in node.children(&mut cur) {
            if child.kind() == "type_spec" {
                return get_name(&child, language, kind, source);
            }
        }
    }

    None
}

/// Rust: extract the correct name from an `impl_item`.
/// `impl Foo` → "Foo", `impl Display for Foo` → "Foo".
fn rust_impl_name(node: &Node, source: &[u8]) -> Option<String> {
    let mut cur = node.walk();
    let children: Vec<Node> = node.children(&mut cur).collect();

    // Look for `for` keyword — if present, take the type_identifier after it.
    let mut saw_for = false;
    for child in &children {
        if child.kind() == "for" {
            saw_for = true;
            continue;
        }
        if saw_for && matches!(child.kind(), "type_identifier" | "scoped_type_identifier" | "generic_type") {
            // For generic_type like `Foo<T>`, extract just the base name
            if child.kind() == "generic_type" {
                let mut c2 = child.walk();
                for gc in child.children(&mut c2) {
                    if matches!(gc.kind(), "type_identifier" | "scoped_type_identifier") {
                        return Some(node_text(&gc, source).to_owned());
                    }
                }
            }
            return Some(node_text(child, source).to_owned());
        }
    }

    // No `for` keyword → plain `impl Type`, take first type_identifier
    for child in &children {
        if matches!(child.kind(), "type_identifier" | "scoped_type_identifier" | "generic_type") {
            if child.kind() == "generic_type" {
                let mut c2 = child.walk();
                for gc in child.children(&mut c2) {
                    if matches!(gc.kind(), "type_identifier" | "scoped_type_identifier") {
                        return Some(node_text(&gc, source).to_owned());
                    }
                }
            }
            return Some(node_text(child, source).to_owned());
        }
    }

    None
}

/// Rust: extract the trait name from `impl Trait for Type`.
/// Returns `None` for plain `impl Type`.
fn rust_impl_trait_name(node: &Node, source: &[u8]) -> Option<String> {
    let mut cur = node.walk();
    let children: Vec<Node> = node.children(&mut cur).collect();

    // Check if there's a `for` keyword — if so, the type before it is the trait.
    let has_for = children.iter().any(|c| c.kind() == "for");
    if !has_for {
        return None;
    }

    for child in &children {
        if child.kind() == "for" {
            break;
        }
        if matches!(child.kind(), "type_identifier" | "scoped_type_identifier" | "generic_type") {
            if child.kind() == "generic_type" {
                let mut c2 = child.walk();
                for gc in child.children(&mut c2) {
                    if matches!(gc.kind(), "type_identifier" | "scoped_type_identifier") {
                        return Some(node_text(&gc, source).to_owned());
                    }
                }
            }
            return Some(node_text(child, source).to_owned());
        }
    }
    None
}

/// Rust: collect names from `use` declarations into the import map.
/// Handles simple paths, braced groups, and aliases.
fn rust_collect_use_names(
    node: &Node,
    source: &[u8],
    import_map: &mut HashMap<String, String>,
) {
    // Walk the use_declaration children to find scoped_identifier, use_list, use_as_clause, etc.
    let text = node_text(node, source).trim().to_owned();

    // Handle `use foo::bar as alias;`
    if text.contains(" as ") {
        let trimmed = text.trim_start_matches("pub ")
            .trim_start_matches("use ")
            .trim_end_matches(';');
        if let Some(as_pos) = trimmed.find(" as ") {
            let path = &trimmed[..as_pos];
            let alias = trimmed[as_pos + 4..].trim();
            let module = path.rsplit_once("::").map_or(path, |(m, _)| m);
            import_map.insert(alias.to_owned(), module.to_owned());
            return;
        }
    }

    // Handle `use foo::bar::{Baz, Qux};`
    if text.contains('{') {
        let trimmed = text.trim_start_matches("pub ")
            .trim_start_matches("use ")
            .trim_end_matches(';');
        if let Some(brace_pos) = trimmed.find('{') {
            let base = trimmed[..brace_pos].trim_end_matches("::");
            let inner = &trimmed[brace_pos + 1..];
            let inner = inner.trim_end_matches('}');
            for item in inner.split(',') {
                let item = item.trim();
                if item.is_empty() || item == "*" || item == "self" {
                    continue;
                }
                // Handle `Foo as Bar` inside braces
                if let Some(as_pos) = item.find(" as ") {
                    let alias = item[as_pos + 4..].trim();
                    import_map.insert(alias.to_owned(), base.to_owned());
                } else {
                    import_map.insert(item.to_owned(), base.to_owned());
                }
            }
            return;
        }
    }

    // Simple: `use std::collections::HashMap;`
    let trimmed = text.trim_start_matches("pub ")
        .trim_start_matches("use ")
        .trim_end_matches(';')
        .trim();
    if trimmed.contains("::") && !trimmed.contains('*') {
        if let Some((module, name)) = trimmed.rsplit_once("::") {
            import_map.insert(name.to_owned(), module.to_owned());
        }
    }
}

// ---------------------------------------------------------------------------
// Parameter extraction
// ---------------------------------------------------------------------------

fn get_params(node: &Node, source: &[u8]) -> String {
    let mut cur = node.walk();
    for child in node.children(&mut cur) {
        if matches!(child.kind(), "parameters" | "formal_parameters" | "parameter_list") {
            return node_text(&child, source).to_owned();
        }
    }
    String::new()
}

// ---------------------------------------------------------------------------
// Return-type extraction
// ---------------------------------------------------------------------------

fn get_return_type(node: &Node, language: &str, source: &[u8]) -> String {
    let mut cur = node.walk();
    let mut iter = node.children(&mut cur).peekable();
    while let Some(child) = iter.next() {
        if matches!(
            child.kind(),
            "type" | "return_type" | "type_annotation" | "return_type_definition"
        ) {
            return node_text(&child, source).to_owned();
        }
        // Python: -> annotation (next sibling is the return type)
        if language == "python" && child.kind() == "->" {
            if let Some(next) = iter.peek() {
                return node_text(next, source).to_owned();
            }
        }
    }
    String::new()
}

// ---------------------------------------------------------------------------
// Docstring / signature extraction
// ---------------------------------------------------------------------------

fn get_docstring(node: &Node, language: &str, source: &[u8]) -> String {
    // Python: first expression_statement with a string literal in the body
    if language == "python" {
        let mut cur = node.walk();
        for child in node.children(&mut cur) {
            if child.kind() == "block" {
                let first_stmt: Option<tree_sitter::Node> = {
                    let mut c2 = child.walk();
                    let first = child.children(&mut c2).next();
                    first
                };
                if let Some(stmt) = first_stmt {
                    if stmt.kind() == "expression_statement" {
                        let mut c3 = stmt.walk();
                        for expr in stmt.children(&mut c3) {
                            if matches!(expr.kind(), "string" | "concatenated_string") {
                                let t = node_text(&expr, source);
                                return t.trim_matches(|c| c == '"' || c == '\'').to_owned();
                            }
                        }
                    }
                }
            }
        }
    }
    String::new()
}

fn get_signature(node: &Node, language: &str, source: &[u8]) -> String {
    let params = get_params(node, source);
    let ret = get_return_type(node, language, source);
    if ret.is_empty() {
        params
    } else {
        format!("{params} -> {ret}")
    }
}

// ---------------------------------------------------------------------------
// Inheritance / base class extraction
// ---------------------------------------------------------------------------

fn get_bases(node: &Node, language: &str, source: &[u8]) -> Vec<String> {
    let mut bases = Vec::new();
    let mut cur = node.walk();
    match language {
        "python" => {
            for child in node.children(&mut cur) {
                if child.kind() == "argument_list" {
                    let mut c2 = child.walk();
                    for arg in child.children(&mut c2) {
                        if matches!(arg.kind(), "identifier" | "attribute") {
                            bases.push(node_text(&arg, source).to_owned());
                        }
                    }
                }
            }
        }
        "java" | "kotlin" => {
            for child in node.children(&mut cur) {
                if matches!(
                    child.kind(),
                    "superclass"
                        | "super_interfaces"
                        | "extends_type"
                        | "implements_type"
                        | "type_identifier"
                        | "supertype"
                        | "delegation_specifier"
                ) {
                    bases.push(node_text(&child, source).to_owned());
                }
            }
        }
        "csharp" => {
            for child in node.children(&mut cur) {
                if child.kind() == "base_list" {
                    let mut c2 = child.walk();
                    for sub in child.children(&mut c2) {
                        if matches!(sub.kind(), "identifier" | "generic_name" | "qualified_name") {
                            bases.push(node_text(&sub, source).to_owned());
                        }
                    }
                }
            }
        }
        "cpp" => {
            for child in node.children(&mut cur) {
                if child.kind() == "base_class_clause" {
                    let mut c2 = child.walk();
                    for sub in child.children(&mut c2) {
                        if sub.kind() == "type_identifier" {
                            bases.push(node_text(&sub, source).to_owned());
                        }
                    }
                }
            }
        }
        "javascript" | "typescript" | "tsx" => {
            for child in node.children(&mut cur) {
                if matches!(child.kind(), "extends_clause" | "implements_clause" | "class_heritage") {
                    let mut c2 = child.walk();
                    for sub in child.children(&mut c2) {
                        if matches!(
                            sub.kind(),
                            "identifier" | "type_identifier" | "nested_identifier"
                        ) {
                            bases.push(node_text(&sub, source).to_owned());
                        }
                    }
                }
            }
        }
        "go" => {
            for child in node.children(&mut cur) {
                if child.kind() == "type_spec" {
                    let mut c2 = child.walk();
                    for sub in child.children(&mut c2) {
                        if matches!(sub.kind(), "struct_type" | "interface_type") {
                            let mut c3 = sub.walk();
                            for field_list in sub.children(&mut c3) {
                                if field_list.kind() == "field_declaration_list" {
                                    let mut c4 = field_list.walk();
                                    for f in field_list.children(&mut c4) {
                                        if f.kind() == "type_identifier" {
                                            bases.push(node_text(&f, source).to_owned());
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        "rust" => {
            // `impl Trait for Type` → trait is the "base" (Inherits edge).
            if node.kind() == "impl_item" {
                if let Some(trait_name) = rust_impl_trait_name(node, source) {
                    bases.push(trait_name);
                }
            }
        }
        _ => {}
    }
    bases
}

// ---------------------------------------------------------------------------
// Import extraction
// ---------------------------------------------------------------------------

fn extract_imports(node: &Node, language: &str, source: &[u8]) -> Vec<String> {
    let mut imports = Vec::new();
    let text = node_text(node, source);

    match language {
        "python" => {
            if node.kind() == "import_from_statement" {
                let mut cur = node.walk();
                for child in node.children(&mut cur) {
                    if child.kind() == "dotted_name" {
                        imports.push(node_text(&child, source).to_owned());
                        break;
                    }
                }
            } else {
                let mut cur = node.walk();
                for child in node.children(&mut cur) {
                    if child.kind() == "dotted_name" {
                        imports.push(node_text(&child, source).to_owned());
                    }
                }
            }
        }
        "javascript" | "typescript" | "tsx" => {
            let mut cur = node.walk();
            for child in node.children(&mut cur) {
                if child.kind() == "string" {
                    let val = node_text(&child, source).trim_matches(|c| c == '\'' || c == '"');
                    imports.push(val.to_owned());
                }
            }
        }
        "go" => {
            let mut cur = node.walk();
            for child in node.children(&mut cur) {
                if child.kind() == "import_spec_list" {
                    let mut c2 = child.walk();
                    for spec in child.children(&mut c2) {
                        if spec.kind() == "import_spec" {
                            let mut c3 = spec.walk();
                            for s in spec.children(&mut c3) {
                                if s.kind() == "interpreted_string_literal" {
                                    let val = node_text(&s, source).trim_matches('"');
                                    imports.push(val.to_owned());
                                }
                            }
                        }
                    }
                } else if child.kind() == "import_spec" {
                    let mut c2 = child.walk();
                    for s in child.children(&mut c2) {
                        if s.kind() == "interpreted_string_literal" {
                            let val = node_text(&s, source).trim_matches('"');
                            imports.push(val.to_owned());
                        }
                    }
                }
            }
        }
        "rust" => {
            let cleaned = text
                .trim_start_matches("use ")
                .trim_end_matches(';')
                .trim();
            imports.push(cleaned.to_owned());
        }
        "c" | "cpp" => {
            let mut cur = node.walk();
            for child in node.children(&mut cur) {
                if matches!(child.kind(), "system_lib_string" | "string_literal") {
                    let val = node_text(&child, source).trim_matches(|c| c == '<' || c == '>' || c == '"');
                    imports.push(val.to_owned());
                }
            }
        }
        "java" => {
            let parts: Vec<&str> = text.split_whitespace().collect();
            if parts.len() >= 2 {
                imports.push(parts.last().unwrap().trim_end_matches(';').to_owned());
            }
        }
        "csharp" => {
            let mut cur = node.walk();
            for child in node.children(&mut cur) {
                match child.kind() {
                    "qualified_name" | "identifier" => {
                        imports.push(node_text(&child, source).to_owned());
                    }
                    _ => {}
                }
            }
        }
        "ruby" => {
            if text.contains("require") {
                let re = RUBY_IMPORT_RE.get_or_init(|| {
                    regex::Regex::new(r#"['"](.+?)['"]"#).unwrap()
                });
                if let Some(cap) = re.captures(text) {
                    imports.push(cap[1].to_owned());
                }
            }
        }
        "zig" => {
            // `usingnamespace` imports: extract the expression text
            imports.push(text.to_owned());
        }
        _ => {
            imports.push(text.to_owned());
        }
    }
    imports
}

// ---------------------------------------------------------------------------
// Import name collection (for call resolution)
// ---------------------------------------------------------------------------

fn collect_import_names(
    node: &Node,
    language: &str,
    source: &[u8],
    import_map: &mut HashMap<String, String>,
) {
    match language {
        "python" => {
            if node.kind() == "import_from_statement" {
                let mut module: Option<String> = None;
                let mut seen_import_kw = false;
                let mut cur = node.walk();
                for child in node.children(&mut cur) {
                    match child.kind() {
                        "dotted_name" if !seen_import_kw => {
                            module = Some(node_text(&child, source).to_owned());
                        }
                        "import" => seen_import_kw = true,
                        "identifier" | "dotted_name" if seen_import_kw => {
                            if let Some(ref m) = module {
                                import_map
                                    .insert(node_text(&child, source).to_owned(), m.clone());
                            }
                        }
                        "aliased_import" if seen_import_kw => {
                            if let Some(ref m) = module {
                                let mut c2 = child.walk();
                                let names: Vec<String> = child
                                    .children(&mut c2)
                                    .filter(|n| matches!(n.kind(), "identifier" | "dotted_name"))
                                    .map(|n| node_text(&n, source).to_owned())
                                    .collect();
                                if let Some(last) = names.last() {
                                    import_map.insert(last.clone(), m.clone());
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        "javascript" | "typescript" | "tsx" => {
            let mut module: Option<String> = None;
            let mut cur = node.walk();
            for child in node.children(&mut cur) {
                if child.kind() == "string" {
                    module =
                        Some(node_text(&child, source).trim_matches(|c| c == '\'' || c == '"').to_owned());
                }
            }
            if let Some(m) = module {
                let mut cur = node.walk();
                for child in node.children(&mut cur) {
                    if child.kind() == "import_clause" {
                        collect_js_import_names(&child, &m, source, import_map);
                    }
                }
            }
        }
        "rust" => {
            // `use std::collections::HashMap;` → maps "HashMap" → "std::collections"
            // `use foo::bar::{Baz, Qux};` → maps "Baz" → "foo::bar", "Qux" → "foo::bar"
            // `use foo::bar as alias;` → maps "alias" → "foo::bar"
            rust_collect_use_names(node, source, import_map);
        }
        "zig" => {
            // Zig imports are `const x = @import("path");` which are
            // variable_declarations, not a dedicated import node.
            // We handle them in collect_file_scope instead.
        }
        _ => {}
    }
}

fn collect_js_import_names(
    clause: &Node,
    module: &str,
    source: &[u8],
    import_map: &mut HashMap<String, String>,
) {
    let mut cur = clause.walk();
    for child in clause.children(&mut cur) {
        if child.kind() == "identifier" {
            import_map.insert(node_text(&child, source).to_owned(), module.to_owned());
        } else if child.kind() == "named_imports" {
            let mut c2 = child.walk();
            for spec in child.children(&mut c2) {
                if spec.kind() == "import_specifier" {
                    let mut c3 = spec.walk();
                    let names: Vec<String> = spec
                        .children(&mut c3)
                        .filter(|n| matches!(n.kind(), "identifier" | "property_identifier"))
                        .map(|n| node_text(&n, source).to_owned())
                        .collect();
                    if let Some(last) = names.last() {
                        import_map.insert(last.clone(), module.to_owned());
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Call-name extraction
// ---------------------------------------------------------------------------

fn get_call_name(node: &Node, source: &[u8]) -> Option<String> {
    // Zig builtin_function: `@import(...)` — the node itself contains
    // a builtin_identifier child (`@import`).
    if node.kind() == "builtin_function" {
        let mut cur = node.walk();
        for child in node.children(&mut cur) {
            if child.kind() == "builtin_identifier" {
                return Some(node_text(&child, source).to_owned());
            }
        }
        // Fallback: extract @name from the text
        let text = node_text(node, source);
        if let Some(paren) = text.find('(') {
            return Some(text[..paren].to_owned());
        }
        return Some(text.to_owned());
    }

    let mut cur = node.walk();
    let first = node.children(&mut cur).next()?;

    if first.kind() == "identifier" {
        return Some(node_text(&first, source).to_owned());
    }

    let member_types = &[
        "attribute",
        "member_expression",
        "member_access_expression",
        "field_expression",
        "field_access",       // Zig: foo.bar()
        "selector_expression",
    ];
    if member_types.contains(&first.kind()) {
        let mut c2 = first.walk();
        let children: Vec<Node> = first.children(&mut c2).collect();
        for child in children.iter().rev() {
            if matches!(
                child.kind(),
                "identifier" | "property_identifier" | "field_identifier" | "field_name"
            ) {
                return Some(node_text(child, source).to_owned());
            }
        }
        return Some(node_text(&first, source).to_owned());
    }

    if matches!(first.kind(), "scoped_identifier" | "qualified_name") {
        return Some(node_text(&first, source).to_owned());
    }

    None
}

// ---------------------------------------------------------------------------
// Qualified name
// ---------------------------------------------------------------------------

fn qualify(name: &str, file_path: &str, enclosing_class: Option<&str>, namespace: Option<&str>) -> String {
    match (namespace, enclosing_class) {
        (Some(ns), Some(cls)) => format!("{file_path}::{ns}.{cls}.{name}"),
        (Some(ns), None) => format!("{file_path}::{ns}.{name}"),
        (None, Some(cls)) => format!("{file_path}::{cls}.{name}"),
        (None, None) => format!("{file_path}::{name}"),
    }
}

// ---------------------------------------------------------------------------
// Resolve call target
// ---------------------------------------------------------------------------

fn resolve_call_target(
    call_name: &str,
    file_path: &str,
    import_map: &HashMap<String, String>,
    defined_names: &HashSet<String>,
) -> String {
    if defined_names.contains(call_name) {
        return qualify(call_name, file_path, None, None);
    }
    if let Some(module) = import_map.get(call_name) {
        // Best-effort: qualify against the module path
        return qualify(call_name, module, None, None);
    }
    call_name.to_owned()
}

// ---------------------------------------------------------------------------
// Pre-scan (collect_file_scope)
// ---------------------------------------------------------------------------

fn collect_file_scope(
    root: &Node,
    lt: &CompiledQueries,
    language: &str,
    source: &[u8],
) -> (HashMap<String, String>, HashSet<String>) {
    let mut import_map: HashMap<String, String> = HashMap::new();
    let mut defined_names: HashSet<String> = HashSet::new();

    const DECORATOR_WRAPPERS: &[&str] = &["decorated_definition", "decorator"];

    let mut cur = root.walk();
    for child in root.children(&mut cur) {
        let node_type = child.kind();

        // Collect children into a Vec to avoid cursor lifetime issues.
        let children_of_child: Vec<Node> = {
            let mut c2 = child.walk();
            child.children(&mut c2).collect()
        };

        // Unwrap decorator wrappers: find inner def node by kind string.
        let inner_kind: Option<String> = if DECORATOR_WRAPPERS.contains(&node_type) {
            children_of_child
                .iter()
                .find(|n| lt.is_func(n.kind()) || lt.is_class(n.kind()))
                .map(|n| n.kind().to_owned())
        } else {
            None
        };
        let effective_kind: &str = inner_kind.as_deref().unwrap_or(node_type);

        if lt.is_func(effective_kind) || lt.is_class(effective_kind) {
            let kind_str = if lt.is_class(effective_kind) { "class" } else { "function" };
            let name = if inner_kind.is_some() {
                children_of_child
                    .iter()
                    .find(|n| n.kind() == effective_kind)
                    .and_then(|n| get_name(n, language, kind_str, source))
            } else {
                get_name(&child, language, kind_str, source)
            };
            if let Some(n) = name {
                defined_names.insert(n);
            }
        }

        if lt.is_import(node_type) {
            collect_import_names(&child, language, source, &mut import_map);
        }
    }

    // For Zig, scan top-level variable_declarations for @import bindings
    // and `const Foo = struct { ... }` type bindings.
    if language == "zig" {
        let mut zig_cur = root.walk();
        for child in root.children(&mut zig_cur) {
            if child.kind() == "variable_declaration" {
                // Collect @import bindings
                zig_collect_import(&child, source, &mut import_map);
                // Collect type bindings (const Foo = struct { ... })
                if let Some((name, _, _)) = zig_unwrap_type_binding(&child, source, lt) {
                    defined_names.insert(name);
                }
            }
        }
    }

    // For C#, also scan one level into namespace declarations to find
    // classes/functions that are not direct children of the root.
    if language == "csharp" {
        let mut ns_cur = root.walk();
        for ns_child in root.children(&mut ns_cur) {
            if matches!(ns_child.kind(), "namespace_declaration" | "file_scoped_namespace_declaration") {
                let mut c2 = ns_child.walk();
                for inner in ns_child.children(&mut c2) {
                    if inner.kind() == "declaration_list" {
                        let mut c3 = inner.walk();
                        for decl in inner.children(&mut c3) {
                            let nt = decl.kind();
                            if lt.is_func(nt) || lt.is_class(nt) || lt.is_type(nt) {
                                if let Some(name) = get_name(&decl, language, "class", source) {
                                    defined_names.insert(name);
                                }
                            }
                            if lt.is_import(nt) {
                                collect_import_names(&decl, language, source, &mut import_map);
                            }
                        }
                    }
                    // file_scoped_namespace_declaration has classes as direct children
                    let nt = inner.kind();
                    if lt.is_func(nt) || lt.is_class(nt) || lt.is_type(nt) {
                        if let Some(name) = get_name(&inner, language, "class", source) {
                            defined_names.insert(name);
                        }
                    }
                }
            }
        }
    }

    // For Rust, scan one level into `mod` blocks to find definitions
    // that are not direct children of the root.
    if language == "rust" {
        let mut mod_cur = root.walk();
        for child in root.children(&mut mod_cur) {
            if child.kind() == "mod_item" {
                let mut c2 = child.walk();
                for inner in child.children(&mut c2) {
                    if inner.kind() == "declaration_list" {
                        let mut c3 = inner.walk();
                        for decl in inner.children(&mut c3) {
                            let nt = decl.kind();
                            if lt.is_func(nt) || lt.is_class(nt) || lt.is_type(nt) {
                                if let Some(name) = get_name(&decl, language, "class", source) {
                                    defined_names.insert(name);
                                }
                            }
                            if lt.is_import(nt) {
                                collect_import_names(&decl, language, source, &mut import_map);
                            }
                        }
                    }
                }
            }
        }
    }

    (import_map, defined_names)
}

// ---------------------------------------------------------------------------
// Recursive AST walk
// ---------------------------------------------------------------------------

const MAX_AST_DEPTH: usize = 180;

/// Walk context passed by reference through the recursive AST traversal,
/// replacing the previous 12-parameter signature.
struct WalkCtx<'a> {
    source: &'a [u8],
    language: &'a str,
    file_path: &'a str,
    import_map: &'a HashMap<String, String>,
    defined_names: &'a HashSet<String>,
    lt: &'a CompiledQueries,
}

fn extract_from_tree(
    root: &Node,
    ctx: &WalkCtx<'_>,
    nodes: &mut Vec<NodeInfo>,
    edges: &mut Vec<EdgeInfo>,
    enclosing_class: Option<&str>,
    enclosing_func: Option<&str>,
    enclosing_namespace: Option<&str>,
    depth: usize,
) {
    if depth > MAX_AST_DEPTH {
        return;
    }

    let WalkCtx { source, language, file_path, import_map, defined_names, lt } = ctx;

    // Track the active namespace. For file-scoped namespace declarations
    // (C# `namespace X.Y;`), the classes are siblings rather than children,
    // so we set the namespace here and let subsequent iterations pick it up.
    let mut active_ns: Option<String> = enclosing_namespace.map(|s| s.to_owned());

    let mut cur = root.walk();
    for child in root.children(&mut cur) {
        let node_type = child.kind();

        // --- C# Namespaces ---
        if *language == "csharp"
            && matches!(node_type, "namespace_declaration" | "file_scoped_namespace_declaration")
        {
            let ns_name = {
                let mut c2 = child.walk();
                let found = child
                    .children(&mut c2)
                    .find(|c| matches!(c.kind(), "qualified_name" | "identifier"))
                    .map(|c| node_text(&c, source).to_owned());
                found
            };
            if let Some(ref ns) = ns_name {
                let full_ns = match active_ns.as_deref() {
                    Some(outer) => format!("{outer}.{ns}"),
                    None => ns.clone(),
                };
                if node_type == "file_scoped_namespace_declaration" {
                    // File-scoped: classes are siblings, not children.
                    // Set the namespace for subsequent loop iterations.
                    active_ns = Some(full_ns);
                } else {
                    // Block-scoped: classes are children of this node.
                    // Recurse into the namespace with the full namespace set.
                    extract_from_tree(
                        &child, ctx, nodes, edges, enclosing_class, enclosing_func,
                        Some(&full_ns), depth + 1,
                    );
                }
                continue;
            }
        }

        // --- Rust: mod blocks as namespaces ---
        // `mod foo { fn bar() {} }` → functions inside get qualified as `file::foo.bar`
        // `pub mod foo;` (no body) → treated as external module reference
        if *language == "rust" && node_type == "mod_item" {
            let mod_name = {
                let mut c2 = child.walk();
                let found = child.children(&mut c2)
                    .find(|c| c.kind() == "identifier")
                    .map(|c| node_text(&c, source).to_owned());
                found
            };
            if let Some(ref name) = mod_name {
                // Check if this mod has a body (declaration_list)
                let has_body = {
                    let mut c2 = child.walk();
                    let result = child.children(&mut c2).any(|c| c.kind() == "declaration_list");
                    result
                };
                if has_body {
                    // Block mod: recurse into the body with namespace set
                    let full_ns = match active_ns.as_deref() {
                        Some(outer) => format!("{outer}.{name}"),
                        None => name.clone(),
                    };
                    extract_from_tree(
                        &child, ctx, nodes, edges, enclosing_class, enclosing_func,
                        Some(&full_ns), depth + 1,
                    );
                } else {
                    // `pub mod foo;` → external module reference, emit ImportsFrom edge
                    edges.push(EdgeInfo {
                        source_qualified: file_path.to_string(),
                        target_qualified: name.clone(),
                        kind: EdgeKind::ImportsFrom,
                        file_path: file_path.to_string(),
                        line: child.start_position().row + 1,
                    });
                }
                continue;
            }
        }

        // --- TypeScript: namespace/module blocks as namespaces ---
        // `namespace Foo { ... }` or `module Foo { ... }` in .d.ts files
        if matches!(*language, "typescript" | "tsx")
            && matches!(node_type, "module" | "internal_module")
        {
            let ns_name = {
                let mut c2 = child.walk();
                let found = child.children(&mut c2)
                    .find(|c| matches!(c.kind(), "identifier" | "nested_identifier" | "string"))
                    .map(|c| {
                        let t = node_text(&c, source);
                        t.trim_matches(|ch| ch == '"' || ch == '\'').to_owned()
                    });
                found
            };
            if let Some(ref name) = ns_name {
                let full_ns = match active_ns.as_deref() {
                    Some(outer) => format!("{outer}.{name}"),
                    None => name.clone(),
                };
                extract_from_tree(
                    &child, ctx, nodes, edges, enclosing_class, enclosing_func,
                    Some(&full_ns), depth + 1,
                );
                continue;
            }
        }

        let enc_ns = active_ns.as_deref();

        // --- Zig: variable_declaration containing @import or anonymous types ---
        if *language == "zig" && node_type == "variable_declaration" {
            // Check for `const std = @import("std");` → emit ImportsFrom edge
            let child_text = node_text(&child, source);
            if child_text.contains("@import") {
                // Extract the import path from `@import("...")`
                if let Some(start) = child_text.find("@import(\"") {
                    let after = &child_text[start + 9..];
                    if let Some(end) = after.find('"') {
                        let import_path = &after[..end];
                        edges.push(EdgeInfo {
                            source_qualified: file_path.to_string(),
                            target_qualified: import_path.to_string(),
                            kind: EdgeKind::ImportsFrom,
                            file_path: file_path.to_string(),
                            line: child.start_position().row + 1,
                        });
                    }
                }
                continue;
            }

            // `const Foo = struct { ... }` → variable_declaration contains
            // struct_declaration as a child.  The name comes from the variable,
            // not the (anonymous) type expression.
            if let Some(zig_res) = zig_unwrap_type_binding(&child, source, lt) {
                let (var_name, inner_node, node_kind) = zig_res;
                let line_start = child.start_position().row + 1;
                let line_end = child.end_position().row + 1;
                let qualified = qualify(&var_name, file_path, enclosing_class, enc_ns);

                nodes.push(NodeInfo {
                    name: var_name.clone(),
                    qualified_name: qualified.clone(),
                    kind: node_kind,
                    file_path: file_path.to_string(),
                    line_start,
                    line_end,
                    language: language.to_string(),
                    is_test: false,
                    docstring: String::new(),
                    signature: String::new(),
                    body_hash: body_hash(&source[child.byte_range()]),
                });

                edges.push(EdgeInfo {
                    source_qualified: file_path.to_string(),
                    target_qualified: qualified.clone(),
                    kind: EdgeKind::Contains,
                    file_path: file_path.to_string(),
                    line: line_start,
                });

                // Recurse into the inner type body (struct fields, methods, etc.)
                extract_from_tree(
                    &inner_node, ctx, nodes, edges,
                    Some(&var_name), enclosing_func, enc_ns, depth + 1,
                );
                continue;
            }
        }

        // --- Zig: test declarations ---
        // `test "description" { ... }` are test_declaration nodes.
        if *language == "zig" && node_type == "test_declaration" {
            let test_name = zig_test_name(&child, source);
            let qualified = qualify(&test_name, file_path, enclosing_class, enc_ns);
            let line_start = child.start_position().row + 1;
            let line_end = child.end_position().row + 1;

            nodes.push(NodeInfo {
                name: test_name.clone(),
                qualified_name: qualified.clone(),
                kind: NodeKind::Test,
                file_path: file_path.to_string(),
                line_start,
                line_end,
                language: language.to_string(),
                is_test: true,
                docstring: String::new(),
                signature: String::new(),
                body_hash: body_hash(&source[child.byte_range()]),
            });

            let container = match enclosing_func {
                Some(f) => qualify(f, file_path, enclosing_class, enc_ns),
                None => file_path.to_string(),
            };
            edges.push(EdgeInfo {
                source_qualified: container,
                target_qualified: qualified.clone(),
                kind: EdgeKind::Contains,
                file_path: file_path.to_string(),
                line: line_start,
            });

            extract_from_tree(
                &child, ctx, nodes, edges, enclosing_class, Some(&test_name), enc_ns, depth + 1,
            );
            continue;
        }

        // --- Classes ---
        if lt.is_class(node_type) {
            if let Some(name) = get_name(&child, language, "class", source) {
                let line_start = child.start_position().row + 1;
                let line_end = child.end_position().row + 1;
                let qualified = qualify(&name, file_path, enclosing_class, enc_ns);

                nodes.push(NodeInfo {
                    name: name.clone(),
                    qualified_name: qualified.clone(),
                    kind: NodeKind::Class,
                    file_path: file_path.to_string(),
                    line_start,
                    line_end,
                    language: language.to_string(),
                    is_test: false,
                    docstring: String::new(),
                    signature: String::new(),
                    body_hash: body_hash(&source[child.byte_range()]),
                });

                edges.push(EdgeInfo {
                    source_qualified: file_path.to_string(),
                    target_qualified: qualified.clone(),
                    kind: EdgeKind::Contains,
                    file_path: file_path.to_string(),
                    line: line_start,
                });

                // Inheritance edges
                for base in get_bases(&child, language, source) {
                    edges.push(EdgeInfo {
                        source_qualified: qualified.clone(),
                        target_qualified: base,
                        kind: EdgeKind::Inherits,
                        file_path: file_path.to_string(),
                        line: line_start,
                    });
                }

                extract_from_tree(&child, ctx, nodes, edges, Some(&name), None, enc_ns, depth + 1);
                continue;
            }
        }

        // --- Types (interfaces, type aliases) ---
        if lt.is_type(node_type) {
            if let Some(name) = get_name(&child, language, "type", source) {
                let line_start = child.start_position().row + 1;
                let line_end = child.end_position().row + 1;
                let qualified = qualify(&name, file_path, enclosing_class, enc_ns);

                nodes.push(NodeInfo {
                    name: name.clone(),
                    qualified_name: qualified.clone(),
                    kind: NodeKind::Type,
                    file_path: file_path.to_string(),
                    line_start,
                    line_end,
                    language: language.to_string(),
                    is_test: false,
                    docstring: String::new(),
                    signature: String::new(),
                    body_hash: body_hash(&source[child.byte_range()]),
                });

                let container = match enclosing_class {
                    Some(cls) => qualify(cls, file_path, None, enc_ns),
                    None => file_path.to_string(),
                };
                edges.push(EdgeInfo {
                    source_qualified: container,
                    target_qualified: qualified.clone(),
                    kind: EdgeKind::Contains,
                    file_path: file_path.to_string(),
                    line: line_start,
                });

                // For Rust trait declarations, recurse into the body to find
                // function signatures and default method implementations.
                if node_type == "trait_item" {
                    extract_from_tree(
                        &child, ctx, nodes, edges, Some(&name), None, enc_ns, depth + 1,
                    );
                }

                // For interface declarations, index direct members (property signatures, methods).
                if node_type == "interface_declaration" {
                    // Body container varies by language: interface_body (TS), object_type (TS),
                    // declaration_list (C#)
                    let mut outer_cur = child.walk();
                    for body_child in child.children(&mut outer_cur) {
                        if body_child.kind() == "interface_body" || body_child.kind() == "object_type" || body_child.kind() == "declaration_list" {
                            let mut body_cur = body_child.walk();
                            for member in body_child.children(&mut body_cur) {
                                if member.kind() == "property_signature" || member.kind() == "method_declaration" {
                                    let prop_name = get_name(&member, language, "type", source)
                                        .unwrap_or_default();
                                    if prop_name.is_empty() {
                                        continue;
                                    }
                                    let prop_qn = format!("{qualified}.{prop_name}");
                                    let prop_sig = node_text(&member, source).to_string();

                                    nodes.push(NodeInfo {
                                        name: prop_name,
                                        qualified_name: prop_qn.clone(),
                                        kind: NodeKind::Type,
                                        file_path: file_path.to_string(),
                                        line_start: member.start_position().row + 1,
                                        line_end: member.end_position().row + 1,
                                        language: language.to_string(),
                                        is_test: false,
                                        docstring: String::new(),
                                        signature: prop_sig,
                                        body_hash: body_hash(&source[member.byte_range()]),
                                    });

                                    edges.push(EdgeInfo {
                                        source_qualified: qualified.clone(),
                                        target_qualified: prop_qn,
                                        kind: EdgeKind::Contains,
                                        file_path: file_path.to_string(),
                                        line: member.start_position().row + 1,
                                    });
                                }
                            }
                        }
                    }
                }

                continue;
            }
        }

        // --- Functions ---
        if lt.is_func(node_type) {
            if let Some(name) = get_name(&child, language, "function", source) {
                let has_test_attr = match *language {
                    "rust" => rust_fn_has_test_attr(root, &child, source),
                    "csharp" => csharp_fn_has_test_attr(&child, source),
                    _ => false,
                };
                let is_test = has_test_attr || is_test_function(&name, file_path);
                let kind = if is_test { NodeKind::Test } else { NodeKind::Function };
                let qualified = qualify(&name, file_path, enclosing_class, enc_ns);
                let line_start = child.start_position().row + 1;
                let line_end = child.end_position().row + 1;
                let sig = get_signature(&child, language, source);
                let doc = get_docstring(&child, language, source);

                nodes.push(NodeInfo {
                    name: name.clone(),
                    qualified_name: qualified.clone(),
                    kind,
                    file_path: file_path.to_string(),
                    line_start,
                    line_end,
                    language: language.to_string(),
                    is_test,
                    docstring: doc,
                    signature: sig,
                    body_hash: body_hash(&source[child.byte_range()]),
                });

                let container = match enclosing_class {
                    Some(cls) => qualify(cls, file_path, None, enc_ns),
                    None => file_path.to_string(),
                };
                edges.push(EdgeInfo {
                    source_qualified: container,
                    target_qualified: qualified.clone(),
                    kind: EdgeKind::Contains,
                    file_path: file_path.to_string(),
                    line: line_start,
                });

                extract_from_tree(&child, ctx, nodes, edges, enclosing_class, Some(&name), enc_ns, depth + 1);
                continue;
            }
        }

        // --- Imports ---
        if lt.is_import(node_type) {
            // Ruby: `call` is also the call_type; only emit import for "require" calls.
            let is_ruby_require = *language != "ruby" || node_text(&child, source).contains("require");
            if is_ruby_require {
                for target in extract_imports(&child, language, source) {
                    edges.push(EdgeInfo {
                        source_qualified: file_path.to_string(),
                        target_qualified: target,
                        kind: EdgeKind::ImportsFrom,
                        file_path: file_path.to_string(),
                        line: child.start_position().row + 1,
                    });
                }
                if *language != "ruby" {
                    continue;
                }
            }
        }

        // --- Calls ---
        if lt.is_call(node_type) {
            if let Some(call_name) = get_call_name(&child, source) {
                // JS/TS test-runner wrappers in test files → Test nodes
                if matches!(*language, "javascript" | "typescript" | "tsx")
                    && is_test_file(file_path)
                    && TEST_RUNNER_NAMES.contains(&call_name.as_str())
                {
                    let test_desc = get_first_string_arg(&child, source);
                    let synthetic_name = match test_desc {
                        Some(d) => format!("{call_name}:{d}"),
                        None => call_name.clone(),
                    };
                    let qualified = qualify(&synthetic_name, file_path, enclosing_class, enc_ns);
                    let line_start = child.start_position().row + 1;
                    let line_end = child.end_position().row + 1;

                    nodes.push(NodeInfo {
                        name: synthetic_name.clone(),
                        qualified_name: qualified.clone(),
                        kind: NodeKind::Test,
                        file_path: file_path.to_string(),
                        line_start,
                        line_end,
                        language: language.to_string(),
                        is_test: true,
                        docstring: String::new(),
                        signature: String::new(),
                        body_hash: body_hash(&source[child.byte_range()]),
                    });

                    let container = match enclosing_func {
                        Some(f) => qualify(f, file_path, enclosing_class, enc_ns),
                        None => file_path.to_string(),
                    };
                    edges.push(EdgeInfo {
                        source_qualified: container,
                        target_qualified: qualified.clone(),
                        kind: EdgeKind::Contains,
                        file_path: file_path.to_string(),
                        line: line_start,
                    });

                    extract_from_tree(&child, ctx, nodes, edges, enclosing_class, Some(&synthetic_name), enc_ns, depth + 1);
                    continue;
                }

                if let Some(func) = enclosing_func {
                    let caller = qualify(func, file_path, enclosing_class, enc_ns);
                    let target =
                        resolve_call_target(&call_name, file_path, import_map, defined_names);
                    edges.push(EdgeInfo {
                        source_qualified: caller,
                        target_qualified: target,
                        kind: EdgeKind::Calls,
                        file_path: file_path.to_string(),
                        line: child.start_position().row + 1,
                    });
                }
            }
        }

        // Recurse into all other nodes
        extract_from_tree(&child, ctx, nodes, edges, enclosing_class, enclosing_func, enc_ns, depth + 1);
    }
}

/// Extract the first string literal argument from a call node (for test descriptions).
fn get_first_string_arg(call_node: &Node, source: &[u8]) -> Option<String> {
    let mut cur = call_node.walk();
    for child in call_node.children(&mut cur) {
        if child.kind() == "arguments" {
            let mut c2 = child.walk();
            for arg in child.children(&mut c2) {
                if matches!(arg.kind(), "string" | "template_string") {
                    let t = node_text(&arg, source);
                    return Some(t.trim_matches(|c| c == '\'' || c == '"' || c == '`').to_owned());
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Post-pass: resolve bare call targets & emit TESTED_BY edges
// ---------------------------------------------------------------------------

fn resolve_call_targets_pass(nodes: &[NodeInfo], edges: Vec<EdgeInfo>) -> Vec<EdgeInfo> {
    // Build symbol table: bare_name -> qualified_name
    let mut symbols: HashMap<String, String> = HashMap::new();
    for node in nodes {
        if matches!(node.kind, NodeKind::Function | NodeKind::Class | NodeKind::Type | NodeKind::Test) {
            let bare = &node.name;
            if !symbols.contains_key(bare.as_str()) {
                symbols.insert(bare.clone(), node.qualified_name.clone());
            }
        }
    }

    edges
        .into_iter()
        .map(|edge| {
            if edge.kind == EdgeKind::Calls && !edge.target_qualified.contains("::") {
                if let Some(qualified) = symbols.get(&edge.target_qualified) {
                    return EdgeInfo {
                        target_qualified: qualified.clone(),
                        ..edge
                    };
                }
            }
            edge
        })
        .collect()
}

/// Post-pass: reclassify `Inherits` edges as `Implements` when the target is
/// a known Type node (i.e. an interface).  This works cross-language because
/// interfaces are tagged `@definition.type` in the `.scm` query files.
fn reclassify_inheritance_pass(nodes: &[NodeInfo], edges: Vec<EdgeInfo>) -> Vec<EdgeInfo> {
    let type_names: HashSet<&str> = nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Type)
        .map(|n| n.name.as_str())
        .collect();

    edges
        .into_iter()
        .map(|e| {
            if e.kind == EdgeKind::Inherits {
                // Extract the bare name from the target (last segment after :: or .)
                let bare = e
                    .target_qualified
                    .rsplit('.')
                    .next()
                    .unwrap_or(&e.target_qualified);
                if type_names.contains(bare) {
                    return EdgeInfo {
                        kind: EdgeKind::Implements,
                        ..e
                    };
                }
            }
            e
        })
        .collect()
}

fn emit_tested_by_edges(nodes: &[NodeInfo], edges: &[EdgeInfo]) -> Vec<EdgeInfo> {
    let test_qnames: HashSet<&str> = nodes
        .iter()
        .filter(|n| n.is_test)
        .map(|n| n.qualified_name.as_str())
        .collect();

    let mut extra = Vec::new();
    for edge in edges {
        if edge.kind == EdgeKind::Calls && test_qnames.contains(edge.source_qualified.as_str()) {
            extra.push(EdgeInfo {
                source_qualified: edge.target_qualified.clone(),
                target_qualified: edge.source_qualified.clone(),
                kind: EdgeKind::TestedBy,
                file_path: edge.file_path.clone(),
                line: edge.line,
            });
        }
    }
    extra
}

// ---------------------------------------------------------------------------
// Post-pass: framework-aware edge inference
// ---------------------------------------------------------------------------

/// JS/TS languages that may contain framework-specific patterns.
const JS_LANGS: &[&str] = &["javascript", "typescript", "tsx"];

/// Common Express/Koa/router object names for middleware detection.
const EXPRESS_OBJECTS: &[&str] = &["app", "router", "server", "api", "express"];

/// HTTP method names used by Express/Koa route registration.
const HTTP_METHODS: &[&str] = &["use", "get", "post", "put", "delete", "patch", "all"];

/// Event-listener registration method names.
const EVENT_LISTENER_METHODS: &[&str] = &["on", "once", "addEventListener"];

/// Find the enclosing function node (smallest Function/Test node whose line
/// range contains `line`) and return its qualified name, or the file path if
/// none is found.
fn enclosing_fn_qname(line: usize, nodes: &[NodeInfo], file_path: &str) -> String {
    let mut best: Option<&NodeInfo> = None;
    for node in nodes {
        if !matches!(node.kind, NodeKind::Function | NodeKind::Test) {
            continue;
        }
        if node.file_path != file_path {
            continue;
        }
        if node.line_start <= line && line <= node.line_end {
            // Prefer the narrowest (smallest) enclosing function.
            let is_narrower = best
                .map(|b| (node.line_end - node.line_start) < (b.line_end - b.line_start))
                .unwrap_or(true);
            if is_narrower {
                best = Some(node);
            }
        }
    }
    best.map(|n| n.qualified_name.clone())
        .unwrap_or_else(|| file_path.to_string())
}

/// Try to resolve a bare name against the local node list; fall back to the
/// bare name itself (graph.rs resolves it at insertion time via qname lookup).
fn resolve_target(name: &str, nodes: &[NodeInfo]) -> String {
    nodes
        .iter()
        .find(|n| n.name == name)
        .map(|n| n.qualified_name.clone())
        .unwrap_or_else(|| name.to_string())
}

/// Extract the last identifier argument from an `arguments` AST node, if any.
fn last_identifier_arg<'s>(args_node: &Node, source: &'s [u8]) -> Option<&'s str> {
    let mut cur = args_node.walk();
    args_node
        .children(&mut cur)
        .filter(|n| n.kind() == "identifier")
        .last()
        .map(|n| node_text(&n, source))
}

/// Emit a CALLS edge from the enclosing function to a named handler.
fn push_handler_edge(
    handler_name: &str,
    call_line: usize,
    nodes: &[NodeInfo],
    file_path: &str,
    edges: &mut Vec<EdgeInfo>,
) {
    let source_qname = enclosing_fn_qname(call_line, nodes, file_path);
    let target = resolve_target(handler_name, nodes);
    edges.push(EdgeInfo {
        source_qualified: source_qname,
        target_qualified: target,
        kind: EdgeKind::Calls,
        file_path: file_path.to_string(),
        line: call_line,
    });
}

/// Recursively walk the AST looking for framework-specific call patterns and
/// collect synthetic CALLS edges. Only called for JS/TS languages.
fn walk_for_framework_edges(
    node: &Node,
    source: &[u8],
    nodes: &[NodeInfo],
    file_path: &str,
    edges: &mut Vec<EdgeInfo>,
) {
    walk_for_framework_edges_inner(node, source, nodes, file_path, edges, 0);
}

fn walk_for_framework_edges_inner(
    node: &Node,
    source: &[u8],
    nodes: &[NodeInfo],
    file_path: &str,
    edges: &mut Vec<EdgeInfo>,
    depth: usize,
) {
    if depth > MAX_AST_DEPTH {
        return;
    }
    let node_type = node.kind();

    // Pattern 1: JSX component instantiation — <ComponentName /> or <ComponentName>
    // Only PascalCase names; lowercase are HTML intrinsics and skipped.
    if matches!(node_type, "jsx_self_closing_element" | "jsx_opening_element") {
        let mut cur = node.walk();
        let component_name: Option<String> = node.children(&mut cur).find_map(|child| {
            let kind = child.kind();
            if kind == "identifier" {
                let text = node_text(&child, source);
                if text.chars().next().is_some_and(|c| c.is_uppercase()) {
                    Some(text.to_string())
                } else {
                    None
                }
            } else if kind == "member_expression" {
                // <UI.Button /> → extract "Button" (last segment)
                let full = node_text(&child, source);
                let last = full.rsplit('.').next().unwrap_or(full);
                if last.chars().next().is_some_and(|c| c.is_uppercase()) {
                    Some(last.to_string())
                } else {
                    None
                }
            } else {
                None
            }
        });

        if let Some(comp) = component_name {
            let line = node.start_position().row + 1;
            push_handler_edge(&comp, line, nodes, file_path, edges);
        }
    }

    // Patterns 2 & 3: `obj.method(...)` call expressions — Express routes and event emitters.
    if node_type == "call_expression" {
        let mut cur = node.walk();
        let children: Vec<Node> = node.children(&mut cur).collect();

        if let Some(callee) = children.first() {
            if callee.kind() == "member_expression" {
                let mut c2 = callee.walk();
                let callee_children: Vec<Node> = callee.children(&mut c2).collect();

                let obj_name = callee_children
                    .first()
                    .filter(|n| n.kind() == "identifier")
                    .map(|n| node_text(n, source));
                let method_name = callee_children
                    .iter()
                    .rev()
                    .find(|n| matches!(n.kind(), "property_identifier" | "identifier"))
                    .map(|n| node_text(n, source));

                let args_node = children.iter().find(|n| n.kind() == "arguments");

                // Pattern 2: Express/Koa route handler — app.get('/path', handler)
                let is_express_route = obj_name.is_some_and(|o| EXPRESS_OBJECTS.contains(&o))
                    && method_name.is_some_and(|m| HTTP_METHODS.contains(&m));

                if is_express_route {
                    if let Some(args) = args_node {
                        if let Some(handler_name) = last_identifier_arg(args, source) {
                            let line = node.start_position().row + 1;
                            push_handler_edge(handler_name, line, nodes, file_path, edges);
                        }
                    }
                }

                // Pattern 3: Event emitter — emitter.on('event', handler)
                let is_event_listener =
                    method_name.is_some_and(|m| EVENT_LISTENER_METHODS.contains(&m));

                if is_event_listener {
                    if let Some(args) = args_node {
                        if let Some(handler_name) = last_identifier_arg(args, source) {
                            let line = node.start_position().row + 1;
                            push_handler_edge(handler_name, line, nodes, file_path, edges);
                        }
                    }
                }
            }
        }
    }

    let mut cur = node.walk();
    for child in node.children(&mut cur) {
        walk_for_framework_edges_inner(&child, source, nodes, file_path, edges, depth + 1);
    }
}

/// Post-processing pass: detect framework-specific call patterns and emit
/// synthetic CALLS edges that the main AST walk misses.
///
/// Currently detects:
/// - JSX component instantiation (tsx/jsx: `<ComponentName />` → CALLS edge)
/// - Express/Koa route handlers (`app.use/get/post` → CALLS edge)
/// - Event emitter registrations (`.on('event', handler)` → CALLS edge)
/// - Pytest fixtures (Python test param names matching defined functions)
fn framework_edges_pass(
    nodes: &[NodeInfo],
    tree_root: &Node,
    source: &[u8],
    language: &str,
    file_path: &str,
) -> Vec<EdgeInfo> {
    let mut edges: Vec<EdgeInfo> = Vec::new();

    if JS_LANGS.contains(&language) {
        walk_for_framework_edges(tree_root, source, nodes, file_path, &mut edges);
    }

    // Pytest fixtures: match test-function param names against locally-defined functions.
    if language == "python" {
        let fixture_fns: HashMap<&str, &str> = nodes
            .iter()
            .filter(|n| {
                matches!(n.kind, NodeKind::Function)
                    && !n.is_test
                    && n.file_path == file_path
            })
            .map(|n| (n.name.as_str(), n.qualified_name.as_str()))
            .collect();

        for node in nodes {
            if !node.is_test || node.file_path != file_path {
                continue;
            }
            let sig = node.signature.trim();
            if sig.is_empty() {
                continue;
            }
            let inner = sig.trim_start_matches('(').trim_end_matches(')');
            for param in inner.split(',') {
                let param_name = param.trim().split(':').next().unwrap_or("").trim();
                if param_name.is_empty() || matches!(param_name, "self" | "cls") {
                    continue;
                }
                if let Some(&fixture_qname) = fixture_fns.get(param_name) {
                    edges.push(EdgeInfo {
                        source_qualified: node.qualified_name.clone(),
                        target_qualified: fixture_qname.to_string(),
                        kind: EdgeKind::Calls,
                        file_path: file_path.to_string(),
                        line: node.line_start,
                    });
                }
            }
        }
    }

    edges
}

// ---------------------------------------------------------------------------
// Vue SFC script-block extraction (regex-based fallback)
// ---------------------------------------------------------------------------
//
// tree-sitter-vue crates on crates.io (0.0.3, 0.1.x) are experimental and
// target an older tree-sitter API; we use a regex extractor instead.
//
// The extractor returns (script_bytes, inner_language, start_line_offset)
// where `start_line_offset` is the 0-based line index of the first line
// *inside* the script block, used to adjust reported line numbers.

static VUE_SCRIPT_RE: OnceLock<regex::Regex> = OnceLock::new();

/// Extract the `<script>` block from a Vue SFC source file.
///
/// Returns `(script_content, lang, start_line_offset)`:
/// - `script_content` — raw bytes of the script body (between the tags)
/// - `lang` — "typescript" if `lang="ts"`, otherwise "javascript"
/// - `start_line_offset` — number of newlines before the script body begins
///
/// Returns `None` when no `<script>` block is found.
fn extract_vue_script(source: &[u8]) -> Option<(Vec<u8>, &'static str, usize)> {
    let text = std::str::from_utf8(source).ok()?;

    let re = VUE_SCRIPT_RE.get_or_init(|| {
        // Captures:
        //   group 1 — optional `lang="..."` attribute value
        //   group 2 — script body content
        regex::Regex::new(
            r#"(?si)<script(?:[^>]*\blang\s*=\s*["']([^"']+)["'][^>]*)?\s*>(.*?)</script>"#,
        )
        .expect("Vue script regex is valid")
    });

    let caps = re.captures(text)?;

    let lang_attr = caps.get(1).map(|m| m.as_str()).unwrap_or("js");
    let lang: &'static str = if lang_attr.eq_ignore_ascii_case("ts")
        || lang_attr.eq_ignore_ascii_case("typescript")
    {
        "typescript"
    } else {
        "javascript"
    };

    let body_match = caps.get(2)?;
    let body = body_match.as_str();
    let body_start = body_match.start();

    // Count newlines before the body to derive the line offset.
    // Use bytes for speed — newline is single-byte in all encodings we care about.
    let start_line_offset = text.as_bytes()[..body_start]
        .iter()
        .filter(|&&b| b == b'\n')
        .count();

    Some((body.as_bytes().to_vec(), lang, start_line_offset))
}

// ---------------------------------------------------------------------------
// Public API: CodeParser
// ---------------------------------------------------------------------------

/// Multi-language code parser backed by tree-sitter.
pub struct CodeParser;

impl CodeParser {
    pub fn new() -> Self {
        Self
    }

    /// Detect the programming language from a file extension.
    pub fn detect_language(&self, path: &Path) -> Option<&'static str> {
        detect_language(path)
    }

    /// Parse source bytes and extract nodes + edges.
    pub fn parse_bytes(
        &self,
        path: &Path,
        source: &[u8],
    ) -> Result<(Vec<NodeInfo>, Vec<EdgeInfo>)> {
        let language = match detect_language(path) {
            Some(l) => l,
            None => return Ok((vec![], vec![])),
        };

        let file_path = path.to_string_lossy().replace('\\', "/");
        let file_path = file_path.as_str();

        // --- Vue SFC: 2-pass parse (no tree-sitter Tree returned) ---
        // Pass 1: regex-extract the <script> block.
        // Pass 2: re-parse that content with the JS/TS grammar.
        if language == "vue" {
            return self.parse_vue_sfc(path, source, file_path);
        }

        let (nodes, edges, _tree) = self.parse_bytes_with_tree(path, source, None)?;
        Ok((nodes, edges))
    }

    /// Parse source bytes with optional old tree for incremental re-parsing.
    ///
    /// When `old_tree` is `Some`, tree-sitter reuses unchanged AST regions,
    /// reducing parse time from ~5 ms to <1 ms for typical edits.
    ///
    /// Returns `(nodes, edges, new_tree)` so the caller can cache the tree.
    /// Vue SFC files are not supported — use `parse_bytes` for Vue instead.
    pub fn parse_bytes_with_tree(
        &self,
        path: &Path,
        source: &[u8],
        old_tree: Option<&tree_sitter::Tree>,
    ) -> Result<(Vec<NodeInfo>, Vec<EdgeInfo>, tree_sitter::Tree)> {
        let language = match detect_language(path) {
            Some(l) => l,
            None => {
                return Err(CrgError::TreeSitter(
                    "no language detected for incremental parse".into(),
                ))
            }
        };

        // Vue SFC uses a two-pass path that doesn't produce a single Tree.
        // Callers must use parse_bytes for Vue files.
        if language == "vue" {
            return Err(CrgError::TreeSitter(
                "incremental parse not supported for Vue SFC".into(),
            ));
        }

        let mut parser = make_parser(language).ok_or_else(|| {
            CrgError::TreeSitter(format!("No grammar for language: {language}"))
        })?;

        let tree = parser
            .parse(source, old_tree)
            .ok_or_else(|| CrgError::TreeSitter("parse returned None".into()))?;

        let file_path = path.to_string_lossy().replace('\\', "/");
        let file_path = file_path.as_str();
        let root = tree.root_node();
        let test_file = is_test_file(file_path);
        let line_count = source.iter().filter(|&&b| b == b'\n').count() + 1;

        let mut nodes: Vec<NodeInfo> = Vec::new();
        let mut edges: Vec<EdgeInfo> = Vec::new();

        nodes.push(NodeInfo {
            name: file_path.to_owned(),
            qualified_name: file_path.to_owned(),
            kind: NodeKind::File,
            file_path: file_path.to_owned(),
            line_start: 1,
            line_end: line_count,
            language: language.to_owned(),
            is_test: test_file,
            docstring: String::new(),
            signature: String::new(),
            body_hash: body_hash(source),
        });

        let lt = get_lang_types(language)
            .ok_or_else(|| CrgError::Other(format!("unsupported language: {language}")))?;
        let (import_map, defined_names) = collect_file_scope(&root, lt, language, source);

        let ctx = WalkCtx {
            source,
            language,
            file_path,
            import_map: &import_map,
            defined_names: &defined_names,
            lt,
        };
        extract_from_tree(&root, &ctx, &mut nodes, &mut edges, None, None, None, 0);

        edges = resolve_call_targets_pass(&nodes, edges);
        edges = reclassify_inheritance_pass(&nodes, edges);
        edges.extend(framework_edges_pass(&nodes, &tree.root_node(), source, language, file_path));

        if test_file {
            let tested_by = emit_tested_by_edges(&nodes, &edges);
            edges.extend(tested_by);
        }

        Ok((nodes, edges, tree))
    }

    /// Vue SFC 2-pass parser: extract `<script>` block, re-parse with JS/TS grammar.
    fn parse_vue_sfc(
        &self,
        _path: &Path,
        source: &[u8],
        file_path: &str,
    ) -> Result<(Vec<NodeInfo>, Vec<EdgeInfo>)> {
        let line_count = source.iter().filter(|&&b| b == b'\n').count() + 1;
        let test_file = is_test_file(file_path);

        let mut nodes: Vec<NodeInfo> = Vec::new();
        let mut edges: Vec<EdgeInfo> = Vec::new();

        // Always emit a File node for the .vue file itself.
        nodes.push(NodeInfo {
            name: file_path.to_owned(),
            qualified_name: file_path.to_owned(),
            kind: NodeKind::File,
            file_path: file_path.to_owned(),
            line_start: 1,
            line_end: line_count,
            language: "vue".to_owned(),
            is_test: test_file,
            docstring: String::new(),
            signature: String::new(),
            body_hash: body_hash(source),
        });

        // Extract the <script> block.
        let Some((script_bytes, script_lang, line_offset)) = extract_vue_script(source) else {
            // No <script> block found — return just the File node.
            return Ok((nodes, edges));
        };

        let mut parser = make_parser(script_lang).ok_or_else(|| {
            CrgError::TreeSitter(format!("No grammar for Vue script language: {script_lang}"))
        })?;

        let tree = parser
            .parse(&script_bytes, None)
            .ok_or_else(|| CrgError::TreeSitter("Vue script parse returned None".into()))?;

        let root = tree.root_node();
        let lt = get_lang_types(script_lang)
            .ok_or_else(|| CrgError::Other(format!("unsupported language: {script_lang}")))?;
        let (import_map, defined_names) =
            collect_file_scope(&root, lt, script_lang, &script_bytes);

        let ctx = WalkCtx {
            source: &script_bytes,
            language: script_lang,
            file_path,
            import_map: &import_map,
            defined_names: &defined_names,
            lt,
        };

        let mut script_nodes: Vec<NodeInfo> = Vec::new();
        let mut script_edges: Vec<EdgeInfo> = Vec::new();
        extract_from_tree(&root, &ctx, &mut script_nodes, &mut script_edges, None, None, None, 0);

        // Adjust line numbers by the script block's offset within the .vue file.
        // Language is already set to script_lang by extract_from_tree via ctx.language.
        for node in &mut script_nodes {
            if node.kind != NodeKind::File {
                node.line_start += line_offset;
                node.line_end += line_offset;
            }
        }
        for edge in &mut script_edges {
            edge.line += line_offset;
        }

        // Merge: skip the File node emitted by extract_from_tree for the script,
        // since we already have the .vue File node.
        nodes.extend(script_nodes.into_iter().filter(|n| n.kind != NodeKind::File));
        edges.extend(script_edges);

        edges = resolve_call_targets_pass(&nodes, edges);
        edges = reclassify_inheritance_pass(&nodes, edges);

        // Framework-aware edge inference for the Vue script block.
        edges.extend(framework_edges_pass(&nodes, &root, &script_bytes, script_lang, file_path));

        if test_file {
            let tested_by = emit_tested_by_edges(&nodes, &edges);
            edges.extend(tested_by);
        }

        Ok((nodes, edges))
    }

    /// Convenience: read file and parse.
    pub fn parse_file(&self, path: &Path) -> Result<(Vec<NodeInfo>, Vec<EdgeInfo>)> {
        let source = std::fs::read(path)?;
        self.parse_bytes(path, &source)
    }
}

impl Default for CodeParser {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn parse(filename: &str, src: &str) -> (Vec<NodeInfo>, Vec<EdgeInfo>) {
        let parser = CodeParser::new();
        let path = PathBuf::from(filename);
        parser.parse_bytes(&path, src.as_bytes()).expect("parse failed")
    }

    /// Returns `true` if the tree-sitter grammar for the given extension is
    /// available on this system. On some Windows builds the C ABI grammar
    /// libraries fail to initialise (set_language returns an error). Tests
    /// that require actual parsing use this to skip gracefully instead of
    /// panicking so that the rest of the test suite still passes.
    fn grammar_available(ext: &str) -> bool {
        let path = PathBuf::from(format!("check.{ext}"));
        let parser = CodeParser::new();
        parser.parse_bytes(&path, b"").is_ok()
    }

    #[test]
    fn detect_python() {
        let parser = CodeParser::new();
        assert_eq!(parser.detect_language(Path::new("foo.py")), Some("python"));
        assert_eq!(parser.detect_language(Path::new("bar.rs")), Some("rust"));
        assert_eq!(parser.detect_language(Path::new("baz.txt")), None);
    }

    #[test]
    fn python_class_and_function() {
        if !grammar_available("py") { return; }
        let src = r#"
class MyClass:
    def method(self):
        pass

def top_level():
    pass
"#;
        let (nodes, edges) = parse("foo.py", src);
        let kinds: Vec<&str> = nodes.iter().map(|n| n.kind.as_str()).collect();
        assert!(kinds.contains(&"Class"), "expected Class node");
        assert!(kinds.contains(&"Function"), "expected Function node");
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"MyClass"));
        assert!(names.contains(&"method"));
        assert!(names.contains(&"top_level"));

        let contains: Vec<_> = edges.iter().filter(|e| e.kind == EdgeKind::Contains).collect();
        assert!(!contains.is_empty(), "expected CONTAINS edges");
    }

    #[test]
    fn python_calls_edge() {
        if !grammar_available("py") { return; }
        let src = r#"
def callee():
    pass

def caller():
    callee()
"#;
        let (_, edges) = parse("foo.py", src);
        let calls: Vec<_> = edges.iter().filter(|e| e.kind == EdgeKind::Calls).collect();
        assert!(!calls.is_empty(), "expected CALLS edge");
    }

    #[test]
    fn rust_functions() {
        if !grammar_available("rs") { return; }
        let src = r#"
struct Foo {}
fn bar() {}
fn baz() { bar(); }
"#;
        let (nodes, _) = parse("main.rs", src);
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"Foo"));
        assert!(names.contains(&"bar"));
        assert!(names.contains(&"baz"));
    }

    #[test]
    fn test_function_detection() {
        if !grammar_available("py") { return; }
        let src = r#"
def test_something():
    pass

def normal():
    pass
"#;
        let (nodes, _) = parse("test_foo.py", src);
        let test_nodes: Vec<_> = nodes.iter().filter(|n| n.is_test).collect();
        assert!(!test_nodes.is_empty(), "expected test node");
    }

    #[test]
    fn javascript_class_inheritance() {
        if !grammar_available("js") { return; }
        let src = r#"
class Animal {}
class Dog extends Animal {}
"#;
        let (_, edges) = parse("animals.js", src);
        let inherits: Vec<_> = edges.iter().filter(|e| e.kind == EdgeKind::Inherits).collect();
        assert!(!inherits.is_empty(), "expected INHERITS edge");
    }

    #[test]
    fn typescript_imports() {
        if !grammar_available("ts") { return; }
        let src = r#"import { foo } from './foo';"#;
        let (_, edges) = parse("bar.ts", src);
        let imports: Vec<_> = edges.iter().filter(|e| e.kind == EdgeKind::ImportsFrom).collect();
        assert!(!imports.is_empty(), "expected IMPORTS_FROM edge");
    }

    #[test]
    fn go_functions() {
        let src = r#"
package main

func Hello() string { return "hi" }
func main() { Hello() }
"#;
        if !grammar_available("go") { return; }
        let (nodes, edges) = parse("main.go", src);
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"Hello"));
        assert!(names.contains(&"main"));
        let calls: Vec<_> = edges.iter().filter(|e| e.kind == EdgeKind::Calls).collect();
        assert!(!calls.is_empty(), "expected CALLS edge in go");
    }

    /// Vue SFC parsing test.
    ///
    /// Two-pass approach:
    ///   1. Regex-extract the `<script lang="ts">` block from the `.vue` file.
    ///   2. Re-parse the extracted content with the TypeScript grammar.
    ///
    /// The fixture at `tests/fixtures/test.vue` contains a `setup()` function
    /// and an `import { ref } from 'vue'` statement inside a `<script lang="ts">` block.
    #[test]
    fn vue_sfc_parsing() {
        if !grammar_available("ts") { return; }

        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/test.vue");
        let src = std::fs::read(&fixture)
            .expect("tests/fixtures/test.vue must exist");

        let parser = CodeParser::new();

        // Language detection: .vue is now supported.
        assert_eq!(
            parser.detect_language(&fixture),
            Some("vue"),
            "detect_language should return Some(\"vue\") for .vue files"
        );

        let (nodes, edges) = parser
            .parse_bytes(&fixture, &src)
            .expect("parse_bytes must not error on .vue file");

        // At least a File node must exist.
        let file_nodes: Vec<_> = nodes.iter().filter(|n| n.kind == NodeKind::File).collect();
        assert_eq!(file_nodes.len(), 1, "should have exactly one File node");

        // The setup() function should be extracted from the <script> block.
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(
            names.contains(&"setup"),
            "expected setup function extracted from <script>; got: {names:?}"
        );

        // The `import { ref } from 'vue'` statement should produce an IMPORTS_FROM edge.
        let import_edges: Vec<_> = edges
            .iter()
            .filter(|e| e.kind == EdgeKind::ImportsFrom)
            .collect();
        assert!(
            !import_edges.is_empty(),
            "expected at least one IMPORTS_FROM edge from the Vue script block"
        );

        // All non-File nodes extracted from the script block carry js/ts language.
        let script_nodes: Vec<_> = nodes
            .iter()
            .filter(|n| n.kind != NodeKind::File)
            .collect();
        assert!(
            script_nodes
                .iter()
                .all(|n| n.language == "typescript" || n.language == "javascript"),
            "script-block nodes must carry 'typescript' or 'javascript' language, not 'vue'"
        );
    }

    // -----------------------------------------------------------------------
    // detect_language for all supported extensions
    // -----------------------------------------------------------------------

    #[test]
    fn detect_language_all_extensions() {
        let cases: &[(&str, &str)] = &[
            ("foo.py",    "python"),
            ("foo.js",    "javascript"),
            ("foo.mjs",   "javascript"),
            ("foo.jsx",   "javascript"),
            ("foo.ts",    "typescript"),
            ("foo.mts",   "typescript"),
            ("foo.tsx",   "tsx"),
            ("foo.rs",    "rust"),
            ("foo.go",    "go"),
            ("foo.java",  "java"),
            ("foo.c",     "c"),
            ("foo.h",     "c"),
            ("foo.cpp",   "cpp"),
            ("foo.cc",    "cpp"),
            ("foo.cs",    "csharp"),
            ("foo.rb",    "ruby"),
            ("foo.kt",    "kotlin"),
            ("foo.swift", "swift"),
            ("foo.zig",   "zig"),
            ("build.zon", "zig"),
        ];
        let parser = CodeParser::new();
        for (filename, expected) in cases {
            let detected = parser.detect_language(Path::new(filename));
            assert_eq!(
                detected,
                Some(*expected),
                "{filename} should be detected as {expected}"
            );
        }
    }

    #[test]
    fn detect_language_unsupported_returns_none() {
        let parser = CodeParser::new();
        assert_eq!(parser.detect_language(Path::new("readme.md")), None);
        assert_eq!(parser.detect_language(Path::new("data.json")), None);
        assert_eq!(parser.detect_language(Path::new("image.png")), None);
        assert_eq!(parser.detect_language(Path::new("noextension")), None);
    }

    // -----------------------------------------------------------------------
    // Python: body_hash, qualified_name, test detection, imports
    // -----------------------------------------------------------------------

    #[test]
    fn python_body_hash_is_non_empty() {
        if !grammar_available("py") { return; }
        let src = "def foo():\n    pass\n";
        let (nodes, _) = parse("f.py", src);
        let func = nodes.iter().find(|n| n.name == "foo").unwrap();
        assert!(!func.body_hash.is_empty());
    }

    #[test]
    fn python_qualified_name_format() {
        if !grammar_available("py") { return; }
        let src = "def my_func():\n    pass\n";
        let (nodes, _) = parse("module/foo.py", src);
        let func = nodes.iter().find(|n| n.name == "my_func").unwrap();
        assert!(
            func.qualified_name.contains("::"),
            "qualified_name '{}' should contain '::'",
            func.qualified_name
        );
        assert!(
            func.qualified_name.ends_with("my_func"),
            "qualified_name '{}' should end with 'my_func'",
            func.qualified_name
        );
    }

    #[test]
    fn python_test_function_is_test() {
        if !grammar_available("py") { return; }
        let src = r#"
def test_add():
    assert 1 + 1 == 2

def not_a_test():
    pass
"#;
        let (nodes, _) = parse("tests/test_math.py", src);
        let test_fn = nodes.iter().find(|n| n.name == "test_add").unwrap();
        assert!(test_fn.is_test, "test_add should be flagged as is_test");
        let normal_fn = nodes.iter().find(|n| n.name == "not_a_test").unwrap();
        assert!(!normal_fn.is_test, "not_a_test should not be is_test");
    }

    #[test]
    fn python_imports_produce_edges() {
        if !grammar_available("py") { return; }
        let src = r#"
import os
from sys import argv

def greet(name):
    pass
"#;
        let (_, edges) = parse("greet.py", src);
        let imports: Vec<_> = edges.iter().filter(|e| e.kind == EdgeKind::ImportsFrom).collect();
        assert!(!imports.is_empty(), "should have IMPORTS_FROM edges");
    }

    // -----------------------------------------------------------------------
    // TypeScript: functions, classes, imports
    // -----------------------------------------------------------------------

    #[test]
    fn typescript_functions_and_classes() {
        if !grammar_available("ts") { return; }
        let src = r#"
class MyService {
    doWork(): void {}
}

function helper(): string {
    return "hi";
}
"#;
        let (nodes, _) = parse("service.ts", src);
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"MyService"), "should have MyService class");
        assert!(names.contains(&"helper"), "should have helper function");
    }

    #[test]
    fn typescript_imports_produce_edges() {
        if !grammar_available("ts") { return; }
        let src = r#"import { ComponentA } from './components';"#;
        let (_, edges) = parse("index.ts", src);
        let imports: Vec<_> = edges.iter().filter(|e| e.kind == EdgeKind::ImportsFrom).collect();
        assert!(!imports.is_empty(), "should have IMPORTS_FROM edges for TS");
    }

    // -----------------------------------------------------------------------
    // Rust: structs, use statements, calls
    // -----------------------------------------------------------------------

    #[test]
    fn rust_structs_and_use() {
        if !grammar_available("rs") { return; }
        let src = r#"
use std::collections::HashMap;

struct Config {
    value: u32,
}

fn load_config() -> Config {
    Config { value: 42 }
}
"#;
        let (nodes, edges) = parse("config.rs", src);
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"Config"), "should have Config struct");
        assert!(names.contains(&"load_config"), "should have load_config fn");
        let imports: Vec<_> = edges.iter().filter(|e| e.kind == EdgeKind::ImportsFrom).collect();
        assert!(!imports.is_empty(), "should have IMPORTS_FROM for use statement");
    }

    #[test]
    fn rust_calls_edge() {
        if !grammar_available("rs") { return; }
        let src = r#"
fn helper() -> u32 { 42 }
fn main() { let _ = helper(); }
"#;
        let (_, edges) = parse("main.rs", src);
        let calls: Vec<_> = edges.iter().filter(|e| e.kind == EdgeKind::Calls).collect();
        assert!(!calls.is_empty(), "should have CALLS edge from main to helper");
    }

    // -----------------------------------------------------------------------
    // Go: imports, structs
    // -----------------------------------------------------------------------

    #[test]
    fn go_imports_edge() {
        if !grammar_available("go") { return; }
        let src = r#"
package main

import "fmt"

func greet() {
    fmt.Println("hello")
}
"#;
        let (_, edges) = parse("main.go", src);
        let imports: Vec<_> = edges.iter().filter(|e| e.kind == EdgeKind::ImportsFrom).collect();
        assert!(!imports.is_empty(), "should have IMPORTS_FROM for go import");
    }

    #[test]
    fn go_struct_extracted() {
        if !grammar_available("go") { return; }
        let src = r#"
package main

type Server struct {
    port int
}

func (s *Server) Start() {}
"#;
        let (nodes, _) = parse("server.go", src);
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"Server"), "should have Server struct");
        assert!(names.contains(&"Start"), "should have Start method");
    }

    // -----------------------------------------------------------------------
    // EdgeKind::Contains links container to children
    // -----------------------------------------------------------------------

    #[test]
    fn contains_edges_link_file_to_children() {
        if !grammar_available("py") { return; }
        let src = r#"
def top_fn():
    pass

class MyClass:
    pass
"#;
        let (nodes, edges) = parse("module.py", src);
        let file_node = nodes.iter().find(|n| n.kind == NodeKind::File).unwrap();
        let contains: Vec<_> = edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Contains && e.source_qualified == file_node.qualified_name)
            .collect();
        assert!(!contains.is_empty(), "file should CONTAINS its children");
    }

    // -----------------------------------------------------------------------
    // CALLS edges created for function calls
    // -----------------------------------------------------------------------

    #[test]
    fn calls_edges_created_for_function_calls() {
        if !grammar_available("py") { return; }
        let src = r#"
def compute():
    return 42

def run():
    x = compute()
    return x
"#;
        let (_, edges) = parse("run.py", src);
        let call_edges: Vec<_> = edges.iter().filter(|e| e.kind == EdgeKind::Calls).collect();
        assert!(!call_edges.is_empty(), "should have CALLS edges");
        let calls_compute = call_edges
            .iter()
            .any(|e| e.target_qualified.ends_with("compute"));
        assert!(calls_compute, "should have a CALLS edge targeting compute");
    }

    // -----------------------------------------------------------------------
    // File node is always emitted
    // -----------------------------------------------------------------------

    #[test]
    fn file_node_always_emitted() {
        if !grammar_available("py") { return; }
        let src = "x = 1\n";
        let (nodes, _) = parse("simple.py", src);
        let file_nodes: Vec<_> = nodes.iter().filter(|n| n.kind == NodeKind::File).collect();
        assert_eq!(file_nodes.len(), 1, "should have exactly one File node");
    }

    // -----------------------------------------------------------------------
    // Unsupported extension returns Ok with empty results (no panic)
    // -----------------------------------------------------------------------

    #[test]
    fn unsupported_extension_returns_ok_empty() {
        let parser = CodeParser::new();
        for filename in &["data.json", "readme.md", "image.png", "archive.tar.gz", "noextension"] {
            let result = parser.parse_bytes(std::path::Path::new(filename), b"some content");
            assert!(
                result.is_ok(),
                "parse_bytes should not error on unknown extension '{filename}'"
            );
            let (nodes, edges) = result.unwrap();
            assert!(
                nodes.is_empty(),
                "nodes should be empty for unknown extension '{filename}'"
            );
            assert!(
                edges.is_empty(),
                "edges should be empty for unknown extension '{filename}'"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Framework-aware edge inference tests
    // -----------------------------------------------------------------------

    #[test]
    fn jsx_component_emits_calls_edge() {
        if !grammar_available("tsx") { return; }
        let src = r#"
function Button() { return null; }
function App() {
    return <Button />;
}
"#;
        let (_, edges) = parse("app.tsx", src);
        let calls: Vec<_> = edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls && e.target_qualified.contains("Button"))
            .collect();
        assert!(!calls.is_empty(), "expected CALLS edge from App to Button via JSX");
    }

    #[test]
    fn jsx_lowercase_element_no_edge() {
        if !grammar_available("tsx") { return; }
        // HTML intrinsics like <div> should NOT produce CALLS edges.
        let src = r#"
function App() {
    return <div className="container" />;
}
"#;
        let (_, edges) = parse("app.tsx", src);
        let calls_to_div: Vec<_> = edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls && e.target_qualified.contains("div"))
            .collect();
        assert!(calls_to_div.is_empty(), "lowercase JSX elements should not produce CALLS edges");
    }

    #[test]
    fn express_route_handler_emits_calls_edge() {
        if !grammar_available("js") { return; }
        let src = r#"
function handleHome() {}
const app = {};
app.get('/home', handleHome);
"#;
        let (_, edges) = parse("server.js", src);
        let calls: Vec<_> = edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls && e.target_qualified.contains("handleHome"))
            .collect();
        assert!(!calls.is_empty(), "expected CALLS edge to Express handler handleHome");
    }

    #[test]
    fn event_emitter_on_emits_calls_edge() {
        if !grammar_available("js") { return; }
        let src = r#"
function onData() {}
const emitter = {};
emitter.on('data', onData);
"#;
        let (_, edges) = parse("events.js", src);
        let calls: Vec<_> = edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls && e.target_qualified.contains("onData"))
            .collect();
        assert!(!calls.is_empty(), "expected CALLS edge to event handler onData");
    }

    #[test]
    fn pytest_fixture_emits_calls_edge() {
        if !grammar_available("py") { return; }
        let src = r#"
def db_connection():
    return None

def test_query(db_connection):
    pass
"#;
        let (_, edges) = parse("tests/test_db.py", src);
        let calls: Vec<_> = edges
            .iter()
            .filter(|e| {
                e.kind == EdgeKind::Calls
                    && e.source_qualified.contains("test_query")
                    && e.target_qualified.contains("db_connection")
            })
            .collect();
        assert!(!calls.is_empty(), "expected CALLS edge from test_query to pytest fixture db_connection");
    }

    // -----------------------------------------------------------------------
    // TypeScript: interface / type-alias indexing
    // -----------------------------------------------------------------------

    #[test]
    fn typescript_interface_produces_type_node() {
        if !grammar_available("ts") { return; }
        let src = r#"
export interface UserConfig {
    name: string;
    age: number;
    experimental: {
        turbopackUseBuiltinSass: boolean;
    };
}
"#;
        let (nodes, _edges) = parse("config.ts", src);
        let type_nodes: Vec<_> = nodes.iter().filter(|n| n.kind == NodeKind::Type).collect();
        assert!(!type_nodes.is_empty(), "expected at least one Type node");
        assert!(type_nodes.iter().any(|n| n.name == "UserConfig"), "expected UserConfig interface");
    }

    #[test]
    fn typescript_type_alias_produces_type_node() {
        if !grammar_available("ts") { return; }
        let src = r#"
type Theme = "light" | "dark";
type Config = { debug: boolean };
"#;
        let (nodes, _) = parse("types.ts", src);
        let type_nodes: Vec<_> = nodes.iter().filter(|n| n.kind == NodeKind::Type).collect();
        assert!(type_nodes.iter().any(|n| n.name == "Theme"), "expected Theme type alias");
        assert!(type_nodes.iter().any(|n| n.name == "Config"), "expected Config type alias");
    }

    #[test]
    fn typescript_interface_properties_indexed() {
        if !grammar_available("ts") { return; }
        let src = r#"
export interface ExperimentalConfig {
    turbopackUseBuiltinSass: boolean;
    sassOptions: object;
    useLightningcss: boolean;
}
"#;
        let (nodes, edges) = parse("config.ts", src);
        let type_nodes: Vec<_> = nodes.iter().filter(|n| n.kind == NodeKind::Type).collect();

        // Parent interface node
        assert!(type_nodes.iter().any(|n| n.name == "ExperimentalConfig"),
            "expected ExperimentalConfig interface node");

        // Property nodes
        assert!(type_nodes.iter().any(|n| n.name == "turbopackUseBuiltinSass"),
            "expected turbopackUseBuiltinSass property node");
        assert!(type_nodes.iter().any(|n| n.name == "sassOptions"),
            "expected sassOptions property node");

        // Contains edges from interface to properties
        let contains_edges: Vec<_> = edges.iter()
            .filter(|e| e.kind == EdgeKind::Contains && e.source_qualified.contains("ExperimentalConfig"))
            .collect();
        assert!(contains_edges.len() >= 2, "expected Contains edges from interface to properties");
    }

    #[test]
    fn csharp_using_directive() {
        if !grammar_available("cs") { return; }
        let src = "using System.Collections.Generic;\n";
        let (_, edges) = parse("Program.cs", src);
        let imports: Vec<_> = edges.iter().filter(|e| e.kind == EdgeKind::ImportsFrom).collect();
        assert!(!imports.is_empty(), "expected IMPORTS_FROM edge");
        assert!(imports.iter().any(|e| e.target_qualified == "System.Collections.Generic"),
            "expected import target 'System.Collections.Generic', got: {:?}",
            imports.iter().map(|e| &e.target_qualified).collect::<Vec<_>>());
    }

    #[test]
    fn csharp_using_static() {
        if !grammar_available("cs") { return; }
        let src = "using static System.Math;\n";
        let (_, edges) = parse("Program.cs", src);
        let imports: Vec<_> = edges.iter().filter(|e| e.kind == EdgeKind::ImportsFrom).collect();
        assert!(!imports.is_empty(), "expected IMPORTS_FROM edge for using static");
        assert!(imports.iter().any(|e| e.target_qualified == "System.Math"),
            "expected import target 'System.Math', got: {:?}",
            imports.iter().map(|e| &e.target_qualified).collect::<Vec<_>>());
    }

    #[test]
    fn csharp_global_using() {
        if !grammar_available("cs") { return; }
        let src = "global using System;\n";
        let (_, edges) = parse("GlobalUsings.cs", src);
        let imports: Vec<_> = edges.iter().filter(|e| e.kind == EdgeKind::ImportsFrom).collect();
        assert!(!imports.is_empty(), "expected IMPORTS_FROM edge for global using");
        assert!(imports.iter().any(|e| e.target_qualified == "System"),
            "expected import target 'System', got: {:?}",
            imports.iter().map(|e| &e.target_qualified).collect::<Vec<_>>());
    }

    #[test]
    fn csharp_inheritance_base_list() {
        if !grammar_available("cs") { return; }
        let src = r#"
public class Animal {
    public virtual void Speak() { }
}
public class Dog : Animal {
    public override void Speak() { }
}
"#;
        let (_, edges) = parse("Animals.cs", src);
        let inherits: Vec<_> = edges.iter().filter(|e| e.kind == EdgeKind::Inherits).collect();
        assert!(!inherits.is_empty(), "expected INHERITS edge from Dog to Animal");
        assert!(inherits.iter().any(|e| e.target_qualified.contains("Animal")),
            "expected inheritance target to contain 'Animal'");
    }

    #[test]
    fn csharp_multiple_bases() {
        if !grammar_available("cs") { return; }
        let src = r#"
public class Base { }
public class Derived : Base {
    public void DoStuff() { }
}
"#;
        let (_, edges) = parse("Multi.cs", src);
        let inherits: Vec<_> = edges.iter()
            .filter(|e| e.kind == EdgeKind::Inherits || e.kind == EdgeKind::Implements)
            .collect();
        assert!(!inherits.is_empty(), "expected inheritance/implements edges");
    }

    #[test]
    fn csharp_basic_class_and_method() {
        if !grammar_available("cs") { return; }
        let src = r#"
public class MyService {
    public void DoWork() {
        Console.WriteLine("hello");
    }
    public int Calculate(int x) {
        return x * 2;
    }
}
"#;
        let (nodes, edges) = parse("Service.cs", src);
        let class_nodes: Vec<_> = nodes.iter().filter(|n| n.kind == NodeKind::Class).collect();
        assert!(class_nodes.iter().any(|n| n.name == "MyService"), "expected MyService class");
        let func_nodes: Vec<_> = nodes.iter().filter(|n| n.kind == NodeKind::Function).collect();
        assert!(func_nodes.iter().any(|n| n.name == "DoWork"), "expected DoWork method");
        assert!(func_nodes.iter().any(|n| n.name == "Calculate"), "expected Calculate method");
        let contains: Vec<_> = edges.iter().filter(|e| e.kind == EdgeKind::Contains).collect();
        assert!(!contains.is_empty(), "expected CONTAINS edges");
    }

    #[test]
    fn csharp_interface_is_type_node() {
        if !grammar_available("cs") { return; }
        let src = r#"
public interface IRepository {
    void Save();
    int Count();
}
"#;
        let (nodes, _) = parse("IRepository.cs", src);
        let type_nodes: Vec<_> = nodes.iter().filter(|n| n.kind == NodeKind::Type).collect();
        assert!(type_nodes.iter().any(|n| n.name == "IRepository"), "expected IRepository as Type node, not Class");
        // Should NOT be a Class node
        let class_nodes: Vec<_> = nodes.iter().filter(|n| n.kind == NodeKind::Class && n.name == "IRepository").collect();
        assert!(class_nodes.is_empty(), "IRepository should not be a Class node");
    }

    #[test]
    fn csharp_record_declaration() {
        if !grammar_available("cs") { return; }
        let src = "public record Person(string Name, int Age);\n";
        let (nodes, _) = parse("Person.cs", src);
        let class_nodes: Vec<_> = nodes.iter().filter(|n| n.kind == NodeKind::Class).collect();
        assert!(class_nodes.iter().any(|n| n.name == "Person"), "expected Person record as Class node");
    }

    #[test]
    fn csharp_member_access_call() {
        if !grammar_available("cs") { return; }
        let src = r#"
public class Foo {
    public void Bar() {
        Console.WriteLine("test");
        var result = list.Where(x => x > 0).Select(x => x * 2);
    }
}
"#;
        let (_, edges) = parse("Foo.cs", src);
        let calls: Vec<_> = edges.iter().filter(|e| e.kind == EdgeKind::Calls).collect();
        assert!(!calls.is_empty(), "expected CALLS edges for member access expressions");
        // Should have calls for WriteLine, Where, Select
        let call_targets: Vec<&str> = calls.iter().map(|e| e.target_qualified.as_str()).collect();
        assert!(call_targets.iter().any(|t| t.contains("WriteLine")), "expected WriteLine call");
    }

    #[test]
    fn csharp_property_indexed() {
        if !grammar_available("cs") { return; }
        let src = r#"
public class Config {
    public string Name { get; set; }
    public int Value { get; set; }
}
"#;
        let (nodes, _) = parse("Config.cs", src);
        let func_nodes: Vec<_> = nodes.iter().filter(|n| n.kind == NodeKind::Function).collect();
        assert!(func_nodes.iter().any(|n| n.name == "Name"), "expected Name property as Function node");
        assert!(func_nodes.iter().any(|n| n.name == "Value"), "expected Value property as Function node");
    }

    #[test]
    fn csharp_test_attribute_nunit() {
        if !grammar_available("cs") { return; }
        let src = r#"
using NUnit.Framework;

[TestFixture]
public class MyTests {
    [Test]
    public void Should_Work() {
        Assert.Pass();
    }
}
"#;
        let (nodes, _) = parse("tests/MyTests.cs", src);
        let test_nodes: Vec<_> = nodes.iter().filter(|n| n.is_test).collect();
        assert!(!test_nodes.is_empty(), "expected test node for [Test] attributed method");
        assert!(test_nodes.iter().any(|n| n.name == "Should_Work"),
            "expected Should_Work to be detected as test");
    }

    #[test]
    fn csharp_fact_attribute_xunit() {
        if !grammar_available("cs") { return; }
        let src = r#"
using Xunit;

public class CalculatorTests {
    [Fact]
    public void Add_Returns_Sum() {
        Assert.Equal(4, 2 + 2);
    }

    [Theory]
    public void Add_Multiple(int a, int b) {
        Assert.True(a + b > 0);
    }
}
"#;
        let (nodes, _) = parse("tests/CalculatorTests.cs", src);
        let test_nodes: Vec<_> = nodes.iter().filter(|n| n.is_test).collect();
        assert!(test_nodes.len() >= 2, "expected at least 2 test nodes for [Fact] and [Theory]");
        assert!(test_nodes.iter().any(|n| n.name == "Add_Returns_Sum"), "expected Fact test");
        assert!(test_nodes.iter().any(|n| n.name == "Add_Multiple"), "expected Theory test");
    }

    #[test]
    fn csharp_non_test_method() {
        if !grammar_available("cs") { return; }
        let src = r#"
public class Service {
    public void Process() {
        // no test attribute
    }
}
"#;
        let (nodes, _) = parse("Service.cs", src);
        let func_nodes: Vec<_> = nodes.iter().filter(|n| n.kind == NodeKind::Function).collect();
        assert!(!func_nodes.is_empty(), "expected function nodes");
        let test_nodes: Vec<_> = nodes.iter().filter(|n| n.is_test).collect();
        assert!(test_nodes.is_empty(), "non-test methods should not be marked as tests");
    }

    // -----------------------------------------------------------------------
    // C#: namespace-aware qualified names
    // -----------------------------------------------------------------------

    #[test]
    fn csharp_namespace_qualified_name() {
        if !grammar_available("cs") { return; }
        let src = r#"
namespace MyApp.Models {
    public class User {
        public void Save() { }
    }
}
"#;
        let (nodes, _) = parse("User.cs", src);
        let class_node = nodes.iter().find(|n| n.name == "User").expect("expected User class");
        assert!(class_node.qualified_name.contains("MyApp.Models"),
            "qualified name '{}' should contain namespace 'MyApp.Models'",
            class_node.qualified_name);
        let method_node = nodes.iter().find(|n| n.name == "Save").expect("expected Save method");
        assert!(method_node.qualified_name.contains("MyApp.Models"),
            "method qualified name '{}' should contain namespace",
            method_node.qualified_name);
    }

    #[test]
    fn csharp_file_scoped_namespace() {
        if !grammar_available("cs") { return; }
        let src = r#"
namespace MyApp.Services;

public class UserService {
    public void Process() { }
}
"#;
        let (nodes, _) = parse("UserService.cs", src);
        let class_node = nodes.iter().find(|n| n.name == "UserService").expect("expected UserService class");
        assert!(class_node.qualified_name.contains("MyApp.Services"),
            "qualified name '{}' should contain namespace 'MyApp.Services'",
            class_node.qualified_name);
    }

    #[test]
    fn csharp_no_namespace_unchanged() {
        if !grammar_available("cs") { return; }
        let src = r#"
public class TopLevel {
    public void Run() { }
}
"#;
        let (nodes, _) = parse("TopLevel.cs", src);
        let class_node = nodes.iter().find(|n| n.name == "TopLevel").expect("expected TopLevel class");
        // Without namespace, format should be file_path::TopLevel
        assert_eq!(class_node.qualified_name, "TopLevel.cs::TopLevel",
            "without namespace, qualified name should be simple");
    }

    // -----------------------------------------------------------------------
    // Zig tests
    // -----------------------------------------------------------------------

    #[test]
    fn zig_struct_from_variable_declaration() {
        if !grammar_available("zig") { return; }
        let src = r#"
const Allocator = struct {
    ptr: *anyopaque,
    pub fn alloc(self: Allocator, n: usize) ?[]u8 {
        return null;
    }
};
"#;
        let (nodes, edges) = parse("alloc.zig", src);
        let class_nodes: Vec<_> = nodes.iter().filter(|n| n.kind == NodeKind::Class).collect();
        assert!(class_nodes.iter().any(|n| n.name == "Allocator"),
            "expected Allocator struct as Class node, got: {:?}",
            class_nodes.iter().map(|n| &n.name).collect::<Vec<_>>());
        let func_nodes: Vec<_> = nodes.iter().filter(|n| n.kind == NodeKind::Function).collect();
        assert!(func_nodes.iter().any(|n| n.name == "alloc"),
            "expected alloc function inside struct");
        let contains: Vec<_> = edges.iter().filter(|e| e.kind == EdgeKind::Contains).collect();
        assert!(!contains.is_empty(), "expected CONTAINS edges");
    }

    #[test]
    fn zig_enum_from_variable_declaration() {
        if !grammar_available("zig") { return; }
        let src = r#"
const Color = enum {
    red,
    green,
    blue,
};
"#;
        let (nodes, _) = parse("color.zig", src);
        let class_nodes: Vec<_> = nodes.iter().filter(|n| n.kind == NodeKind::Class).collect();
        assert!(class_nodes.iter().any(|n| n.name == "Color"),
            "expected Color enum as Class node");
    }

    #[test]
    fn zig_union_from_variable_declaration() {
        if !grammar_available("zig") { return; }
        let src = r#"
const Token = union(enum) {
    number: f64,
    string: []const u8,
};
"#;
        let (nodes, _) = parse("token.zig", src);
        let class_nodes: Vec<_> = nodes.iter().filter(|n| n.kind == NodeKind::Class).collect();
        assert!(class_nodes.iter().any(|n| n.name == "Token"),
            "expected Token union as Class node");
    }

    #[test]
    fn zig_function_declaration() {
        if !grammar_available("zig") { return; }
        let src = r#"
pub fn add(a: i32, b: i32) i32 {
    return a + b;
}

fn helper() void {}
"#;
        let (nodes, _) = parse("math.zig", src);
        let func_nodes: Vec<_> = nodes.iter().filter(|n| n.kind == NodeKind::Function).collect();
        assert!(func_nodes.iter().any(|n| n.name == "add"), "expected add function");
        assert!(func_nodes.iter().any(|n| n.name == "helper"), "expected helper function");
    }

    #[test]
    fn zig_test_declaration() {
        if !grammar_available("zig") { return; }
        let src = r#"
const std = @import("std");

fn add(a: i32, b: i32) i32 {
    return a + b;
}

test "add works correctly" {
    const result = add(2, 3);
    try std.testing.expect(result == 5);
}

test "add with zero" {
    try std.testing.expect(add(0, 5) == 5);
}
"#;
        let (nodes, _) = parse("math.zig", src);
        let test_nodes: Vec<_> = nodes.iter().filter(|n| n.is_test).collect();
        assert!(test_nodes.len() >= 2,
            "expected at least 2 test nodes, got {}: {:?}",
            test_nodes.len(), test_nodes.iter().map(|n| &n.name).collect::<Vec<_>>());
        assert!(test_nodes.iter().any(|n| n.name == "add works correctly"),
            "expected 'add works correctly' test");
        assert!(test_nodes.iter().any(|n| n.name == "add with zero"),
            "expected 'add with zero' test");
    }

    #[test]
    fn zig_import_edges() {
        if !grammar_available("zig") { return; }
        let src = r#"
const std = @import("std");
const mem = @import("mem");

pub fn main() void {
    std.debug.print("hello\n", .{});
}
"#;
        let (_, edges) = parse("main.zig", src);
        let imports: Vec<_> = edges.iter().filter(|e| e.kind == EdgeKind::ImportsFrom).collect();
        assert!(imports.iter().any(|e| e.target_qualified == "std"),
            "expected import edge for std, got: {:?}",
            imports.iter().map(|e| &e.target_qualified).collect::<Vec<_>>());
        assert!(imports.iter().any(|e| e.target_qualified == "mem"),
            "expected import edge for mem");
    }

    #[test]
    fn zig_call_edges() {
        if !grammar_available("zig") { return; }
        let src = r#"
fn helper() void {}

pub fn main() void {
    helper();
}
"#;
        let (_, edges) = parse("main.zig", src);
        let calls: Vec<_> = edges.iter().filter(|e| e.kind == EdgeKind::Calls).collect();
        assert!(!calls.is_empty(), "expected CALLS edges");
        assert!(calls.iter().any(|e| e.target_qualified.contains("helper")),
            "expected call to helper, got: {:?}",
            calls.iter().map(|e| &e.target_qualified).collect::<Vec<_>>());
    }

    #[test]
    fn zig_error_set_is_type() {
        if !grammar_available("zig") { return; }
        let src = r#"
const FileOpenError = error{
    AccessDenied,
    FileNotFound,
};
"#;
        let (nodes, _) = parse("errors.zig", src);
        let type_nodes: Vec<_> = nodes.iter().filter(|n| n.kind == NodeKind::Type).collect();
        assert!(type_nodes.iter().any(|n| n.name == "FileOpenError"),
            "expected FileOpenError as Type node, got: {:?}",
            type_nodes.iter().map(|n| &n.name).collect::<Vec<_>>());
    }

    #[test]
    fn zig_struct_with_methods_and_calls() {
        if !grammar_available("zig") { return; }
        let src = r#"
const std = @import("std");

const ArrayList = struct {
    items: []u8,

    pub fn init() ArrayList {
        return ArrayList{ .items = &[_]u8{} };
    }

    pub fn append(self: *ArrayList, item: u8) void {
        std.debug.print("appending\n", .{});
    }
};

pub fn main() void {
    var list = ArrayList.init();
    list.append(42);
}
"#;
        let (nodes, edges) = parse("list.zig", src);

        // Struct as class
        assert!(nodes.iter().any(|n| n.kind == NodeKind::Class && n.name == "ArrayList"),
            "expected ArrayList class");

        // Methods inside struct
        let func_names: Vec<_> = nodes.iter()
            .filter(|n| n.kind == NodeKind::Function)
            .map(|n| n.name.as_str())
            .collect();
        assert!(func_names.contains(&"init"), "expected init method");
        assert!(func_names.contains(&"append"), "expected append method");
        assert!(func_names.contains(&"main"), "expected main function");

        // Contains edges for struct methods
        let contains: Vec<_> = edges.iter()
            .filter(|e| e.kind == EdgeKind::Contains && e.target_qualified.contains("ArrayList"))
            .collect();
        assert!(!contains.is_empty(), "expected CONTAINS edges for ArrayList");
    }

    // -----------------------------------------------------------------------
    // Rust enhancement tests
    // -----------------------------------------------------------------------

    #[test]
    fn rust_mod_as_namespace() {
        if !grammar_available("rs") { return; }
        let src = r#"
mod utils {
    pub fn helper() -> i32 {
        42
    }

    pub struct Config {
    }
}

fn main() {
    utils::helper();
}
"#;
        let (nodes, _) = parse("main.rs", src);
        let func = nodes.iter().find(|n| n.name == "helper").expect("expected helper");
        assert!(func.qualified_name.contains("utils.helper"),
            "expected qualified name to include namespace 'utils', got: {}", func.qualified_name);
        let cls = nodes.iter().find(|n| n.name == "Config").expect("expected Config");
        assert!(cls.qualified_name.contains("utils.Config"),
            "expected Config qualified name to include namespace 'utils', got: {}", cls.qualified_name);
    }

    #[test]
    fn rust_nested_mod_namespaces() {
        if !grammar_available("rs") { return; }
        let src = r#"
mod outer {
    mod inner {
        pub fn deep() {}
    }
}
"#;
        let (nodes, _) = parse("lib.rs", src);
        let func = nodes.iter().find(|n| n.name == "deep").expect("expected deep");
        assert!(func.qualified_name.contains("outer.inner.deep"),
            "expected nested namespace, got: {}", func.qualified_name);
    }

    #[test]
    fn rust_extern_mod_import_edge() {
        if !grammar_available("rs") { return; }
        let src = r#"
pub mod config;
pub mod utils;

fn main() {}
"#;
        let (_, edges) = parse("lib.rs", src);
        let imports: Vec<_> = edges.iter()
            .filter(|e| e.kind == EdgeKind::ImportsFrom)
            .collect();
        assert!(imports.iter().any(|e| e.target_qualified == "config"),
            "expected ImportsFrom edge for config module, got: {:?}",
            imports.iter().map(|e| &e.target_qualified).collect::<Vec<_>>());
        assert!(imports.iter().any(|e| e.target_qualified == "utils"),
            "expected ImportsFrom edge for utils module");
    }

    #[test]
    fn rust_impl_trait_for_type_name() {
        if !grammar_available("rs") { return; }
        let src = r#"
struct Foo;

trait Display {
    fn fmt(&self) -> String;
}

impl Display for Foo {
    fn fmt(&self) -> String {
        String::new()
    }
}
"#;
        let (nodes, _) = parse("foo.rs", src);
        // The impl block should be named "Foo" (the type), not "Display" (the trait)
        let impl_node = nodes.iter()
            .find(|n| n.kind == NodeKind::Class && n.name == "Foo" && n.line_start > 5)
            .expect("expected impl block named 'Foo' (the type being implemented for)");
        assert!(impl_node.qualified_name.contains("Foo"),
            "impl qualified name should contain Foo, got: {}", impl_node.qualified_name);

        // There should be two fmt nodes: one in trait Display, one in impl Foo.
        // The impl's fmt should be qualified under Foo, not Display.
        let fmt_funcs: Vec<_> = nodes.iter().filter(|n| n.name == "fmt").collect();
        assert!(fmt_funcs.len() >= 2,
            "expected fmt in both trait and impl, got {} instances", fmt_funcs.len());
        assert!(fmt_funcs.iter().any(|n| n.qualified_name.contains("Foo.fmt")),
            "expected one fmt qualified under Foo, got: {:?}",
            fmt_funcs.iter().map(|n| &n.qualified_name).collect::<Vec<_>>());
        assert!(fmt_funcs.iter().any(|n| n.qualified_name.contains("Display.fmt")),
            "expected one fmt qualified under Display (trait signature)");
    }

    #[test]
    fn rust_impl_trait_inherits_edge() {
        if !grammar_available("rs") { return; }
        let src = r#"
struct MyStruct;

trait MyTrait {
    fn do_thing(&self);
}

impl MyTrait for MyStruct {
    fn do_thing(&self) {}
}
"#;
        let (_, edges) = parse("foo.rs", src);
        // Since MyTrait is a Type node (trait), the Inherits edge gets
        // reclassified to Implements by the post-pass.
        let impl_edges: Vec<_> = edges.iter()
            .filter(|e| e.kind == EdgeKind::Implements || e.kind == EdgeKind::Inherits)
            .collect();
        assert!(impl_edges.iter().any(|e| e.target_qualified == "MyTrait"),
            "expected Implements/Inherits edge to MyTrait, got: {:?}",
            impl_edges.iter().map(|e| (&e.kind, &e.source_qualified, &e.target_qualified)).collect::<Vec<_>>());
    }

    #[test]
    fn rust_trait_as_type_node() {
        if !grammar_available("rs") { return; }
        let src = r#"
pub trait Iterator {
    fn next(&mut self) -> Option<i32>;
    fn size_hint(&self) -> (usize, Option<usize>);
}
"#;
        let (nodes, _) = parse("iter.rs", src);
        let type_nodes: Vec<_> = nodes.iter().filter(|n| n.kind == NodeKind::Type).collect();
        assert!(type_nodes.iter().any(|n| n.name == "Iterator"),
            "expected Iterator as Type node, got: {:?}",
            type_nodes.iter().map(|n| &n.name).collect::<Vec<_>>());
        // Should NOT be a Class node
        let class_nodes: Vec<_> = nodes.iter()
            .filter(|n| n.kind == NodeKind::Class && n.name == "Iterator")
            .collect();
        assert!(class_nodes.is_empty(), "Iterator trait should not be a Class node");
    }

    #[test]
    fn rust_trait_method_signatures_indexed() {
        if !grammar_available("rs") { return; }
        let src = r#"
pub trait Repository {
    fn find_by_id(&self, id: u64) -> Option<String>;
    fn save(&mut self, item: String) -> bool;
}
"#;
        let (nodes, _) = parse("repo.rs", src);
        let func_nodes: Vec<_> = nodes.iter()
            .filter(|n| n.kind == NodeKind::Function)
            .collect();
        assert!(func_nodes.iter().any(|n| n.name == "find_by_id"),
            "expected find_by_id function signature in trait, got: {:?}",
            func_nodes.iter().map(|n| &n.name).collect::<Vec<_>>());
        assert!(func_nodes.iter().any(|n| n.name == "save"),
            "expected save function signature in trait");
    }

    #[test]
    fn rust_use_import_resolution() {
        if !grammar_available("rs") { return; }
        let src = r#"
use std::collections::HashMap;
use std::io::{Read, Write};
use crate::utils::helper as my_helper;

fn main() {
    let map = HashMap::new();
    my_helper();
}
"#;
        let (_, edges) = parse("main.rs", src);
        let calls: Vec<_> = edges.iter().filter(|e| e.kind == EdgeKind::Calls).collect();
        // HashMap call should resolve — check it's not just "HashMap"
        let hm_call = calls.iter().find(|e| e.target_qualified.contains("HashMap"));
        assert!(hm_call.is_some(), "expected call to HashMap, got: {:?}",
            calls.iter().map(|e| &e.target_qualified).collect::<Vec<_>>());
        // my_helper should resolve via alias
        let helper_call = calls.iter().find(|e| e.target_qualified.contains("my_helper") || e.target_qualified.contains("helper"));
        assert!(helper_call.is_some(), "expected call to my_helper/helper");
    }

    #[test]
    fn rust_plain_impl_name_unchanged() {
        if !grammar_available("rs") { return; }
        let src = r#"
struct Vec<T> {
}

impl Vec<i32> {
    fn push(&mut self, item: i32) {}
}
"#;
        let (nodes, _) = parse("vec.rs", src);
        // Plain impl (no trait) should still be named after the type
        let impl_node = nodes.iter()
            .find(|n| n.kind == NodeKind::Class && n.line_start > 3)
            .expect("expected impl block");
        assert!(impl_node.name == "Vec",
            "plain impl should be named Vec, got: {}", impl_node.name);
    }

    // -----------------------------------------------------------------------
    // TypeScript enhancement tests
    // -----------------------------------------------------------------------

    #[test]
    fn ts_public_field_definition_indexed() {
        if !grammar_available("ts") { return; }
        let src = r#"
class Config {
    public readonly name: string;
    public value: number = 42;

    constructor(name: string) {
        this.name = name;
    }
}
"#;
        let (nodes, _) = parse("config.ts", src);
        let func_nodes: Vec<_> = nodes.iter()
            .filter(|n| n.kind == NodeKind::Function)
            .collect();
        let func_names: Vec<&str> = func_nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(func_names.contains(&"name") || func_names.contains(&"value"),
            "expected class fields as Function nodes, got: {:?}", func_names);
    }

    #[test]
    fn ts_namespace_qualified_names() {
        if !grammar_available("ts") { return; }
        let src = r#"
namespace Models {
    export interface User {
        name: string;
    }

    export function createUser(): User {
        return { name: "" };
    }
}
"#;
        let (nodes, _) = parse("models.ts", src);
        // Check if namespace is reflected in qualified names
        let create_fn = nodes.iter().find(|n| n.name == "createUser");
        if let Some(f) = create_fn {
            assert!(f.qualified_name.contains("Models"),
                "expected Models namespace in qualified name, got: {}", f.qualified_name);
        }
    }
}
