#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use std::path::Path;

#[derive(Arbitrary, Debug)]
struct FuzzInput {
    source: Vec<u8>,
    extension_idx: u8,
}

const EXTENSIONS: &[&str] = &[
    "py", "ts", "tsx", "js", "jsx", "rs", "go", "java",
    "c", "cpp", "cs", "rb", "php", "kt", "swift", "vue",
    "unknown",
];

fuzz_target!(|input: FuzzInput| {
    let ext = EXTENSIONS[input.extension_idx as usize % EXTENSIONS.len()];
    let path = Path::new("fuzz_input").with_extension(ext);
    let parser = code_review_graph::parser::CodeParser::new();
    // parse_bytes should never panic — any panic is a bug
    let _ = parser.parse_bytes(&path, &input.source);
});
