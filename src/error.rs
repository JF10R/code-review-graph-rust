//! Error types for code-review-graph.

use thiserror::Error;

#[derive(Error, Debug)]
pub enum CrgError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Bincode encode error: {0}")]
    BincodeEncode(#[from] bincode::error::EncodeError),

    #[error("Bincode decode error: {0}")]
    BincodeDecode(#[from] bincode::error::DecodeError),

    #[error("Tree-sitter error: {0}")]
    TreeSitter(String),

    #[error("Git error: {0}")]
    Git(String),

    #[error("Invalid repo root: {0}")]
    InvalidRepoRoot(String),

    #[error("Tool error: {0}")]
    Tool(String),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, CrgError>;
