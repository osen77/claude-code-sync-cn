//! Tombstone registry for tracking intentional session deletions.
//!
//! When a user explicitly deletes a session via `ccs session delete` or the
//! interactive Cleanup action, the deletion is recorded here so that:
//!
//! 1. **Cross-device propagation**: other devices pulling from the sync repo
//!    can tell an intentional deletion (should remove locally) from an
//!    accidental one (should restore from the repo).
//! 2. **Push protection**: the registry lives inside the sync repo at
//!    `.ccs/deletions.json` and travels with commits, so deletion intent is
//!    preserved across sync boundaries without a separate state channel.
//!
//! The registry is intentionally simple: an append-by-replace list keyed by
//! `session_id`. Records are never removed in the first version (no GC); see
//! the plan's "known limitations" section.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Subdirectory inside the sync repo that holds ccs bookkeeping files.
const CCS_DIR: &str = ".ccs";

/// File name of the deletion registry within the `.ccs` directory.
const DELETIONS_FILE: &str = "deletions.json";

/// Current schema version of the registry file.
const CURRENT_VERSION: u32 = 1;

/// Why a session was deleted. Drives commit message wording and lets future
/// tooling distinguish user-driven deletes from batch cleanup or forced prune.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeleteReason {
    /// User explicitly deleted a single session.
    Explicit,
    /// Batch cleanup of garbage sessions (empty / no title).
    Cleanup,
    /// Forced physical deletion via `ccs push --prune`.
    Prune,
}

impl DeleteReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            DeleteReason::Explicit => "explicit",
            DeleteReason::Cleanup => "cleanup",
            DeleteReason::Prune => "prune",
        }
    }
}

/// A single deletion record. Keyed by `session_id` within the registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeletionRecord {
    /// The Claude/Codex session id (UUID).
    pub session_id: String,
    /// Path of the deleted file relative to the sync repo root, e.g.
    /// `projects/my-project/<session_id>.jsonl`. Used by `ccs restore` to
    /// locate the file before it was removed.
    pub repo_relative_path: String,
    /// Project name extracted from `cwd`, matching the sync repo layout.
    pub project_name: String,
    /// Session source: `"claude"` or `"codex"`.
    pub source: String,
    /// ISO-8601 UTC timestamp of the deletion.
    pub deleted_at: String,
    /// Device that performed the deletion.
    pub device: String,
    /// Why the session was deleted.
    pub reason: DeleteReason,
}

/// The on-disk registry. Serialised as pretty JSON at
/// `<sync_repo>/.ccs/deletions.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TombstoneRegistry {
    /// Schema version, for forward-compatible migrations.
    pub version: u32,
    /// All deletion records, in insertion order. Deduplicated by `session_id`.
    pub records: Vec<DeletionRecord>,
}

impl Default for TombstoneRegistry {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            records: Vec::new(),
        }
    }
}

impl TombstoneRegistry {
    /// Path to the registry file inside a given sync repo.
    pub fn file_path(repo_path: &Path) -> PathBuf {
        repo_path.join(CCS_DIR).join(DELETIONS_FILE)
    }

    /// Load the registry from a sync repo. Returns an empty registry when the
    /// file does not exist yet (first deletion on this device).
    pub fn load(repo_path: &Path) -> Result<Self> {
        Self::load_from_path(&Self::file_path(repo_path))
    }

    /// Load from an explicit file path. Mainly for tests, but also used by
    /// `load` to centralise the read logic.
    pub fn load_from_path(file_path: &Path) -> Result<Self> {
        if !file_path.exists() {
            return Ok(Self::default());
        }

        let content = fs::read_to_string(file_path).with_context(|| {
            format!(
                "Failed to read tombstone registry from: {}",
                file_path.display()
            )
        })?;

        let registry: TombstoneRegistry = serde_json::from_str(&content).with_context(|| {
            format!(
                "Failed to parse tombstone registry JSON from: {}",
                file_path.display()
            )
        })?;

        Ok(registry)
    }

    /// Save the registry to its default location inside the sync repo.
    pub fn save(&self, repo_path: &Path) -> Result<()> {
        self.save_to_path(&Self::file_path(repo_path))
    }

    /// Save to an explicit file path, creating parent directories as needed.
    pub fn save_to_path(&self, file_path: &Path) -> Result<()> {
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create tombstone directory: {}", parent.display())
            })?;
        }

        let content =
            serde_json::to_string_pretty(self).context("Failed to serialize tombstone registry")?;

        fs::write(file_path, content).with_context(|| {
            format!(
                "Failed to write tombstone registry to: {}",
                file_path.display()
            )
        })?;

        Ok(())
    }

    /// Add a deletion record. If a record with the same `session_id` already
    /// exists, it is replaced (the latest deletion wins). This keeps the
    /// registry deduplicated and lets a re-delete refresh the metadata.
    pub fn add(&mut self, record: DeletionRecord) {
        if let Some(existing) = self
            .records
            .iter_mut()
            .find(|r| r.session_id == record.session_id)
        {
            *existing = record;
        } else {
            self.records.push(record);
        }
    }

    /// Add many records at once, deduplicating as in [`add`].
    pub fn add_many(&mut self, records: impl IntoIterator<Item = DeletionRecord>) {
        for record in records {
            self.add(record);
        }
    }

    /// Returns true if the given session id has an intentional deletion on
    /// record. Used by pull to propagate remote deletions locally.
    pub fn contains(&self, session_id: &str) -> bool {
        self.records.iter().any(|r| r.session_id == session_id)
    }

    /// Convenience alias for [`contains`].
#[allow(dead_code)]
    pub fn is_deleted(&self, session_id: &str) -> bool {
        self.contains(session_id)
    }

    /// Number of records held.
#[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Whether the registry holds no records.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_record(session_id: &str, reason: DeleteReason) -> DeletionRecord {
        DeletionRecord {
            session_id: session_id.to_string(),
            repo_relative_path: format!("projects/my-project/{session_id}.jsonl"),
            project_name: "my-project".to_string(),
            source: "claude".to_string(),
            deleted_at: "2026-06-20T00:00:00+00:00".to_string(),
            device: "test-device".to_string(),
            reason,
        }
    }

    #[test]
    fn load_returns_empty_when_file_missing() {
        let tmp = TempDir::new().unwrap();
        let registry = TombstoneRegistry::load(tmp.path()).unwrap();
        assert!(registry.is_empty());
        assert_eq!(registry.version, CURRENT_VERSION);
    }

    #[test]
    fn save_then_load_roundtrips() {
        let tmp = TempDir::new().unwrap();
        let mut registry = TombstoneRegistry::default();
        registry.add(sample_record("abc-123", DeleteReason::Explicit));
        registry.add(sample_record("def-456", DeleteReason::Cleanup));

        registry.save(tmp.path()).unwrap();

        let loaded = TombstoneRegistry::load(tmp.path()).unwrap();
        assert_eq!(loaded.len(), 2);
        assert!(loaded.contains("abc-123"));
        assert!(loaded.contains("def-456"));
        assert_eq!(loaded.records[0].reason, DeleteReason::Explicit);
        assert_eq!(loaded.records[1].reason, DeleteReason::Cleanup);
    }

    #[test]
    fn add_dedupes_by_session_id() {
        let mut registry = TombstoneRegistry::default();
        registry.add(sample_record("abc-123", DeleteReason::Explicit));
        // Same id, different reason — should replace, not append.
        registry.add(sample_record("abc-123", DeleteReason::Cleanup));

        assert_eq!(registry.len(), 1);
        assert_eq!(registry.records[0].reason, DeleteReason::Cleanup);
    }

    #[test]
    fn add_many_dedupes() {
        let mut registry = TombstoneRegistry::default();
        registry.add(sample_record("abc-123", DeleteReason::Explicit));
        registry.add_many(vec![
            sample_record("abc-123", DeleteReason::Cleanup),
            sample_record("def-456", DeleteReason::Prune),
        ]);

        assert_eq!(registry.len(), 2);
        assert_eq!(
            registry
                .records
                .iter()
                .find(|r| r.session_id == "abc-123")
                .unwrap()
                .reason,
            DeleteReason::Cleanup
        );
    }

    #[test]
    fn contains_and_is_deleted_agree() {
        let mut registry = TombstoneRegistry::default();
        registry.add(sample_record("abc-123", DeleteReason::Explicit));

        assert!(registry.contains("abc-123"));
        assert!(registry.is_deleted("abc-123"));
        assert!(!registry.contains("missing-id"));
    }

    #[test]
    fn file_path_is_under_ccs_dir() {
        let path = TombstoneRegistry::file_path(Path::new("/tmp/fake-repo"));
        assert!(path.ends_with(".ccs/deletions.json"));
    }

    #[test]
    fn save_creates_parent_ccs_dir() {
        let tmp = TempDir::new().unwrap();
        let registry = TombstoneRegistry::default();

        // .ccs/ does not exist yet; save must create it.
        registry.save(tmp.path()).unwrap();
        assert!(TombstoneRegistry::file_path(tmp.path()).exists());
    }

    #[test]
    fn delete_reason_as_str() {
        assert_eq!(DeleteReason::Explicit.as_str(), "explicit");
        assert_eq!(DeleteReason::Cleanup.as_str(), "cleanup");
        assert_eq!(DeleteReason::Prune.as_str(), "prune");
    }

    #[test]
    fn load_from_corrupt_file_errors() {
        let tmp = TempDir::new().unwrap();
        let file = TombstoneRegistry::file_path(tmp.path());
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "not json").unwrap();

        let result = TombstoneRegistry::load(tmp.path());
        assert!(result.is_err());
    }
}
