//! Session index cache — avoids re-parsing JSONL files on every `ccs session list` run.
//!
//! The cache is stored at `{config_dir}/session_index.json` and keyed by canonical
//! file path. An entry is considered valid only when both `file_size` and `mtime_secs`
//! match the on-disk file; any mismatch triggers a fresh parse.

use crate::handlers::session::SessionSummary;
use log::{debug, warn};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const CACHE_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct SessionIndexCache {
    pub version: u32,
    /// key = canonical file path as UTF-8 string (lossy)
    pub entries: HashMap<String, CachedEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedEntry {
    pub file_size: u64,
    pub mtime_secs: i64,
    pub source: String,
    pub session_id: String,
    pub title: String,
    pub project_name: String,
    pub project_dir: String,
    pub message_count: usize,
    pub user_message_count: usize,
    pub assistant_message_count: usize,
    pub first_timestamp: Option<String>,
    pub last_activity: Option<String>,
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

impl SessionIndexCache {
    /// Create an empty cache with the current version.
    fn empty() -> Self {
        SessionIndexCache {
            version: CACHE_VERSION,
            entries: HashMap::new(),
        }
    }

    /// Load the cache from `{config_dir}/session_index.json`.
    ///
    /// Returns an empty cache on any error (missing file, parse failure,
    /// version mismatch). Never panics.
    pub fn load(config_dir: &Path) -> Self {
        let path = cache_path(config_dir);

        let data = match std::fs::read(&path) {
            Ok(d) => d,
            Err(e) => {
                debug!("Session cache not found or unreadable ({path:?}): {e}");
                return Self::empty();
            }
        };

        let cache: SessionIndexCache = match serde_json::from_slice(&data) {
            Ok(c) => c,
            Err(e) => {
                warn!("Session cache corrupt ({path:?}): {e} — starting fresh");
                return Self::empty();
            }
        };

        if cache.version != CACHE_VERSION {
            warn!(
                "Session cache version mismatch (got {}, want {}) — starting fresh",
                cache.version, CACHE_VERSION
            );
            return Self::empty();
        }

        debug!("Loaded session cache with {} entries", cache.entries.len());
        cache
    }

    /// Save the cache to `{config_dir}/session_index.json`.
    ///
    /// Creates `config_dir` if it does not exist. Logs warnings on error
    /// but does not propagate them — the cache is advisory.
    pub fn save(&self, config_dir: &Path) {
        if let Err(e) = std::fs::create_dir_all(config_dir) {
            warn!("Cannot create config dir {config_dir:?}: {e}");
            return;
        }

        let path = cache_path(config_dir);
        let json = match serde_json::to_vec(self) {
            Ok(j) => j,
            Err(e) => {
                warn!("Failed to serialize session cache: {e}");
                return;
            }
        };

        if let Err(e) = std::fs::write(&path, &json) {
            warn!("Failed to write session cache to {path:?}: {e}");
        } else {
            debug!(
                "Saved session cache ({} entries) to {path:?}",
                self.entries.len()
            );
        }
    }

    /// Return a cached [`SessionSummary`] for the given file **only if** both
    /// `file_size` and `mtime_secs` match the stored entry exactly.
    ///
    /// `key` must be the same string used for `insert` (typically
    /// `file_path.to_string_lossy()`).
    pub fn lookup(
        &self,
        key: &str,
        file_path: &Path,
        file_size: u64,
        mtime_secs: i64,
    ) -> Option<SessionSummary> {
        let entry = self.entries.get(key)?;

        if entry.file_size != file_size || entry.mtime_secs != mtime_secs {
            return None;
        }

        Some(SessionSummary {
            source: entry.source.clone(),
            session_id: entry.session_id.clone(),
            title: entry.title.clone(),
            project_name: entry.project_name.clone(),
            project_dir: PathBuf::from(&entry.project_dir),
            file_path: file_path.to_path_buf(),
            message_count: entry.message_count,
            user_message_count: entry.user_message_count,
            assistant_message_count: entry.assistant_message_count,
            first_timestamp: entry.first_timestamp.clone(),
            last_activity: entry.last_activity.clone(),
            file_size,
        })
    }

    /// Insert or update a cache entry.
    ///
    /// `key` should be `file_path.to_string_lossy()` — the same value used for
    /// `lookup` and `retain_existing`.
    pub fn insert(
        &mut self,
        key: String,
        file_size: u64,
        mtime_secs: i64,
        summary: &SessionSummary,
    ) {
        self.entries.insert(
            key,
            CachedEntry {
                file_size,
                mtime_secs,
                source: summary.source.clone(),
                session_id: summary.session_id.clone(),
                title: summary.title.clone(),
                project_name: summary.project_name.clone(),
                project_dir: summary.project_dir.to_string_lossy().to_string(),
                message_count: summary.message_count,
                user_message_count: summary.user_message_count,
                assistant_message_count: summary.assistant_message_count,
                first_timestamp: summary.first_timestamp.clone(),
                last_activity: summary.last_activity.clone(),
            },
        );
    }

    /// Remove all entries whose keys are **not** present in `seen_paths`.
    ///
    /// Call this after a full scan to evict stale entries for deleted files.
    pub fn retain_existing(&mut self, seen_paths: &HashSet<String>) {
        let before = self.entries.len();
        self.entries.retain(|k, _| seen_paths.contains(k));
        let removed = before - self.entries.len();
        if removed > 0 {
            debug!("Pruned {removed} stale entries from session cache");
        }
    }
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Extract the last-modified time from file metadata as seconds since UNIX epoch.
pub fn mtime_secs(meta: &std::fs::Metadata) -> Option<i64> {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
}

fn cache_path(config_dir: &Path) -> PathBuf {
    config_dir.join("session_index.json")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_summary(file_path: &Path, project_dir: &Path) -> SessionSummary {
        SessionSummary {
            source: "claude".to_string(),
            session_id: "test-session-id".to_string(),
            title: "Test session title".to_string(),
            project_name: "my-project".to_string(),
            project_dir: project_dir.to_path_buf(),
            file_path: file_path.to_path_buf(),
            message_count: 10,
            user_message_count: 5,
            assistant_message_count: 5,
            first_timestamp: Some("2024-01-01T00:00:00Z".to_string()),
            last_activity: Some("2024-01-02T00:00:00Z".to_string()),
            file_size: 1234,
        }
    }

    fn path_key(p: &Path) -> String {
        p.to_string_lossy().to_string()
    }

    #[test]
    fn test_cold_cache_returns_none() {
        let dir = TempDir::new().unwrap();
        let cache = SessionIndexCache::load(dir.path());
        let fake_path = dir.path().join("fake.jsonl");
        assert!(cache
            .lookup(&path_key(&fake_path), &fake_path, 100, 999)
            .is_none());
    }

    #[test]
    fn test_insert_and_lookup() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("session.jsonl");
        let project_dir = dir.path().join("project");
        let key = path_key(&file_path);

        let summary = make_summary(&file_path, &project_dir);
        let file_size = 1234_u64;
        let mtime = 1700000000_i64;

        let mut cache = SessionIndexCache::empty();
        cache.insert(key.clone(), file_size, mtime, &summary);

        // Matching size + mtime → Some
        let result = cache.lookup(&key, &file_path, file_size, mtime);
        assert!(result.is_some());
        let s = result.unwrap();
        assert_eq!(s.session_id, "test-session-id");
        assert_eq!(s.file_size, file_size);

        // Different size → None
        assert!(cache
            .lookup(&key, &file_path, file_size + 1, mtime)
            .is_none());

        // Different mtime → None
        assert!(cache
            .lookup(&key, &file_path, file_size, mtime + 1)
            .is_none());
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        let dir = TempDir::new().unwrap();
        let config_dir = dir.path().join("config");
        let file_path = dir.path().join("session.jsonl");
        let project_dir = dir.path().join("project");
        let key = path_key(&file_path);

        let summary = make_summary(&file_path, &project_dir);
        let file_size = 4096_u64;
        let mtime = 1700000001_i64;

        let mut cache = SessionIndexCache::empty();
        cache.insert(key.clone(), file_size, mtime, &summary);
        cache.save(&config_dir);

        let loaded = SessionIndexCache::load(&config_dir);
        assert_eq!(loaded.version, CACHE_VERSION);
        assert!(!loaded.entries.is_empty());

        let result = loaded.lookup(&key, &file_path, file_size, mtime);
        assert!(result.is_some());
        let s = result.unwrap();
        assert_eq!(s.session_id, "test-session-id");
        assert_eq!(s.project_name, "my-project");
        assert_eq!(s.message_count, 10);
        assert_eq!(s.first_timestamp, Some("2024-01-01T00:00:00Z".to_string()));
    }

    #[test]
    fn test_corrupt_cache_returns_empty() {
        let dir = TempDir::new().unwrap();
        let cache_file = cache_path(dir.path());
        std::fs::write(&cache_file, b"not valid json !!!").unwrap();

        let cache = SessionIndexCache::load(dir.path());
        assert!(cache.entries.is_empty());
        assert_eq!(cache.version, CACHE_VERSION);
    }

    #[test]
    fn test_version_mismatch_returns_empty() {
        let dir = TempDir::new().unwrap();
        let cache_file = cache_path(dir.path());
        let bad_version = serde_json::json!({
            "version": 999,
            "entries": {}
        });
        std::fs::write(&cache_file, serde_json::to_vec(&bad_version).unwrap()).unwrap();

        let cache = SessionIndexCache::load(dir.path());
        assert!(cache.entries.is_empty());
        assert_eq!(cache.version, CACHE_VERSION);
    }

    #[test]
    fn test_retain_existing_prunes() {
        let dir = TempDir::new().unwrap();
        let path_a = dir.path().join("a.jsonl");
        let path_b = dir.path().join("b.jsonl");
        let project_dir = dir.path().join("proj");
        let key_a = path_key(&path_a);
        let key_b = path_key(&path_b);

        let summary_a = make_summary(&path_a, &project_dir);
        let summary_b = make_summary(&path_b, &project_dir);

        let mut cache = SessionIndexCache::empty();
        cache.insert(key_a.clone(), 100, 111, &summary_a);
        cache.insert(key_b.clone(), 200, 222, &summary_b);
        assert_eq!(cache.entries.len(), 2);

        // Retain only path_a
        let mut seen = HashSet::new();
        seen.insert(key_a.clone());
        cache.retain_existing(&seen);

        assert_eq!(cache.entries.len(), 1);
        assert!(cache.lookup(&key_a, &path_a, 100, 111).is_some());
        assert!(cache.lookup(&key_b, &path_b, 200, 222).is_none());
    }
}
