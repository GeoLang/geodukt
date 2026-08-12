//! Incremental processing — hash-based change detection.

use crate::hex;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// State store for incremental processing — tracks hashes of source files.
#[derive(Debug, Default)]
pub struct IncrementalState {
    /// Map from source name to content hash.
    pub hashes: HashMap<String, String>,
}

impl IncrementalState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load state from a JSON file.
    pub fn load(path: &Path) -> Self {
        match fs::read_to_string(path) {
            Ok(content) => {
                let hashes: HashMap<String, String> =
                    serde_json::from_str(&content).unwrap_or_default();
                Self { hashes }
            }
            Err(_) => Self::default(),
        }
    }

    /// Save state to a JSON file.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let json = serde_json::to_string_pretty(&self.hashes)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, json)
    }

    /// Check if a source file has changed since the last run.
    pub fn has_changed(&self, name: &str, path: &str) -> bool {
        let current_hash = hash_file(path);
        match self.hashes.get(name) {
            Some(stored) => *stored != current_hash,
            None => true,
        }
    }

    /// Update the hash for a source.
    pub fn update(&mut self, name: &str, path: &str) {
        let hash = hash_file(path);
        self.hashes.insert(name.to_string(), hash);
    }

    /// Determine which sources need reprocessing.
    pub fn changed_sources(&self, sources: &[(String, String)]) -> Vec<String> {
        sources
            .iter()
            .filter(|(name, path)| self.has_changed(name, path))
            .map(|(name, _)| name.clone())
            .collect()
    }
}

/// Compute SHA-256 hash of a file's contents.
fn hash_file(path: &str) -> String {
    match fs::read(path) {
        Ok(content) => {
            let mut hasher = Sha256::new();
            hasher.update(&content);
            hex::encode_lowercase(&hasher.finalize())
        }
        Err(_) => String::new(),
    }
}

/// Compute SHA-256 hash of arbitrary bytes.
pub fn hash_bytes(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode_lowercase(&hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_hash_file() {
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(b"hello world").unwrap();
        let h1 = hash_file(tmp.path().to_str().unwrap());
        assert!(!h1.is_empty());
        assert_eq!(h1.len(), 64); // SHA-256 hex
    }

    // inputs picked so each digest holds a byte below 0x10, which catches a dropped zero pad
    #[test]
    fn test_hash_file_golden() {
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(b"geodukt incremental golden input 1")
            .unwrap();
        assert_eq!(
            hash_file(tmp.path().to_str().unwrap()),
            "394526f6803c691ceb0b79928793a7e9046e18fd9c61d5bccadf5538286104c2"
        );
    }

    #[test]
    fn test_hash_bytes_golden() {
        assert_eq!(
            hash_bytes(b"geodukt hash_bytes golden input 1"),
            "d2e11e0f20c653e5109338cdd2b35946b08a44748a9a8b0a5a9d554fa4321619"
        );
    }

    #[test]
    fn test_incremental_state() {
        let mut state = IncrementalState::new();
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(b"data").unwrap();
        let path = tmp.path().to_str().unwrap();

        assert!(state.has_changed("src", path));
        state.update("src", path);
        assert!(!state.has_changed("src", path));
    }
}
