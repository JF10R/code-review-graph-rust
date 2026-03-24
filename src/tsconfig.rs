//! TypeScript tsconfig.json path alias resolver.
//!
//! Parses tsconfig.json (including JSONC comments and `extends`),
//! resolves `paths` aliases to real file paths.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;

use crate::error::{CrgError, Result};

/// Extensions probed when resolving an alias target.
const PROBE_EXTENSIONS: &[&str] = &[".ts", ".tsx", ".js", ".jsx", ".vue"];

/// Tsconfig filenames to look for when walking up the directory tree.
const TSCONFIG_NAMES: &[&str] = &["tsconfig.json", "tsconfig.app.json"];

/// Resolver for TypeScript path aliases from tsconfig.json.
pub struct TsconfigResolver {
    /// Mapping from alias pattern to replacement paths.
    paths: Vec<(String, Vec<String>)>,
    /// Resolved base directory for path aliases.
    base_dir: PathBuf,
}

impl TsconfigResolver {
    /// Load and parse a tsconfig.json file.
    ///
    /// Handles JSONC comments, trailing commas, and `extends` chains.
    pub fn new(tsconfig_path: &Path) -> Result<Self> {
        let mut seen = HashSet::new();
        let compiler_options = resolve_extends(tsconfig_path, &mut seen)?;

        let base_url = compiler_options
            .get("baseUrl")
            .and_then(|v| v.as_str())
            .unwrap_or(".");

        let tsconfig_dir = tsconfig_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let base_dir = tsconfig_dir.join(base_url).canonicalize().unwrap_or_else(|_| {
            tsconfig_dir.join(base_url)
        });

        let raw_paths = compiler_options
            .get("paths")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();

        let paths = raw_paths
            .into_iter()
            .map(|(pattern, replacements)| {
                let reps = replacements
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                (pattern, reps)
            })
            .collect();

        Ok(Self { paths, base_dir })
    }

    /// Find the tsconfig.json for a source file by walking up the directory tree,
    /// then construct a resolver from it. Returns `None` if no tsconfig is found.
    pub fn for_file(file_path: &Path) -> Option<Self> {
        let mut current = file_path.parent()?.canonicalize().ok()?;

        loop {
            for name in TSCONFIG_NAMES {
                let candidate = current.join(name);
                if candidate.is_file() {
                    return Self::new(&candidate).ok();
                }
            }
            let parent = current.parent()?.to_path_buf();
            if parent == current {
                return None;
            }
            current = parent;
        }
    }

    /// Resolve an import specifier to a file path, if it matches any alias.
    pub fn resolve(&self, specifier: &str) -> Option<PathBuf> {
        match_and_probe(specifier, &self.paths, &self.base_dir)
    }
}

/// Strip JSONC comments (`//` and `/* */`) and trailing commas from a string.
pub fn strip_jsonc_comments(input: &str) -> String {
    static BLOCK: OnceLock<Regex> = OnceLock::new();
    static LINE: OnceLock<Regex> = OnceLock::new();
    static TRAILING: OnceLock<Regex> = OnceLock::new();

    let block = BLOCK.get_or_init(|| Regex::new(r"(?s)/\*.*?\*/").expect("valid regex"));
    let text = block.replace_all(input, "");
    let line = LINE.get_or_init(|| Regex::new(r"//[^\n]*").expect("valid regex"));
    let text = line.replace_all(&text, "");
    let trailing = TRAILING.get_or_init(|| Regex::new(r",\s*([\]\}])").expect("valid regex"));
    trailing.replace_all(&text, "$1").into_owned()
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Recursively resolve the tsconfig `extends` chain.
///
/// Returns the merged `compilerOptions` map with child values taking priority.
fn resolve_extends(
    tsconfig_path: &Path,
    seen: &mut HashSet<String>,
) -> Result<serde_json::Map<String, Value>> {
    let canonical = tsconfig_path
        .canonicalize()
        .unwrap_or_else(|_| tsconfig_path.to_path_buf())
        .to_string_lossy()
        .into_owned();

    if seen.contains(&canonical) {
        // Cycle detected — return empty options
        return Ok(serde_json::Map::new());
    }
    seen.insert(canonical);

    let raw = std::fs::read_to_string(tsconfig_path).map_err(CrgError::Io)?;

    let stripped = strip_jsonc_comments(&raw);
    let data: Value = serde_json::from_str(&stripped)?;

    let mut result: serde_json::Map<String, Value> = serde_json::Map::new();

    // Resolve parent first so child options win
    if let Some(extends) = data.get("extends").and_then(|v| v.as_str()) {
        if extends.starts_with('.') {
            let parent_path = tsconfig_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(extends);
            let parent_path = if parent_path.extension().is_none() {
                parent_path.with_extension("json")
            } else {
                parent_path
            };
            if parent_path.is_file() {
                let parent_opts = resolve_extends(&parent_path, seen)?;
                result.extend(parent_opts);
            }
        }
    }

    // Merge child compilerOptions (child wins)
    if let Some(child_opts) = data.get("compilerOptions").and_then(|v| v.as_object()) {
        result.extend(child_opts.clone());
    }

    Ok(result)
}

/// Match `import_str` against alias patterns and probe the filesystem.
fn match_and_probe(
    import_str: &str,
    paths: &[(String, Vec<String>)],
    base_dir: &Path,
) -> Option<PathBuf> {
    for (pattern, replacements) in paths {
        let suffix = match_pattern(pattern, import_str)?;

        for replacement in replacements {
            let mapped = if replacement.contains('*') {
                replacement.replacen('*', suffix, 1)
            } else {
                replacement.clone()
            };

            let candidate_base = base_dir.join(&mapped);
            let candidate_base = candidate_base
                .canonicalize()
                .unwrap_or(candidate_base);

            if let Some(found) = probe_path(&candidate_base) {
                return Some(found);
            }
        }
    }
    None
}

/// Return the wildcard-matched suffix if `pattern` matches `import_str`.
///
/// - `"@/*"` vs `"@/hooks/foo"` → `Some("hooks/foo")`
/// - `"@utils"` vs `"@utils"` → `Some("")`
/// - `"@/*"` vs `"react"` → `None`
fn match_pattern<'a>(pattern: &str, import_str: &'a str) -> Option<&'a str> {
    if !pattern.contains('*') {
        return if import_str == pattern { Some("") } else { None };
    }

    let star_pos = pattern.find('*').expect("checked above");
    let prefix = &pattern[..star_pos];
    let suffix_pat = &pattern[star_pos + 1..];

    if !import_str.starts_with(prefix) {
        return None;
    }
    if !suffix_pat.is_empty() && !import_str.ends_with(suffix_pat) {
        return None;
    }

    let end = if suffix_pat.is_empty() {
        import_str.len()
    } else {
        import_str.len() - suffix_pat.len()
    };
    Some(&import_str[prefix.len()..end])
}

/// Probe `base` and `base` + known extensions for an existing file.
fn probe_path(base: &Path) -> Option<PathBuf> {
    if base.is_file() {
        return Some(base.to_path_buf());
    }
    for ext in PROBE_EXTENSIONS {
        let candidate = base.with_extension(ext.trim_start_matches('.'));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    if base.is_dir() {
        for ext in PROBE_EXTENSIONS {
            let candidate = base.join(format!("index{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_block_comment() {
        let input = r#"{ /* comment */ "key": "value" }"#;
        let result = strip_jsonc_comments(input);
        let v: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["key"], "value");
    }

    #[test]
    fn strip_line_comment() {
        let input = "{\n  // line comment\n  \"key\": \"value\"\n}";
        let result = strip_jsonc_comments(input);
        let v: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["key"], "value");
    }

    #[test]
    fn strip_trailing_comma() {
        let input = r#"{ "key": "value", }"#;
        let result = strip_jsonc_comments(input);
        let v: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["key"], "value");
    }

    #[test]
    fn match_wildcard_pattern() {
        assert_eq!(match_pattern("@/*", "@/hooks/foo"), Some("hooks/foo"));
        assert_eq!(match_pattern("@utils", "@utils"), Some(""));
        assert_eq!(match_pattern("@/*", "react"), None);
        assert_eq!(match_pattern("@utils", "@utils/extra"), None);
    }
}
