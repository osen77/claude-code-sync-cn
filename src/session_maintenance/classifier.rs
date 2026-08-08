use crate::session_cache::FileFingerprint;
use crate::session_model::SessionIdentity;
use chrono::{DateTime, Duration, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

// Consumed by the maintenance state layer when classifier results are persisted.
#[allow(dead_code)]
pub(crate) const CLASSIFIER_VERSION: u32 = 1;
pub(crate) const DEFAULT_THRESHOLD: u16 = 70;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifierPolicy {
    pub threshold: u16,
    pub hide_after_hours: u64,
    pub(crate) temporary_roots: Vec<PathBuf>,
}

impl ClassifierPolicy {
    pub(crate) fn conservative(hide_after_hours: u64) -> Self {
        Self {
            threshold: DEFAULT_THRESHOLD,
            hide_after_hours,
            temporary_roots: default_temporary_roots(),
        }
    }

    pub(crate) fn with_temporary_roots(
        hide_after_hours: u64,
        temporary_roots: Vec<PathBuf>,
    ) -> Self {
        Self {
            threshold: DEFAULT_THRESHOLD,
            hide_after_hours,
            temporary_roots,
        }
    }
}

fn default_temporary_roots() -> Vec<PathBuf> {
    vec![
        PathBuf::from("/tmp"),
        PathBuf::from("/private/tmp"),
        PathBuf::from("/var/tmp"),
        PathBuf::from(r"C:\Temp"),
        PathBuf::from(r"C:\Windows\Temp"),
    ]
}

#[cfg(not(test))]
#[allow(dead_code)]
fn _classifier_api_anchor() {
    let _ = ClassifierPolicy::conservative;
    let _ = ClassifierPolicy::with_temporary_roots;
    let _ = classify;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Classification {
    Keep,
    TestCandidate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasonCode {
    ExplicitTestMarker,
    FixtureSessionId,
    FixtureTemporaryCwd,
    ExactTestTitle,
    AutomatedValidationTitle,
    FewUserMessages,
    FewTotalMessages,
    ShortDuration,
    TemporaryCwd,
    RecentActivityProtection,
    CustomTitleProtection,
    LongConversationProtection,
    KeepProtection,
}

#[derive(Debug, Clone)]
pub struct MaintenanceCandidate {
    pub identity: SessionIdentity,
    #[allow(dead_code)]
    pub original_relative_path: PathBuf,
    #[allow(dead_code)]
    pub project_name: String,
    pub project_dir: PathBuf,
    pub title: String,
    pub has_custom_title: bool,
    pub user_message_count: usize,
    pub message_count: usize,
    pub first_activity: Option<DateTime<Utc>>,
    pub last_activity: Option<DateTime<Utc>>,
    #[allow(dead_code)]
    pub size: u64,
    #[allow(dead_code)]
    pub fingerprint: FileFingerprint,
    pub explicit_test: bool,
    pub keep: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassificationDecision {
    pub classification: Classification,
    pub score: u16,
    pub reasons: Vec<ReasonCode>,
}

/// Classify one session using the conservative, protection-first policy.
pub(crate) fn classify(
    candidate: &MaintenanceCandidate,
    policy: &ClassifierPolicy,
    now: DateTime<Utc>,
) -> ClassificationDecision {
    if candidate.keep {
        return keep_decision(ReasonCode::KeepProtection);
    }
    if candidate.has_custom_title {
        return keep_decision(ReasonCode::CustomTitleProtection);
    }
    if is_recent(candidate.last_activity, now, policy.hide_after_hours) {
        return keep_decision(ReasonCode::RecentActivityProtection);
    }
    if candidate.message_count > 20 {
        return keep_decision(ReasonCode::LongConversationProtection);
    }
    if conversation_duration(candidate).is_some_and(|duration| duration > Duration::hours(2)) {
        return keep_decision(ReasonCode::LongConversationProtection);
    }

    if candidate.explicit_test {
        return ClassificationDecision {
            classification: Classification::TestCandidate,
            score: 100,
            reasons: vec![ReasonCode::ExplicitTestMarker],
        };
    }

    let mut score = 0_u16;
    let mut reasons = Vec::new();

    if is_fixture_session_id(&candidate.identity.session_id) {
        score = score.saturating_add(60);
        reasons.push(ReasonCode::FixtureSessionId);
    }

    let normalized_title = candidate.title.trim().to_lowercase();
    if matches!(
        normalized_title.as_str(),
        "测试" | "test" | "hello" | "hi" | "试一下"
    ) {
        score = score.saturating_add(35);
        reasons.push(ReasonCode::ExactTestTitle);
    }

    if ["fixture", "smoke test", "test brief"]
        .iter()
        .any(|keyword| normalized_title.contains(keyword))
    {
        score = score.saturating_add(25);
        reasons.push(ReasonCode::AutomatedValidationTitle);
    }

    if candidate.user_message_count <= 2 {
        score = score.saturating_add(20);
        reasons.push(ReasonCode::FewUserMessages);
    }
    if candidate.message_count <= 6 {
        score = score.saturating_add(10);
        reasons.push(ReasonCode::FewTotalMessages);
    }
    if conversation_duration(candidate)
        .is_some_and(|duration| (Duration::zero()..=Duration::minutes(15)).contains(&duration))
    {
        score = score.saturating_add(15);
        reasons.push(ReasonCode::ShortDuration);
    }

    if is_temporary_cwd(&candidate.project_dir, &policy.temporary_roots) {
        score = score.saturating_add(20);
        if is_fixture_session_id(&candidate.identity.session_id)
            && is_fixture_cwd(&candidate.project_dir)
        {
            reasons.push(ReasonCode::FixtureTemporaryCwd);
        } else {
            reasons.push(ReasonCode::TemporaryCwd);
        }
    }

    let classification = if score >= policy.threshold {
        Classification::TestCandidate
    } else {
        Classification::Keep
    };
    ClassificationDecision {
        classification,
        score,
        reasons,
    }
}

fn keep_decision(reason: ReasonCode) -> ClassificationDecision {
    ClassificationDecision {
        classification: Classification::Keep,
        score: 0,
        reasons: vec![reason],
    }
}

fn is_recent(
    last_activity: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    hide_after_hours: u64,
) -> bool {
    let Some(last_activity) = last_activity else {
        return false;
    };
    let max_hours = (i64::MAX as u64) / 3_600;
    let hide_after = Duration::hours(hide_after_hours.min(max_hours) as i64);
    now.signed_duration_since(last_activity) < hide_after
}

fn conversation_duration(candidate: &MaintenanceCandidate) -> Option<Duration> {
    Some(
        candidate
            .last_activity?
            .signed_duration_since(candidate.first_activity?),
    )
}

fn is_fixture_session_id(session_id: &str) -> bool {
    static FIXTURE_ID: OnceLock<Regex> = OnceLock::new();
    FIXTURE_ID
        .get_or_init(|| {
            Regex::new(r"^(cc|cx|om)(-cache)?-task[0-9]+$").expect("valid fixture ID regex")
        })
        .is_match(session_id)
}

fn is_temporary_cwd(project_dir: &Path, temporary_roots: &[PathBuf]) -> bool {
    temporary_roots
        .iter()
        .any(|root| !root.as_os_str().is_empty() && project_dir.starts_with(root))
}

fn is_fixture_cwd(project_dir: &Path) -> bool {
    let Some(name) = project_dir.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let Some(task_prefix) = name.strip_prefix("task") else {
        return false;
    };
    let Some(task_number) = task_prefix.strip_suffix("-project") else {
        return false;
    };
    !task_number.is_empty()
        && task_number
            .chars()
            .all(|character| character.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_cache::FileFingerprint;
    use crate::session_model::{SessionIdentity, SessionSource};
    use chrono::{DateTime, Duration, Utc};
    use serial_test::serial;
    use std::path::PathBuf;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-08-08T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn policy() -> ClassifierPolicy {
        ClassifierPolicy::with_temporary_roots(24, vec![PathBuf::from("/tmp")])
    }

    fn candidate(
        title: &str,
        user_message_count: usize,
        message_count: usize,
        duration_minutes: i64,
        session_id: &str,
    ) -> MaintenanceCandidate {
        let last_activity = now() - Duration::hours(48);
        let first_activity = last_activity - Duration::minutes(duration_minutes);
        MaintenanceCandidate {
            identity: SessionIdentity {
                source: SessionSource::Claude,
                session_id: session_id.to_string(),
            },
            original_relative_path: PathBuf::from("project/session.jsonl"),
            project_name: "project".to_string(),
            project_dir: PathBuf::from("/Users/example/project"),
            title: title.to_string(),
            has_custom_title: false,
            user_message_count,
            message_count,
            first_activity: Some(first_activity),
            last_activity: Some(last_activity),
            size: 128,
            fingerprint: FileFingerprint {
                digest: "fingerprint".to_string(),
                bytes: 128,
            },
            explicit_test: false,
            keep: false,
        }
    }

    #[test]
    fn exact_test_title_alone_does_not_cross_threshold() {
        let candidate = candidate("test", 5, 3, 60, "550e8400-e29b-41d4-a716-446655440000");
        let decision = classify(&candidate, &policy(), now());
        assert_eq!(decision.classification, Classification::Keep);
        assert!(decision.reasons.contains(&ReasonCode::ExactTestTitle));
    }

    #[test]
    fn multiple_low_value_signals_cross_threshold() {
        let candidate = candidate("test", 2, 1, 5, "550e8400-e29b-41d4-a716-446655440000");
        let decision = classify(&candidate, &policy(), now());
        assert_eq!(decision.classification, Classification::TestCandidate);
        assert_eq!(decision.score, 80);
    }

    #[test]
    fn custom_title_and_recent_activity_are_hard_protections() {
        let mut custom = candidate("test", 2, 1, 5, "cc-task4");
        custom.has_custom_title = true;
        assert_eq!(
            classify(&custom, &policy(), now()).classification,
            Classification::Keep
        );

        let mut recent = candidate("test", 2, 1, 5, "cc-task4");
        recent.last_activity = Some(now() - chrono::Duration::hours(2));
        assert_eq!(
            classify(&recent, &policy(), now()).classification,
            Classification::Keep
        );
    }

    #[test]
    fn explicit_keep_overrides_explicit_test_marker() {
        let mut candidate = candidate("test", 2, 1, 5, "cc-task4");
        candidate.explicit_test = true;
        candidate.keep = true;
        assert_eq!(
            classify(&candidate, &policy(), now()).classification,
            Classification::Keep
        );
    }

    #[test]
    fn score_at_threshold_is_a_test_candidate() {
        let candidate = candidate("test", 2, 7, 5, "ordinary-session");
        let decision = classify(&candidate, &policy(), now());
        assert_eq!(decision.classification, Classification::TestCandidate);
        assert_eq!(decision.score, 70);
    }

    #[test]
    fn long_conversation_protection_covers_message_count_over_twenty() {
        let candidate = candidate("test", 2, 21, 5, "ordinary-session");
        let decision = classify(&candidate, &policy(), now());
        assert_eq!(decision.classification, Classification::Keep);
        assert!(decision
            .reasons
            .contains(&ReasonCode::LongConversationProtection));
    }

    #[test]
    fn long_conversation_protection_covers_duration_over_two_hours() {
        let candidate = candidate("test", 2, 1, 121, "ordinary-session");
        let decision = classify(&candidate, &policy(), now());
        assert_eq!(decision.classification, Classification::Keep);
        assert!(decision
            .reasons
            .contains(&ReasonCode::LongConversationProtection));
    }

    #[test]
    fn fixture_id_regex_accepts_only_supported_task_shapes() {
        for session_id in ["cc-task4", "cx-cache-task9", "om-task1"] {
            let candidate = candidate("ordinary", 5, 3, 60, session_id);
            let decision = classify(&candidate, &policy(), now());
            assert!(decision.reasons.contains(&ReasonCode::FixtureSessionId));
        }
        for session_id in ["cc-task", "cc-task4-extra", "claude-task4", "cc-taskx"] {
            let candidate = candidate("ordinary", 5, 3, 60, session_id);
            let decision = classify(&candidate, &policy(), now());
            assert!(!decision.reasons.contains(&ReasonCode::FixtureSessionId));
        }
    }

    #[test]
    fn fixture_temporary_cwd_has_dedicated_reason() {
        let mut candidate = candidate("ordinary", 5, 3, 60, "cc-task4");
        candidate.project_dir = PathBuf::from("/tmp/task3-project");
        let decision = classify(&candidate, &policy(), now());
        assert!(decision.reasons.contains(&ReasonCode::FixtureTemporaryCwd));
    }

    #[test]
    fn automated_validation_title_alone_does_not_cross_threshold() {
        let candidate = candidate("smoke test", 5, 3, 60, "ordinary-session");
        let decision = classify(&candidate, &policy(), now());
        assert_eq!(decision.classification, Classification::Keep);
        assert!(decision.score < DEFAULT_THRESHOLD);
        assert!(decision
            .reasons
            .contains(&ReasonCode::AutomatedValidationTitle));
    }

    #[test]
    fn explicit_test_marker_scores_one_hundred() {
        let mut candidate = candidate("ordinary", 5, 3, 60, "ordinary-session");
        candidate.explicit_test = true;
        let decision = classify(&candidate, &policy(), now());
        assert_eq!(decision.classification, Classification::TestCandidate);
        assert_eq!(decision.score, 100);
        assert_eq!(decision.reasons, vec![ReasonCode::ExplicitTestMarker]);
    }

    #[test]
    #[serial]
    fn changing_tmpdir_does_not_change_same_policy_classification() {
        let mut candidate = candidate("ordinary", 5, 3, 60, "ordinary-session");
        candidate.project_dir = PathBuf::from("/tmp/project");
        let policy = ClassifierPolicy::conservative(24);
        let original = std::env::var_os("TMPDIR");

        std::env::set_var("TMPDIR", "/tmp/first");
        let first = classify(&candidate, &policy, now());
        std::env::set_var("TMPDIR", "/tmp/second");
        let second = classify(&candidate, &policy, now());

        match original {
            Some(value) => std::env::set_var("TMPDIR", value),
            None => std::env::remove_var("TMPDIR"),
        }
        assert_eq!(first, second);
    }
}
