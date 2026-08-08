//! Session maintenance domain orchestration.

pub(crate) mod classifier;
#[allow(dead_code)]
pub(crate) mod recycle;
#[allow(dead_code)]
pub(crate) mod state;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::filter::SessionMaintenanceSettings;
use crate::path_security::safe_relative_path_within_root;
use crate::session_cache::fingerprint_file;
use crate::session_model::{SessionIdentity, SessionSource, SessionSummary};

use self::classifier::{classify, ClassifierPolicy, MaintenanceCandidate};
use self::recycle::{purge_session, reconcile_pending, recycle_session, MaintenanceRoots};
use self::state::{
    identity_key, reconcile_fingerprint, LifecycleState, MaintenanceEntry, StateStore,
};

/// Controls whether maintenance may persist state or move files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaintenanceMode {
    Disabled,
    #[allow(dead_code)]
    DryRun,
    Apply,
}

/// Clock dependency used to make maintenance decisions deterministic in tests.
pub(crate) trait MaintenanceClock {
    fn now(&self) -> DateTime<Utc>;
}

/// Production clock for maintenance runs.
pub(crate) struct SystemMaintenanceClock;

impl MaintenanceClock for SystemMaintenanceClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Inputs discovered by the session scanner and trusted by the orchestrator.
pub(crate) struct MaintenanceInput<'a> {
    pub summaries: &'a [SessionSummary],
    pub completed_sources: &'a HashSet<SessionSource>,
    pub roots: &'a MaintenanceRoots,
    pub config_dir: &'a Path,
    pub settings: &'a SessionMaintenanceSettings,
    pub clock: &'a dyn MaintenanceClock,
}

/// Aggregate result of one maintenance run.
#[derive(Debug, Default)]
pub struct MaintenanceReport {
    pub candidates: usize,
    pub hidden: usize,
    pub recycled: usize,
    pub purged: usize,
    pub restored_visible: usize,
    pub file_actions: usize,
    pub remaining_actions: usize,
    pub warnings: usize,
    pub(crate) visibility: VisibilityIndex,
}

/// Lifecycle states to overlay on session listings.
#[derive(Debug, Default, Clone)]
pub(crate) struct VisibilityIndex {
    pub(crate) states: HashMap<SessionIdentity, LifecycleState>,
}

/// Construct a classifier candidate from a validated scanner summary.
pub(crate) fn candidate_from_summary(
    summary: &SessionSummary,
    roots: &MaintenanceRoots,
    existing: Option<&MaintenanceEntry>,
) -> Result<MaintenanceCandidate> {
    let identity = summary.identity()?;
    let source_root = roots.source_root(identity.source);
    let original_relative_path = safe_relative_path_within_root(source_root, &summary.file_path)
        .context("session file is not a safe source candidate")?;
    let fingerprint = fingerprint_file(&summary.file_path)?;
    let source_label = identity.source.as_str().to_string();
    let parse_timestamp = |value: &Option<String>| -> Result<Option<DateTime<Utc>>> {
        value
            .as_deref()
            .map(|timestamp| {
                DateTime::parse_from_rfc3339(timestamp)
                    .map(|parsed| parsed.with_timezone(&Utc))
                    .with_context(|| format!("invalid session timestamp for {source_label}"))
            })
            .transpose()
    };

    Ok(MaintenanceCandidate {
        identity,
        original_relative_path,
        project_name: summary.project_name.clone(),
        project_dir: summary.project_dir.clone(),
        title: summary.title.clone(),
        has_custom_title: summary.has_custom_title,
        user_message_count: summary.user_message_count,
        message_count: summary.message_count,
        first_activity: parse_timestamp(&summary.first_timestamp)?,
        last_activity: parse_timestamp(&summary.last_activity)?,
        size: summary.file_size,
        fingerprint,
        explicit_test: existing.is_some_and(|entry| entry.explicit_test),
        keep: existing.is_some_and(|entry| entry.keep),
    })
}

fn visibility_from_state(state: &state::MaintenanceState) -> VisibilityIndex {
    VisibilityIndex {
        states: state
            .entries
            .values()
            .map(|entry| (entry.identity.clone(), entry.lifecycle))
            .collect(),
    }
}

fn maintenance_policy(settings: &SessionMaintenanceSettings) -> Option<ClassifierPolicy> {
    (settings.classifier == "conservative").then(|| {
        // Temporary roots belong to the orchestration environment, not the classifier's
        // policy defaults, so callers and tests can control this boundary explicitly.
        ClassifierPolicy::with_temporary_roots(
            settings.hide_after_hours,
            vec![
                std::env::temp_dir(),
                PathBuf::from("/tmp"),
                PathBuf::from("/private/tmp"),
            ],
        )
    })
}

/// Reconcile pending filesystem work, classify completed source summaries, and optionally apply
/// lifecycle transitions. Any invalid input or state fails safe with a warning-only report.
pub(crate) fn run_maintenance(
    input: MaintenanceInput<'_>,
    mode: MaintenanceMode,
) -> Result<MaintenanceReport> {
    let store = StateStore::from_config_dir(input.config_dir);
    let mut report = MaintenanceReport::default();
    let mut state = match store.load() {
        Ok(state) => state,
        Err(_) => {
            report.warnings = 1;
            return Ok(report);
        }
    };
    report.visibility = visibility_from_state(&state);

    if mode == MaintenanceMode::Disabled {
        return Ok(report);
    }
    let Some(policy) = maintenance_policy(input.settings) else {
        report.warnings = 1;
        return Ok(report);
    };

    // A pending transaction is recoverable only when its source scan completed. DryRun must
    // remain entirely read-only, so it intentionally skips reconciliation.
    if mode == MaintenanceMode::Apply {
        if let Some(pending) = state.pending.as_ref() {
            if input.completed_sources.contains(&pending.identity.source)
                && reconcile_pending(&store, input.roots, input.clock.now()).is_err()
            {
                report.warnings += 1;
                return Ok(report);
            }
            state = match store.load() {
                Ok(state) => state,
                Err(_) => {
                    report.warnings += 1;
                    return Ok(report);
                }
            };
            report.visibility = visibility_from_state(&state);
        }
    }

    let mut grouped: HashMap<SessionIdentity, Vec<&SessionSummary>> = HashMap::new();
    for summary in input.summaries {
        let Ok(identity) = summary.identity() else {
            report.warnings += 1;
            continue;
        };
        if !input.completed_sources.contains(&identity.source) {
            continue;
        }
        grouped.entry(identity).or_default().push(summary);
    }

    let mut candidates = Vec::new();
    for (identity, summaries) in grouped {
        if summaries.len() != 1 {
            report.warnings += 1;
            continue;
        }
        let summary = summaries[0];
        let key = identity_key(&identity);
        let existing = state.entries.get(&key);
        match candidate_from_summary(summary, input.roots, existing) {
            Ok(candidate) => candidates.push((candidate, existing.cloned())),
            Err(_) => report.warnings += 1,
        }
    }
    report.candidates = candidates.len();

    let mut planned_file_actions = 0usize;
    for (candidate, existing) in candidates {
        let key = identity_key(&candidate.identity);
        let decision = classify(&candidate, &policy, input.clock.now());
        let entry = existing;

        if let Some(current) = entry.as_ref() {
            if reconcile_fingerprint(current, &candidate.fingerprint.digest)
                == state::LifecycleTransition::RestoreVisible
            {
                report.restored_visible += 1;
                if mode == MaintenanceMode::Apply {
                    let fingerprint = candidate.fingerprint.digest.clone();
                    store.update(|saved| {
                        if let Some(current) = saved.entries.get_mut(&key) {
                            current.lifecycle = LifecycleState::Visible;
                            current.fingerprint = fingerprint;
                            current.hidden_since = None;
                            current.recycled_at = None;
                            current.purged_at = None;
                        }
                        Ok(())
                    })?;
                }
                continue;
            }
        }

        if mode == MaintenanceMode::Apply {
            if let Some(current) = entry.as_ref() {
                if current.classifier_version != classifier::CLASSIFIER_VERSION {
                    update_entry_metadata(
                        &store,
                        &key,
                        current,
                        &candidate,
                        &decision,
                        input.clock.now(),
                    )?;
                }
            }
        }
        let transition =
            state::next_lifecycle(entry.as_ref(), &decision, input.clock.now(), input.settings);
        match (entry.as_ref(), transition) {
            (None, state::LifecycleTransition::Hide) => {
                report.hidden += 1;
                if mode == MaintenanceMode::Apply {
                    let new_entry = MaintenanceEntry {
                        identity: candidate.identity.clone(),
                        original_relative_path: candidate.original_relative_path.clone(),
                        project_name: candidate.project_name.clone(),
                        fingerprint: candidate.fingerprint.digest.clone(),
                        lifecycle: LifecycleState::Hidden,
                        classifier_version: classifier::CLASSIFIER_VERSION,
                        score: decision.score,
                        reason_codes: decision.reasons.clone(),
                        hidden_since: Some(input.clock.now()),
                        recycled_at: None,
                        purged_at: None,
                        keep: candidate.keep,
                        explicit_test: candidate.explicit_test,
                    };
                    store.update(|saved| {
                        saved.entries.insert(key.clone(), new_entry);
                        Ok(())
                    })?;
                }
            }
            (Some(current), state::LifecycleTransition::Hide) => {
                report.hidden += 1;
                if mode == MaintenanceMode::Apply {
                    update_entry_metadata(
                        &store,
                        &key,
                        current,
                        &candidate,
                        &decision,
                        input.clock.now(),
                    )?;
                }
            }
            (Some(current), state::LifecycleTransition::Recycle) => {
                if planned_file_actions >= input.settings.max_actions_per_run {
                    report.remaining_actions += 1;
                    continue;
                }
                if mode == MaintenanceMode::Apply {
                    recycle_session(&store, input.roots, current, input.clock.now())?;
                }
                planned_file_actions += 1;
                report.file_actions += 1;
                report.recycled += 1;
            }
            (Some(current), state::LifecycleTransition::PurgeLocal) => {
                if planned_file_actions >= input.settings.max_actions_per_run {
                    report.remaining_actions += 1;
                    continue;
                }
                if mode == MaintenanceMode::Apply {
                    purge_session(&store, input.roots, current, input.clock.now())?;
                }
                planned_file_actions += 1;
                report.file_actions += 1;
                report.purged += 1;
            }
            _ => {}
        }
    }

    if mode == MaintenanceMode::Apply {
        state = store.load()?;
        report.visibility = visibility_from_state(&state);
    }
    Ok(report)
}

fn update_entry_metadata(
    store: &StateStore,
    key: &str,
    current: &MaintenanceEntry,
    candidate: &MaintenanceCandidate,
    decision: &classifier::ClassificationDecision,
    now: DateTime<Utc>,
) -> Result<()> {
    let current = current.clone();
    store.update(|saved| {
        let entry = saved
            .entries
            .get_mut(key)
            .context("maintenance entry disappeared during classification")?;
        if entry.fingerprint != current.fingerprint {
            anyhow::bail!("stale maintenance entry during classification")
        }
        entry.classifier_version = classifier::CLASSIFIER_VERSION;
        entry.score = decision.score;
        entry.reason_codes = decision.reasons.clone();
        entry.project_name = candidate.project_name.clone();
        entry.keep = candidate.keep;
        entry.explicit_test = candidate.explicit_test;
        if entry.hidden_since.is_none() {
            entry.hidden_since = Some(now);
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::SessionMaintenanceSettings;
    use crate::session_model::{SessionIdentity, SessionSource, SessionSummary};
    use chrono::{DateTime, Duration, Utc};
    use std::collections::HashSet;
    use std::fs;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    struct MaintenanceFixture {
        _temp: TempDir,
        source_file: PathBuf,
        roots: recycle::MaintenanceRoots,
        config_dir: PathBuf,
        now: DateTime<Utc>,
    }

    impl MaintenanceFixture {
        fn new(source: SessionSource, age_days: i64) -> Self {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path();
            let claude = root.join("claude");
            let codex = root.join("codex");
            let omp = root.join("omp");
            let recycle = root.join("recycle");
            let config_dir = root.join("config");
            for dir in [&claude, &codex, &omp, &recycle, &config_dir] {
                fs::create_dir_all(dir).unwrap();
            }
            let source_root = match source {
                SessionSource::Claude => &claude,
                SessionSource::Codex => &codex,
                SessionSource::Omp => &omp,
            };
            let source_file = source_root.join("project").join("session.jsonl");
            fs::create_dir_all(source_file.parent().unwrap()).unwrap();
            fs::write(&source_file, b"session fixture").unwrap();
            Self {
                _temp: temp,
                source_file,
                roots: recycle::MaintenanceRoots {
                    claude,
                    codex,
                    omp,
                    recycle,
                },
                config_dir,
                now: DateTime::parse_from_rfc3339("2026-08-08T12:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            }
            .with_age(age_days)
        }

        fn with_age(self, age_days: i64) -> Self {
            let age = self.now - Duration::days(age_days);
            let content = format!(
                "{{\"timestamp\":\"{}\"}}\n{{\"timestamp\":\"{}\"}}\n",
                age.to_rfc3339(),
                age.to_rfc3339()
            );
            fs::write(&self.source_file, content).unwrap();
            self
        }

        fn old_test_candidate(source: SessionSource, age_days: i64) -> Self {
            Self::new(source, age_days)
        }

        fn hidden_for_days(source: SessionSource, days: i64) -> Self {
            let fixture = Self::new(source, 120);
            let identity = SessionIdentity {
                source,
                session_id: "cx-task6".to_string(),
            };
            let fingerprint = crate::session_cache::fingerprint_file(&fixture.source_file)
                .unwrap()
                .digest;
            let entry = state::MaintenanceEntry {
                identity: identity.clone(),
                original_relative_path: PathBuf::from("project/session.jsonl"),
                project_name: "project".to_string(),
                fingerprint,
                lifecycle: state::LifecycleState::Hidden,
                classifier_version: classifier::CLASSIFIER_VERSION,
                score: 100,
                reason_codes: vec![classifier::ReasonCode::ExplicitTestMarker],
                hidden_since: Some(fixture.now - Duration::days(days)),
                recycled_at: None,
                purged_at: None,
                keep: false,
                explicit_test: true,
            };
            state::StateStore::from_config_dir(&fixture.config_dir)
                .update(|saved| {
                    saved.entries.insert(state::identity_key(&identity), entry);
                    Ok(())
                })
                .unwrap();
            fixture
        }

        fn with_recyclable_sessions(count: usize) -> Self {
            let fixture = Self::new(SessionSource::Claude, 120);
            let store = state::StateStore::from_config_dir(&fixture.config_dir);
            store
                .update(|saved| {
                    for index in 0..count {
                        let relative = PathBuf::from(format!("project/session-{index}.jsonl"));
                        let path = fixture.roots.claude.join(&relative);
                        fs::write(&path, format!("fixture-{index}")).unwrap();
                        let fingerprint = crate::session_cache::fingerprint_file(&path)
                            .unwrap()
                            .digest;
                        let identity = SessionIdentity {
                            source: SessionSource::Claude,
                            session_id: format!("cc-task6-{index}"),
                        };
                        saved.entries.insert(
                            state::identity_key(&identity),
                            state::MaintenanceEntry {
                                identity,
                                original_relative_path: relative,
                                project_name: "project".to_string(),
                                fingerprint,
                                lifecycle: state::LifecycleState::Hidden,
                                classifier_version: classifier::CLASSIFIER_VERSION,
                                score: 100,
                                reason_codes: vec![classifier::ReasonCode::ExplicitTestMarker],
                                hidden_since: Some(fixture.now - Duration::days(8)),
                                recycled_at: None,
                                purged_at: None,
                                keep: false,
                                explicit_test: true,
                            },
                        );
                    }
                    Ok(())
                })
                .unwrap();
            fixture
        }

        fn summary(
            &self,
            source: SessionSource,
            session_id: &str,
            file_path: &Path,
        ) -> SessionSummary {
            let age = self.now - Duration::days(120);
            SessionSummary {
                source: source.as_str().to_string(),
                session_id: session_id.to_string(),
                title: "test".to_string(),
                project_name: "project".to_string(),
                project_dir: PathBuf::from("/tmp/task-6-project"),
                file_path: file_path.to_path_buf(),
                message_count: 1,
                user_message_count: 1,
                assistant_message_count: 0,
                first_timestamp: Some((age - Duration::minutes(5)).to_rfc3339()),
                last_activity: Some(age.to_rfc3339()),
                file_size: fs::metadata(file_path).unwrap().len(),
                has_custom_title: false,
            }
        }

        fn run_with(
            &self,
            summaries: Vec<SessionSummary>,
            completed: HashSet<SessionSource>,
            settings: SessionMaintenanceSettings,
            mode: MaintenanceMode,
        ) -> MaintenanceReport {
            run_maintenance(
                MaintenanceInput {
                    summaries: Box::leak(summaries.into_boxed_slice()),
                    completed_sources: Box::leak(Box::new(completed)),
                    roots: &self.roots,
                    config_dir: &self.config_dir,
                    settings: Box::leak(Box::new(settings)),
                    clock: Box::leak(Box::new(FixedClock(self.now))),
                },
                mode,
            )
            .unwrap()
        }
    }

    struct FixedClock(DateTime<Utc>);

    impl MaintenanceClock for FixedClock {
        fn now(&self) -> DateTime<Utc> {
            self.0
        }
    }

    fn all_complete() -> HashSet<SessionSource> {
        HashSet::from([
            SessionSource::Claude,
            SessionSource::Codex,
            SessionSource::Omp,
        ])
    }

    #[test]
    fn incomplete_source_never_advances_destructive_state() {
        let fixture = MaintenanceFixture::hidden_for_days(SessionSource::Codex, 8);
        let completed = HashSet::from([SessionSource::Claude, SessionSource::Omp]);
        let summary = fixture.summary(SessionSource::Codex, "cx-task6", &fixture.source_file);
        let report = fixture.run_with(
            vec![summary],
            completed,
            SessionMaintenanceSettings::default(),
            MaintenanceMode::Apply,
        );
        assert_eq!(report.recycled, 0);
        assert!(fixture.source_file.exists());
    }

    #[test]
    fn old_candidate_first_run_only_becomes_hidden() {
        let fixture = MaintenanceFixture::old_test_candidate(SessionSource::Claude, 120);
        let summary = fixture.summary(SessionSource::Claude, "cc-task6", &fixture.source_file);
        let report = fixture.run_with(
            vec![summary],
            all_complete(),
            SessionMaintenanceSettings::default(),
            MaintenanceMode::Apply,
        );
        assert_eq!(report.hidden, 1);
        assert_eq!(report.recycled, 0);
        assert!(fixture.source_file.exists());
    }

    #[test]
    fn action_budget_reports_remaining_work() {
        let fixture = MaintenanceFixture::with_recyclable_sessions(3);
        let settings = SessionMaintenanceSettings {
            max_actions_per_run: 2,
            ..SessionMaintenanceSettings::default()
        };
        let summaries = (0..3)
            .map(|index| {
                fixture.summary(
                    SessionSource::Claude,
                    &format!("cc-task6-{index}"),
                    &fixture
                        .roots
                        .claude
                        .join(format!("project/session-{index}.jsonl")),
                )
            })
            .collect();
        let report = fixture.run_with(summaries, all_complete(), settings, MaintenanceMode::Apply);
        assert_eq!(report.file_actions, 2);
        assert_eq!(report.remaining_actions, 1);
    }

    #[test]
    fn duplicate_source_identity_and_unknown_profile_are_fail_safe() {
        let duplicate = MaintenanceFixture::old_test_candidate(SessionSource::Claude, 120);
        let summary = duplicate.summary(SessionSource::Claude, "cc-task6", &duplicate.source_file);
        let report = duplicate.run_with(
            vec![summary.clone(), summary],
            all_complete(),
            SessionMaintenanceSettings::default(),
            MaintenanceMode::Apply,
        );
        assert_eq!(report.file_actions, 0);
        assert!(report.warnings > 0);

        let invalid = MaintenanceFixture::old_test_candidate(SessionSource::Claude, 120);
        let settings = SessionMaintenanceSettings {
            classifier: "unknown".to_string(),
            ..SessionMaintenanceSettings::default()
        };
        let report = invalid.run_with(
            vec![invalid.summary(SessionSource::Claude, "cc-task6", &invalid.source_file)],
            all_complete(),
            settings,
            MaintenanceMode::Apply,
        );
        assert_eq!(report.file_actions, 0);
        assert!(report.warnings > 0);
    }

    #[test]
    fn dry_run_does_not_write_state_or_recycle_files() {
        let fixture = MaintenanceFixture::old_test_candidate(SessionSource::Claude, 120);
        let state_path = fixture.config_dir.join("session-maintenance.json");
        let before = fs::read(&state_path).ok();
        let report = fixture.run_with(
            vec![fixture.summary(SessionSource::Claude, "cc-task6", &fixture.source_file)],
            all_complete(),
            SessionMaintenanceSettings::default(),
            MaintenanceMode::DryRun,
        );
        assert_eq!(report.hidden, 1);
        assert_eq!(fs::read(&state_path).ok(), before);
        assert!(fixture.source_file.exists());
    }

    #[test]
    fn invalid_state_is_fail_safe() {
        let fixture = MaintenanceFixture::old_test_candidate(SessionSource::Claude, 120);
        fs::write(
            fixture.config_dir.join("session-maintenance.json"),
            b"{\"version\":999,\"entries\":{},\"pending\":null}",
        )
        .unwrap();
        let report = fixture.run_with(
            vec![fixture.summary(SessionSource::Claude, "cc-task6", &fixture.source_file)],
            all_complete(),
            SessionMaintenanceSettings::default(),
            MaintenanceMode::Apply,
        );
        assert_eq!(report.file_actions, 0);
        assert!(report.warnings > 0);
        assert!(fixture.source_file.exists());
    }
}
