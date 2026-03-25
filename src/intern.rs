//! Global string interner for high-frequency fields.
//!
//! `file_path` and `language` fields repeat across thousands of nodes.
//! Interning reduces memory (~14 unique languages, ~1000 unique file paths
//! in a typical repo) and makes equality checks O(1).

use lasso::{Spur, ThreadedRodeo};
use std::sync::OnceLock;

static INTERNER: OnceLock<ThreadedRodeo> = OnceLock::new();

pub fn interner() -> &'static ThreadedRodeo {
    INTERNER.get_or_init(ThreadedRodeo::default)
}

pub fn intern(s: &str) -> Spur {
    interner().get_or_intern(s)
}

pub fn resolve(key: Spur) -> &'static str {
    interner().resolve(&key)
}
