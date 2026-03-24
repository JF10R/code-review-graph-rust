//! Shared postcard+zstd persistence helpers.
//!
//! Both `graph.rs` and `embeddings.rs` persist data as
//! `MAGIC (4B) | CRC-32 LE (4B) | zstd(postcard(T))`.
//! This module provides the generic save/load functions.

use std::io::Write as _;
use std::path::Path;

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::error::{CrgError, Result};

/// Magic bytes at the start of every `.bin.zst` file.
const MAGIC: &[u8; 4] = b"CRG\x01";

/// Serialize `data` with postcard, compress with zstd, and atomically write to `path`.
///
/// `label` is used in error messages (e.g. `"graph"`, `"embeddings"`).
pub fn save_blob<T: Serialize>(data: &T, path: &Path, label: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let payload = postcard::to_allocvec(data)?;
    let compressed = zstd::encode_all(&payload[..], 3).map_err(CrgError::Io)?;
    let crc = crc32fast::hash(&compressed);

    let tmp = tempfile::NamedTempFile::new_in(path.parent().unwrap_or(Path::new(".")))?;
    {
        let mut f = tmp.as_file();
        f.write_all(MAGIC)?;
        f.write_all(&crc.to_le_bytes())?;
        f.write_all(&compressed)?;
        f.flush()?;
    }
    tmp.persist(path)
        .map_err(|e| CrgError::Io(e.error))?;

    log::debug!("{label}: saved {} bytes to {}", compressed.len() + 8, path.display());
    Ok(())
}

/// Read and deserialize a `.bin.zst` blob written by [`save_blob`].
pub fn load_blob<T: DeserializeOwned>(path: &Path, label: &str) -> Result<T> {
    let bytes = std::fs::read(path)?;
    if bytes.len() < 8 {
        return Err(CrgError::Other(format!("{label} file too short")));
    }
    if &bytes[0..4] != MAGIC {
        return Err(CrgError::Other(format!("corrupt {label} file (bad magic)")));
    }
    let stored_crc = u32::from_le_bytes(
        bytes[4..8]
            .try_into()
            .map_err(|_| CrgError::Other(format!("corrupt {label} file (bad crc field)")))?,
    );
    let compressed = &bytes[8..];
    if crc32fast::hash(compressed) != stored_crc {
        return Err(CrgError::Other(format!("{label} file CRC mismatch")));
    }
    let decompressed = zstd::decode_all(compressed).map_err(CrgError::Io)?;
    let data: T = postcard::from_bytes(&decompressed)?;
    Ok(data)
}
