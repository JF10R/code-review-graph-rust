//! Centralized path normalization for cross-platform consistency.
//!
//! Windows `canonicalize()` produces `\\?\D:\...` UNC paths. The graph stores
//! paths as HashMap keys, so inconsistent forms cause lookup misses.
//! All paths are normalized to forward slashes with no UNC prefix.

/// Normalize a path string: strip \\?\ or //?/ UNC prefix, convert backslashes
/// to forward slashes, remove trailing slash.
pub fn normalize_path(path: &str) -> String {
    let s = path.strip_prefix(r"\\?\")
        .or_else(|| path.strip_prefix("//?/"))
        .unwrap_or(path);
    let normalized = s.replace('\\', "/");
    normalized.trim_end_matches('/').to_string()
}

/// Normalize a qualified name (file_path::name or file_path::Class.method).
/// Only the file_path prefix (before `::`) is normalized.
pub fn normalize_qualified(qn: &str) -> String {
    if let Some(idx) = qn.find("::") {
        format!("{}::{}", normalize_path(&qn[..idx]), &qn[idx+2..])
    } else {
        normalize_path(qn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_path_unc() {
        assert_eq!(normalize_path(r"\\?\C:\foo\bar"), "C:/foo/bar");
    }

    #[test]
    fn test_normalize_path_fwd_unc() {
        assert_eq!(normalize_path("//?/C:/foo/bar"), "C:/foo/bar");
    }

    #[test]
    fn test_normalize_path_backslashes() {
        assert_eq!(normalize_path(r"C:\foo\bar"), "C:/foo/bar");
    }

    #[test]
    fn test_normalize_path_trailing_slash() {
        assert_eq!(normalize_path("C:/foo/bar/"), "C:/foo/bar");
    }

    #[test]
    fn test_normalize_path_already_normal() {
        assert_eq!(normalize_path("C:/foo/bar"), "C:/foo/bar");
    }

    #[test]
    fn test_normalize_qualified_with_separator() {
        assert_eq!(
            normalize_qualified(r"\\?\C:\foo\bar::MyClass"),
            "C:/foo/bar::MyClass"
        );
    }

    #[test]
    fn test_normalize_qualified_no_separator() {
        assert_eq!(normalize_qualified(r"C:\foo"), "C:/foo");
    }
}
