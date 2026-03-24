//! Multi-language source code parser using tree-sitter.
//!
//! Extracts functions, classes, imports, calls, and inheritance from ASTs.
//! Supports 14 languages via native tree-sitter grammar crates.

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
        // tree-sitter-kotlin 0.3 depends on tree-sitter 0.20 (vs our 0.24).
        // The Language types are incompatible at the type level; we skip kotlin
        // grammar support until the crate is updated to tree-sitter 0.24+.
        "kotlin" => None,
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
// Node-type tables per language
// ---------------------------------------------------------------------------

fn class_types(lang: &str) -> &'static [&'static str] {
    match lang {
        "python" => &["class_definition"],
        "javascript" | "typescript" | "tsx" => &["class_declaration", "class"],
        "go" => &["type_declaration"],
        "rust" => &["struct_item", "enum_item", "impl_item"],
        "java" => &["class_declaration", "interface_declaration", "enum_declaration"],
        "c" => &["struct_specifier", "type_definition"],
        "cpp" => &["class_specifier", "struct_specifier"],
        "csharp" => &[
            "class_declaration",
            "interface_declaration",
            "enum_declaration",
            "struct_declaration",
        ],
        "ruby" => &["class", "module"],
        "kotlin" => &["class_declaration", "object_declaration"],
        "swift" => &["class_declaration", "struct_declaration", "protocol_declaration"],
        "php" => &["class_declaration", "interface_declaration"],
        _ => &[],
    }
}

fn function_types(lang: &str) -> &'static [&'static str] {
    match lang {
        "python" => &["function_definition"],
        "javascript" | "typescript" | "tsx" => {
            &["function_declaration", "method_definition", "arrow_function"]
        }
        "go" => &["function_declaration", "method_declaration"],
        "rust" => &["function_item"],
        "java" => &["method_declaration", "constructor_declaration"],
        "c" | "cpp" => &["function_definition"],
        "csharp" => &["method_declaration", "constructor_declaration"],
        "ruby" => &["method", "singleton_method"],
        "kotlin" => &["function_declaration"],
        "swift" => &["function_declaration"],
        "php" => &["function_definition", "method_declaration"],
        _ => &[],
    }
}

fn import_types(lang: &str) -> &'static [&'static str] {
    match lang {
        "python" => &["import_statement", "import_from_statement"],
        "javascript" | "typescript" | "tsx" => &["import_statement"],
        "go" => &["import_declaration"],
        "rust" => &["use_declaration"],
        "java" => &["import_declaration"],
        "c" | "cpp" => &["preproc_include"],
        "csharp" => &["using_directive"],
        "ruby" => &["call"],
        "kotlin" => &["import_header"],
        "swift" => &["import_declaration"],
        "php" => &["namespace_use_declaration"],
        _ => &[],
    }
}

fn call_types(lang: &str) -> &'static [&'static str] {
    match lang {
        "python" => &["call"],
        "javascript" | "typescript" | "tsx" => &["call_expression", "new_expression"],
        "go" => &["call_expression"],
        "rust" => &["call_expression", "macro_invocation"],
        "java" => &["method_invocation", "object_creation_expression"],
        "c" | "cpp" => &["call_expression"],
        "csharp" => &["invocation_expression", "object_creation_expression"],
        "ruby" => &["call", "method_call"],
        "kotlin" => &["call_expression"],
        "swift" => &["call_expression"],
        "php" => &["function_call_expression", "member_call_expression"],
        _ => &[],
    }
}

/// Pre-computed node-kind sets for a single language, built once per parse.
/// Passed by reference into the recursive walk to avoid per-call allocations.
struct LangTypes {
    cls: HashSet<&'static str>,
    func: HashSet<&'static str>,
    imp: HashSet<&'static str>,
    call: HashSet<&'static str>,
}

impl LangTypes {
    fn new(lang: &str) -> Self {
        Self {
            cls: class_types(lang).iter().copied().collect(),
            func: function_types(lang).iter().copied().collect(),
            imp: import_types(lang).iter().copied().collect(),
            call: call_types(lang).iter().copied().collect(),
        }
    }
}

// Lazily-built cache: one LangTypes per language, constructed at most once.
static LANG_TYPES_CACHE: OnceLock<HashMap<&'static str, LangTypes>> = OnceLock::new();

fn get_lang_types(language: &str) -> &'static LangTypes {
    let cache = LANG_TYPES_CACHE.get_or_init(|| {
        let mut m = HashMap::new();
        for lang in &[
            "python", "javascript", "typescript", "tsx", "rust", "go", "java",
            "c", "cpp", "csharp", "ruby", "php", "kotlin", "swift",
        ] {
            m.insert(*lang, LangTypes::new(lang));
        }
        m
    });
    cache.get(language).expect("unsupported language")
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
    if name.starts_with("test_") || name.starts_with("Test") || name.ends_with("_test") {
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
    let name_kinds = &[
        "identifier",
        "name",
        "type_identifier",
        "property_identifier",
        "simple_identifier",
        "constant",
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
                let mut c2 = child.walk();
                for stmt in child.children(&mut c2) {
                    if stmt.kind() == "expression_statement" {
                        let mut c3 = stmt.walk();
                        for expr in stmt.children(&mut c3) {
                            if matches!(expr.kind(), "string" | "concatenated_string") {
                                let t = node_text(&expr, source);
                                return t.trim_matches(|c| c == '"' || c == '\'').to_owned();
                            }
                        }
                    }
                    break; // only check first statement
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
                if matches!(child.kind(), "extends_clause" | "implements_clause") {
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
    lt: &LangTypes,
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
                .find(|n| lt.func.contains(n.kind()) || lt.cls.contains(n.kind()))
                .map(|n| n.kind().to_owned())
        } else {
            None
        };
        let effective_kind: &str = inner_kind.as_deref().unwrap_or(node_type);

        if lt.func.contains(effective_kind) || lt.cls.contains(effective_kind) {
            let kind_str = if lt.cls.contains(effective_kind) { "class" } else { "function" };
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

        if lt.imp.contains(node_type) {
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
    lt: &'a LangTypes,
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
        if lt.cls.contains(node_type) {
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
        if lt.func.contains(node_type) {
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
        if lt.imp.contains(node_type) {
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
        if lt.call.contains(node_type) {
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

        let lt = get_lang_types(language);
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

    #[test]
    fn detect_python() {
        let parser = CodeParser::new();
        assert_eq!(parser.detect_language(Path::new("foo.py")), Some("python"));
        assert_eq!(parser.detect_language(Path::new("bar.rs")), Some("rust"));
        assert_eq!(parser.detect_language(Path::new("baz.txt")), None);
    }

    #[test]
    fn python_class_and_function() {
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
        let (nodes, edges) = parse("main.go", src);
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"Hello"));
        assert!(names.contains(&"main"));
        let calls: Vec<_> = edges.iter().filter(|e| e.kind == EdgeKind::Calls).collect();
        assert!(!calls.is_empty(), "expected CALLS edge in go");
    }
}
