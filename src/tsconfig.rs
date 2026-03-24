//! TypeScript tsconfig.json path alias resolver.
//!
//! Parses tsconfig.json (including JSONC comments and `extends`),
//! resolves `paths` aliases to real file paths.

use std::path::{Path, PathBuf};

use crate::error::Result;

/// Resolver for TypeScript path aliases from tsconfig.json.
pub struct TsconfigResolver {
    /// Mapping from alias pattern to replacement paths.
    paths: Vec<(String, Vec<String>)>,
    /// Base URL for path resolution.
    base_url: PathBuf,
}

impl TsconfigResolver {
    /// Load and parse a tsconfig.json file.
    pub fn new(tsconfig_path: &Path) -> Result<Self> {
        let _ = tsconfig_path;
        todo!()
    }

    /// Resolve an import specifier to a file path, if it matches any alias.
    pub fn resolve(&self, specifier: &str) -> Option<PathBuf> {
        let _ = specifier;
        todo!()
    }
}

/// Strip JSONC comments (// and /* */) from a string.
pub fn strip_jsonc_comments(input: &str) -> String {
    let _ = input;
    todo!()
}
