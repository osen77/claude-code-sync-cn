use crate::atomic_file::{persist_json_atomic, FileLock};
use crate::filter::SessionMaintenanceSettings;
use crate::session_maintenance::classifier::{Classification, ClassificationDecision, ReasonCode};
use crate::session_model::SessionIdentity;
use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

const STATE_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LifecycleState {
    Visible,
    Hidden,
    Recycled,
    PurgedLocal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LifecycleTransition {
    NoChange,
    Hide,
    Recycle,
    PurgeLocal,
    RestoreVisible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PendingOperationKind {
    Recycle,
    Restore,
    Purge,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct MaintenanceEntry {
    pub identity: SessionIdentity,
    pub original_relative_path: PathBuf,
    pub project_name: String,
    pub fingerprint: String,
    pub lifecycle: LifecycleState,
    pub classifier_version: u32,
    pub score: u16,
    pub reason_codes: Vec<ReasonCode>,
    pub hidden_since: Option<DateTime<Utc>>,
    pub recycled_at: Option<DateTime<Utc>>,
    pub purged_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub keep: bool,
    #[serde(default)]
    pub explicit_test: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PendingOperation {
    pub identity: SessionIdentity,
    pub operation: PendingOperationKind,
    pub source_relative_path: PathBuf,
    pub staging_relative_path: PathBuf,
    pub recycle_relative_path: PathBuf,
    pub expected_fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct MaintenanceState {
    pub version: u32,
    pub entries: HashMap<String, MaintenanceEntry>,
    pub pending: Option<PendingOperation>,
}

impl Default for MaintenanceState {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            entries: HashMap::new(),
            pending: None,
        }
    }
}

pub(crate) fn identity_key(identity: &SessionIdentity) -> String {
    format!("{}:{}", identity.source.as_str(), identity.session_id)
}

pub(crate) fn reconcile_fingerprint(
    entry: &MaintenanceEntry,
    fingerprint: &str,
) -> LifecycleTransition {
    if entry.fingerprint == fingerprint {
        LifecycleTransition::NoChange
    } else {
        LifecycleTransition::RestoreVisible
    }
}

pub(crate) fn next_lifecycle(
    entry: Option<&MaintenanceEntry>,
    decision: &ClassificationDecision,
    now: DateTime<Utc>,
    settings: &SessionMaintenanceSettings,
) -> LifecycleTransition {
    let is_candidate = decision.classification == Classification::TestCandidate;
    let Some(entry) = entry else {
        return if is_candidate {
            LifecycleTransition::Hide
        } else {
            LifecycleTransition::NoChange
        };
    };

    if !is_candidate {
        return LifecycleTransition::NoChange;
    }

    match entry.lifecycle {
        LifecycleState::Visible | LifecycleState::PurgedLocal => LifecycleTransition::NoChange,
        LifecycleState::Hidden => {
            if elapsed_at_least(entry.hidden_since, now, settings.recycle_after_days) {
                LifecycleTransition::Recycle
            } else {
                LifecycleTransition::NoChange
            }
        }
        LifecycleState::Recycled => {
            if elapsed_at_least(entry.recycled_at, now, settings.purge_after_days) {
                LifecycleTransition::PurgeLocal
            } else {
                LifecycleTransition::NoChange
            }
        }
    }
}

fn elapsed_at_least(start: Option<DateTime<Utc>>, now: DateTime<Utc>, days: u64) -> bool {
    let Some(start) = start else {
        return false;
    };
    let max_days = i64::MAX as u64 / 86_400;
    let threshold = Duration::days(days.min(max_days) as i64);
    now.signed_duration_since(start) >= threshold
}

pub(crate) struct StateStore {
    state_path: PathBuf,
    lock_path: PathBuf,
}

pub(crate) struct LockedState<'a> {
    store: &'a StateStore,
    pub(crate) state: MaintenanceState,
}

impl LockedState<'_> {
    pub(crate) fn persist(&self) -> Result<()> {
        persist_json_atomic(&self.store.state_path, &self.state)
    }
}

impl StateStore {
    pub(crate) fn from_config_dir(config_dir: &Path) -> Self {
        Self {
            state_path: config_dir.join("session-maintenance.json"),
            lock_path: config_dir.join("session-maintenance.lock"),
        }
    }

    pub(crate) fn load(&self) -> Result<MaintenanceState> {
        self.load_unlocked()
    }

    pub(crate) fn transaction<F, T>(&self, transaction: F) -> Result<T>
    where
        F: FnOnce(&mut LockedState<'_>) -> Result<T>,
    {
        let _lock = FileLock::acquire(&self.lock_path)
            .with_context(|| format!("failed to lock {}", self.lock_path.display()))?;
        let state = self.load_unlocked()?;
        let mut locked = LockedState { store: self, state };
        transaction(&mut locked)
    }

    pub(crate) fn update<F, T>(&self, update: F) -> Result<T>
    where
        F: FnOnce(&mut MaintenanceState) -> Result<T>,
    {
        self.transaction(|locked| {
            let result = update(&mut locked.state)?;
            locked.persist()?;
            Ok(result)
        })
    }

    fn load_unlocked(&self) -> Result<MaintenanceState> {
        let bytes = match fs::read(&self.state_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(MaintenanceState::default())
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read {}", self.state_path.display()))
            }
        };

        let state: MaintenanceState = serde_json::from_slice(&bytes)
            .with_context(|| format!("invalid maintenance state {}", self.state_path.display()))?;
        if state.version != STATE_VERSION {
            anyhow::bail!(
                "unsupported maintenance state version {} (expected {})",
                state.version,
                STATE_VERSION
            );
        }
        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::SessionMaintenanceSettings;
    use crate::session_maintenance::classifier::{
        Classification, ClassificationDecision, ReasonCode, CLASSIFIER_VERSION,
    };
    use crate::session_model::{SessionIdentity, SessionSource};
    use chrono::{DateTime, Duration, Utc};
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-08-08T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn settings() -> SessionMaintenanceSettings {
        SessionMaintenanceSettings::default()
    }

    fn test_decision() -> ClassificationDecision {
        ClassificationDecision {
            classification: Classification::TestCandidate,
            score: 100,
            reasons: vec![ReasonCode::ExplicitTestMarker],
        }
    }

    fn identity() -> SessionIdentity {
        SessionIdentity {
            source: SessionSource::Claude,
            session_id: "session-1".to_string(),
        }
    }

    fn hidden_entry(at: DateTime<Utc>) -> MaintenanceEntry {
        MaintenanceEntry {
            identity: identity(),
            original_relative_path: PathBuf::from("project/session.jsonl"),
            project_name: "project".to_string(),
            fingerprint: "fingerprint".to_string(),
            lifecycle: LifecycleState::Hidden,
            classifier_version: CLASSIFIER_VERSION,
            score: 100,
            reason_codes: vec![ReasonCode::ExplicitTestMarker],
            hidden_since: Some(at),
            recycled_at: None,
            purged_at: None,
            keep: false,
            explicit_test: true,
        }
    }

    fn recycled_entry(at: DateTime<Utc>) -> MaintenanceEntry {
        let mut entry = hidden_entry(now());
        entry.lifecycle = LifecycleState::Recycled;
        entry.recycled_at = Some(at);
        entry
    }

    #[test]
    fn first_match_only_enters_hidden_even_for_old_session() {
        let transition = next_lifecycle(None, &test_decision(), now(), &settings());
        assert_eq!(transition, LifecycleTransition::Hide);
    }

    #[test]
    fn hidden_entry_recycles_after_seven_days() {
        let entry = hidden_entry(now() - Duration::days(7));
        assert_eq!(
            next_lifecycle(Some(&entry), &test_decision(), now(), &settings()),
            LifecycleTransition::Recycle
        );
    }

    #[test]
    fn hidden_entry_purges_only_after_thirty_days() {
        let entry = recycled_entry(now() - Duration::days(30));
        assert_eq!(
            next_lifecycle(Some(&entry), &test_decision(), now(), &settings()),
            LifecycleTransition::PurgeLocal
        );
    }

    #[test]
    fn changed_fingerprint_returns_visible() {
        let mut entry = hidden_entry(now() - Duration::days(7));
        entry.fingerprint = "old".to_string();
        assert_eq!(
            reconcile_fingerprint(&entry, "new"),
            LifecycleTransition::RestoreVisible
        );
    }

    #[test]
    fn unchanged_hidden_entry_waits_before_recycling() {
        let entry = hidden_entry(now() - Duration::days(6));
        assert_eq!(
            next_lifecycle(Some(&entry), &test_decision(), now(), &settings()),
            LifecycleTransition::NoChange
        );
    }

    #[test]
    fn hidden_entry_stays_unchanged_when_classifier_protects_it() {
        let entry = hidden_entry(now() - Duration::days(6));
        let mut decision = test_decision();
        decision.classification = Classification::Keep;
        assert_eq!(
            next_lifecycle(Some(&entry), &decision, now(), &settings()),
            LifecycleTransition::NoChange
        );
    }

    #[test]
    fn first_match_keep_does_not_create_hidden_entry() {
        let mut decision = test_decision();
        decision.classification = Classification::Keep;
        assert_eq!(
            next_lifecycle(None, &decision, now(), &settings()),
            LifecycleTransition::NoChange
        );
    }

    #[test]
    fn missing_state_loads_default_without_creating_a_file() {
        let dir = tempdir().unwrap();
        let store = StateStore::from_config_dir(dir.path());

        assert_eq!(store.load().unwrap().version, STATE_VERSION);
        assert!(!dir.path().join("session-maintenance.json").exists());
    }

    #[test]
    fn failed_update_does_not_persist_mutations() {
        let dir = tempdir().unwrap();
        let store = StateStore::from_config_dir(dir.path());
        let entry = hidden_entry(now());

        assert!(store
            .update(|state| -> anyhow::Result<()> {
                state
                    .entries
                    .insert(identity_key(&entry.identity), entry.clone());
                anyhow::bail!("abort update")
            })
            .is_err());
        assert!(!dir.path().join("session-maintenance.json").exists());
    }

    #[test]
    fn update_merges_sequential_state_changes() {
        let dir = tempdir().unwrap();
        let store = StateStore::from_config_dir(dir.path());
        let first = hidden_entry(now());
        let mut second = first.clone();
        second.identity.session_id = "session-2".to_string();

        store
            .update(|state| {
                state
                    .entries
                    .insert(identity_key(&first.identity), first.clone());
                Ok(())
            })
            .unwrap();
        store
            .update(|state| {
                state
                    .entries
                    .insert(identity_key(&second.identity), second.clone());
                Ok(())
            })
            .unwrap();

        let loaded = store.load().unwrap();
        assert_eq!(loaded.entries.len(), 2);
        assert!(loaded.entries.contains_key(&identity_key(&first.identity)));
        assert!(loaded.entries.contains_key(&identity_key(&second.identity)));
    }

    #[test]
    fn transaction_can_persist_multiple_durability_boundaries() {
        let dir = tempdir().unwrap();
        let store = StateStore::from_config_dir(dir.path());
        let first = hidden_entry(now());
        let mut second = first.clone();
        second.identity.session_id = "session-2".to_string();

        store
            .transaction(|locked| {
                locked
                    .state
                    .entries
                    .insert(identity_key(&first.identity), first.clone());
                locked.persist()?;
                locked
                    .state
                    .entries
                    .insert(identity_key(&second.identity), second.clone());
                locked.persist()?;
                Ok(())
            })
            .unwrap();

        assert_eq!(store.load().unwrap().entries.len(), 2);
    }

    #[test]
    fn malformed_state_is_rejected_without_writing() {
        let dir = tempdir().unwrap();
        let store = StateStore::from_config_dir(dir.path());
        let path = dir.path().join("session-maintenance.json");
        let original = b"{not-json";
        std::fs::write(&path, original).unwrap();

        assert!(store.update(|_| Ok(())).is_err());
        assert_eq!(std::fs::read(path).unwrap(), original);
    }

    #[test]
    fn unsupported_state_version_is_rejected_without_writing() {
        let dir = tempdir().unwrap();
        let store = StateStore::from_config_dir(dir.path());
        let path = dir.path().join("session-maintenance.json");
        let original = br#"{"version":99,"entries":{},"pending":null}"#;
        std::fs::write(&path, original).unwrap();

        assert!(store.transaction(|_| Ok(())).is_err());
        assert_eq!(std::fs::read(path).unwrap(), original);
    }
}
