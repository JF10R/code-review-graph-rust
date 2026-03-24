//! Multi-language source code parser using tree-sitter.
//!
//! Extracts functions, classes, imports, calls, and inheritance from ASTs.
//! Supports 14 languages (+ Vue SFC) via native tree-sitter grammar crates.
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
];

/// Parse a `.scm` source and collect node-kind names per tag category into
/// four `HashSet`s: (classes, functions, imports, calls).
///
/// Only the outermost node kind (first bare word inside the leading `(`) of
/// each S-expression line is collected; child field constraints are ignored.
fn parse_scm(scm: &str) -> (HashSet<String>, HashSet<String>, HashSet<String>, HashSet<String>) {
    let mut cls:  HashSet<String> = HashSet::new();
    let mut func: HashSet<String> = HashSet::new();
    let mut imp:  HashSet<String> = HashSet::new();
    let mut call: HashSet<String> = HashSet::new();

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
            } else {
                // Tag not found on this line yet — put the kind back.
                current_kind = Some(kind);
            }
        }
    }

    (cls, func, imp, call)
}

/// Pre-computed node-kind sets for a single language, derived from the
/// language's embedded `.scm` query file.  Passed by reference into the
/// recursive walk to avoid per-call allocations.
struct CompiledQueries {
    cls:  HashSet<String>,
    func: HashSet<String>,
    imp:  HashSet<String>,
    call: HashSet<String>,
}

impl CompiledQueries {
    fn from_scm(scm: &str) -> Self {
        let (cls, func, imp, call) = parse_scm(scm);
        Self { cls, func, imp, call }
    }

    #[inline] fn is_class(&self, kind: &str)    -> bool { self.cls.contains(kind)  }
    #[inline] fn is_func(&self, kind: &str)     -> bool { self.func.contains(kind) }
    #[inline] fn is_import(&self, kind: &str)   -> bool { self.imp.contains(kind)  }
    #[inline] fn is_call(&self, kind: &str)     -> bool { self.call.contains(kind) }
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

// ---------------------------------------------------------------------------
// SHA-256 body hash
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
        "java" | "csharp" | "kotlin" => {
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
        "java" | "csharp" => {
            let parts: Vec<&str> = text.split_whitespace().collect();
            if parts.len() >= 2 {
                imports.push(parts.last().unwrap().trim_end_matches(';').to_owned());
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
    let mut cur = node.walk();
    let first = node.children(&mut cur).next()?;

    if first.kind() == "identifier" {
        return Some(node_text(&first, source).to_owned());
    }

    let member_types = &[
        "attribute",
        "member_expression",
        "field_expression",
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

fn qualify(name: &str, file_path: &str, enclosing_class: Option<&str>) -> String {
    match enclosing_class {
        Some(cls) => format!("{file_path}::{cls}.{name}"),
        None => format!("{file_path}::{name}"),
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
        return qualify(call_name, file_path, None);
    }
    if let Some(module) = import_map.get(call_name) {
        // Best-effort: qualify against the module path
        return qualify(call_name, module, None);
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
    depth: usize,
) {
    if depth > MAX_AST_DEPTH {
        return;
    }

    let WalkCtx { source, language, file_path, import_map, defined_names, lt } = ctx;

    let mut cur = root.walk();
    for child in root.children(&mut cur) {
        let node_type = child.kind();

        // --- Classes ---
        if lt.is_class(node_type) {
            if let Some(name) = get_name(&child, language, "class", source) {
                let line_start = child.start_position().row + 1;
                let line_end = child.end_position().row + 1;
                let qualified = qualify(&name, file_path, enclosing_class);

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

                extract_from_tree(&child, ctx, nodes, edges, Some(&name), None, depth + 1);
                continue;
            }
        }

        // --- Functions ---
        if lt.is_func(node_type) {
            if let Some(name) = get_name(&child, language, "function", source) {
                let is_test = is_test_function(&name, file_path);
                let kind = if is_test { NodeKind::Test } else { NodeKind::Function };
                let qualified = qualify(&name, file_path, enclosing_class);
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
                    Some(cls) => qualify(cls, file_path, None),
                    None => file_path.to_string(),
                };
                edges.push(EdgeInfo {
                    source_qualified: container,
                    target_qualified: qualified.clone(),
                    kind: EdgeKind::Contains,
                    file_path: file_path.to_string(),
                    line: line_start,
                });

                extract_from_tree(&child, ctx, nodes, edges, enclosing_class, Some(&name), depth + 1);
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
                    let qualified = qualify(&synthetic_name, file_path, enclosing_class);
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
                        Some(f) => qualify(f, file_path, enclosing_class),
                        None => file_path.to_string(),
                    };
                    edges.push(EdgeInfo {
                        source_qualified: container,
                        target_qualified: qualified.clone(),
                        kind: EdgeKind::Contains,
                        file_path: file_path.to_string(),
                        line: line_start,
                    });

                    extract_from_tree(&child, ctx, nodes, edges, enclosing_class, Some(&synthetic_name), depth + 1);
                    continue;
                }

                if let Some(func) = enclosing_func {
                    let caller = qualify(func, file_path, enclosing_class);
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
        extract_from_tree(&child, ctx, nodes, edges, enclosing_class, enclosing_func, depth + 1);
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
    let node_type = node.kind();

    // Pattern 1: JSX component instantiation — <ComponentName /> or <ComponentName>
    // Only PascalCase names; lowercase are HTML intrinsics and skipped.
    if matches!(node_type, "jsx_self_closing_element" | "jsx_opening_element") {
        let mut cur = node.walk();
        let component_name: Option<String> = node.children(&mut cur).find_map(|child| {
            let text = node_text(&child, source);
            if matches!(child.kind(), "identifier" | "member_expression")
                && text.chars().next().map_or(false, |c| c.is_uppercase())
            {
                Some(text.to_string())
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
                let is_express_route = obj_name.map_or(false, |o| EXPRESS_OBJECTS.contains(&o))
                    && method_name.map_or(false, |m| HTTP_METHODS.contains(&m));

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
                    method_name.map_or(false, |m| EVENT_LISTENER_METHODS.contains(&m));

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
        walk_for_framework_edges(&child, source, nodes, file_path, edges);
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

        // --- Vue SFC: 2-pass parse ---
        // Pass 1: regex-extract the <script> block.
        // Pass 2: re-parse that content with the JS/TS grammar.
        if language == "vue" {
            return self.parse_vue_sfc(path, source, file_path);
        }

        let mut parser = make_parser(language).ok_or_else(|| {
            CrgError::TreeSitter(format!("No grammar for language: {language}"))
        })?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| CrgError::TreeSitter("parse returned None".into()))?;

        let root = tree.root_node();
        let test_file = is_test_file(file_path);
        let line_count = source.iter().filter(|&&b| b == b'\n').count() + 1;

        let mut nodes: Vec<NodeInfo> = Vec::new();
        let mut edges: Vec<EdgeInfo> = Vec::new();

        // File node
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
        extract_from_tree(&root, &ctx, &mut nodes, &mut edges, None, None, 0);

        edges = resolve_call_targets_pass(&nodes, edges);

        // Framework-aware edge inference (JSX, Express, event emitters, pytest)
        edges.extend(framework_edges_pass(&nodes, &tree.root_node(), source, language, file_path));

        if test_file {
            let tested_by = emit_tested_by_edges(&nodes, &edges);
            edges.extend(tested_by);
        }

        Ok((nodes, edges))
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
        extract_from_tree(&root, &ctx, &mut script_nodes, &mut script_edges, None, None, 0);

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
}
