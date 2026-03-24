//! Persistent configuration for code-review-graph.
//!
//! Stored at:
//!   Linux/Mac  — `~/.config/code-review-graph/config.json`
//!   Windows    — `%APPDATA%/code-review-graph/config.json`

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Known config keys
// ---------------------------------------------------------------------------

pub const VALID_KEYS: &[&str] = &[
    "embedding-provider",
    "openai-api-key",
    "voyage-api-key",
    "gemini-api-key",
    "embedding-model",
];

pub fn validate_config_key(key: &str) -> anyhow::Result<()> {
    if !VALID_KEYS.contains(&key) {
        anyhow::bail!(
            "Unknown config key '{}'. Valid keys: {}",
            key,
            VALID_KEYS.join(", ")
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// AppConfig
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Default)]
pub struct AppConfig {
    #[serde(flatten)]
    pub values: HashMap<String, String>,
}

impl AppConfig {
    /// Path to the config file (platform-appropriate).
    pub fn config_path() -> PathBuf {
        let base = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."));
        base.join("code-review-graph").join("config.json")
    }

    /// Load config from disk. Returns an empty config if the file doesn't exist.
    pub fn load() -> Self {
        let path = Self::config_path();
        match std::fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Persist config to disk.
    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(&self)?;
        std::fs::write(&path, [json.as_bytes(), b"\n"].concat())?;
        Ok(())
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(|s| s.as_str())
    }

    pub fn set(&mut self, key: &str, value: &str) {
        self.values.insert(key.to_string(), value.to_string());
    }

    pub fn remove(&mut self, key: &str) {
        self.values.remove(key);
    }
}

// ---------------------------------------------------------------------------
// Display helpers
// ---------------------------------------------------------------------------

/// Mask an API key for display: `sk-proj-abc123def456` → `sk-proj-***456`
pub fn mask_key(key: &str) -> String {
    if key.len() <= 8 {
        return "***".to_string();
    }
    let suffix = &key[key.len() - 3..];
    let prefix_end = key.find('-').map(|i| i + 1).unwrap_or(3);
    format!("{}***{}", &key[..prefix_end], suffix)
}

pub fn display_value(config_key: &str, value: &str) -> String {
    if config_key.contains("key") {
        mask_key(value)
    } else {
        value.to_string()
    }
}
