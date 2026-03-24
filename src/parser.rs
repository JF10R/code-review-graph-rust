//! Multi-language source code parser using tree-sitter.
//!
//! Extracts functions, classes, imports, calls, and inheritance from ASTs.
//! Supports 14+ languages via native tree-sitter grammar crates.

use std::path::Path;

use crate::error::Result;
use crate::types::{EdgeInfo, NodeInfo};

/// Multi-language code parser backed by tree-sitter.
pub struct CodeParser {
    // Cached parsers per language — lazily initialized.
    // In Rust, tree_sitter::Parser is not Send, so we store per-thread.
    _private: (),
}

impl CodeParser {
    pub fn new() -> Self {
        Self { _private: () }
    }

    /// Detect the programming language from a file extension.
    /// Returns `None` for unsupported or non-source files.
    pub fn detect_language(&self, path: &Path) -> Option<&'static str> {
        let ext = path.extension()?.to_str()?;
        match ext {
            "py" => Some("python"),
            "js" | "mjs" | "cjs" => Some("javascript"),
            "ts" | "mts" | "cts" => Some("typescript"),
            "tsx" => Some("tsx"),
            "rs" => Some("rust"),
            "go" => Some("go"),
            "java" => Some("java"),
            "c" | "h" => Some("c"),
            "cpp" | "cc" | "cxx" | "hpp" | "hxx" | "hh" => Some("cpp"),
            "cs" => Some("c_sharp"),
            "rb" => Some("ruby"),
            "php" => Some("php"),
            "kt" | "kts" => Some("kotlin"),
            "swift" => Some("swift"),
            _ => None,
        }
    }

    /// Parse source bytes and extract nodes + edges.
    pub fn parse_bytes(
        &self,
        path: &Path,
        source: &[u8],
    ) -> Result<(Vec<NodeInfo>, Vec<EdgeInfo>)> {
        let _ = (path, source);
        todo!("Implement tree-sitter parsing")
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
