//! Session management handlers
//!
//! Provides interactive session management for Claude Code conversations.
//! Supports listing, viewing, renaming, and deleting sessions with a
//! hierarchical navigation interface.

use anyhow::{Context, Result};
use colored::Colorize;
use inquire::{Confirm, Select, Text};
use serde_json::json;
#[cfg(test)]
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
#[cfg(debug_assertions)]
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(debug_assertions)]
use std::time::Duration;
use std::time::Instant;

use crate::codex::{
    codex_history_path, codex_sessions_dir, load_codex_history_titles, CodexSession,
};
use crate::config::ConfigManager;
use crate::filter::{ConfigSyncSettings, FilterConfig};
use crate::omp::{omp_sessions_dir, OmpSession};
use crate::parser::ConversationSession;
use crate::path_security::{
    canonical_utf8_key, safe_join_within_root, safe_join_within_sync_projects_root,
    safe_project_relative_path, safe_relative_path_within_root, validate_directory_candidate,
    validate_directory_root, validate_regular_candidate, validate_sync_projects_root,
};
use crate::scm;
use crate::session_cache::{
    fingerprint_file, merge_scan_with_report, CacheDelta, CacheFileState, CacheRemoval,
    CacheRetention, CacheUpsert, CachedEntry, SessionIndexCache,
};
use crate::session_diagnostics::{
    error_kind_from_error, legacy_io_warning, legacy_io_warning_from_error, ScanDiagnostics,
    ScanWarningCategory, ScanWarningErrorKind,
};
use crate::sync::discovery::{
    claude_projects_dir, discover_sessions, extract_project_name, find_local_project_by_name,
};
use crate::sync::tombstone::{DeleteReason, DeletionRecord, TombstoneRegistry};
use crate::sync::SyncState;

/// Identifies the system that produced a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionSource {
    /// Claude Code session.
    Claude,
    /// Codex session.
    Codex,
    /// Oh My Pi session.
    Omp,
}

/// Operations supported by a session source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceCapabilities {
    /// Whether the session can be opened in its source application.
    pub can_open: bool,
    /// Whether the session can be renamed.
    pub can_rename: bool,
    /// Whether the session can be deleted.
    pub can_delete: bool,
    /// Whether the session participates in synchronization.
    pub participates_in_sync: bool,
}

impl SessionSource {
    /// Returns the stable lowercase source identifier.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Omp => "omp",
        }
    }

    /// Returns the short display label used by session listings.
    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "CC",
            Self::Codex => "CX",
            Self::Omp => "OM",
        }
    }

    /// Returns the operations supported by this source.
    pub fn capabilities(self) -> SourceCapabilities {
        match self {
            Self::Claude => SourceCapabilities {
                can_open: true,
                can_rename: true,
                can_delete: true,
                participates_in_sync: true,
            },
            Self::Codex => SourceCapabilities {
                can_open: false,
                can_rename: false,
                can_delete: false,
                participates_in_sync: false,
            },
            Self::Omp => SourceCapabilities {
                can_open: true,
                can_rename: false,
                can_delete: false,
                participates_in_sync: false,
            },
        }
    }
}

impl TryFrom<&str> for SessionSource {
    type Error = anyhow::Error;

    fn try_from(value: &str) -> Result<Self> {
        match value {
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            "omp" => Ok(Self::Omp),
            other => anyhow::bail!("Unknown session source: {other}"),
        }
    }
}

/// Stable identity for a session, including its producing source.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionIdentity {
    /// Source that produced the session.
    pub source: SessionSource,
    /// Source-local session identifier.
    pub session_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionSourceFilter {
    All,
    Claude,
    Codex,
    Omp,
}

impl SessionSourceFilter {
    fn includes(self, source: SessionSource) -> bool {
        matches!(
            (self, source),
            (Self::All, _)
                | (Self::Claude, SessionSource::Claude)
                | (Self::Codex, SessionSource::Codex)
                | (Self::Omp, SessionSource::Omp)
        )
    }

    fn includes_claude(self) -> bool {
        matches!(self, Self::All | Self::Claude)
    }

    fn includes_codex(self) -> bool {
        matches!(self, Self::All | Self::Codex)
    }

    fn includes_omp(self) -> bool {
        matches!(self, Self::All | Self::Omp)
    }
}

fn cleanup_available(source: SessionSourceFilter) -> bool {
    source.includes(SessionSource::Claude)
}

fn ensure_restore_source_supported(source: SessionSourceFilter) -> Result<()> {
    if source.includes_claude() {
        Ok(())
    } else {
        anyhow::bail!("Restore is only supported for Claude sessions")
    }
}

fn source_label(source: &str) -> &'static str {
    SessionSource::try_from(source)
        .map(SessionSource::label)
        .unwrap_or("??")
}

fn memory_dir_name_for_source(source: &str) -> &'static str {
    match source {
        "codex" => ".memory",
        "omp" => ".memory",
        _ => "memory",
    }
}

fn memory_dir_for_source(project_dir: &Path, source: &str) -> PathBuf {
    project_dir.join(memory_dir_name_for_source(source))
}

/// User data configuration for saving custom open commands
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
struct UserData {
    /// Global command template for all projects
    /// Uses {path} and {session_id} placeholders
    #[serde(default)]
    command_template: Option<String>,
}

/// Project summary for listing
#[derive(Debug, Clone)]
pub struct ProjectSummary {
    pub name: String,
    pub dir_path: PathBuf,
    pub session_count: usize,
    pub last_activity: Option<String>,
}

/// Session summary for listing and operations
#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub source: String,
    pub session_id: String,
    pub title: String,
    pub project_name: String,
    pub project_dir: PathBuf,
    pub file_path: PathBuf,
    pub message_count: usize,
    pub user_message_count: usize,
    pub assistant_message_count: usize,
    pub first_timestamp: Option<String>,
    pub last_activity: Option<String>,
    pub file_size: u64,
}

impl SessionSummary {
    /// Create a SessionSummary from a ConversationSession.
    /// Message counts use "turn" granularity: consecutive assistant entries
    /// between two user messages count as one assistant turn.
    pub fn from_session(
        session: &ConversationSession,
        project_name: &str,
        project_dir: &Path,
    ) -> Self {
        let file_size = fs::metadata(&session.file_path)
            .map(|m| m.len())
            .unwrap_or(0);

        // Count turns: user turns = non-tool-result user entries,
        // assistant turns = groups of consecutive assistant entries between user entries
        let mut user_count = 0;
        let mut assistant_count = 0;
        let mut in_assistant_turn = false;

        for entry in &session.entries {
            match entry.entry_type.as_str() {
                "user" => {
                    if ConversationSession::is_tool_result_entry(entry) {
                        continue;
                    }
                    user_count += 1;
                    in_assistant_turn = false;
                }
                "assistant" => {
                    if !in_assistant_turn {
                        assistant_count += 1;
                        in_assistant_turn = true;
                    }
                }
                _ => {}
            }
        }

        SessionSummary {
            source: SessionSource::Claude.as_str().to_string(),
            session_id: session.session_id.clone(),
            title: session.title().unwrap_or_else(|| "(No title)".to_string()),
            project_name: project_name.to_string(),
            project_dir: project_dir.to_path_buf(),
            file_path: PathBuf::from(&session.file_path),
            message_count: user_count + assistant_count,
            user_message_count: user_count,
            assistant_message_count: assistant_count,
            first_timestamp: session.first_timestamp(),
            last_activity: session.latest_timestamp(),
            file_size,
        }
    }

    /// Get a truncated title for display (Unicode-safe)
    pub fn display_title(&self, max_chars: usize) -> String {
        let title = self.title.replace('\n', " ");
        let chars: Vec<char> = title.chars().collect();

        if chars.len() > max_chars {
            let truncated: String = chars[..max_chars - 3].iter().collect();
            format!("{}...", truncated)
        } else {
            title
        }
    }

    /// Format relative time for display
    pub fn relative_time(&self) -> String {
        self.last_activity
            .as_ref()
            .map(|ts| format_relative_time(ts))
            .unwrap_or_else(|| "Unknown".to_string())
    }

    fn source_kind(&self) -> Result<SessionSource> {
        SessionSource::try_from(self.source.as_str())
    }

    fn identity(&self) -> Result<SessionIdentity> {
        Ok(SessionIdentity {
            source: self.source_kind()?,
            session_id: self.session_id.clone(),
        })
    }
}

impl SessionSummary {
    /// Create a SessionSummary from a Codex session.
    pub fn from_codex_session(session: &CodexSession, project_name: &str, title: String) -> Self {
        let file_size = fs::metadata(&session.file_path)
            .map(|m| m.len())
            .unwrap_or(0);
        let messages = session.display_messages(false);
        let user_count = messages.iter().filter(|m| m.role == "user").count();
        let assistant_count = messages.iter().filter(|m| m.role == "assistant").count();

        SessionSummary {
            source: SessionSource::Codex.as_str().to_string(),
            session_id: session.session_id.clone(),
            title,
            project_name: project_name.to_string(),
            project_dir: session
                .cwd
                .as_deref()
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    session
                        .file_path
                        .parent()
                        .map(Path::to_path_buf)
                        .unwrap_or_default()
                }),
            file_path: session.file_path.clone(),
            message_count: user_count + assistant_count,
            user_message_count: user_count,
            assistant_message_count: assistant_count,
            first_timestamp: session.first_timestamp(),
            last_activity: session.latest_timestamp(),
            file_size,
        }
    }

    /// Create a SessionSummary from an OMP session.
    pub fn from_omp_session(session: &OmpSession, project_name: &str) -> Self {
        let file_size = fs::metadata(&session.file_path)
            .map(|m| m.len())
            .unwrap_or(0);
        let messages = session.display_messages();
        let user_count = messages.iter().filter(|m| m.role == "user").count();
        let assistant_count = messages.iter().filter(|m| m.role == "assistant").count();
        let title = session.title_from_messages(&messages);

        SessionSummary {
            source: SessionSource::Omp.as_str().to_string(),
            session_id: session.session_id.clone(),
            title,
            project_name: project_name.to_string(),
            project_dir: session
                .cwd
                .as_deref()
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    session
                        .file_path
                        .parent()
                        .and_then(|p| p.parent())
                        .map(Path::to_path_buf)
                        .unwrap_or_default()
                }),
            file_path: session.file_path.clone(),
            message_count: user_count + assistant_count,
            user_message_count: user_count,
            assistant_message_count: assistant_count,
            first_timestamp: session.first_timestamp(),
            last_activity: session.latest_timestamp(),
            file_size,
        }
    }
}

/// Format a timestamp as relative time (e.g., "Today", "Yesterday", "3 days ago")
fn format_relative_time(timestamp: &str) -> String {
    use chrono::{DateTime, Utc};

    if let Ok(dt) = DateTime::parse_from_rfc3339(timestamp) {
        let now = Utc::now();
        let dt_utc = dt.with_timezone(&Utc);
        let duration = now.signed_duration_since(dt_utc);

        let days = duration.num_days();
        let hours = duration.num_hours();
        let minutes = duration.num_minutes();

        if days == 0 {
            if hours == 0 {
                if minutes <= 1 {
                    "Just now".to_string()
                } else {
                    format!("{} min ago", minutes)
                }
            } else if hours == 1 {
                "1 hour ago".to_string()
            } else {
                format!("{} hours ago", hours)
            }
        } else if days == 1 {
            "Yesterday".to_string()
        } else if days < 7 {
            format!("{} days ago", days)
        } else if days < 30 {
            let weeks = days / 7;
            if weeks == 1 {
                "1 week ago".to_string()
            } else {
                format!("{} weeks ago", weeks)
            }
        } else {
            let months = days / 30;
            if months == 1 {
                "1 month ago".to_string()
            } else {
                format!("{} months ago", months)
            }
        }
    } else {
        "Unknown".to_string()
    }
}

/// Menu choice for project selection
enum ProjectMenuChoice {
    Select(ProjectSummary),
    Exit,
}

/// Menu choice for session selection
#[allow(clippy::large_enum_variant)]
enum SessionMenuChoice {
    Select(SessionSummary),
    Search,
    Cleanup,
    SwitchProject,
    Exit,
}

/// Menu choice for session actions
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActionChoice {
    OpenInEditor,
    ViewDetails,
    Rename,
    Delete,
    Back,
}

fn action_choices_for_source(source: SessionSource) -> Vec<ActionChoice> {
    let capabilities = source.capabilities();
    let mut actions = Vec::new();
    if capabilities.can_open {
        actions.push(ActionChoice::OpenInEditor);
    }
    actions.push(ActionChoice::ViewDetails);
    if capabilities.can_rename {
        actions.push(ActionChoice::Rename);
    }
    if capabilities.can_delete {
        actions.push(ActionChoice::Delete);
    }
    actions.push(ActionChoice::Back);
    actions
}

fn ensure_can_rename(session: &SessionSummary) -> Result<()> {
    let source = session.source_kind()?;
    if source.capabilities().can_rename {
        Ok(())
    } else {
        anyhow::bail!(
            "{} sessions are read-only and cannot be renamed",
            source.label()
        )
    }
}

fn ensure_can_delete(session: &SessionSummary) -> Result<()> {
    let source = session.source_kind()?;
    if source.capabilities().can_delete {
        Ok(())
    } else {
        anyhow::bail!(
            "{} sessions are read-only and cannot be deleted",
            source.label()
        )
    }
}

// ============================================================================
// Core Functions
// ============================================================================

/// Filesystem roots used by the three session sources.
#[derive(Debug, Clone)]
pub(crate) struct SessionRoots {
    pub claude_projects: PathBuf,
    pub codex_sessions: PathBuf,
    pub codex_history: PathBuf,
    pub omp_sessions: PathBuf,
}

impl SessionRoots {
    fn discover() -> Result<Self> {
        Ok(Self {
            claude_projects: claude_projects_dir()?,
            codex_sessions: codex_sessions_dir()?,
            codex_history: codex_history_path()?,
            omp_sessions: omp_sessions_dir()?,
        })
    }
}

/// Scan all projects and return summaries
pub fn scan_all_projects() -> Result<Vec<ProjectSummary>> {
    let claude_dir = claude_projects_dir()?;

    match fs::symlink_metadata(&claude_dir) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            legacy_io_warning("claude", "metadata");
            return Ok(Vec::new());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Vec::new());
        }
        Err(error) => {
            let error = anyhow::Error::new(error);
            legacy_io_warning_from_error("claude", "metadata", &error);
            return Ok(Vec::new());
        }
    }
    if let Err(error) = validate_directory_root(&claude_dir) {
        legacy_io_warning_from_error("claude", "root", &error);
        return Ok(Vec::new());
    }

    let mut projects = Vec::new();
    // Use a filter with no file size limit for session listing
    let filter = FilterConfig::no_size_limit();

    let entries = match fs::read_dir(&claude_dir) {
        Ok(entries) => entries,
        Err(error) => {
            let error = anyhow::Error::new(error);
            legacy_io_warning_from_error("claude", "read_dir", &error);
            return Ok(Vec::new());
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                let error = anyhow::Error::new(error);
                legacy_io_warning_from_error("claude", "read_dir", &error);
                continue;
            }
        };
        let path = entry.path();

        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => {
                let error = anyhow::Error::new(error);
                legacy_io_warning_from_error("claude", "project_metadata", &error);
                continue;
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            continue;
        }
        if let Err(error) = validate_directory_candidate(&claude_dir, &path) {
            legacy_io_warning_from_error("claude", "project_boundary", &error);
            continue;
        }

        let dir_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();

        // Skip hidden directories
        if dir_name.starts_with('.') {
            continue;
        }

        // Scan sessions in this project; one unreadable project must not abort the list.
        let sessions = match discover_sessions(&path, &filter) {
            Ok(sessions) => sessions,
            Err(error) => {
                legacy_io_warning_from_error("claude", "discover", &error);
                continue;
            }
        };

        if sessions.is_empty() {
            continue;
        }

        // Get project name from session's cwd field (more accurate than directory name)
        // Fall back to extract_project_name if no cwd is available, unless it's a
        // non-ASCII encoded dir ending in '-'
        let project_name = sessions
            .iter()
            .find_map(|s| s.project_name().map(|n| n.to_string()))
            .unwrap_or_else(|| {
                if dir_name.ends_with('-') {
                    dir_name.to_string()
                } else {
                    extract_project_name(dir_name).to_string()
                }
            });

        // Count only valid sessions (with messages and real titles)
        let valid_session_count = sessions.iter().filter(|s| is_valid_session(s)).count();

        // Skip projects with no valid sessions
        if valid_session_count == 0 {
            continue;
        }

        // Find latest activity from valid sessions only
        let last_activity = sessions
            .iter()
            .filter(|s| s.message_count() > 0)
            .filter_map(|s| s.latest_timestamp())
            .max();

        projects.push(ProjectSummary {
            name: project_name,
            dir_path: path,
            session_count: valid_session_count,
            last_activity,
        });
    }

    // Sort by last activity (most recent first)
    projects.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));

    Ok(projects)
}

/// Check if a ConversationSession is valid (has messages and a real title)
fn is_valid_session(session: &ConversationSession) -> bool {
    session.message_count() > 0 && session.title().is_some()
}

/// Check if a SessionSummary is valid (has messages and a real title)
fn is_valid_session_summary(summary: &SessionSummary) -> bool {
    summary.message_count > 0 && summary.title != "(No title)"
}

/// Scan sessions for a specific project, returns (valid_sessions, filtered_count).
///
/// This legacy public helper is retained for downstream callers. The active multi-source
/// listing path uses `scan_all_session_summaries` instead.
#[allow(dead_code)] // Public pre-diagnostics compatibility API; covered by scan wrapper tests.
pub fn scan_project_sessions_with_filtered(
    project: &ProjectSummary,
) -> Result<(Vec<SessionSummary>, usize)> {
    // Use a filter with no file size limit for session listing
    let filter = FilterConfig::no_size_limit();
    let sessions = discover_sessions(&project.dir_path, &filter)?;

    let all_summaries: Vec<SessionSummary> = sessions
        .iter()
        .map(|s| SessionSummary::from_session(s, &project.name, &project.dir_path))
        .collect();

    let total_count = all_summaries.len();

    let mut valid_summaries: Vec<SessionSummary> = all_summaries
        .into_iter()
        .filter(is_valid_session_summary)
        .collect();

    // Sort by last activity (most recent first)
    valid_summaries.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));

    let filtered_count = total_count - valid_summaries.len();
    Ok((valid_summaries, filtered_count))
}

/// Scan sessions for a specific project.
///
/// This legacy public helper is retained for downstream callers. The active multi-source
/// listing path uses `scan_all_session_summaries` instead.
#[allow(dead_code)] // Public pre-diagnostics compatibility API; delegates to filtered wrapper.
pub fn scan_project_sessions(project: &ProjectSummary) -> Result<Vec<SessionSummary>> {
    let (sessions, _) = scan_project_sessions_with_filtered(project)?;
    Ok(sessions)
}

/// Result of scanning the configured session sources.
#[derive(Debug)]
pub struct SessionScanResult {
    /// Sessions that survived parsing, source filtering, project filtering, and validity checks.
    pub summaries: Vec<SessionSummary>,
    /// Counters and bounded warnings collected during the scan.
    pub diagnostics: ScanDiagnostics,
}

#[derive(Debug, Default)]
struct SourceScanTracker {
    seen_by_source: HashMap<String, HashSet<String>>,
    started_sources: HashSet<String>,
    incomplete_sources: HashSet<String>,
}

impl SourceScanTracker {
    fn begin(&mut self, source: &str) {
        self.started_sources.insert(source.to_string());
        self.seen_by_source.entry(source.to_string()).or_default();
    }

    fn seen(&mut self, source: &str, key: String) {
        self.seen_by_source
            .entry(source.to_string())
            .or_default()
            .insert(key);
    }

    fn mark_incomplete(&mut self, source: &str) {
        self.incomplete_sources.insert(source.to_string());
    }

    fn retention(&self) -> CacheRetention {
        let completed_sources = self
            .started_sources
            .difference(&self.incomplete_sources)
            .cloned()
            .collect();
        CacheRetention {
            seen_by_source: self.seen_by_source.clone(),
            completed_sources,
        }
    }
}

fn elapsed_millis(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn scan_warning_message(diagnostics: &ScanDiagnostics) -> Option<String> {
    diagnostics.degraded().then(|| diagnostics.summary_line())
}

fn emit_scan_warning_to<F>(diagnostics: &ScanDiagnostics, mut writer: F)
where
    F: FnMut(&str),
{
    if let Some(message) = scan_warning_message(diagnostics) {
        writer(&format!(
            "WARNING: {message}. Run with --debug and inspect the ccs log for details."
        ));
    }
}

fn emit_scan_warning(diagnostics: &ScanDiagnostics) {
    emit_scan_warning_to(diagnostics, |message| eprintln!("{message}"));
}

fn scan_summaries_for_interactive(result: SessionScanResult) -> Vec<SessionSummary> {
    emit_scan_warning(&result.diagnostics);
    result.summaries
}

fn scan_summaries_for_mutation(result: SessionScanResult) -> Result<Vec<SessionSummary>> {
    emit_scan_warning(&result.diagnostics);
    if result.diagnostics.degraded() {
        anyhow::bail!(
            "session mutation aborted because the source scan was incomplete: {}",
            result.diagnostics.summary_line()
        );
    }
    Ok(result.summaries)
}

fn attach_scan_diagnostics(
    mut payload: serde_json::Value,
    diagnostics: &ScanDiagnostics,
) -> serde_json::Value {
    if let serde_json::Value::Object(object) = &mut payload {
        object.insert("schema_version".to_string(), json!(1));
        object.insert(
            "diagnostics".to_string(),
            serde_json::to_value(diagnostics).expect("scan diagnostics should serialize"),
        );
    }
    payload
}

#[cfg(debug_assertions)]
fn wait_for_test_cache_merge_gate() -> Result<()> {
    let Some(ready) = env::var_os("CCS_TEST_SESSION_CACHE_MERGE_READY") else {
        return Ok(());
    };
    let Some(release) = env::var_os("CCS_TEST_SESSION_CACHE_MERGE_RELEASE") else {
        anyhow::bail!(
            "CCS_TEST_SESSION_CACHE_MERGE_READY requires CCS_TEST_SESSION_CACHE_MERGE_RELEASE"
        );
    };
    let ready = PathBuf::from(ready);
    let release = PathBuf::from(release);
    fs::write(&ready, b"ready")
        .with_context(|| format!("write test cache merge ready marker {}", ready.display()))?;
    let deadline = Instant::now() + Duration::from_secs(30);
    while !release.exists() {
        if Instant::now() >= deadline {
            anyhow::bail!(
                "timed out waiting for test cache merge release marker {}",
                release.display()
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    if env::var_os("CCS_TEST_SESSION_CACHE_MERGE_FAIL").is_some() {
        anyhow::bail!("forced test cache merge failure");
    }
    if env::var_os("CCS_TEST_SESSION_CACHE_HOLD_AFTER_RELEASE").is_some() {
        let Some(hold_release) = env::var_os("CCS_TEST_SESSION_CACHE_HOLD_RELEASE") else {
            anyhow::bail!(
                "CCS_TEST_SESSION_CACHE_HOLD_AFTER_RELEASE requires CCS_TEST_SESSION_CACHE_HOLD_RELEASE"
            );
        };
        let hold_release = PathBuf::from(hold_release);
        let hold_deadline = Instant::now() + Duration::from_secs(30);
        while !hold_release.exists() {
            if Instant::now() >= hold_deadline {
                anyhow::bail!(
                    "timed out waiting for test cache hold release marker {}",
                    hold_release.display()
                );
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    Ok(())
}

/// Scan all sources and retain the legacy summary-only return type.
#[allow(dead_code)]
fn scan_all_session_summaries(
    project_filter: Option<&str>,
    source: SessionSourceFilter,
) -> Result<Vec<SessionSummary>> {
    Ok(scan_all_session_summaries_with_report(project_filter, source)?.summaries)
}

/// Scan all session sources using discovered filesystem roots.
fn scan_all_session_summaries_with_report(
    project_filter: Option<&str>,
    source: SessionSourceFilter,
) -> Result<SessionScanResult> {
    let discovery_started = Instant::now();
    let roots = SessionRoots::discover()?;
    let discovery_ms = elapsed_millis(discovery_started);
    let config_dir = ConfigManager::config_dir()?;
    let mut result =
        scan_all_session_summaries_with_roots(project_filter, source, &roots, &config_dir)?;
    result.diagnostics.source_discovery_ms = discovery_ms;
    Ok(result)
}

/// Scan all session sources with injected roots and cache configuration.
///
/// This is crate-visible so tests can exercise cold, warm, corrupt-cache, and
/// filesystem-error paths without touching a user's real session directories.
pub(crate) fn scan_all_session_summaries_with_roots(
    project_filter: Option<&str>,
    source: SessionSourceFilter,
    roots: &SessionRoots,
    config_dir: &Path,
) -> Result<SessionScanResult> {
    let started = Instant::now();
    let mut diagnostics = ScanDiagnostics::new();
    let cache_started = Instant::now();
    let cache_status = SessionIndexCache::load_with_status(config_dir);
    diagnostics.cache_load_ms = elapsed_millis(cache_started);
    let cache_warning = cache_status.warning;
    let cache = cache_status.cache;
    if let Some(warning) = cache_warning.as_deref() {
        diagnostics.record_warning(
            None,
            "load",
            ScanWarningCategory::Cache,
            Some(config_dir),
            warning,
        );
    }

    let mut delta = CacheDelta::default();
    let mut tracker = SourceScanTracker::default();
    let mut summaries = Vec::new();

    if source.includes_claude() {
        tracker.begin("claude");
        let scan_started = Instant::now();
        scan_claude_summaries_cached(
            &roots.claude_projects,
            &cache,
            &mut delta,
            &mut tracker,
            &mut summaries,
            &mut diagnostics,
            project_filter,
        )?;
        diagnostics.claude_scan_ms = elapsed_millis(scan_started);
    }
    if source.includes_codex() {
        tracker.begin("codex");
        let scan_started = Instant::now();
        scan_codex_summaries_cached(
            &roots.codex_sessions,
            &roots.codex_history,
            &cache,
            &mut delta,
            &mut tracker,
            &mut summaries,
            &mut diagnostics,
            project_filter,
        )?;
        diagnostics.codex_scan_ms = elapsed_millis(scan_started);
    }
    if source.includes_omp() {
        tracker.begin("omp");
        let scan_started = Instant::now();
        scan_omp_summaries_cached(
            &roots.omp_sessions,
            &cache,
            &mut delta,
            &mut tracker,
            &mut summaries,
            &mut diagnostics,
            project_filter,
        )?;
        diagnostics.omp_scan_ms = elapsed_millis(scan_started);
    }

    let retention = tracker.retention();

    #[cfg(debug_assertions)]
    wait_for_test_cache_merge_gate()?;

    let cache_save_started = Instant::now();
    let cache_save_result = merge_scan_with_report(config_dir, &delta, &retention);
    diagnostics.cache_save_ms = elapsed_millis(cache_save_started);
    match cache_save_result {
        Ok(report) => {
            for issue in report.revalidation_issues {
                diagnostics.record_warning_with_kind(
                    None,
                    "merge",
                    ScanWarningCategory::Cache,
                    issue.error_kind,
                    Some(Path::new(&issue.key)),
                    &issue.detail,
                );
            }
        }
        Err(error) => {
            let error_text = format!("{error:#}");
            let duplicate_recovery_warning = match cache_warning.as_deref() {
                Some("cache data invalid") => {
                    error_text.contains("invalid cache requires complete source scans")
                }
                Some("cache version mismatch") => {
                    error_text.contains("version mismatch requires complete source scans")
                }
                _ => false,
            };
            if !duplicate_recovery_warning {
                diagnostics.record_warning_from_error(
                    None,
                    "merge",
                    ScanWarningCategory::Cache,
                    Some(config_dir),
                    &error,
                );
            }
        }
    }

    summaries.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));
    diagnostics.elapsed_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;

    Ok(SessionScanResult {
        summaries,
        diagnostics,
    })
}

struct CandidateFile {
    path_key: Option<String>,
    file_size: u64,
    mtime_secs: i64,
    content_fingerprint: String,
}

impl CandidateFile {
    fn file_state(&self) -> CacheFileState {
        CacheFileState {
            file_size: self.file_size,
            mtime_secs: self.mtime_secs,
            content_fingerprint: self.content_fingerprint.clone(),
        }
    }
}

fn cache_entry_from_candidate(candidate: &CandidateFile, summary: &SessionSummary) -> CachedEntry {
    CachedEntry {
        file_size: candidate.file_size,
        mtime_secs: candidate.mtime_secs,
        content_fingerprint: candidate.content_fingerprint.clone(),
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
    }
}

fn cache_mtime_from_modified(modified: std::io::Result<std::time::SystemTime>) -> Option<i64> {
    let modified = modified.ok()?;
    let duration = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    i64::try_from(duration.as_secs()).ok()
}

fn inspect_candidate_file(
    root: &Path,
    file_path: &Path,
    filter: &FilterConfig,
    source: &str,
    tracker: &mut SourceScanTracker,
    diagnostics: &mut ScanDiagnostics,
) -> Option<CandidateFile> {
    if file_path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
        return None;
    }
    diagnostics.files_seen += 1;
    if !filter.should_include(file_path) {
        diagnostics.files_skipped += 1;
        return None;
    }

    let metadata_started = Instant::now();
    let metadata = match fs::symlink_metadata(file_path) {
        Ok(metadata) => metadata,
        Err(error) => {
            tracker.mark_incomplete(source);
            diagnostics.metadata_ms = diagnostics
                .metadata_ms
                .saturating_add(elapsed_millis(metadata_started));
            let error = anyhow::Error::new(error);
            diagnostics.record_warning_from_error(
                Some(source),
                "metadata",
                ScanWarningCategory::Io,
                Some(file_path),
                &error,
            );
            return None;
        }
    };
    if metadata.file_type().is_symlink() {
        diagnostics.files_skipped += 1;
        return None;
    }
    if !metadata.file_type().is_file() {
        tracker.mark_incomplete(source);
        diagnostics.metadata_ms = diagnostics
            .metadata_ms
            .saturating_add(elapsed_millis(metadata_started));
        diagnostics.record_warning_with_kind(
            Some(source),
            "metadata",
            ScanWarningCategory::Io,
            ScanWarningErrorKind::Unknown,
            Some(file_path),
            "candidate is not a regular file",
        );
        return None;
    }
    if let Err(error) = validate_regular_candidate(root, file_path) {
        tracker.mark_incomplete(source);
        diagnostics.files_skipped += 1;
        diagnostics.record_warning_from_error(
            Some(source),
            "metadata",
            ScanWarningCategory::Io,
            Some(file_path),
            &error,
        );
        return None;
    }
    diagnostics.bytes_considered = diagnostics.bytes_considered.saturating_add(metadata.len());

    let modified = match metadata.modified() {
        Ok(modified) => modified,
        Err(error) => {
            tracker.mark_incomplete(source);
            diagnostics.metadata_ms = diagnostics
                .metadata_ms
                .saturating_add(elapsed_millis(metadata_started));
            let error = anyhow::Error::new(error);
            diagnostics.record_warning_from_error(
                Some(source),
                "metadata",
                ScanWarningCategory::Io,
                Some(file_path),
                &error,
            );
            return None;
        }
    };
    let mtime_secs = match cache_mtime_from_modified(Ok(modified)) {
        Some(mtime_secs) => mtime_secs,
        None => {
            tracker.mark_incomplete(source);
            diagnostics.metadata_ms = diagnostics
                .metadata_ms
                .saturating_add(elapsed_millis(metadata_started));
            diagnostics.record_warning_with_kind(
                Some(source),
                "metadata",
                ScanWarningCategory::Io,
                ScanWarningErrorKind::Unknown,
                Some(file_path),
                "modified timestamp unavailable; candidate is not cacheable",
            );
            return None;
        }
    };
    diagnostics.metadata_ms = diagnostics
        .metadata_ms
        .saturating_add(elapsed_millis(metadata_started));

    let fingerprint_started = Instant::now();
    let fingerprint = match fingerprint_file(file_path) {
        Ok(fingerprint) => fingerprint,
        Err(error) => {
            tracker.mark_incomplete(source);
            diagnostics.fingerprint_ms = diagnostics
                .fingerprint_ms
                .saturating_add(elapsed_millis(fingerprint_started));
            diagnostics.record_warning_from_error(
                Some(source),
                "fingerprint",
                ScanWarningCategory::Io,
                Some(file_path),
                &error,
            );
            return None;
        }
    };
    diagnostics.fingerprint_ms = diagnostics
        .fingerprint_ms
        .saturating_add(elapsed_millis(fingerprint_started));
    diagnostics.fingerprinted_bytes = diagnostics
        .fingerprinted_bytes
        .saturating_add(fingerprint.bytes);

    Some(CandidateFile {
        path_key: canonical_utf8_key(file_path),
        file_size: metadata.len(),
        mtime_secs,
        content_fingerprint: fingerprint.digest,
    })
}

fn handle_walk_entry(
    result: Result<walkdir::DirEntry, walkdir::Error>,
    source: &str,
    diagnostics: &mut ScanDiagnostics,
) -> Option<walkdir::DirEntry> {
    match result {
        Ok(entry) => Some(entry),
        Err(error) => {
            let path = error.path().map(Path::to_path_buf);
            let error = anyhow::Error::new(error);
            diagnostics.record_warning_from_error(
                Some(source),
                "read_dir",
                ScanWarningCategory::Io,
                path.as_deref(),
                &error,
            );
            None
        }
    }
}

fn root_is_available(root: &Path, source: &str, diagnostics: &mut ScanDiagnostics) -> bool {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            if let Err(error) = validate_directory_root(root) {
                diagnostics.record_warning_from_error(
                    Some(source),
                    "root_boundary",
                    ScanWarningCategory::Io,
                    Some(root),
                    &error,
                );
                false
            } else {
                true
            }
        }
        Ok(_) => {
            diagnostics.record_warning_with_kind(
                Some(source),
                "metadata",
                ScanWarningCategory::Io,
                ScanWarningErrorKind::Unknown,
                Some(root),
                "session source root must be a non-symlink directory",
            );
            false
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            let error = anyhow::Error::new(error);
            diagnostics.record_warning_from_error(
                Some(source),
                "metadata",
                ScanWarningCategory::Io,
                Some(root),
                &error,
            );
            false
        }
    }
}

#[cfg(test)]
thread_local! {
    static TEST_REMOVE_BEFORE_PARSE: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

#[cfg(test)]
fn set_test_remove_before_parse(path: Option<PathBuf>) {
    TEST_REMOVE_BEFORE_PARSE.with(|value| *value.borrow_mut() = path);
}

#[cfg(test)]
fn run_parser_test_hook(path: &Path) {
    TEST_REMOVE_BEFORE_PARSE.with(|value| {
        if value.borrow().as_deref() == Some(path) {
            let _ = fs::remove_file(path);
            *value.borrow_mut() = None;
        }
    });
}

#[cfg(not(test))]
fn run_parser_test_hook(_path: &Path) {}

fn handle_parser_error(
    source: &str,
    file_path: &Path,
    candidate: &CandidateFile,
    error: &anyhow::Error,
    tracker: &mut SourceScanTracker,
    delta: &mut CacheDelta,
    diagnostics: &mut ScanDiagnostics,
) {
    let error_kind = error_kind_from_error(error);
    let is_io_error = matches!(
        error_kind,
        ScanWarningErrorKind::PermissionDenied
            | ScanWarningErrorKind::NotFound
            | ScanWarningErrorKind::ReadFailed
            | ScanWarningErrorKind::ChangedDuringRead
    );
    if is_io_error {
        tracker.mark_incomplete(source);
        diagnostics.record_warning_from_error(
            Some(source),
            "parse",
            ScanWarningCategory::Io,
            Some(file_path),
            error,
        );
        return;
    }

    if let Some(key) = candidate.path_key.clone() {
        delta.removals.push(CacheRemoval {
            key,
            expected: candidate.file_state(),
        });
    }
    diagnostics.record_warning_with_kind(
        Some(source),
        "parse",
        ScanWarningCategory::Data,
        ScanWarningErrorKind::InvalidData,
        Some(file_path),
        &format!("{error:#}"),
    );
}

/// Scan Claude Code sessions with index cache and diagnostics.
fn scan_claude_summaries_cached(
    root: &Path,
    cache: &SessionIndexCache,
    delta: &mut CacheDelta,
    tracker: &mut SourceScanTracker,
    summaries: &mut Vec<SessionSummary>,
    diagnostics: &mut ScanDiagnostics,
    project_filter: Option<&str>,
) -> Result<()> {
    use walkdir::WalkDir;

    if !root_is_available(root, "claude", diagnostics) {
        tracker.mark_incomplete("claude");
        return Ok(());
    }
    let filter = FilterConfig::no_size_limit();
    let mut session_map: HashMap<String, SessionSummary> = HashMap::new();

    // A Claude root that cannot be opened is one degraded source, not a fatal
    // scan failure: preserve results from Codex/OMP and any earlier source.
    let project_entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) => {
            tracker.mark_incomplete("claude");
            let error = anyhow::Error::new(error);
            diagnostics.record_warning_from_error(
                Some("claude"),
                "read_dir",
                ScanWarningCategory::Io,
                Some(root),
                &error,
            );
            return Ok(());
        }
    };
    for dir_entry in project_entries {
        let dir_entry = match dir_entry {
            Ok(entry) => entry,
            Err(error) => {
                tracker.mark_incomplete("claude");
                let error = anyhow::Error::new(error);
                diagnostics.record_warning_from_error(
                    Some("claude"),
                    "read_dir",
                    ScanWarningCategory::Io,
                    Some(root),
                    &error,
                );
                continue;
            }
        };
        let project_path = dir_entry.path();
        let file_type = match dir_entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                tracker.mark_incomplete("claude");
                let error = anyhow::Error::new(error);
                diagnostics.record_warning_from_error(
                    Some("claude"),
                    "metadata",
                    ScanWarningCategory::Io,
                    Some(&project_path),
                    &error,
                );
                continue;
            }
        };
        if !file_type.is_dir() {
            continue;
        }
        let dir_name = project_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if dir_name.starts_with('.') {
            continue;
        }

        let mut dir_project_name: Option<String> = None;
        for entry in WalkDir::new(&project_path).follow_links(false).into_iter() {
            if entry.is_err() {
                tracker.mark_incomplete("claude");
            }
            let Some(entry) = handle_walk_entry(entry, "claude", diagnostics) else {
                continue;
            };
            let file_path = entry.path();
            let Some(candidate) =
                inspect_candidate_file(root, file_path, &filter, "claude", tracker, diagnostics)
            else {
                continue;
            };
            if let Some(path_key) = candidate.path_key.as_ref() {
                tracker.seen("claude", path_key.clone());
                if let Some(summary) = cache.lookup_with_fingerprint(
                    path_key,
                    file_path,
                    candidate.file_size,
                    candidate.mtime_secs,
                    &candidate.content_fingerprint,
                ) {
                    diagnostics.cache_hits += 1;
                    if dir_project_name.is_none() {
                        dir_project_name = Some(summary.project_name.clone());
                    }
                    if project_filter.is_some_and(|name| summary.project_name != name)
                        || !is_valid_session_summary(&summary)
                    {
                        diagnostics.files_skipped += 1;
                        continue;
                    }
                    session_map
                        .entry(summary.session_id.clone())
                        .and_modify(|existing| {
                            if summary.message_count > existing.message_count {
                                *existing = summary.clone();
                            }
                        })
                        .or_insert(summary);
                    continue;
                }
                diagnostics.cache_misses += 1;
            }

            let parse_started = Instant::now();
            diagnostics.parsed_bytes = diagnostics.parsed_bytes.saturating_add(candidate.file_size);
            run_parser_test_hook(file_path);
            match ConversationSession::from_file_with_report(file_path) {
                Ok(outcome) => {
                    diagnostics.parse_ms = diagnostics
                        .parse_ms
                        .saturating_add(elapsed_millis(parse_started));
                    diagnostics.files_parsed += 1;
                    let malformed_lines = outcome.malformed_lines;
                    let session = outcome.value;
                    if malformed_lines > 0 {
                        if let Some(key) = candidate.path_key.clone() {
                            delta.removals.push(CacheRemoval {
                                key,
                                expected: candidate.file_state(),
                            });
                        }
                        diagnostics.record_warning(
                            Some("claude"),
                            "parse",
                            ScanWarningCategory::Data,
                            Some(file_path),
                            &format!("partial parse malformed_lines={malformed_lines}"),
                        );
                    }
                    if dir_project_name.is_none() {
                        dir_project_name = session.project_name().map(str::to_string);
                    }
                    let project_name = dir_project_name.clone().unwrap_or_else(|| {
                        if dir_name.ends_with('-') {
                            dir_name.to_string()
                        } else {
                            extract_project_name(dir_name).to_string()
                        }
                    });
                    let mut summary =
                        SessionSummary::from_session(&session, &project_name, &project_path);
                    summary.file_size = candidate.file_size;
                    if malformed_lines == 0 {
                        if let Some(key) = candidate.path_key.clone() {
                            delta.upserts.push(CacheUpsert {
                                key,
                                expected: candidate.file_state(),
                                entry: cache_entry_from_candidate(&candidate, &summary),
                            });
                        }
                    }
                    if project_filter.is_some_and(|name| summary.project_name != name)
                        || !is_valid_session_summary(&summary)
                    {
                        diagnostics.files_skipped += 1;
                        continue;
                    }
                    session_map
                        .entry(summary.session_id.clone())
                        .and_modify(|existing| {
                            if summary.message_count > existing.message_count {
                                *existing = summary.clone();
                            }
                        })
                        .or_insert(summary);
                }
                Err(error) => {
                    diagnostics.parse_ms = diagnostics
                        .parse_ms
                        .saturating_add(elapsed_millis(parse_started));
                    handle_parser_error(
                        "claude",
                        file_path,
                        &candidate,
                        &error,
                        tracker,
                        delta,
                        diagnostics,
                    );
                }
            }
        }
    }

    summaries.extend(session_map.into_values());
    Ok(())
}

/// Scan Codex sessions with index cache and diagnostics.
#[allow(clippy::too_many_arguments)]
fn scan_codex_summaries_cached(
    sessions_root: &Path,
    history_path: &Path,
    cache: &SessionIndexCache,
    delta: &mut CacheDelta,
    tracker: &mut SourceScanTracker,
    summaries: &mut Vec<SessionSummary>,
    diagnostics: &mut ScanDiagnostics,
    project_filter: Option<&str>,
) -> Result<()> {
    use walkdir::WalkDir;

    if !root_is_available(sessions_root, "codex", diagnostics) {
        tracker.mark_incomplete("codex");
        return Ok(());
    }
    let filter = FilterConfig::no_size_limit();
    let titles = match load_codex_history_titles(history_path) {
        Ok(titles) => titles,
        Err(error) => {
            diagnostics.record_warning_from_error(
                Some("codex"),
                "read",
                ScanWarningCategory::Io,
                Some(history_path),
                &error,
            );
            HashMap::new()
        }
    };

    for entry in WalkDir::new(sessions_root).follow_links(false).into_iter() {
        if entry.is_err() {
            tracker.mark_incomplete("codex");
        }
        let Some(entry) = handle_walk_entry(entry, "codex", diagnostics) else {
            continue;
        };
        let file_path = entry.path();
        let Some(candidate) = inspect_candidate_file(
            sessions_root,
            file_path,
            &filter,
            "codex",
            tracker,
            diagnostics,
        ) else {
            continue;
        };
        if let Some(path_key) = candidate.path_key.as_ref() {
            tracker.seen("codex", path_key.clone());
            if let Some(summary) = cache.lookup_with_fingerprint(
                path_key,
                file_path,
                candidate.file_size,
                candidate.mtime_secs,
                &candidate.content_fingerprint,
            ) {
                diagnostics.cache_hits += 1;
                if project_filter.is_some_and(|name| summary.project_name != name) {
                    diagnostics.files_skipped += 1;
                } else if is_valid_session_summary(&summary) {
                    summaries.push(summary);
                } else {
                    diagnostics.files_skipped += 1;
                }
                continue;
            }
            diagnostics.cache_misses += 1;
        }

        let parse_started = Instant::now();
        diagnostics.parsed_bytes = diagnostics.parsed_bytes.saturating_add(candidate.file_size);
        run_parser_test_hook(file_path);
        match CodexSession::from_file_with_report(file_path) {
            Ok(outcome) => {
                diagnostics.parse_ms = diagnostics
                    .parse_ms
                    .saturating_add(elapsed_millis(parse_started));
                diagnostics.files_parsed += 1;
                let malformed_lines = outcome.malformed_lines;
                let session = outcome.value;
                let project_name = session.project_name().unwrap_or("codex");
                let title = session.title(titles.get(&session.session_id).map(String::as_str));
                let mut summary = SessionSummary::from_codex_session(&session, project_name, title);
                summary.file_size = candidate.file_size;
                if malformed_lines > 0 {
                    if let Some(key) = candidate.path_key.clone() {
                        delta.removals.push(CacheRemoval {
                            key,
                            expected: candidate.file_state(),
                        });
                    }
                    diagnostics.record_warning(
                        Some("codex"),
                        "parse",
                        ScanWarningCategory::Data,
                        Some(file_path),
                        &format!("partial parse malformed_lines={malformed_lines}"),
                    );
                } else if let Some(key) = candidate.path_key.clone() {
                    delta.upserts.push(CacheUpsert {
                        key,
                        expected: candidate.file_state(),
                        entry: cache_entry_from_candidate(&candidate, &summary),
                    });
                }
                if project_filter.is_some_and(|name| summary.project_name != name) {
                    diagnostics.files_skipped += 1;
                } else if is_valid_session_summary(&summary) {
                    summaries.push(summary);
                } else {
                    diagnostics.files_skipped += 1;
                }
            }
            Err(error) => {
                diagnostics.parse_ms = diagnostics
                    .parse_ms
                    .saturating_add(elapsed_millis(parse_started));
                handle_parser_error(
                    "codex",
                    file_path,
                    &candidate,
                    &error,
                    tracker,
                    delta,
                    diagnostics,
                );
            }
        }
    }
    Ok(())
}

/// Scan OMP sessions with index cache and diagnostics.
fn scan_omp_summaries_cached(
    root: &Path,
    cache: &SessionIndexCache,
    delta: &mut CacheDelta,
    tracker: &mut SourceScanTracker,
    summaries: &mut Vec<SessionSummary>,
    diagnostics: &mut ScanDiagnostics,
    project_filter: Option<&str>,
) -> Result<()> {
    use walkdir::WalkDir;

    if !root_is_available(root, "omp", diagnostics) {
        tracker.mark_incomplete("omp");
        return Ok(());
    }
    let filter = FilterConfig::no_size_limit();

    for entry in WalkDir::new(root).follow_links(false).into_iter() {
        if entry.is_err() {
            tracker.mark_incomplete("omp");
        }
        let Some(entry) = handle_walk_entry(entry, "omp", diagnostics) else {
            continue;
        };
        let file_path = entry.path();
        let Some(candidate) =
            inspect_candidate_file(root, file_path, &filter, "omp", tracker, diagnostics)
        else {
            continue;
        };
        let cached_summary = candidate.path_key.as_ref().and_then(|path_key| {
            tracker.seen("omp", path_key.clone());
            let summary = cache.lookup_with_fingerprint(
                path_key,
                file_path,
                candidate.file_size,
                candidate.mtime_secs,
                &candidate.content_fingerprint,
            );
            if summary.is_some() {
                diagnostics.cache_hits += 1;
            } else {
                diagnostics.cache_misses += 1;
            }
            summary
        });

        let summary = if let Some(summary) = cached_summary {
            summary
        } else {
            let parse_started = Instant::now();
            diagnostics.parsed_bytes = diagnostics.parsed_bytes.saturating_add(candidate.file_size);
            run_parser_test_hook(file_path);
            match OmpSession::from_file_with_report(file_path) {
                Ok(outcome) => {
                    diagnostics.parse_ms = diagnostics
                        .parse_ms
                        .saturating_add(elapsed_millis(parse_started));
                    diagnostics.files_parsed += 1;
                    let malformed_lines = outcome.malformed_lines;
                    let session = outcome.value;
                    let project_name = session.project_name().unwrap_or_else(|| {
                        file_path
                            .parent()
                            .and_then(|path| path.file_name())
                            .and_then(|name| name.to_str())
                            .unwrap_or("omp")
                            .to_string()
                    });
                    let mut summary = SessionSummary::from_omp_session(&session, &project_name);
                    summary.file_size = candidate.file_size;
                    if malformed_lines > 0 {
                        if let Some(key) = candidate.path_key.clone() {
                            delta.removals.push(CacheRemoval {
                                key,
                                expected: candidate.file_state(),
                            });
                        }
                        diagnostics.record_warning(
                            Some("omp"),
                            "parse",
                            ScanWarningCategory::Data,
                            Some(file_path),
                            &format!("partial parse malformed_lines={malformed_lines}"),
                        );
                    } else if let Some(key) = candidate.path_key.clone() {
                        delta.upserts.push(CacheUpsert {
                            key,
                            expected: candidate.file_state(),
                            entry: cache_entry_from_candidate(&candidate, &summary),
                        });
                    }
                    summary
                }
                Err(error) => {
                    diagnostics.parse_ms = diagnostics
                        .parse_ms
                        .saturating_add(elapsed_millis(parse_started));
                    handle_parser_error(
                        "omp",
                        file_path,
                        &candidate,
                        &error,
                        tracker,
                        delta,
                        diagnostics,
                    );
                    continue;
                }
            }
        };

        if project_filter.is_some_and(|name| summary.project_name != name) {
            diagnostics.files_skipped += 1;
        } else if is_valid_session_summary(&summary) {
            summaries.push(summary);
        } else {
            diagnostics.files_skipped += 1;
        }
    }
    Ok(())
}

/// Get filtered (invalid) sessions for cleanup
pub fn get_filtered_sessions(project: &ProjectSummary) -> Result<Vec<SessionSummary>> {
    let filter = FilterConfig::no_size_limit();
    let sessions = discover_sessions(&project.dir_path, &filter)?;

    let filtered: Vec<SessionSummary> = sessions
        .iter()
        .map(|s| SessionSummary::from_session(s, &project.name, &project.dir_path))
        .filter(|s| !is_valid_session_summary(s))
        .collect();

    Ok(filtered)
}

/// Build project summaries by grouping sessions by project_name
fn build_projects_from_sessions(sessions: &[SessionSummary]) -> Vec<ProjectSummary> {
    let mut map: std::collections::HashMap<String, (PathBuf, usize, Option<String>)> =
        std::collections::HashMap::new();

    for s in sessions {
        let entry = map
            .entry(s.project_name.clone())
            .or_insert_with(|| (s.project_dir.clone(), 0, None));
        entry.1 += 1;
        if s.last_activity > entry.2 {
            entry.2 = s.last_activity.clone();
        }
    }

    let mut projects: Vec<ProjectSummary> = map
        .into_iter()
        .map(|(name, (dir_path, count, last))| ProjectSummary {
            name,
            dir_path,
            session_count: count,
            last_activity: last,
        })
        .collect();

    projects.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));
    projects
}

/// Detect if current directory corresponds to a Claude project
pub fn detect_current_project() -> Result<Option<ProjectSummary>> {
    let cwd = std::env::current_dir()?;
    let project_name = cwd.file_name().and_then(|n| n.to_str()).unwrap_or_default();

    if project_name.is_empty() {
        return Ok(None);
    }

    let projects = scan_all_projects()?;

    // Find project matching current directory name
    Ok(projects.into_iter().find(|p| p.name == project_name))
}

/// Append a custom-title entry to a session file.
///
/// This is a private low-level file operation. Callers with a session summary
/// must use [`rename_session_with_guard`] so source capabilities are enforced.
fn append_custom_title_entry(file_path: &Path, session_id: &str, new_title: &str) -> Result<()> {
    use std::io::Write;

    let entry = json!({
        "type": "custom-title",
        "customTitle": new_title,
        "sessionId": session_id,
    });

    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(file_path)
        .with_context(|| format!("Failed to open file: {}", file_path.display()))?;

    writeln!(file, "{}", serde_json::to_string(&entry)?)
        .with_context(|| format!("Failed to write to file: {}", file_path.display()))?;

    Ok(())
}

/// Rename a session after checking the source capability.
fn rename_session_with_guard(session: &SessionSummary, new_title: &str) -> Result<()> {
    ensure_can_rename(session)?;
    ensure_path_within_claude_projects(&session.file_path)?;
    append_custom_title_entry(&session.file_path, &session.session_id, new_title)
}

/// Delete a session file after checking the source capability.
fn delete_local_session(session: &SessionSummary) -> Result<()> {
    ensure_can_delete(session)?;
    ensure_path_within_claude_projects(&session.file_path)?;
    delete_session_file(&session.file_path)
}

/// Delete a session file from the local filesystem only.
///
/// This is a private low-level file operation: it removes the `.jsonl` file
/// and does not touch the sync repo, write a tombstone, or commit.
fn delete_session_file(file_path: &Path) -> Result<()> {
    fs::remove_file(file_path)
        .with_context(|| format!("Failed to delete file: {}", file_path.display()))?;
    Ok(())
}

fn ensure_path_within_root(file_path: &Path, root: &Path) -> Result<()> {
    let canonical_root = fs::canonicalize(root)
        .with_context(|| format!("Failed to resolve Claude projects root: {}", root.display()))?;
    let canonical_file = fs::canonicalize(file_path)
        .with_context(|| format!("Failed to resolve session path: {}", file_path.display()))?;

    if !canonical_file.starts_with(&canonical_root) {
        anyhow::bail!(
            "Raw session mutation is only allowed inside Claude projects: {}",
            file_path.display()
        );
    }

    Ok(())
}

fn ensure_path_within_claude_projects(file_path: &Path) -> Result<()> {
    let root = claude_projects_dir()?;
    ensure_path_within_root(file_path, &root)
}

/// Rename a raw Claude session file after validating its project-root containment.
#[allow(dead_code)] // Deprecated public raw-path API retained for downstream compatibility; guarded by tests.
#[deprecated(note = "use source-aware session handlers")]
pub fn rename_session(file_path: &Path, session_id: &str, new_title: &str) -> Result<()> {
    ensure_path_within_claude_projects(file_path)?;
    append_custom_title_entry(file_path, session_id, new_title)
}

/// Delete a raw Claude session file after validating its project-root containment.
#[allow(dead_code)] // Deprecated public raw-path API retained for downstream compatibility; guarded by tests.
#[deprecated(note = "use source-aware session handlers")]
pub fn delete_session(file_path: &Path) -> Result<()> {
    ensure_path_within_claude_projects(file_path)?;
    delete_session_file(file_path)
}

/// Compute the path of a session file relative to the sync repo's `projects/`
/// directory, mirroring [`crate::sync::push`]'s `compute_relative_path` logic
/// but operating on a [`SessionSummary`] instead of a [`ConversationSession`].
///
/// Returns `None` when the session is not under `~/.claude/projects/`
/// (e.g. Codex sessions, which are not synced and have no repo representation).
fn repo_relative_path(session: &SessionSummary, filter: &FilterConfig) -> Result<Option<PathBuf>> {
    let claude_dir = claude_projects_dir()?;
    repo_relative_path_from_root(session, filter, &claude_dir)
}

fn repo_relative_path_from_root(
    session: &SessionSummary,
    filter: &FilterConfig,
    claude_dir: &Path,
) -> Result<Option<PathBuf>> {
    // Codex and OMP sessions live outside ~/.claude/projects/ and are not synced.
    if session.source == "codex" || session.source == "omp" {
        return Ok(None);
    }

    let filename = session
        .file_path
        .file_name()
        .context("session path has no filename")?;
    let relative = if filter.use_project_name_only {
        safe_project_relative_path(&session.project_name, filename)?
    } else {
        safe_relative_path_within_root(claude_dir, &session.file_path)?
    };
    Ok(Some(relative))
}

/// Build a [`DeletionRecord`] for a session, without persisting it.
fn build_deletion_record(
    session: &SessionSummary,
    repo_rel: &Path,
    reason: DeleteReason,
) -> DeletionRecord {
    DeletionRecord {
        session_id: session.session_id.clone(),
        repo_relative_path: repo_rel.to_string_lossy().to_string(),
        project_name: session.project_name.clone(),
        source: session.source.clone(),
        deleted_at: chrono::Utc::now().to_rfc3339(),
        device: ConfigSyncSettings::default().get_device_name(),
        reason,
    }
}

fn sync_repo_file_exists_safely(
    sync_repo_path: &Path,
    projects_dir: &Path,
    relative: &Path,
) -> Result<bool> {
    match fs::symlink_metadata(projects_dir) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to inspect sync projects root: {}",
                    projects_dir.display()
                )
            })
        }
    }

    let canonical_root = validate_sync_projects_root(sync_repo_path, projects_dir)?;
    let candidate = safe_join_within_sync_projects_root(sync_repo_path, projects_dir, relative)?;
    if !candidate.exists() {
        return Ok(false);
    }
    validate_regular_candidate(&canonical_root, &candidate)?;
    Ok(true)
}

fn remove_sync_repo_file(
    sync_repo_path: &Path,
    projects_dir: &Path,
    relative: &Path,
) -> Result<bool> {
    if !sync_repo_file_exists_safely(sync_repo_path, projects_dir, relative)? {
        return Ok(false);
    }

    // Rebuild and revalidate the root-relative candidate immediately before
    // unlinking it. Same-UID replacement between checks and unlink remains the
    // documented TOCTOU residual.
    let canonical_root = validate_sync_projects_root(sync_repo_path, projects_dir)?;
    let candidate = safe_join_within_sync_projects_root(sync_repo_path, projects_dir, relative)?;
    validate_regular_candidate(&canonical_root, &candidate)?;
    fs::remove_file(&candidate)
        .with_context(|| format!("failed to remove sync-repo file: {}", relative.display()))?;
    Ok(true)
}

/// Delete a single session: remove the local file, and for synced sessions
/// also remove the sync-repo copy, register a tombstone, and commit.
///
/// This is the unified entry point for intentional deletions. It makes the
/// deletion atomic across the local tree and the sync repo, and records the
/// intent so other devices can distinguish it from accidental loss.
///
/// Only sources with delete capability may use this entry point.
///
/// `reason` drives both the tombstone entry and the commit message prefix.
pub fn delete_session_with_commit(session: &SessionSummary, reason: DeleteReason) -> Result<()> {
    ensure_can_delete(session)?;
    ensure_path_within_claude_projects(&session.file_path)?;

    let filter = FilterConfig::load()?;
    // Compute the repository-relative path before deleting the local file;
    // full-layout validation needs the source file to still exist.
    let repo_rel = repo_relative_path(session, &filter)?;

    // 1. Sources without a sync-repo representation need no further work.
    let Some(repo_rel) = repo_rel else {
        delete_local_session(session)?;
        log::debug!(
            "Session {} is not synced (source={}); local file only removed",
            session.session_id,
            session.source
        );
        return Ok(());
    };

    let state = SyncState::load()?;
    let projects_dir = state.sync_repo_path.join(&filter.sync_subdirectory);
    // Validate the untrusted sync root before changing the local file. This
    // prevents a malformed checkout from causing a partial delete.
    let repo_file_present =
        sync_repo_file_exists_safely(&state.sync_repo_path, &projects_dir, &repo_rel)?;

    // 2. Always remove the local file after the sync-repo boundary preflight.
    delete_local_session(session)?;

    // 3. Remove the sync-repo copy if present. Missing is fine (e.g. never
    //    pushed yet) — the tombstone still records the intent.
    if repo_file_present {
        if let Err(e) = remove_sync_repo_file(&state.sync_repo_path, &projects_dir, &repo_rel) {
            log::warn!(
                "Failed to remove sync-repo copy {}: {}",
                repo_rel.display(),
                e
            );
        }
    }

    // 4. Register the tombstone.
    let record = build_deletion_record(session, &repo_rel, reason.clone());
    let mut registry = TombstoneRegistry::load(&state.sync_repo_path)?;
    registry.add(record);
    registry.save(&state.sync_repo_path)?;

    // 5. Commit the deletion + tombstone together.
    let repo = scm::open(&state.sync_repo_path)?;
    repo.stage_all()?;
    if repo.has_changes()? {
        let message = format!(
            "delete(session): {} {}",
            reason.as_str(),
            session.session_id
        );
        repo.commit(&message)?;
        log::info!("Committed session deletion: {}", message);
    }

    Ok(())
}

/// Remove a session's local file and sync-repo copy, returning a tombstone
/// record for the caller to batch-persist.
///
/// Unlike [`delete_session_with_commit`], this does NOT save the tombstone
/// registry or commit — the caller is expected to accumulate records and
/// perform a single save + commit at the end (used by batch cleanup so the
/// history gets one commit instead of N).
///
/// Returns `None` when the local file could not be removed (the session is
/// left untouched and the caller can continue with the rest).
fn remove_session_for_batch(
    session: &SessionSummary,
    reason: DeleteReason,
    filter: &FilterConfig,
    state: &SyncState,
) -> Result<Option<DeletionRecord>> {
    ensure_can_delete(session)?;

    // Compute this before deleting the local file because full-layout
    // validation checks the existing source path.
    let repo_rel = repo_relative_path(session, filter)?;

    // 1. Codex sessions have no repo representation.
    let Some(repo_rel) = repo_rel else {
        delete_local_session(session)?;
        return Ok(None);
    };

    let projects_dir = state.sync_repo_path.join(&filter.sync_subdirectory);
    // Preflight the untrusted sync root before deleting the local source.
    let repo_file_present =
        sync_repo_file_exists_safely(&state.sync_repo_path, &projects_dir, &repo_rel)?;

    // 2. Remove the local file after the sync-repo boundary preflight.
    delete_local_session(session)?;

    // 3. Rebuild and revalidate the candidate before unlinking it.
    if repo_file_present {
        if let Err(e) = remove_sync_repo_file(&state.sync_repo_path, &projects_dir, &repo_rel) {
            log::warn!(
                "Failed to remove sync-repo copy {}: {}",
                repo_rel.display(),
                e
            );
        }
    }

    Ok(Some(build_deletion_record(session, &repo_rel, reason)))
}

/// Persist accumulated tombstone records and commit the batch deletion in a
/// single commit. Shared by batch cleanup.
fn commit_batch_deletion(
    state: &SyncState,
    records: Vec<DeletionRecord>,
    commit_message: &str,
) -> Result<()> {
    if records.is_empty() {
        return Ok(());
    }

    let mut registry = TombstoneRegistry::load(&state.sync_repo_path)?;
    registry.add_many(records);
    registry.save(&state.sync_repo_path)?;

    let repo = scm::open(&state.sync_repo_path)?;
    repo.stage_all()?;
    if repo.has_changes()? {
        repo.commit(commit_message)?;
        log::info!("Committed batch deletion: {}", commit_message);
    }
    Ok(())
}

// ============================================================================
// Interactive Menu Functions
// ============================================================================

/// Show project selection menu
fn show_project_menu(projects: &[ProjectSummary]) -> Result<ProjectMenuChoice> {
    if projects.is_empty() {
        println!("{}", "No projects found.".yellow());
        return Ok(ProjectMenuChoice::Exit);
    }

    let mut options: Vec<String> = projects
        .iter()
        .map(|p| {
            let time = p
                .last_activity
                .as_ref()
                .map(|t| format_relative_time(t))
                .unwrap_or_else(|| "Unknown".to_string());
            format!("{:<30} {:>3} sessions  {}", p.name, p.session_count, time)
        })
        .collect();

    options.push("Exit".to_string());

    let selection = Select::new("Select a project:", options.clone())
        .with_help_message("Use arrow keys to navigate, Enter to select")
        .prompt();

    match selection {
        Ok(selected) => {
            if selected == "Exit" {
                Ok(ProjectMenuChoice::Exit)
            } else if let Some(idx) = options.iter().position(|o| o == &selected) {
                if idx < projects.len() {
                    Ok(ProjectMenuChoice::Select(projects[idx].clone()))
                } else {
                    Ok(ProjectMenuChoice::Exit)
                }
            } else {
                Ok(ProjectMenuChoice::Exit)
            }
        }
        Err(_) => Ok(ProjectMenuChoice::Exit),
    }
}

fn build_session_menu_options(
    sessions: &[SessionSummary],
    filtered_count: usize,
    cleanup_enabled: bool,
) -> Vec<String> {
    let search_option = "Search sessions...".to_string();
    let switch_option = "Switch project".to_string();
    let exit_option = "Exit".to_string();

    let mut options: Vec<String> = Vec::with_capacity(sessions.len() + 4);
    options.push(search_option);

    let has_mixed_sources = sessions
        .first()
        .map(|first| sessions.iter().any(|s| s.source != first.source))
        .unwrap_or(false);
    for (i, s) in sessions.iter().enumerate() {
        if has_mixed_sources {
            options.push(format!(
                "[{:>2}] {} {:<37} {:>3} msgs  {}",
                i + 1,
                source_label(&s.source),
                s.display_title(37),
                s.message_count,
                s.relative_time()
            ));
        } else {
            options.push(format!(
                "[{:>2}] {:<40} {:>3} msgs  {}",
                i + 1,
                s.display_title(40),
                s.message_count,
                s.relative_time()
            ));
        }
    }

    if cleanup_enabled {
        let cleanup_option = if filtered_count > 0 {
            format!("Cleanup [{}]", filtered_count)
        } else {
            "Cleanup [0]".to_string()
        };
        options.push(cleanup_option);
    }
    options.push(switch_option);
    options.push(exit_option);
    options
}

/// Show session selection menu for a project
fn show_session_menu(
    project: &ProjectSummary,
    sessions: &[SessionSummary],
    filtered_count: usize,
    cleanup_enabled: bool,
) -> Result<SessionMenuChoice> {
    println!();
    println!(
        "{} {} - {} sessions",
        "Project:".cyan().bold(),
        project.name.bold(),
        sessions.len()
    );
    println!();

    if sessions.is_empty() {
        println!("{}", "No sessions found in this project.".yellow());
        return Ok(SessionMenuChoice::SwitchProject);
    }

    let search_option = "Search sessions...".to_string();
    let cleanup_option = cleanup_enabled.then(|| {
        if filtered_count > 0 {
            format!("Cleanup [{}]", filtered_count)
        } else {
            "Cleanup [0]".to_string()
        }
    });
    let switch_option = "Switch project".to_string();
    let exit_option = "Exit".to_string();
    let options = build_session_menu_options(sessions, filtered_count, cleanup_enabled);

    let selection = Select::new("Select a session:", options.clone())
        .with_help_message("Use arrow keys to navigate, Enter to select")
        .prompt();

    match selection {
        Ok(selected) => {
            if selected == exit_option {
                Ok(SessionMenuChoice::Exit)
            } else if selected == switch_option {
                Ok(SessionMenuChoice::SwitchProject)
            } else if selected == search_option {
                Ok(SessionMenuChoice::Search)
            } else if cleanup_option.as_deref() == Some(selected.as_str()) {
                Ok(SessionMenuChoice::Cleanup)
            } else if let Some(idx) = options.iter().position(|o| o == &selected) {
                // offset by 1 for the search option
                let session_idx = idx - 1;
                if session_idx < sessions.len() {
                    Ok(SessionMenuChoice::Select(sessions[session_idx].clone()))
                } else {
                    Ok(SessionMenuChoice::SwitchProject)
                }
            } else {
                Ok(SessionMenuChoice::Exit)
            }
        }
        Err(_) => Ok(SessionMenuChoice::Exit),
    }
}

/// Search sessions by keyword in user messages (delegates to search_sessions_full)
fn search_sessions(
    sessions: &[SessionSummary],
    keyword: &str,
) -> Vec<(SessionSummary, Vec<String>)> {
    // Split input into multiple keywords for AND matching
    let keywords: Vec<&str> = keyword.split_whitespace().collect();
    search_sessions_full(sessions, &keywords, 60, true)
        .into_iter()
        .map(|r| {
            let snippets = r.matches.into_iter().map(|m| m.snippet).collect();
            (r.summary, snippets)
        })
        .collect()
}

/// Extract a snippet around the first keyword match
fn extract_match_snippet(text: &str, keyword_lower: &str, max_len: usize) -> String {
    let text_lower = text.to_lowercase();
    let text_chars: Vec<char> = text.chars().collect();
    let lower_chars: Vec<char> = text_lower.chars().collect();

    // Find match position in char indices
    let keyword_chars: Vec<char> = keyword_lower.chars().collect();
    let match_pos = lower_chars
        .windows(keyword_chars.len())
        .position(|w| w == keyword_chars.as_slice())
        .unwrap_or(0);

    let total = text_chars.len();
    if total <= max_len {
        return text.replace('\n', " ");
    }

    // Center the snippet around the match
    let half = max_len / 2;
    let start = match_pos.saturating_sub(half);
    let end = (start + max_len).min(total);
    let start = if end == total {
        end.saturating_sub(max_len)
    } else {
        start
    };

    let snippet: String = text_chars[start..end].iter().collect();
    let snippet = snippet.replace('\n', " ");

    let prefix = if start > 0 { "..." } else { "" };
    let suffix = if end < total { "..." } else { "" };
    format!("{}{}{}", prefix, snippet, suffix)
}

/// Show search results and let user select
fn show_search_results(
    results: &[(SessionSummary, Vec<String>)],
    keyword: &str,
) -> Result<SessionMenuChoice> {
    println!();
    println!(
        "{} Found {} sessions matching \"{}\"",
        "Search:".cyan().bold(),
        results.len(),
        keyword
    );
    println!();

    if results.is_empty() {
        println!("{}", "No matching sessions found.".yellow());
        // Wait for user input
        let _ = Text::new("Press Enter to continue...")
            .with_help_message("")
            .prompt();
        return Ok(SessionMenuChoice::SwitchProject);
    }

    // Display results with snippets
    for (i, (session, snippets)) in results.iter().enumerate() {
        println!(
            "{} {} ({} msgs, {})",
            format!("[{:>2}]", i + 1).cyan(),
            session.display_title(50).bold(),
            session.message_count,
            session.relative_time()
        );
        // Show first 2 matched snippets
        for snippet in snippets.iter().take(2) {
            println!("     {}", snippet.dimmed());
        }
        if snippets.len() > 2 {
            println!(
                "     {}",
                format!("... and {} more matches", snippets.len() - 2).dimmed()
            );
        }
    }
    println!();

    let back_option = "Back to session list".to_string();
    let mut options: Vec<String> = results
        .iter()
        .enumerate()
        .map(|(i, (s, _))| format!("[{:>2}] {}", i + 1, s.display_title(50),))
        .collect();
    options.push(back_option.clone());

    let selection = Select::new("Select a session:", options.clone())
        .with_help_message("Use arrow keys to navigate, Enter to select")
        .prompt();

    match selection {
        Ok(selected) => {
            if selected == back_option {
                Ok(SessionMenuChoice::SwitchProject) // reuse to go back
            } else if let Some(idx) = options.iter().position(|o| o == &selected) {
                if idx < results.len() {
                    Ok(SessionMenuChoice::Select(results[idx].0.clone()))
                } else {
                    Ok(SessionMenuChoice::SwitchProject)
                }
            } else {
                Ok(SessionMenuChoice::SwitchProject)
            }
        }
        Err(_) => Ok(SessionMenuChoice::SwitchProject),
    }
}

/// Show action menu for a selected session
fn show_action_menu(session: &SessionSummary) -> Result<ActionChoice> {
    println!();
    println!(
        "{} {}",
        "Selected:".cyan().bold(),
        session.display_title(60).bold()
    );
    println!();

    let source = session.source_kind()?;
    let actions = action_choices_for_source(source);
    let open_label = if source == SessionSource::Omp {
        "Open in OMP"
    } else {
        "Open in Claude"
    };
    let labels: Vec<&str> = actions
        .iter()
        .map(|action| match action {
            ActionChoice::OpenInEditor => open_label,
            ActionChoice::ViewDetails => "View details",
            ActionChoice::Rename => "Rename session",
            ActionChoice::Delete => "Delete session",
            ActionChoice::Back => "Back to session list",
        })
        .collect();

    let selection = Select::new("Choose an action:", labels.clone())
        .with_help_message("Use arrow keys to navigate, Enter to select")
        .prompt();

    match selection {
        Ok(selected) => labels
            .iter()
            .position(|label| *label == selected)
            .map(|index| actions[index])
            .ok_or_else(|| anyhow::anyhow!("Unknown session action selected")),
        Err(_) => Ok(ActionChoice::Back),
    }
}

/// Show session details with all user messages
fn show_session_details(session: &SessionSummary) -> Result<()> {
    println!();
    println!("{}", "=".repeat(60).cyan());
    println!("{}", "Session Details".cyan().bold());
    println!("{}", "=".repeat(60).cyan());
    println!();

    println!("{:<15} {}", "Title:".bold(), session.title);
    println!("{:<15} {}", "Project:".bold(), session.project_name);
    println!("{:<15} {}", "Session ID:".bold(), session.session_id);
    println!(
        "{:<15} {} (User: {}, Assistant: {})",
        "Messages:".bold(),
        session.message_count,
        session.user_message_count,
        session.assistant_message_count
    );
    println!(
        "{:<15} {}",
        "Created:".bold(),
        session
            .first_timestamp
            .as_ref()
            .map(|t| format_relative_time(t))
            .unwrap_or_else(|| "Unknown".to_string())
    );
    println!(
        "{:<15} {}",
        "Last Activity:".bold(),
        session.relative_time()
    );
    println!(
        "{:<15} {:.2} KB",
        "File Size:".bold(),
        session.file_size as f64 / 1024.0
    );
    println!(
        "{:<15} {}",
        "File Path:".bold(),
        session.file_path.display()
    );

    // Show conversation (both user and assistant messages)
    println!();
    println!("{}", "-".repeat(60).cyan());
    println!("{}", "Conversation".cyan().bold());
    println!("{}", "-".repeat(60).cyan());

    let messages = collect_display_messages_for_summary(session, true);

    if messages.is_empty() {
        println!();
        println!("{}", "(No messages found)".dimmed());
    } else {
        for m in &messages {
            println!();

            let time_str = m
                .timestamp
                .as_ref()
                .map(|t| format_relative_time(t))
                .unwrap_or_default();

            let role_label = match m.role.as_str() {
                "user" => "[User]".green().bold(),
                "assistant" => "[Assistant]".blue().bold(),
                _ => format!("[{}]", m.role).normal(),
            };

            println!(
                "{} {} {}",
                format!("[{}]", m.index).cyan(),
                role_label,
                time_str.dimmed()
            );

            for line in m.content.lines() {
                println!("  {}", line);
            }
        }
    }

    println!();
    println!("{}", "=".repeat(60).cyan());
    println!();

    // Wait for user input
    let _ = Text::new("Press Enter to continue...")
        .with_help_message("")
        .prompt();

    Ok(())
}

/// Load session commands configuration from file
fn load_user_data() -> Result<UserData> {
    let path = ConfigManager::user_data_path()?;
    if !path.exists() {
        return Ok(UserData::default());
    }
    let content = fs::read_to_string(&path)?;
    let data: UserData = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse user data: {}", path.display()))?;
    Ok(data)
}

/// Save user data configuration to file
fn save_user_data(data: &UserData) -> Result<()> {
    let path = ConfigManager::user_data_path()?;
    let content =
        serde_json::to_string_pretty(data).with_context(|| "Failed to serialize user data")?;
    fs::write(&path, content)
        .with_context(|| format!("Failed to write user data: {}", path.display()))?;
    Ok(())
}

/// Open session in editor by executing `claude --resume {session_id}` or `omp --resume {session_id}`
/// based on the session source. Returns: Ok(true) = executed command, Ok(false) = cancelled
fn open_in_editor(session: &SessionSummary) -> Result<bool> {
    // Get project path from session's cwd field
    let project_path = if let Ok(conv) = ConversationSession::from_file(&session.file_path) {
        conv.cwd().map(|s| s.to_string())
    } else {
        None
    };

    // Build default command based on session source
    let default_cmd = match session.source.as_str() {
        "omp" => format!("omp --resume {}", session.session_id),
        _ => format!("claude --resume {}", session.session_id),
    };

    // Try to load saved command template
    let mut initial_cmd = default_cmd.clone();
    if let Ok(data) = load_user_data() {
        if let Some(template) = &data.command_template {
            // Replace placeholders with actual values
            let mut saved_cmd = template.replace("{session_id}", &session.session_id);
            if let Some(ref path) = project_path {
                saved_cmd = saved_cmd.replace("{path}", path);
            }
            initial_cmd = saved_cmd;
        }
    }

    println!();
    let cmd = Text::new("Command to execute:")
        .with_initial_value(&initial_cmd)
        .with_help_message("Edit the command if needed. Use {session_id} and {path} as placeholders. Press Enter to execute")
        .prompt();

    match cmd {
        Ok(cmd) => {
            let cmd = cmd.trim().to_string();
            if cmd.is_empty() {
                // Clear saved custom command to restore default
                if let Ok(mut data) = load_user_data() {
                    if data.command_template.is_some() {
                        data.command_template = None;
                        if let Err(e) = save_user_data(&data) {
                            println!(
                                "{} Failed to clear saved command: {}",
                                "WARNING:".yellow(),
                                e
                            );
                        } else {
                            println!(
                                "{} Saved command cleared, using default next time",
                                "INFO:".cyan()
                            );
                        }
                    }
                }
                println!("{}", "Command is empty, cancelled.".yellow());
                return Ok(false);
            }

            // Save custom command if it's different from default
            // Convert actual values back to placeholders
            let mut template = cmd.clone();
            template = template.replace(&session.session_id, "{session_id}");
            if let Some(ref path) = project_path {
                template = template.replace(path, "{path}");
            }

            if template != default_cmd {
                // User modified the command, save it
                let mut data = match load_user_data() {
                    Ok(d) => d,
                    Err(e) => {
                        log::warn!("Failed to load user data: {}, using default", e);
                        UserData::default()
                    }
                };
                data.command_template = Some(template);
                if let Err(e) = save_user_data(&data) {
                    println!("{} Failed to save command: {}", "WARNING:".yellow(), e);
                } else {
                    println!("{} Command saved for future use", "INFO:".cyan());
                }
            }

            println!();
            println!("{} {}", "Executing:".cyan().bold(), cmd);
            println!();

            // Execute the command using the user's preferred shell in interactive mode
            // This ensures that aliases, functions (like claude-auto), and customized PATH
            // environments are properly loaded before execution.
            let status = if cfg!(target_os = "windows") {
                // PowerShell profile scripts define user aliases/functions (e.g. a custom
                // `cc-auto` wrapper), so we invoke `powershell -Command` instead of `cmd /C` —
                // cmd.exe has no knowledge of the user's PowerShell profile and fails with
                // "not recognized" for anything defined only as a PowerShell alias/function.
                // We use raw_arg() so std::process::Command doesn't add its own quotes around
                // the command string, which would otherwise break paths/`&&` chains.
                #[cfg(target_os = "windows")]
                use std::os::windows::process::CommandExt;

                #[cfg(target_os = "windows")]
                let mut command = std::process::Command::new("powershell");

                #[cfg(target_os = "windows")]
                {
                    command
                        .arg("-NoLogo")
                        .arg("-NonInteractive")
                        .arg("-Command")
                        .raw_arg(&cmd);
                    if let Some(path) = &project_path {
                        command.current_dir(path);
                    }
                    command
                        .status()
                        .with_context(|| format!("Failed to execute command: {}", cmd))?
                }

                #[cfg(not(target_os = "windows"))]
                {
                    // This branch should be unreachable when cfg!(target_os = "windows") is true,
                    // but we need it to compile on non-Windows platforms.
                    let mut command = std::process::Command::new("powershell");
                    command
                        .arg("-NoLogo")
                        .arg("-NonInteractive")
                        .arg("-Command")
                        .arg(&cmd);
                    if let Some(path) = &project_path {
                        command.current_dir(path);
                    }
                    command
                        .status()
                        .with_context(|| format!("Failed to execute command: {}", cmd))?
                }
            } else {
                let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".to_string());
                let mut command = std::process::Command::new(shell);
                command.arg("-ic").arg(&cmd);
                if let Some(ref path) = project_path {
                    command.current_dir(path);
                }
                command
                    .status()
                    .with_context(|| format!("Failed to execute command: {}", cmd))?
            };

            if !status.success() {
                println!(
                    "{} Command exited with code: {}",
                    "WARNING:".yellow().bold(),
                    status.code().unwrap_or(-1)
                );
            }

            Ok(true)
        }
        Err(_) => {
            println!("{}", "Cancelled.".yellow());
            Ok(false)
        }
    }
}

/// Interactive rename session
fn rename_session_interactive(session: &mut SessionSummary) -> Result<bool> {
    ensure_can_rename(session)?;

    println!();
    println!("{} {}", "Current title:".dimmed(), session.title);
    println!();

    // Use first 20 chars of current title as default value
    let default_title: String = session.title.chars().take(20).collect();
    let new_title = Text::new("Enter new title:")
        .with_initial_value(&default_title)
        .prompt();

    match new_title {
        Ok(title) => {
            if title.trim().is_empty() {
                println!("{}", "Title cannot be empty.".red());
                return Ok(false);
            }

            if title == session.title {
                println!("{}", "Title unchanged.".yellow());
                return Ok(false);
            }

            rename_session_with_guard(session, &title)?;
            session.title = title.clone();

            println!();
            println!("{} Title updated successfully!", "SUCCESS:".green().bold());
            println!();
            Ok(true)
        }
        Err(_) => {
            println!("{}", "Rename cancelled.".yellow());
            Ok(false)
        }
    }
}

/// Interactive delete session
fn delete_session_interactive(session: &SessionSummary) -> Result<bool> {
    ensure_can_delete(session)?;

    println!();
    println!(
        "{} {}",
        "WARNING:".red().bold(),
        "You are about to delete this session:".red()
    );
    println!();
    println!("  Title: {}", session.display_title(50));
    println!("  Messages: {}", session.message_count);
    println!("  File: {}", session.file_path.display());
    println!();
    println!("{}", "This action cannot be undone!".red().bold());
    println!();

    let confirm = Confirm::new("Are you sure you want to delete this session?")
        .with_default(false)
        .prompt();

    match confirm {
        Ok(true) => {
            delete_session_with_commit(session, DeleteReason::Explicit)?;
            println!();
            println!(
                "{} Session deleted successfully!",
                "SUCCESS:".green().bold()
            );
            println!();
            Ok(true)
        }
        Ok(false) => {
            println!("{}", "Delete cancelled.".yellow());
            Ok(false)
        }
        Err(_) => {
            println!("{}", "Delete cancelled.".yellow());
            Ok(false)
        }
    }
}

/// Interactive cleanup filtered sessions
fn cleanup_sessions_interactive(project: &ProjectSummary) -> Result<usize> {
    let filtered_sessions = get_filtered_sessions(project)?;

    if filtered_sessions.is_empty() {
        println!();
        println!("{}", "No filtered sessions to clean up.".yellow());
        println!();
        return Ok(0);
    }

    for session in &filtered_sessions {
        ensure_can_delete(session)?;
    }

    println!();
    println!(
        "{} Found {} filtered sessions (empty or no title):",
        "Cleanup:".cyan().bold(),
        filtered_sessions.len()
    );
    println!();

    for (i, session) in filtered_sessions.iter().enumerate() {
        let size_kb = session.file_size as f64 / 1024.0;
        println!(
            "  [{:>2}] {} | {} msgs | {:.1} KB",
            i + 1,
            session.display_title(40).dimmed(),
            session.message_count,
            size_kb
        );
    }

    let total_size: u64 = filtered_sessions.iter().map(|s| s.file_size).sum();
    println!();
    println!(
        "  Total: {} files, {:.2} KB",
        filtered_sessions.len(),
        total_size as f64 / 1024.0
    );
    println!();
    println!("{}", "This action cannot be undone!".red().bold());
    println!();

    let confirm = Confirm::new(&format!(
        "Delete all {} filtered sessions?",
        filtered_sessions.len()
    ))
    .with_default(false)
    .prompt();

    match confirm {
        Ok(true) => {
            let filter = FilterConfig::load()?;
            let state = SyncState::load().ok();
            let mut deleted_count = 0;
            let mut records: Vec<DeletionRecord> = Vec::new();

            for session in &filtered_sessions {
                match state {
                    Some(ref st) => {
                        match remove_session_for_batch(session, DeleteReason::Cleanup, &filter, st)
                        {
                            Ok(Some(record)) => {
                                records.push(record);
                                deleted_count += 1;
                            }
                            Ok(None) => {
                                // Local file removed but no repo representation
                                // (e.g. Codex). Still counts as deleted.
                                deleted_count += 1;
                            }
                            Err(e) => {
                                println!(
                                    "{} Failed to delete {}: {}",
                                    "ERROR:".red().bold(),
                                    session.file_path.display(),
                                    e
                                );
                            }
                        }
                    }
                    None => {
                        // No sync repo configured: fall back to guarded local-only delete.
                        if let Err(e) = delete_local_session(session) {
                            println!(
                                "{} Failed to delete {}: {}",
                                "ERROR:".red().bold(),
                                session.file_path.display(),
                                e
                            );
                        } else {
                            deleted_count += 1;
                        }
                    }
                }
            }

            // Persist tombstones and commit once for the whole batch.
            if let Some(ref st) = state {
                if !records.is_empty() {
                    let message = format!("cleanup(session): {} garbage sessions", records.len());
                    if let Err(e) = commit_batch_deletion(st, records, &message) {
                        println!("{} Failed to commit cleanup: {}", "ERROR:".red().bold(), e);
                    }
                }
            }

            println!();
            println!(
                "{} Deleted {} sessions!",
                "SUCCESS:".green().bold(),
                deleted_count
            );
            println!();
            Ok(deleted_count)
        }
        Ok(false) => {
            println!("{}", "Cleanup cancelled.".yellow());
            Ok(0)
        }
        Err(_) => {
            println!("{}", "Cleanup cancelled.".yellow());
            Ok(0)
        }
    }
}

// ============================================================================
// Main Entry Point
// ============================================================================

/// Main interactive session management handler
pub fn handle_session_interactive(
    project_filter: Option<&str>,
    source: SessionSourceFilter,
) -> Result<()> {
    // Check if running in interactive terminal
    if !atty::is(atty::Stream::Stdout) {
        anyhow::bail!(
            "Interactive mode requires a terminal. Use subcommands for non-interactive use."
        );
    }

    println!();
    println!("{}", "Session Manager".cyan().bold());
    println!("{}", "=".repeat(40).cyan());

    // Load all sessions (Claude + Codex) and group into projects
    let mut all_sessions =
        scan_summaries_for_interactive(scan_all_session_summaries_with_report(None, source)?);
    let mut projects = build_projects_from_sessions(&all_sessions);

    if projects.is_empty() {
        println!("{}", "No sessions found.".yellow());
        println!(
            "{}",
            "Run Claude Code or Codex in a project directory first.".dimmed()
        );
        return Ok(());
    }

    // Try to detect current project or use filter
    let initial_project = if let Some(name) = project_filter {
        projects.iter().find(|p| p.name == name).cloned()
    } else {
        match detect_current_project() {
            Ok(project) => project,
            Err(error) => {
                legacy_io_warning_from_error("claude", "detect_current_project", &error);
                None
            }
        }
    };

    // Start with detected project or project list
    let mut current_project = initial_project.clone();

    if let Some(ref proj) = current_project {
        println!();
        println!(
            "{} Detected current project: {}",
            "INFO:".cyan(),
            proj.name.bold()
        );
    }

    loop {
        if let Some(ref project) = current_project {
            // Filter sessions for this project from the pre-loaded list
            let sessions: Vec<SessionSummary> = all_sessions
                .iter()
                .filter(|s| s.project_name == project.name)
                .cloned()
                .collect();

            // Cleanup count is only meaningful for Claude projects
            let filtered_count = if source.includes_claude() {
                scan_all_projects()?
                    .iter()
                    .find(|p| p.name == project.name)
                    .map(|p| get_filtered_sessions(p).map(|f| f.len()).unwrap_or(0))
                    .unwrap_or(0)
            } else {
                0
            };

            match show_session_menu(
                project,
                &sessions,
                filtered_count,
                cleanup_available(source),
            )? {
                SessionMenuChoice::Select(session) => {
                    let mut session = session;
                    let mut list_needs_refresh = false;
                    loop {
                        match show_action_menu(&session)? {
                            ActionChoice::OpenInEditor => {
                                if open_in_editor(&session)? {
                                    return Ok(());
                                }
                            }
                            ActionChoice::ViewDetails => {
                                show_session_details(&session)?;
                            }
                            ActionChoice::Rename => {
                                if rename_session_interactive(&mut session)? {
                                    list_needs_refresh = true;
                                }
                            }
                            ActionChoice::Delete => {
                                if delete_session_interactive(&session)? {
                                    list_needs_refresh = true;
                                    break;
                                }
                            }
                            ActionChoice::Back => {
                                break;
                            }
                        }
                    }
                    if list_needs_refresh {
                        all_sessions = scan_summaries_for_interactive(
                            scan_all_session_summaries_with_report(None, source)?,
                        );
                    }
                }
                SessionMenuChoice::Search => {
                    let keyword = Text::new("Search keyword:")
                        .with_help_message("Search in user messages across all sessions")
                        .prompt();

                    if let Ok(keyword) = keyword {
                        let keyword = keyword.trim().to_string();
                        if !keyword.is_empty() {
                            let results = search_sessions(&sessions, &keyword);
                            if let SessionMenuChoice::Select(session) =
                                show_search_results(&results, &keyword)?
                            {
                                let mut session = session;
                                let mut list_needs_refresh = false;
                                loop {
                                    match show_action_menu(&session)? {
                                        ActionChoice::OpenInEditor => {
                                            open_in_editor(&session)?;
                                            return Ok(());
                                        }
                                        ActionChoice::ViewDetails => {
                                            show_session_details(&session)?;
                                        }
                                        ActionChoice::Rename => {
                                            if rename_session_interactive(&mut session)? {
                                                list_needs_refresh = true;
                                            }
                                        }
                                        ActionChoice::Delete => {
                                            if delete_session_interactive(&session)? {
                                                list_needs_refresh = true;
                                                break;
                                            }
                                        }
                                        ActionChoice::Back => {
                                            break;
                                        }
                                    }
                                }
                                if list_needs_refresh {
                                    all_sessions = scan_summaries_for_interactive(
                                        scan_all_session_summaries_with_report(None, source)?,
                                    );
                                }
                            }
                        }
                    }
                }
                SessionMenuChoice::Cleanup => {
                    if !cleanup_available(source) {
                        anyhow::bail!("Cleanup is only available for Claude sessions.");
                    }
                    if let Some(claude_project) =
                        scan_all_projects()?.iter().find(|p| p.name == project.name)
                    {
                        cleanup_sessions_interactive(claude_project)?;
                    } else {
                        println!(
                            "{}",
                            "Cleanup is only available for Claude sessions.".yellow()
                        );
                    }
                    all_sessions = scan_summaries_for_interactive(
                        scan_all_session_summaries_with_report(None, source)?,
                    );
                }
                SessionMenuChoice::SwitchProject => {
                    current_project = None;
                }
                SessionMenuChoice::Exit => {
                    break;
                }
            }
        } else {
            // Refresh sessions and projects
            all_sessions = scan_summaries_for_interactive(scan_all_session_summaries_with_report(
                None, source,
            )?);
            projects = build_projects_from_sessions(&all_sessions);

            match show_project_menu(&projects)? {
                ProjectMenuChoice::Select(project) => {
                    current_project = Some(project);
                }
                ProjectMenuChoice::Exit => {
                    break;
                }
            }
        }
    }

    println!();
    println!("{}", "Goodbye!".dimmed());
    Ok(())
}

// ============================================================================
// Non-Interactive Handlers
// ============================================================================

/// List sessions (non-interactive)
pub fn handle_session_list(
    project_filter: Option<&str>,
    show_ids: bool,
    source: SessionSourceFilter,
) -> Result<()> {
    let SessionScanResult {
        summaries: sessions,
        diagnostics,
    } = scan_all_session_summaries_with_report(project_filter, source)?;
    emit_scan_warning(&diagnostics);

    if sessions.is_empty() {
        if project_filter.is_some() {
            println!("{}", "No matching project found.".yellow());
        } else {
            println!("{}", "No sessions found.".yellow());
        }
        return Ok(());
    }

    let mut groups: Vec<(String, Vec<SessionSummary>)> = Vec::new();
    for session in sessions {
        if let Some((_, existing)) = groups
            .iter_mut()
            .find(|(name, _)| name == &session.project_name)
        {
            existing.push(session);
        } else {
            groups.push((session.project_name.clone(), vec![session]));
        }
    }

    for (project_name, sessions) in &groups {
        println!();
        println!(
            "{} {} ({} sessions)",
            "Project:".cyan().bold(),
            project_name.bold(),
            sessions.len()
        );
        println!("{}", "-".repeat(60));

        for (i, session) in sessions.iter().enumerate() {
            if show_ids {
                println!(
                    "[{:>2}] [{}] {} | {} | {} msgs | {}",
                    i + 1,
                    source_label(&session.source),
                    session.session_id.dimmed(),
                    session.display_title(40),
                    session.message_count,
                    session.relative_time()
                );
            } else {
                println!(
                    "[{:>2}] [{}] {} | {} msgs | {}",
                    i + 1,
                    source_label(&session.source),
                    session.display_title(50),
                    session.message_count,
                    session.relative_time()
                );
            }
        }
    }

    Ok(())
}

/// List all projects (non-interactive)
pub fn handle_session_projects(source: SessionSourceFilter) -> Result<()> {
    let SessionScanResult {
        summaries: sessions,
        diagnostics,
    } = scan_all_session_summaries_with_report(None, source)?;
    emit_scan_warning(&diagnostics);

    if sessions.is_empty() {
        println!("{}", "No projects found.".yellow());
        return Ok(());
    }

    let mut projects: Vec<ProjectSummary> = Vec::new();
    for session in sessions {
        if let Some(project) = projects.iter_mut().find(|p| p.name == session.project_name) {
            project.session_count += 1;
            if session.last_activity > project.last_activity {
                project.last_activity = session.last_activity;
            }
        } else {
            projects.push(ProjectSummary {
                name: session.project_name.clone(),
                dir_path: session.project_dir.clone(),
                session_count: 1,
                last_activity: session.last_activity.clone(),
            });
        }
    }

    projects.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));

    println!("{} ({} projects)", "Projects".cyan().bold(), projects.len());
    println!("{}", "-".repeat(60));

    for (i, project) in projects.iter().enumerate() {
        let time_str = project
            .last_activity
            .as_ref()
            .map(|t| format_relative_time(t))
            .unwrap_or_else(|| "Unknown".to_string());

        println!(
            "[{:>2}] {} | {} sessions | {}",
            i + 1,
            project.name.bold(),
            project.session_count,
            time_str.dimmed()
        );
    }

    Ok(())
}

// ============================================================================
// Overview
// ============================================================================

#[derive(serde::Serialize)]
struct ProjectOverview {
    name: String,
    path: Option<String>,
    description: Option<String>,
    session_count: usize,
    last_activity: Option<String>,
    recent_sessions: Vec<SessionOverview>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    memory: Vec<String>,
}

#[derive(serde::Serialize)]
struct SessionOverview {
    source: String,
    session_id: String,
    title: String,
    message_count: usize,
    last_activity: Option<String>,
    recent_messages: Vec<String>,
}

/// Truncate text at a word/line boundary, Unicode-safe
fn truncate_chars(text: &str, max_chars: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_chars {
        return text.to_string();
    }
    let truncated: String = chars[..max_chars].iter().collect();
    if let Some(pos) = truncated.rfind(['\n', ' ']) {
        format!("{}...", &truncated[..pos])
    } else {
        format!("{}...", truncated)
    }
}

/// Check if a timestamp is at or after the given cutoff
fn is_after_cutoff(timestamp: Option<&str>, cutoff: &chrono::DateTime<chrono::Utc>) -> bool {
    timestamp.is_some_and(|ts| {
        chrono::DateTime::parse_from_rfc3339(ts)
            .map(|dt| dt.with_timezone(&chrono::Utc) >= *cutoff)
            .unwrap_or(false)
    })
}

/// Read memory entries from the project's memory index file.
///
/// Supports multiple MEMORY.md formats:
/// - List items: `- [Title](file) — description` or `- plain text`
/// - Section headers: `## Section Title` (with optional body lines like `详见 [file]`)
///
/// For list items, extracts title + description. For section headers,
/// combines the heading with the first non-empty body line as context.
fn read_memory_entries(project_dir: &Path, source: &str, max_entries: usize) -> Vec<String> {
    let memory_file = memory_dir_for_source(project_dir, source).join("MEMORY.md");
    let content = match fs::read_to_string(&memory_file) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let lines: Vec<&str> = content.lines().collect();
    let mut entries = Vec::new();
    let mut i = 0;

    while i < lines.len() && entries.len() < max_entries {
        let line = lines[i];

        if line.starts_with("- ") {
            let entry = line.trim_start_matches("- ");
            entries.push(strip_md_link(entry));
        } else if line.starts_with("## ") {
            let heading = line.trim_start_matches('#').trim();
            // Look ahead for the first non-empty prose line as context
            let mut body = None;
            for line in &lines[i + 1..] {
                let next = line.trim();
                if next.is_empty() {
                    continue;
                }
                // Stop at next heading or list item (they'll be processed separately)
                if next.starts_with('#') || next.starts_with("- ") {
                    break;
                }
                // Skip code blocks, tables, and block quotes
                if next.starts_with("```") || next.starts_with('|') || next.starts_with('>') {
                    continue;
                }
                body = Some(strip_md_link(next));
                break;
            }
            // Only emit heading if it has a meaningful body line
            if let Some(desc) = body {
                entries.push(format!("{} — {}", heading, desc));
            }
        }

        i += 1;
    }

    entries
}

/// Strip markdown link syntax: `[Title](url) rest` → `Title rest`,
/// `详见 [file](url) — desc` → `详见 file — desc`
fn strip_md_link(text: &str) -> String {
    let mut result = text.to_string();
    while let Some(open) = result.find('[') {
        if let Some(close) = result[open..].find("](") {
            let close_abs = open + close;
            if let Some(paren_end) = result[close_abs + 2..].find(')') {
                let paren_end_abs = close_abs + 2 + paren_end;
                let link_text = result[open + 1..close_abs].to_string();
                result = format!(
                    "{}{}{}",
                    &result[..open],
                    link_text,
                    &result[paren_end_abs + 1..]
                );
                continue;
            }
        }
        break;
    }
    result
}

/// Read project description from CLAUDE.md (priority) or README.md
fn get_project_description(project_path: &Path, max_chars: usize) -> Option<String> {
    let desc_file = ["CLAUDE.md", "README.md"]
        .iter()
        .map(|f| project_path.join(f))
        .find(|p| p.exists())?;

    let content = fs::read_to_string(&desc_file).ok()?;

    // Skip YAML frontmatter (--- ... ---)
    let content = if let Some(after_prefix) = content.strip_prefix("---") {
        if let Some(end_idx) = after_prefix.find("\n---") {
            let skip = end_idx + 4; // skip past "\n---"
            if skip < after_prefix.len() {
                &after_prefix[skip..]
            } else {
                ""
            }
        } else {
            content.as_str()
        }
    } else {
        content.as_str()
    };

    // Skip markdown headings, blank lines, and Claude Code boilerplate
    let content = content
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.is_empty()
                && !trimmed.starts_with('#')
                && !trimmed.starts_with("This file provides guidance to Claude")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let content = content.trim();
    if content.is_empty() {
        return None;
    }

    Some(truncate_chars(content, max_chars))
}

/// Extract recent meaningful user messages from a session
fn extract_recent_user_messages(
    session: &ConversationSession,
    count: usize,
    min_chars: usize,
) -> Vec<String> {
    let mut messages = Vec::new();

    for entry in session.entries.iter().rev() {
        if messages.len() >= count {
            break;
        }

        if entry.entry_type != "user" || ConversationSession::is_tool_result_entry(entry) {
            continue;
        }

        if let Some(msg) = &entry.message {
            if let Some(text) = ConversationSession::extract_user_text(msg) {
                let text = text.replace('\n', " ");
                let text = text.trim().to_string();
                if text.chars().count() >= min_chars {
                    messages.push(truncate_chars(&text, 100));
                }
            }
        }
    }

    messages.reverse();
    messages
}

/// Overview of all projects with recent session context
pub fn handle_session_overview(
    recent_count: usize,
    since: Option<&str>,
    json_output: bool,
    source: SessionSourceFilter,
) -> Result<()> {
    let since_cutoff = since.map(parse_duration_filter).transpose()?;

    let SessionScanResult {
        summaries: mut sessions,
        diagnostics,
    } = scan_all_session_summaries_with_report(None, source)?;

    if let Some(ref cutoff) = since_cutoff {
        sessions.retain(|s| is_after_cutoff(s.last_activity.as_deref(), cutoff));
    }

    if sessions.is_empty() {
        if json_output {
            let payload = attach_scan_diagnostics(
                json!({
                    "total_projects": 0,
                    "projects": []
                }),
                &diagnostics,
            );
            println!("{}", serde_json::to_string_pretty(&payload)?);
        } else {
            emit_scan_warning(&diagnostics);
            println!("{}", "No projects found.".yellow());
        }
        return Ok(());
    }

    let mut overviews: Vec<ProjectOverview> = Vec::new();
    let total_sessions = sessions.len();

    let mut groups: Vec<(String, Vec<SessionSummary>)> = Vec::new();
    for session in sessions {
        if let Some((_, existing)) = groups
            .iter_mut()
            .find(|(name, _)| name == &session.project_name)
        {
            existing.push(session);
        } else {
            groups.push((session.project_name.clone(), vec![session]));
        }
    }

    for (project_name, mut project_sessions) in groups {
        project_sessions.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));
        let project_path = project_sessions
            .iter()
            .find(|s| s.source == "claude")
            .and_then(|s| {
                ConversationSession::from_file(&s.file_path)
                    .ok()
                    .and_then(|conv| conv.cwd().map(|c| c.to_string()))
            })
            .or_else(|| {
                project_sessions
                    .iter()
                    .find(|s| s.source == "codex" && !s.project_dir.as_os_str().is_empty())
                    .map(|s| s.project_dir.display().to_string())
            })
            .or_else(|| {
                project_sessions
                    .iter()
                    .find(|s| s.source == "omp" && !s.project_dir.as_os_str().is_empty())
                    .map(|s| s.project_dir.display().to_string())
            });

        let description = project_path
            .as_deref()
            .and_then(|p| get_project_description(Path::new(p), 300));

        let recent_sessions: Vec<SessionOverview> = project_sessions
            .iter()
            .take(recent_count)
            .map(|s| {
                let title = s.display_title(50);
                let recent_messages = extract_recent_messages_for_summary(s, 3, 10);
                SessionOverview {
                    source: s.source.clone(),
                    session_id: s.session_id.clone(),
                    title,
                    message_count: s.message_count,
                    last_activity: s.last_activity.clone(),
                    recent_messages,
                }
            })
            .collect();

        let memory = project_sessions
            .iter()
            .find_map(|s| {
                let entries = read_memory_entries(&s.project_dir, &s.source, 10);
                (!entries.is_empty()).then_some(entries)
            })
            .unwrap_or_default();

        overviews.push(ProjectOverview {
            name: project_name,
            path: project_path,
            description,
            session_count: project_sessions.len(),
            last_activity: project_sessions
                .first()
                .and_then(|s| s.last_activity.clone()),
            recent_sessions,
            memory,
        });
    }

    if json_output {
        let payload = attach_scan_diagnostics(
            json!({
                "total_projects": overviews.len(),
                "projects": overviews,
            }),
            &diagnostics,
        );
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        emit_scan_warning(&diagnostics);
        println!(
            "{} projects, {} sessions\n",
            overviews.len().to_string().cyan().bold(),
            total_sessions.to_string().cyan(),
        );

        for (pi, proj) in overviews.iter().enumerate() {
            let time_str = proj
                .last_activity
                .as_ref()
                .map(|t| format_relative_time(t))
                .unwrap_or_else(|| "Unknown".to_string());

            if let Some(desc) = &proj.description {
                let brief: &str = desc.lines().next().unwrap_or(desc);
                let brief = brief.trim_start_matches('#').trim();
                println!(
                    "{} — {}",
                    proj.name.bold(),
                    truncate_chars(brief, 80).dimmed(),
                );
            } else {
                println!("{}", proj.name.bold());
            }
            println!(
                "  {} sessions, last: {}",
                proj.session_count,
                time_str.dimmed(),
            );

            let session_count = proj.recent_sessions.len();
            for (si, sess) in proj.recent_sessions.iter().enumerate() {
                let is_last = si == session_count - 1;
                let branch = if is_last { "└─" } else { "├─" };
                let sess_time = sess
                    .last_activity
                    .as_ref()
                    .map(|t| format_relative_time(t))
                    .unwrap_or_else(|| "?".to_string());

                println!(
                    "  {} [{}] {} ({} msgs, {})",
                    branch,
                    source_label(&sess.source),
                    sess.title,
                    sess.message_count,
                    sess_time.dimmed(),
                );

                let prefix = if is_last { "  " } else { "│ " };
                for msg in &sess.recent_messages {
                    println!("  {}  • {}", prefix, msg.dimmed());
                }
            }

            if !proj.memory.is_empty() {
                println!("  {} {}", "📝".dimmed(), "Memory:".dimmed());
                for entry in &proj.memory {
                    println!("     • {}", truncate_chars(entry, 70).dimmed());
                }
            }

            if pi < overviews.len() - 1 {
                println!();
            }
        }
    }

    Ok(())
}

fn resolve_session_by_id<'a>(
    sessions: &'a [SessionSummary],
    session_id: &str,
    source_filter: SessionSourceFilter,
) -> Result<&'a SessionSummary> {
    let mut candidates = Vec::new();

    for session in sessions {
        let identity = session.identity()?;
        if identity.session_id == session_id && source_filter.includes(identity.source) {
            candidates.push(session);
        }
    }

    match candidates.as_slice() {
        [] => anyhow::bail!("Session not found: {session_id}"),
        [session] => Ok(*session),
        _ => {
            let details = candidates
                .iter()
                .map(|session| {
                    format!(
                        "  {}  project={}  id={}",
                        session.source, session.project_name, session.session_id
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            anyhow::bail!("Ambiguous session ID '{session_id}'. Specify --source:\n{details}")
        }
    }
}

/// Show session details (non-interactive), with optional drill-down flags
#[allow(clippy::too_many_arguments)]
pub fn handle_session_show(
    session_id: &str,
    tail: Option<usize>,
    head: Option<usize>,
    around: Option<&str>,
    num: usize,
    json: bool,
    full: bool,
    source: SessionSourceFilter,
) -> Result<()> {
    let SessionScanResult {
        summaries: sessions,
        diagnostics,
    } = scan_all_session_summaries_with_report(None, source)?;
    if !json {
        emit_scan_warning(&diagnostics);
    }
    let session = resolve_session_by_id(&sessions, session_id, source)?;

    // If no drill-down flags and not json, use interactive view
    if (session.source == "claude" || session.source == "omp")
        && tail.is_none()
        && head.is_none()
        && around.is_none()
        && !json
    {
        show_session_details(session)?;
        return Ok(());
    }

    // Drill-down mode: parse and filter messages
    // JSON or --full uses full content (no truncation); terminal uses simplified.
    // --around always uses full content so its keyword matching stays consistent with
    // `search` (which indexes full content); otherwise simplification (code-block removal
    // + 500-char truncation) would drop keywords that search matched, misplacing the anchor.
    let messages = collect_display_messages_for_summary(session, json || full || around.is_some());

    if messages.is_empty() {
        if json {
            let payload = attach_scan_diagnostics(
                serde_json::json!({
                    "source": session.source,
                    "session_id": session.session_id,
                    "project": session.project_name,
                    "title": session.title,
                    "message_count": 0,
                    "messages": []
                }),
                &diagnostics,
            );
            println!("{}", serde_json::to_string(&payload)?);
        } else {
            println!("(No messages found)");
        }
        return Ok(());
    }

    // Determine slice range
    let total = messages.len();

    // --around: locate the keyword up-front. If it is not present anywhere, tell the user
    // explicitly instead of silently falling back to the start of the session.
    let around_range = if let Some(keyword) = around {
        match find_around_range(&messages, keyword, num) {
            Some(range) => Some(range),
            None => {
                if json {
                    let payload = attach_scan_diagnostics(
                        serde_json::json!({
                            "source": session.source,
                            "session_id": session.session_id,
                            "project": session.project_name,
                            "title": session.title,
                            "message_count": session.message_count,
                            "showing": format!("around:\"{}\":{}:not-found", keyword, num),
                            "messages": [],
                        }),
                        &diagnostics,
                    );
                    println!("{}", serde_json::to_string(&payload)?);
                } else {
                    println!("未在会话中找到关键词: {}", keyword);
                }
                return Ok(());
            }
        }
    } else {
        None
    };

    let (start, end, showing) = if let (Some(keyword), Some((s, e))) = (around, around_range) {
        (s, e, format!("around:\"{}\":{}", keyword, num))
    } else if let Some(n) = tail {
        let s = total.saturating_sub(n);
        (s, total, format!("tail:{}", n))
    } else if let Some(n) = head {
        (0, n.min(total), format!("head:{}", n))
    } else {
        (0, total, "all".to_string())
    };

    let slice = &messages[start..end];

    if json {
        let json_msgs: Vec<serde_json::Value> = slice
            .iter()
            .map(|m| {
                serde_json::json!({
                "index": m.index,
                "role": m.role,
                "timestamp": m.timestamp,
                    "content": m.content,
                })
            })
            .collect();

        let payload = attach_scan_diagnostics(
            serde_json::json!({
                "source": session.source,
                "session_id": session.session_id,
                "project": session.project_name,
                "title": session.title,
                "message_count": session.message_count,
                "showing": showing,
                "messages": json_msgs,
            }),
            &diagnostics,
        );
        println!("{}", serde_json::to_string(&payload)?);
    } else {
        let is_tty = atty::is(atty::Stream::Stdout);
        println!(
            "--- [{}] {} | {} | {} | {} msgs | showing {} ---",
            source_label(&session.source),
            session.session_id,
            session.project_name,
            session.display_title(40),
            session.message_count,
            showing,
        );
        println!();
        for m in slice {
            let role_tag = if m.role == "user" { "U" } else { "A" };
            let time_str = m
                .timestamp
                .as_ref()
                .map(|t| format_compact_relative_time(t))
                .unwrap_or_default();
            if is_tty {
                println!(
                    "[{}] [{}] {}",
                    format!("{}", m.index).cyan(),
                    if m.role == "user" {
                        role_tag.green().bold().to_string()
                    } else {
                        role_tag.blue().bold().to_string()
                    },
                    time_str.dimmed()
                );
            } else {
                println!("[{}] [{}] {}", m.index, role_tag, time_str);
            }
            for line in m.content.lines() {
                println!("  {}", line);
            }
            println!();
        }
    }

    Ok(())
}

// ============================================================================
// Search functionality
// ============================================================================

#[derive(Debug, Clone, serde::Serialize)]
struct SearchMatch {
    role: String,
    snippet: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Default)]
enum MatchMode {
    #[default]
    And, // 0 — sorted first
    Or, // 1 — sorted after AND
}

#[derive(Debug, Clone)]
struct SessionSearchResult {
    summary: SessionSummary,
    matches: Vec<SearchMatch>,
    score: f64,
    match_mode: MatchMode,
}

#[derive(Debug, Clone, serde::Serialize)]
struct MemoryMatch {
    snippet: String,
}

#[derive(Debug, Clone, serde::Serialize)]
struct MemorySearchResult {
    project: String,
    file: String,
    matches: Vec<MemoryMatch>,
    #[serde(skip)]
    match_mode: MatchMode,
}

#[derive(Debug, Clone)]
struct MemorySearchRoot {
    project: String,
    dir_path: PathBuf,
    source: String,
}

/// A processed message ready for display
struct DisplayMessage {
    index: usize,
    role: String,
    timestamp: Option<String>,
    content: String,
}

/// Compute the display range for `--around`: `num` messages before/after the first message
/// whose content contains `keyword` (case-insensitive). Returns `None` when the keyword is
/// absent — the caller reports "not found" rather than silently anchoring at the start.
fn find_around_range(
    messages: &[DisplayMessage],
    keyword: &str,
    num: usize,
) -> Option<(usize, usize)> {
    let keyword_lower = keyword.to_lowercase();
    let pos = messages
        .iter()
        .position(|m| m.content.to_lowercase().contains(&keyword_lower))?;
    let total = messages.len();
    Some((pos.saturating_sub(num), (pos + num + 1).min(total)))
}

/// Collect displayable messages from a conversation.
/// Merges all assistant entries between two user messages into a single reply,
/// with tool calls summarized in one line.
/// When `full_content` is true, uses full text extraction (no truncation/code simplification).
fn collect_display_messages(conv: &ConversationSession, full_content: bool) -> Vec<DisplayMessage> {
    let mut messages = Vec::new();
    let mut index = 0;

    // Accumulator for current assistant turn
    let mut assistant_texts: Vec<String> = Vec::new();
    let mut assistant_tools: Vec<(String, Option<String>)> = Vec::new();
    let mut assistant_ts: Option<String> = None;

    for entry in &conv.entries {
        match entry.entry_type.as_str() {
            "user" | "assistant" => {}
            _ => continue,
        }

        if ConversationSession::is_tool_result_entry(entry) {
            continue;
        }

        let is_user = entry.entry_type == "user";

        if is_user {
            // Flush accumulated assistant turn
            flush_assistant_turn(
                &mut messages,
                &mut index,
                &mut assistant_texts,
                &mut assistant_tools,
                &mut assistant_ts,
            );

            // Emit user message
            if let Some(msg) = entry.message.as_ref() {
                let text = if full_content {
                    ConversationSession::extract_display_content_full(msg, true)
                } else {
                    ConversationSession::extract_display_content(msg, true)
                };
                if let Some(text) = text {
                    index += 1;
                    messages.push(DisplayMessage {
                        index,
                        role: "user".to_string(),
                        timestamp: entry.timestamp.clone(),
                        content: text,
                    });
                }
            }
        } else {
            // Assistant entry: accumulate
            if assistant_ts.is_none() {
                assistant_ts = entry.timestamp.clone();
            }

            if let Some(msg) = entry.message.as_ref() {
                // Single-pass: try_extract_tool_info returns Some for tool-only messages
                if let Some(tools) = ConversationSession::try_extract_tool_info(msg) {
                    assistant_tools.extend(tools);
                } else {
                    let text = if full_content {
                        ConversationSession::extract_display_content_full(msg, false)
                    } else {
                        ConversationSession::extract_display_content(msg, false)
                    };
                    if let Some(text) = text {
                        assistant_texts.push(text);
                    }
                }
            }
        }
    }

    // Flush remaining assistant turn
    flush_assistant_turn(
        &mut messages,
        &mut index,
        &mut assistant_texts,
        &mut assistant_tools,
        &mut assistant_ts,
    );

    messages
}

fn collect_display_messages_for_summary(
    session: &SessionSummary,
    full_content: bool,
) -> Vec<DisplayMessage> {
    if session.source == "codex" {
        let Ok(conv) = CodexSession::from_file(&session.file_path) else {
            return Vec::new();
        };
        return conv
            .display_messages(full_content)
            .into_iter()
            .enumerate()
            .map(|(idx, message)| DisplayMessage {
                index: idx + 1,
                role: message.role,
                timestamp: message.timestamp,
                content: message.content,
            })
            .collect();
    }

    if session.source == "omp" {
        let Ok(conv) = OmpSession::from_file(&session.file_path) else {
            return Vec::new();
        };
        return conv
            .display_messages()
            .into_iter()
            .enumerate()
            .map(|(idx, message)| DisplayMessage {
                index: idx + 1,
                role: message.role,
                timestamp: message.timestamp,
                content: message.content,
            })
            .collect();
    }

    ConversationSession::from_file(&session.file_path)
        .map(|conv| collect_display_messages(&conv, full_content))
        .unwrap_or_default()
}

fn extract_recent_messages_for_summary(
    session: &SessionSummary,
    count: usize,
    min_chars: usize,
) -> Vec<String> {
    if session.source == "codex" {
        let Ok(conv) = CodexSession::from_file(&session.file_path) else {
            return Vec::new();
        };
        let mut messages: Vec<String> = conv
            .display_messages(false)
            .into_iter()
            .rev()
            .filter(|m| m.role == "user")
            .filter_map(|m| {
                let text = m.content.replace('\n', " ");
                let text = text.trim().to_string();
                (text.chars().count() >= min_chars).then(|| truncate_chars(&text, 100))
            })
            .take(count)
            .collect();
        messages.reverse();
        return messages;
    }

    if session.source == "omp" {
        let Ok(conv) = OmpSession::from_file(&session.file_path) else {
            return Vec::new();
        };
        let mut messages: Vec<String> = conv
            .display_messages()
            .into_iter()
            .rev()
            .filter(|m| m.role == "user")
            .filter_map(|m| {
                let text = m.content.replace('\n', " ");
                let text = text.trim().to_string();
                (text.chars().count() >= min_chars).then(|| truncate_chars(&text, 100))
            })
            .take(count)
            .collect();
        messages.reverse();
        return messages;
    }

    ConversationSession::from_file(&session.file_path)
        .map(|conv| extract_recent_user_messages(&conv, count, min_chars))
        .unwrap_or_default()
}

/// Flush accumulated assistant texts and tools into a single DisplayMessage.
fn flush_assistant_turn(
    messages: &mut Vec<DisplayMessage>,
    index: &mut usize,
    texts: &mut Vec<String>,
    tools: &mut Vec<(String, Option<String>)>,
    ts: &mut Option<String>,
) {
    if texts.is_empty() && tools.is_empty() {
        return;
    }
    let mut parts = Vec::new();
    parts.append(texts);
    if !tools.is_empty() {
        parts.push(format_tool_summary(tools));
        tools.clear();
    }
    *index += 1;
    messages.push(DisplayMessage {
        index: *index,
        role: "assistant".to_string(),
        timestamp: ts.take(),
        content: parts.join("\n"),
    });
}

/// Format accumulated tool calls into a compact summary.
/// Groups by tool name, shows files per tool.
/// Output: "[Tools: Read -> file1.rs|file2.rs, Edit -> main.rs, Bash]"
fn format_tool_summary(tools: &[(String, Option<String>)]) -> String {
    use std::collections::BTreeMap;

    // Group files by tool name, preserving order via BTreeMap
    let mut grouped: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (name, file) in tools {
        let entry = grouped.entry(name.as_str()).or_default();
        if let Some(f) = file {
            if !entry.contains(&f.as_str()) {
                entry.push(f.as_str());
            }
        }
    }

    let parts: Vec<String> = grouped
        .into_iter()
        .map(|(name, files)| {
            if files.is_empty() {
                name.to_string()
            } else {
                format!("{} -> {}", name, files.join("|"))
            }
        })
        .collect();

    format!("[Tools: {}]", parts.join(", "))
}

/// Parse a duration string (e.g., "1d", "3h", "1w") into a cutoff DateTime
fn parse_duration_filter(since: &str) -> Result<chrono::DateTime<chrono::Utc>> {
    use chrono::Utc;

    let since = since.trim().to_lowercase();
    if since.len() < 2 {
        anyhow::bail!(
            "Invalid duration: '{}'. Use format like '1d', '3h', '1w'",
            since
        );
    }
    let (num_str, unit) = since.split_at(since.len() - 1);
    let num: i64 = num_str
        .parse()
        .with_context(|| format!("Invalid duration number: '{}'", num_str))?;

    let duration = match unit {
        "m" => chrono::Duration::minutes(num),
        "h" => chrono::Duration::hours(num),
        "d" => chrono::Duration::days(num),
        "w" => chrono::Duration::weeks(num),
        _ => anyhow::bail!(
            "Unknown duration unit '{}'. Use m/h/d/w (e.g., '1d', '3h', '1w')",
            unit
        ),
    };

    Ok(Utc::now() - duration)
}

/// Calculate a 0.0-1.0 recency score (half-life: 7 days)
fn calculate_recency_score(last_activity: Option<&str>) -> f64 {
    use chrono::{DateTime, Utc};

    let Some(ts) = last_activity else {
        return 0.0;
    };
    let Ok(dt) = DateTime::parse_from_rfc3339(ts) else {
        return 0.0;
    };

    let hours_ago = Utc::now()
        .signed_duration_since(dt.with_timezone(&Utc))
        .num_hours() as f64;

    // Half-life of 168 hours (7 days): score = e^(-t * ln2 / 168)
    (-hours_ago / 168.0 * 0.693).exp()
}

/// Compact relative time for search output
fn format_compact_relative_time(timestamp: &str) -> String {
    use chrono::{DateTime, Utc};

    if let Ok(dt) = DateTime::parse_from_rfc3339(timestamp) {
        let duration = Utc::now().signed_duration_since(dt.with_timezone(&Utc));
        let minutes = duration.num_minutes();
        let hours = duration.num_hours();
        let days = duration.num_days();

        if minutes < 1 {
            "now".to_string()
        } else if minutes < 60 {
            format!("{}m ago", minutes)
        } else if hours < 24 {
            format!("{}h ago", hours)
        } else if days < 7 {
            format!("{}d ago", days)
        } else if days < 30 {
            format!("{}w ago", days / 7)
        } else {
            format!("{}mo ago", days / 30)
        }
    } else {
        "?".to_string()
    }
}

/// Search memory files (*.md) in project memory directories
fn search_memory_files(
    roots: &[MemorySearchRoot],
    keywords: &[&str],
    context_chars: usize,
) -> Vec<MemorySearchResult> {
    let keywords_lower: Vec<String> = keywords.iter().map(|k| k.to_lowercase()).collect();
    let multi_keyword = keywords_lower.len() > 1;
    let mut results = Vec::new();

    for root in roots {
        if validate_directory_root(&root.dir_path).is_err() {
            continue;
        }
        let memory_relative = Path::new(memory_dir_name_for_source(&root.source));
        let Ok(memory_dir) = safe_join_within_root(&root.dir_path, memory_relative) else {
            continue;
        };
        let Ok(metadata) = fs::symlink_metadata(&memory_dir) else {
            continue;
        };
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || validate_directory_candidate(&root.dir_path, &memory_dir).is_err()
        {
            continue;
        }

        let Ok(entries) = fs::read_dir(&memory_dir) else {
            continue;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || validate_regular_candidate(&memory_dir, &path).is_err()
            {
                continue;
            }

            let Ok(content) = fs::read_to_string(&path) else {
                continue;
            };

            let file_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown");

            let mut and_matches = Vec::new();
            let mut or_matches = Vec::new();

            for line in content.lines() {
                let line_lower = line.to_lowercase();
                let matched: Vec<&String> = keywords_lower
                    .iter()
                    .filter(|kw| line_lower.contains(kw.as_str()))
                    .collect();

                if matched.is_empty() {
                    continue;
                }

                let snippet = extract_match_snippet(line, matched[0], context_chars);
                let m = MemoryMatch { snippet };

                if matched.len() == keywords_lower.len() {
                    and_matches.push(m);
                } else if multi_keyword {
                    or_matches.push(m);
                }
            }

            let file_label = format!("{}/{}", memory_dir_name_for_source(&root.source), file_name);

            if !and_matches.is_empty() {
                results.push(MemorySearchResult {
                    project: root.project.clone(),
                    file: file_label.clone(),
                    matches: and_matches,
                    match_mode: MatchMode::And,
                });
            }
            if !or_matches.is_empty() {
                results.push(MemorySearchResult {
                    project: root.project.clone(),
                    file: file_label,
                    matches: or_matches,
                    match_mode: MatchMode::Or,
                });
            }
        }
    }

    // Sort: AND first
    results.sort_by(|a, b| a.match_mode.cmp(&b.match_mode));

    results
}

fn memory_search_roots_from_sessions(sessions: &[SessionSummary]) -> Vec<MemorySearchRoot> {
    let mut seen = std::collections::HashSet::new();
    let mut roots = Vec::new();

    for session in sessions {
        if session.project_dir.as_os_str().is_empty() {
            continue;
        }

        let key = (
            session.source.clone(),
            session.project_name.clone(),
            session.project_dir.clone(),
        );
        if !seen.insert(key) {
            continue;
        }

        roots.push(MemorySearchRoot {
            project: session.project_name.clone(),
            dir_path: session.project_dir.clone(),
            source: session.source.clone(),
        });
    }

    roots
}

/// Search sessions across projects (both user and assistant messages).
/// With multiple keywords, collects AND matches (all keywords present)
/// and OR matches (any keyword present), sorted with AND results first.
fn search_sessions_full(
    sessions: &[SessionSummary],
    keywords: &[&str],
    context_chars: usize,
    user_only: bool,
) -> Vec<SessionSearchResult> {
    let keywords_lower: Vec<String> = keywords.iter().map(|k| k.to_lowercase()).collect();
    let multi_keyword = keywords_lower.len() > 1;
    let mut results = Vec::new();

    for session in sessions {
        let mut and_matches = Vec::new();
        let mut or_matches = Vec::new();
        const MAX_MATCHES_PER_SESSION: usize = 20;

        for message in collect_display_messages_for_summary(session, true) {
            if and_matches.len() + or_matches.len() >= MAX_MATCHES_PER_SESSION {
                break;
            }

            if user_only && message.role != "user" {
                continue;
            }

            let text_lower = message.content.to_lowercase();
            let matched_kws: Vec<&String> = keywords_lower
                .iter()
                .filter(|kw| text_lower.contains(kw.as_str()))
                .collect();

            if matched_kws.is_empty() {
                continue;
            }

            let is_and = matched_kws.len() == keywords_lower.len();
            let snippet = extract_match_snippet(&message.content, matched_kws[0], context_chars);
            let m = SearchMatch {
                role: message.role,
                snippet,
            };

            if is_and {
                and_matches.push(m);
            } else if multi_keyword {
                or_matches.push(m);
            }
        }

        let recency_score = calculate_recency_score(session.last_activity.as_deref());

        // Emit AND result if any AND matches
        if !and_matches.is_empty() {
            let match_score = (and_matches.len() as f64).ln_1p();
            let score = recency_score * 0.6 + match_score * 0.4;
            results.push(SessionSearchResult {
                summary: session.clone(),
                matches: and_matches,
                score,
                match_mode: MatchMode::And,
            });
        }

        // Emit OR result for partial matches (only with multi-keyword queries)
        if !or_matches.is_empty() {
            let match_score = (or_matches.len() as f64).ln_1p();
            let score = recency_score * 0.6 + match_score * 0.4;
            results.push(SessionSearchResult {
                summary: session.clone(),
                matches: or_matches,
                score,
                match_mode: MatchMode::Or,
            });
        }
    }

    // Sort: AND first, then by score within each group
    results.sort_by(|a, b| {
        a.match_mode.cmp(&b.match_mode).then_with(|| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    });

    results
}

/// Handle `ccs session search` command
#[allow(clippy::too_many_arguments)]
pub fn handle_session_search(
    keywords: &[&str],
    project_filter: Option<&str>,
    since: Option<&str>,
    context_chars: usize,
    limit: usize,
    user_only: bool,
    json_output: bool,
    source: SessionSourceFilter,
) -> Result<()> {
    let query_display = keywords.join(" ");

    // 1. Parse time filter
    let cutoff = if let Some(since_str) = since {
        Some(parse_duration_filter(since_str)?)
    } else {
        None
    };

    // 2. Scan sessions and derive project memory roots from the selected source.
    let SessionScanResult {
        summaries: mut all_sessions,
        mut diagnostics,
    } = scan_all_session_summaries_with_report(project_filter, source)?;
    if !json_output {
        emit_scan_warning(&diagnostics);
    }
    let search_started = Instant::now();
    let memory_roots = memory_search_roots_from_sessions(&all_sessions);
    if all_sessions.is_empty() && memory_roots.is_empty() {
        diagnostics.search_load_ms = elapsed_millis(search_started);
        if json_output {
            let payload = attach_scan_diagnostics(
                serde_json::json!({
                    "query": query_display,
                    "total_matches": 0,
                    "memory_results": [],
                    "session_results": [],
                }),
                &diagnostics,
            );
            println!("{}", serde_json::to_string(&payload)?);
        } else {
            println!("[0 results | query: \"{}\"]", query_display);
        }
        return Ok(());
    }

    // 3. Search memory files (no time filter - memory is persistent knowledge)
    let memory_results = search_memory_files(&memory_roots, keywords, context_chars);

    // 4. Apply time filter.
    if let Some(ref cutoff_dt) = cutoff {
        all_sessions.retain(|session| {
            if let Some(ref ts) = session.last_activity {
                if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(ts) {
                    return dt.with_timezone(&chrono::Utc) >= *cutoff_dt;
                }
            }
            false
        });
    }

    // 5. Search sessions
    let session_results = search_sessions_full(&all_sessions, keywords, context_chars, user_only);
    diagnostics.search_load_ms = elapsed_millis(search_started);

    // 6. Count totals
    let memory_match_count: usize = memory_results.iter().map(|r| r.matches.len()).sum();
    let session_match_count: usize = session_results.iter().map(|r| r.matches.len()).sum();
    let total_matches = memory_match_count + session_match_count;

    // 7. Output
    if json_output {
        let session_json: Vec<serde_json::Value> = session_results
            .iter()
            .map(|r| {
                serde_json::json!({
                    "session_id": r.summary.session_id,
                    "source": r.summary.source,
                    "project": r.summary.project_name,
                    "title": r.summary.title,
                    "last_activity": r.summary.last_activity,
                    "message_count": r.summary.message_count,
                    "match_mode": if r.match_mode == MatchMode::And { "and" } else { "or" },
                    "matches": r.matches,
                })
            })
            .collect();

        let payload = attach_scan_diagnostics(
            serde_json::json!({
                "query": query_display,
                "total_matches": total_matches,
                "memory_results": memory_results,
                "session_results": session_json,
            }),
            &diagnostics,
        );
        println!("{}", serde_json::to_string(&payload)?);
        return Ok(());
    }

    // Text output
    let is_tty = atty::is(atty::Stream::Stdout);

    if total_matches == 0 {
        println!("[0 results | query: \"{}\"]", query_display);
        return Ok(());
    }

    // Header
    if memory_match_count > 0 && session_match_count > 0 {
        println!(
            "[{} matches: {} in memory, {} in {} sessions | query: \"{}\"]",
            total_matches,
            memory_match_count,
            session_match_count,
            session_results.len(),
            query_display
        );
    } else if memory_match_count > 0 {
        println!(
            "[{} matches in memory | query: \"{}\"]",
            memory_match_count, query_display
        );
    } else {
        println!(
            "[{} matches in {} sessions | query: \"{}\"]",
            session_match_count,
            session_results.len(),
            query_display
        );
    }
    println!();

    let mut shown = 0;

    let multi_keyword = keywords.len() > 1;

    // Memory results first
    if !memory_results.is_empty() {
        if is_tty {
            println!("{}", "=== Memory ===".cyan().bold());
        } else {
            println!("=== Memory ===");
        }
        let mut prev_mode: Option<&MatchMode> = None;
        for result in &memory_results {
            if shown >= limit {
                break;
            }
            if multi_keyword && prev_mode != Some(&result.match_mode) {
                let label = match result.match_mode {
                    MatchMode::And => format!("[AND] all of: {}", query_display),
                    MatchMode::Or => format!("[OR] any of: {}", query_display),
                };
                if is_tty {
                    println!("{}", label.yellow());
                } else {
                    println!("{}", label);
                }
                prev_mode = Some(&result.match_mode);
            }
            let header = format!("--- {} | {} ---", result.project, result.file);
            if is_tty {
                println!("{}", header.dimmed());
            } else {
                println!("{}", header);
            }
            for m in &result.matches {
                if shown >= limit {
                    break;
                }
                println!("  {}", m.snippet);
                shown += 1;
            }
            println!();
        }
    }

    // Session results
    if !session_results.is_empty() && shown < limit {
        if !memory_results.is_empty() {
            if is_tty {
                println!("{}", "=== Sessions ===".cyan().bold());
            } else {
                println!("=== Sessions ===");
            }
        }
        let mut prev_mode: Option<&MatchMode> = None;
        for result in &session_results {
            if shown >= limit {
                break;
            }
            if multi_keyword && prev_mode != Some(&result.match_mode) {
                let label = match result.match_mode {
                    MatchMode::And => format!("[AND] all of: {}", query_display),
                    MatchMode::Or => format!("[OR] any of: {}", query_display),
                };
                if is_tty {
                    println!("{}", label.yellow());
                } else {
                    println!("{}", label);
                }
                prev_mode = Some(&result.match_mode);
            }
            let time_str = result
                .summary
                .last_activity
                .as_ref()
                .map(|t| format_compact_relative_time(t))
                .unwrap_or_else(|| "?".to_string());

            let header = format!(
                "--- [{}] {} | {} | {} | {} | {} msgs ---",
                source_label(&result.summary.source),
                result.summary.session_id,
                result.summary.project_name,
                result.summary.display_title(40),
                time_str,
                result.summary.message_count,
            );

            if is_tty {
                println!("{}", header.dimmed());
            } else {
                println!("{}", header);
            }

            for m in &result.matches {
                if shown >= limit {
                    break;
                }
                let role_tag = if m.role == "user" { "U" } else { "A" };
                println!("  [{}] {}", role_tag, m.snippet);
                shown += 1;
            }
            println!();
        }
    }

    // Footer
    if total_matches > limit {
        println!(
            "[showing {} of {} matches | use -n {} to see more]",
            shown.min(limit),
            total_matches,
            total_matches
        );
    }

    Ok(())
}

/// Rename session (non-interactive) using an explicit source filter.
pub fn handle_session_rename_with_source(
    session_id: &str,
    new_title: &str,
    source: SessionSourceFilter,
) -> Result<()> {
    let sessions =
        scan_summaries_for_mutation(scan_all_session_summaries_with_report(None, source)?)?;
    let session = resolve_session_by_id(&sessions, session_id, source)?;
    ensure_can_rename(session)?;
    rename_session_with_guard(session, new_title)?;
    println!(
        "{} Session renamed successfully!",
        "SUCCESS:".green().bold()
    );
    Ok(())
}

/// Rename session using the legacy all-sources behavior.
#[allow(dead_code)] // Re-exported deprecated handler retained for downstream CLI/library compatibility.
#[deprecated(note = "use handle_session_rename_with_source")]
pub fn handle_session_rename(session_id: &str, new_title: &str) -> Result<()> {
    handle_session_rename_with_source(session_id, new_title, SessionSourceFilter::All)
}

/// Delete session (non-interactive) using an explicit source filter.
pub fn handle_session_delete_with_source(
    session_id: &str,
    force: bool,
    source: SessionSourceFilter,
) -> Result<()> {
    let sessions =
        scan_summaries_for_mutation(scan_all_session_summaries_with_report(None, source)?)?;
    let session = resolve_session_by_id(&sessions, session_id, source)?;
    ensure_can_delete(session)?;

    if !force {
        println!(
            "{} {}",
            "WARNING:".red().bold(),
            "About to delete session:".red()
        );
        println!("  Title: {}", session.display_title(50));
        println!("  File: {}", session.file_path.display());
        println!();

        let confirm = Confirm::new("Proceed with deletion?")
            .with_default(false)
            .prompt();

        if !matches!(confirm, Ok(true)) {
            println!("{}", "Delete cancelled.".yellow());
            return Ok(());
        }
    }

    delete_session_with_commit(session, DeleteReason::Explicit)?;
    println!(
        "{} Session deleted successfully!",
        "SUCCESS:".green().bold()
    );
    Ok(())
}

/// Delete session using the legacy all-sources behavior.
#[allow(dead_code)] // Re-exported deprecated handler retained for downstream CLI/library compatibility.
#[deprecated(note = "use handle_session_delete_with_source")]
pub fn handle_session_delete(session_id: &str, force: bool) -> Result<()> {
    handle_session_delete_with_source(session_id, force, SessionSourceFilter::All)
}

/// Restore a session that exists in the sync repo but is missing locally.
pub fn handle_session_restore_with_source(
    session_id: Option<&str>,
    source: SessionSourceFilter,
) -> Result<()> {
    ensure_restore_source_supported(source)?;
    let state = SyncState::load().context("Failed to load sync state (is sync configured?)")?;
    let filter = FilterConfig::load()?;
    let claude_dir = claude_projects_dir()?;
    validate_directory_root(&claude_dir)?;
    let remote_projects_path = state.sync_repo_path.join(&filter.sync_subdirectory);

    if !remote_projects_path.exists() {
        println!(
            "{}",
            "Sync repository is empty or not initialized.".yellow()
        );
        return Ok(());
    }
    let remote_projects_dir =
        validate_sync_projects_root(&state.sync_repo_path, &remote_projects_path)?;

    println!("{} missing sessions from sync repo...", "Scanning".cyan());

    // 1. Discover all local sessions
    let local_sessions = discover_sessions(&claude_dir, &filter)?;
    let local_ids: std::collections::HashSet<_> = local_sessions
        .iter()
        .map(|s| s.session_id.clone())
        .collect();

    // 2. Discover all remote sessions
    let remote_sessions = discover_sessions(&remote_projects_dir, &filter)?;

    // 3. Find missing (present in remote, not in local)
    let missing_sessions: Vec<_> = remote_sessions
        .into_iter()
        .filter(|s| !local_ids.contains(&s.session_id))
        .collect();

    if missing_sessions.is_empty() {
        println!();
        println!("{}", "No missing sessions found in sync repo.".green());
        println!("{}", "Your local directory is fully up to date.".dimmed());
        return Ok(());
    }

    // 4. Convert to SessionSummary to re-use display logic
    // We map these back to their correct project_name. Since we don't have
    // the local project directory anymore (it might have been deleted too),
    // we use a placeholder or derived dir path based on the remote project dir.
    let mut missing_summaries = Vec::new();
    for rs in &missing_sessions {
        let pname = rs.project_name().unwrap_or("unknown");
        let remote_rel = Path::new(&rs.file_path)
            .strip_prefix(&remote_projects_dir)
            .unwrap_or(Path::new(&rs.file_path));

        let local_proj_dir = if filter.use_project_name_only {
            // Find an existing project dir with this name, or fallback
            find_local_project_by_name(&claude_dir, pname).unwrap_or_else(|| claude_dir.join(pname))
        } else {
            // Reconstruct full path
            let proj_rel = remote_rel.parent().unwrap_or(Path::new(""));
            claude_dir.join(proj_rel)
        };

        missing_summaries.push(SessionSummary::from_session(rs, pname, &local_proj_dir));
    }

    missing_summaries.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));

    // 5. If specific session_id is provided, restore it directly
    if let Some(target_id) = session_id {
        let Some(target) = missing_summaries.iter().find(|s| s.session_id == target_id) else {
            anyhow::bail!("Session ID {} not found among missing sessions", target_id);
        };

        do_restore(
            target,
            &remote_projects_dir,
            &claude_dir,
            &filter,
            &state.sync_repo_path,
        )?;
        println!(
            "{} Restored session: {}",
            "SUCCESS:".green().bold(),
            target.display_title(50)
        );
        return Ok(());
    }

    // 6. Otherwise, interactive selection
    println!();
    println!(
        "{} Found {} session(s) in sync repo that are missing locally:",
        "Restore:".cyan().bold(),
        missing_summaries.len()
    );
    println!();

    let mut options = Vec::new();
    for summary in missing_summaries.iter() {
        let time = summary
            .last_activity
            .as_ref()
            .map(|t| format_relative_time(t))
            .unwrap_or_else(|| "unknown".to_string());

        options.push(format!(
            "{:<40} [{}] {} msgs  {}",
            summary.display_title(40).dimmed(),
            summary.project_name.cyan(),
            summary.message_count,
            time
        ));
    }
    options.push("Cancel".to_string());

    let selection = inquire::Select::new("Select a session to restore:", options.clone())
        .with_page_size(15)
        .prompt();

    match selection {
        Ok(selected) if selected != "Cancel" => {
            if let Some(idx) = options.iter().position(|x| x == &selected) {
                let target = &missing_summaries[idx];
                do_restore(
                    target,
                    &remote_projects_dir,
                    &claude_dir,
                    &filter,
                    &state.sync_repo_path,
                )?;
                println!();
                println!(
                    "{} Restored session: {}",
                    "SUCCESS:".green().bold(),
                    target.display_title(50)
                );
            }
        }
        _ => {
            println!("{}", "Restore cancelled.".yellow());
        }
    }

    Ok(())
}

/// Restore a session using the legacy all-sources behavior.
#[allow(dead_code)] // Re-exported deprecated handler retained for downstream CLI/library compatibility.
#[deprecated(note = "use handle_session_restore_with_source")]
pub fn handle_session_restore(session_id: Option<&str>) -> Result<()> {
    handle_session_restore_with_source(session_id, SessionSourceFilter::All)
}

fn do_restore(
    target: &SessionSummary,
    remote_projects_dir: &Path,
    local_projects_dir: &Path,
    filter: &FilterConfig,
    sync_repo_path: &Path,
) -> Result<()> {
    let remote_projects_dir = validate_sync_projects_root(sync_repo_path, remote_projects_dir)?;
    let filename = target
        .file_path
        .file_name()
        .context("session path has no filename")?;
    let source_rel = if filter.use_project_name_only {
        safe_project_relative_path(&target.project_name, filename)?
    } else {
        target
            .file_path
            .strip_prefix(local_projects_dir)
            .map_err(|_| anyhow::anyhow!("session path is outside local projects root"))?
            .to_path_buf()
    };

    // Validate the remote source immediately before opening it. This rejects
    // file symlinks and symlinked parent components rather than following them.
    let source_file = safe_join_within_root(&remote_projects_dir, &source_rel)?;
    validate_regular_candidate(&remote_projects_dir, &source_file)?;

    // Reconstruct the local destination from the validated relative path. Do
    // not reuse target.file_path, which may have originated from untrusted
    // remote metadata.
    let destination = safe_join_within_root(local_projects_dir, &source_rel)?;
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    let destination = safe_join_within_root(local_projects_dir, &source_rel)?;
    if destination.exists() {
        let metadata = fs::symlink_metadata(&destination)?;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            anyhow::bail!(
                "restore destination is not a regular file: {}",
                destination.display()
            );
        }
    }

    fs::copy(&source_file, &destination).with_context(|| {
        format!(
            "Failed to restore session file from {} to {}",
            source_file.display(),
            destination.display()
        )
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn make_msg(index: usize, content: &str) -> DisplayMessage {
        DisplayMessage {
            index,
            role: "assistant".to_string(),
            timestamp: None,
            content: content.to_string(),
        }
    }

    fn make_test_summary(
        session_id: &str,
        project_name: &str,
        source: SessionSource,
    ) -> SessionSummary {
        SessionSummary {
            source: source.as_str().to_string(),
            session_id: session_id.to_string(),
            title: "Test session".to_string(),
            project_name: project_name.to_string(),
            project_dir: PathBuf::from("/tmp/project"),
            file_path: PathBuf::from(format!("/tmp/{session_id}.jsonl")),
            message_count: 2,
            user_message_count: 1,
            assistant_message_count: 1,
            first_timestamp: Some("2026-08-02T00:00:00Z".to_string()),
            last_activity: Some("2026-08-02T00:01:00Z".to_string()),
            file_size: 100,
        }
    }

    #[test]
    #[serial]
    fn legacy_project_scans_treat_regular_file_root_as_empty() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::write(home.join(".claude/projects"), b"not a directory").unwrap();
        struct EnvGuard {
            home: Option<std::ffi::OsString>,
            userprofile: Option<std::ffi::OsString>,
        }
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                match self.home.take() {
                    Some(value) => std::env::set_var("HOME", value),
                    None => std::env::remove_var("HOME"),
                }
                match self.userprofile.take() {
                    Some(value) => std::env::set_var("USERPROFILE", value),
                    None => std::env::remove_var("USERPROFILE"),
                }
            }
        }
        let _guard = EnvGuard {
            home: std::env::var_os("HOME"),
            userprofile: std::env::var_os("USERPROFILE"),
        };
        std::env::set_var("HOME", &home);
        std::env::set_var("USERPROFILE", &home);

        let projects = scan_all_projects().unwrap();
        let detected = detect_current_project().unwrap();

        assert!(projects.is_empty());
        assert!(detected.is_none());
    }

    #[test]
    fn regular_file_roots_are_unavailable_for_all_sources() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        fs::write(&root, "not a directory").unwrap();

        for source in ["claude", "codex", "omp"] {
            let mut diagnostics = ScanDiagnostics::with_id(format!("I-ROOT-{source}"));
            assert!(!root_is_available(&root, source, &mut diagnostics));
            assert_eq!(diagnostics.io_errors, 1);
            assert_eq!(diagnostics.warnings.len(), 1);
        }
    }

    #[cfg(unix)]
    #[test]
    fn all_source_scanners_reject_file_symlinks() {
        use std::os::unix::fs::symlink;

        let (temp, roots, config) = make_scan_fixture();
        let external = temp.path().join("external.jsonl");
        fs::write(
            &external,
            r#"{"type":"user","sessionId":"external","cwd":"/tmp/external","timestamp":"2026-08-02T00:00:00Z","message":{"role":"user","content":"external"}}"#,
        )
        .unwrap();
        symlink(
            &external,
            roots.claude_projects.join("project-valid/alias.jsonl"),
        )
        .unwrap();
        symlink(&external, roots.codex_sessions.join("2026/alias.jsonl")).unwrap();
        symlink(&external, roots.omp_sessions.join("alias.jsonl")).unwrap();

        let report =
            scan_all_session_summaries_with_roots(None, SessionSourceFilter::All, &roots, &config)
                .unwrap();
        assert_eq!(report.summaries.len(), 3);
        let cache = SessionIndexCache::load(&config);
        assert!(!cache.entries.keys().any(|key| key.contains("alias.jsonl")));
    }

    #[test]
    fn scan_codex_regular_file_root_preserves_other_sources() {
        let (_temp, roots, config) = make_scan_fixture();
        fs::remove_dir_all(&roots.codex_sessions).unwrap();
        fs::write(&roots.codex_sessions, "not a directory").unwrap();

        let report =
            scan_all_session_summaries_with_roots(None, SessionSourceFilter::All, &roots, &config)
                .unwrap();

        assert_eq!(report.summaries.len(), 2);
        assert_eq!(report.diagnostics.io_errors, 1);
        assert_eq!(report.diagnostics.warnings.len(), 3);
        assert!(report
            .diagnostics
            .warnings
            .iter()
            .any(|warning| warning.source.as_deref() == Some("codex")));
    }

    #[test]
    fn scan_omp_regular_file_root_preserves_other_sources() {
        let (_temp, roots, config) = make_scan_fixture();
        fs::remove_dir_all(&roots.omp_sessions).unwrap();
        fs::write(&roots.omp_sessions, "not a directory").unwrap();

        let report =
            scan_all_session_summaries_with_roots(None, SessionSourceFilter::All, &roots, &config)
                .unwrap();

        assert_eq!(report.summaries.len(), 2);
        assert_eq!(report.diagnostics.io_errors, 1);
        assert_eq!(report.diagnostics.warnings.len(), 3);
        assert!(report
            .diagnostics
            .warnings
            .iter()
            .any(|warning| warning.source.as_deref() == Some("omp")));
    }

    #[test]
    fn walk_entry_helper_records_error_and_accepts_valid_entry() {
        let missing = tempfile::tempdir().unwrap().path().join("missing");
        let error = walkdir::WalkDir::new(&missing).into_iter().next().unwrap();
        let mut diagnostics = ScanDiagnostics::with_id("I-WALK0001");
        assert!(handle_walk_entry(error, "claude", &mut diagnostics).is_none());
        assert_eq!(diagnostics.io_errors, 1);
        assert_eq!(diagnostics.warnings.len(), 1);
        assert!(diagnostics.warnings[0].path_hash.is_some());

        let valid = tempfile::tempdir().unwrap();
        let entry = walkdir::WalkDir::new(valid.path())
            .into_iter()
            .next()
            .unwrap();
        assert!(handle_walk_entry(entry, "claude", &mut diagnostics).is_some());
    }

    #[test]
    fn candidate_metadata_error_marks_source_incomplete() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing.jsonl");
        let mut tracker = SourceScanTracker::default();
        tracker.begin("claude");
        let mut diagnostics = ScanDiagnostics::with_id("I-CANDIDATE-META");

        assert!(inspect_candidate_file(
            temp.path(),
            &missing,
            &FilterConfig::no_size_limit(),
            "claude",
            &mut tracker,
            &mut diagnostics,
        )
        .is_none());
        assert!(!tracker.retention().completed_sources.contains("claude"));
        assert_eq!(diagnostics.io_errors, 1);
    }

    #[test]
    fn candidate_fingerprint_io_error_preserves_error_kind() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("fingerprint.jsonl");
        fs::write(&path, b"regular file").unwrap();
        let mut tracker = SourceScanTracker::default();
        tracker.begin("claude");
        let mut diagnostics = ScanDiagnostics::with_id("I-CANDIDATE-FP-IO");

        crate::session_cache::set_test_fingerprint_error_path(Some(path.clone()));
        let result = inspect_candidate_file(
            temp.path(),
            &path,
            &FilterConfig::no_size_limit(),
            "claude",
            &mut tracker,
            &mut diagnostics,
        );
        crate::session_cache::set_test_fingerprint_error_path(None);

        assert!(result.is_none());
        assert_eq!(diagnostics.warnings.len(), 1);
        assert_eq!(diagnostics.warnings[0].operation, "fingerprint");
        assert_eq!(
            diagnostics.warnings[0].error_kind,
            ScanWarningErrorKind::PermissionDenied
        );
    }

    #[test]
    fn candidate_fingerprint_error_marks_source_incomplete() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("fingerprint.jsonl");
        fs::create_dir(&directory).unwrap();
        let mut tracker = SourceScanTracker::default();
        tracker.begin("claude");
        let mut diagnostics = ScanDiagnostics::with_id("I-CANDIDATE-FP");

        assert!(inspect_candidate_file(
            temp.path(),
            &directory,
            &FilterConfig::no_size_limit(),
            "claude",
            &mut tracker,
            &mut diagnostics,
        )
        .is_none());
        assert!(!tracker.retention().completed_sources.contains("claude"));
        assert_eq!(diagnostics.io_errors, 1);
        assert_eq!(diagnostics.warnings[0].operation, "metadata");
    }

    #[test]
    fn session_roots_can_be_injected() {
        let temp = tempfile::tempdir().unwrap();
        let roots = SessionRoots {
            claude_projects: temp.path().join("claude"),
            codex_sessions: temp.path().join("codex/sessions"),
            codex_history: temp.path().join("codex/history.jsonl"),
            omp_sessions: temp.path().join("omp/sessions"),
        };
        assert!(roots.claude_projects.ends_with("claude"));
    }

    #[test]
    fn test_resolve_session_by_id_returns_unique_candidate() {
        let sessions = vec![make_test_summary("id", "project", SessionSource::Claude)];
        let found = resolve_session_by_id(&sessions, "id", SessionSourceFilter::All).unwrap();
        assert_eq!(found.source, "claude");
    }

    #[test]
    fn test_resolve_session_by_id_honors_source_filter() {
        let sessions = vec![
            make_test_summary("id", "claude-project", SessionSource::Claude),
            make_test_summary("id", "codex-project", SessionSource::Codex),
        ];
        let found = resolve_session_by_id(&sessions, "id", SessionSourceFilter::Codex).unwrap();
        assert_eq!(found.source, "codex");
    }

    #[test]
    fn test_resolve_session_by_id_rejects_ambiguous_all_source() {
        let sessions = vec![
            make_test_summary("id", "claude-project", SessionSource::Claude),
            make_test_summary("id", "codex-project", SessionSource::Codex),
        ];
        let error = resolve_session_by_id(&sessions, "id", SessionSourceFilter::All)
            .unwrap_err()
            .to_string();
        assert!(error.contains("Ambiguous session ID"));
        assert!(error.contains("claude"));
        assert!(error.contains("codex"));
        assert!(error.contains("--source"));
    }

    #[test]
    fn test_resolve_session_by_id_reports_not_found() {
        let sessions = vec![make_test_summary("other", "project", SessionSource::Claude)];
        let error = resolve_session_by_id(&sessions, "missing", SessionSourceFilter::All)
            .unwrap_err()
            .to_string();
        assert_eq!(error, "Session not found: missing");
    }

    #[test]
    fn test_session_source_capabilities() {
        assert_eq!(
            SessionSource::Claude.capabilities(),
            SourceCapabilities {
                can_open: true,
                can_rename: true,
                can_delete: true,
                participates_in_sync: true,
            }
        );
        assert_eq!(
            SessionSource::Codex.capabilities(),
            SourceCapabilities {
                can_open: false,
                can_rename: false,
                can_delete: false,
                participates_in_sync: false,
            }
        );
        assert_eq!(
            SessionSource::Omp.capabilities(),
            SourceCapabilities {
                can_open: true,
                can_rename: false,
                can_delete: false,
                participates_in_sync: false,
            }
        );
    }

    #[test]
    fn test_action_choices_follow_source_capabilities() {
        assert_eq!(
            action_choices_for_source(SessionSource::Claude),
            vec![
                ActionChoice::OpenInEditor,
                ActionChoice::ViewDetails,
                ActionChoice::Rename,
                ActionChoice::Delete,
                ActionChoice::Back,
            ]
        );
        assert_eq!(
            action_choices_for_source(SessionSource::Codex),
            vec![ActionChoice::ViewDetails, ActionChoice::Back]
        );
        assert_eq!(
            action_choices_for_source(SessionSource::Omp),
            vec![
                ActionChoice::OpenInEditor,
                ActionChoice::ViewDetails,
                ActionChoice::Back,
            ]
        );
    }

    #[test]
    fn test_non_claude_sources_are_read_only_for_mutations() {
        let claude = make_test_summary("id", "project", SessionSource::Claude);
        let codex = make_test_summary("id", "project", SessionSource::Codex);
        let omp = make_test_summary("id", "project", SessionSource::Omp);

        assert!(ensure_can_rename(&claude).is_ok());
        assert!(ensure_can_delete(&claude).is_ok());
        assert!(ensure_can_rename(&codex)
            .unwrap_err()
            .to_string()
            .contains("read-only"));
        assert!(ensure_can_delete(&codex)
            .unwrap_err()
            .to_string()
            .contains("read-only"));
        assert!(ensure_can_rename(&omp)
            .unwrap_err()
            .to_string()
            .contains("read-only"));
        assert!(ensure_can_delete(&omp)
            .unwrap_err()
            .to_string()
            .contains("read-only"));
    }

    #[test]
    fn test_guarded_rename_does_not_append_for_read_only_sources() {
        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("session.jsonl");
        fs::write(&file_path, "original\n").unwrap();

        for source in [SessionSource::Codex, SessionSource::Omp] {
            let mut session = make_test_summary("id", "project", source);
            session.file_path = file_path.clone();

            let error = rename_session_with_guard(&session, "new title").unwrap_err();
            assert!(error.to_string().contains("read-only"));
            assert_eq!(fs::read_to_string(&file_path).unwrap(), "original\n");
        }
    }

    #[test]
    fn test_guarded_local_delete_preserves_read_only_session_file() {
        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("session.jsonl");
        fs::write(&file_path, "original\n").unwrap();

        for source in [SessionSource::Codex, SessionSource::Omp] {
            let mut session = make_test_summary("id", "project", source);
            session.file_path = file_path.clone();

            let error = delete_local_session(&session).unwrap_err();
            assert!(error.to_string().contains("read-only"));
            assert!(file_path.exists());
        }
    }

    #[test]
    fn test_claude_low_level_file_operations_succeed() {
        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("session.jsonl");
        fs::write(&file_path, "original\n").unwrap();
        let mut session = make_test_summary("id", "project", SessionSource::Claude);
        session.file_path = file_path.clone();

        append_custom_title_entry(&session.file_path, &session.session_id, "new title").unwrap();
        assert!(fs::read_to_string(&file_path)
            .unwrap()
            .contains("custom-title"));

        delete_session_file(&file_path).unwrap();
        assert!(!file_path.exists());
    }

    #[test]
    fn test_session_source_strings_and_labels() {
        assert_eq!(SessionSource::Claude.as_str(), "claude");
        assert_eq!(SessionSource::Codex.as_str(), "codex");
        assert_eq!(SessionSource::Omp.as_str(), "omp");
        assert_eq!(SessionSource::Claude.label(), "CC");
        assert_eq!(SessionSource::Codex.label(), "CX");
        assert_eq!(SessionSource::Omp.label(), "OM");
    }

    #[test]
    fn test_session_source_filter_includes_source() {
        assert!(SessionSourceFilter::All.includes(SessionSource::Claude));
        assert!(SessionSourceFilter::All.includes(SessionSource::Codex));
        assert!(SessionSourceFilter::All.includes(SessionSource::Omp));
        assert!(SessionSourceFilter::Claude.includes(SessionSource::Claude));
        assert!(!SessionSourceFilter::Claude.includes(SessionSource::Codex));
        assert!(SessionSourceFilter::Codex.includes(SessionSource::Codex));
        assert!(SessionSourceFilter::Omp.includes(SessionSource::Omp));
    }

    #[test]
    fn test_session_identity_includes_source() {
        let claude = SessionIdentity {
            source: SessionSource::Claude,
            session_id: "same-id".to_string(),
        };
        let codex = SessionIdentity {
            source: SessionSource::Codex,
            session_id: "same-id".to_string(),
        };
        assert_ne!(claude, codex);
    }

    #[test]
    fn test_summary_rejects_unknown_source() {
        let mut summary = make_test_summary("unknown-id", "project", SessionSource::Claude);
        summary.source = "future-source".to_string();
        let error = summary.source_kind().unwrap_err().to_string();
        assert!(error.contains("Unknown session source"));
    }

    #[test]
    fn test_summary_identity() {
        let summary = make_test_summary("codex-id", "project", SessionSource::Codex);
        let identity = summary.identity().unwrap();
        assert_eq!(identity.source, SessionSource::Codex);
        assert_eq!(identity.session_id, "codex-id");

        let mut unknown = make_test_summary("unknown-id", "project", SessionSource::Claude);
        unknown.source = "future-source".to_string();
        let error = unknown.identity().unwrap_err().to_string();
        assert!(error.contains("Unknown session source"));
    }

    #[test]
    fn test_find_around_range_middle_hit() {
        // 关键词命中中后段 -> 返回 pos 前后 num 条的区间
        let msgs = vec![
            make_msg(1, "开头"),
            make_msg(2, "无关"),
            make_msg(3, "这里有 NonFastForward 符号"),
            make_msg(4, "无关"),
            make_msg(5, "结尾"),
        ];
        // pos = 2 (0-based), num = 1 -> (1, 4)
        assert_eq!(find_around_range(&msgs, "NonFastForward", 1), Some((1, 4)));
    }

    #[test]
    fn test_find_around_range_case_insensitive() {
        let msgs = vec![make_msg(1, "包含 KEYWORD 大写")];
        assert_eq!(find_around_range(&msgs, "keyword", 3), Some((0, 1)));
    }

    #[test]
    fn test_find_around_range_not_found_returns_none() {
        // 关键失败：不再回退到 0，而是返回 None（由调用方提示"未找到"）
        let msgs = vec![make_msg(1, "a"), make_msg(2, "b")];
        assert_eq!(find_around_range(&msgs, "缺失的词", 5), None);
    }

    #[test]
    fn test_find_around_range_boundary_saturating() {
        let msgs = vec![
            make_msg(1, "hit-here"),
            make_msg(2, "x"),
            make_msg(3, "tail-hit"),
        ];
        // 首条命中：start 不下溢
        assert_eq!(find_around_range(&msgs, "hit-here", 2), Some((0, 3)));
        // 末条命中：end 不越界（min total）
        assert_eq!(find_around_range(&msgs, "tail-hit", 5), Some((0, 3)));
    }

    #[test]
    fn test_format_relative_time() {
        // Test with a known timestamp
        let now = chrono::Utc::now();
        let timestamp = now.to_rfc3339();
        let result = format_relative_time(&timestamp);
        assert!(result.contains("ago") || result == "Just now");
    }

    #[test]
    fn test_display_title_truncation() {
        let session = SessionSummary {
            source: "claude".to_string(),
            session_id: "test".to_string(),
            title: "This is a very long title that should be truncated".to_string(),
            project_name: "test".to_string(),
            project_dir: PathBuf::new(),
            file_path: PathBuf::new(),
            message_count: 0,
            user_message_count: 0,
            assistant_message_count: 0,
            first_timestamp: None,
            last_activity: None,
            file_size: 0,
        };

        let short = session.display_title(20);
        assert!(short.chars().count() <= 20);
        assert!(short.ends_with("..."));
    }

    #[test]
    fn test_display_title_unicode() {
        let session = SessionSummary {
            source: "claude".to_string(),
            session_id: "test".to_string(),
            title: "这是一个很长的中文标题需要截断".to_string(),
            project_name: "test".to_string(),
            project_dir: PathBuf::new(),
            file_path: PathBuf::new(),
            message_count: 0,
            user_message_count: 0,
            assistant_message_count: 0,
            first_timestamp: None,
            last_activity: None,
            file_size: 0,
        };

        let short = session.display_title(10);
        assert!(short.chars().count() <= 10);
        assert!(short.ends_with("..."));
    }

    #[test]
    fn test_codex_session_uses_cwd_as_project_dir() {
        let session = CodexSession {
            session_id: "test".to_string(),
            entries: Vec::new(),
            file_path: PathBuf::from("/tmp/codex/sessions/session.jsonl"),
            cwd: Some("/tmp/demo-project".to_string()),
        };

        let summary = SessionSummary::from_codex_session(&session, "demo-project", "Demo".into());
        assert_eq!(summary.project_dir, PathBuf::from("/tmp/demo-project"));
    }

    #[test]
    fn test_memory_dir_name_by_source() {
        assert_eq!(memory_dir_name_for_source("claude"), "memory");
        assert_eq!(memory_dir_name_for_source("codex"), ".memory");
    }

    #[test]
    fn test_parse_duration_filter_days() {
        let cutoff = parse_duration_filter("7d").unwrap();
        let expected = chrono::Utc::now() - chrono::Duration::days(7);
        assert!((cutoff - expected).num_seconds().abs() < 2);
    }

    #[test]
    fn test_parse_duration_filter_hours() {
        let cutoff = parse_duration_filter("3h").unwrap();
        let expected = chrono::Utc::now() - chrono::Duration::hours(3);
        assert!((cutoff - expected).num_seconds().abs() < 2);
    }

    #[test]
    fn test_parse_duration_filter_weeks() {
        let cutoff = parse_duration_filter("2w").unwrap();
        let expected = chrono::Utc::now() - chrono::Duration::weeks(2);
        assert!((cutoff - expected).num_seconds().abs() < 2);
    }

    #[test]
    fn test_parse_duration_filter_minutes() {
        let cutoff = parse_duration_filter("30m").unwrap();
        let expected = chrono::Utc::now() - chrono::Duration::minutes(30);
        assert!((cutoff - expected).num_seconds().abs() < 2);
    }

    #[test]
    fn test_parse_duration_filter_invalid() {
        assert!(parse_duration_filter("abc").is_err());
        assert!(parse_duration_filter("3x").is_err());
        assert!(parse_duration_filter("d").is_err());
    }

    #[test]
    fn test_calculate_recency_score_now() {
        let now = chrono::Utc::now().to_rfc3339();
        let score = calculate_recency_score(Some(&now));
        assert!(score > 0.95);
    }

    #[test]
    fn test_calculate_recency_score_week_ago() {
        let week_ago = (chrono::Utc::now() - chrono::Duration::days(7)).to_rfc3339();
        let score = calculate_recency_score(Some(&week_ago));
        assert!(score > 0.4 && score < 0.6, "score was {}", score);
    }

    #[test]
    fn test_calculate_recency_score_none() {
        assert_eq!(calculate_recency_score(None), 0.0);
    }

    #[test]
    fn test_format_compact_relative_time_now() {
        let now = chrono::Utc::now().to_rfc3339();
        assert_eq!(format_compact_relative_time(&now), "now");
    }

    #[test]
    fn test_format_compact_relative_time_hours() {
        let ts = (chrono::Utc::now() - chrono::Duration::hours(3)).to_rfc3339();
        assert_eq!(format_compact_relative_time(&ts), "3h ago");
    }

    #[test]
    fn test_format_compact_relative_time_days() {
        let ts = (chrono::Utc::now() - chrono::Duration::days(5)).to_rfc3339();
        assert_eq!(format_compact_relative_time(&ts), "5d ago");
    }

    #[test]
    fn test_cleanup_source_policy_and_menu_options() {
        assert!(cleanup_available(SessionSourceFilter::All));
        assert!(cleanup_available(SessionSourceFilter::Claude));
        assert!(!cleanup_available(SessionSourceFilter::Codex));
        assert!(!cleanup_available(SessionSourceFilter::Omp));

        let sessions = vec![make_test_summary("id", "project", SessionSource::Codex)];
        let options = build_session_menu_options(&sessions, 2, false);
        assert!(!options.iter().any(|option| option.starts_with("Cleanup")));

        let options = build_session_menu_options(&sessions, 2, true);
        assert!(options.iter().any(|option| option.starts_with("Cleanup")));
    }

    #[test]
    fn project_name_only_relative_path_rejects_dotdot_project_name() {
        let relative = crate::path_security::safe_project_relative_path(
            "..",
            std::ffi::OsStr::new("session.jsonl"),
        );
        assert!(relative.is_err());
    }

    #[test]
    fn restore_copies_remote_session_into_local_projects_root() {
        let temp = tempfile::tempdir().unwrap();
        let sync_repo = temp.path().join("remote");
        let remote_projects = sync_repo.join("projects");
        let local_projects = temp.path().join("home/.claude/projects");
        let remote_project = remote_projects.join("demo");
        let local_project = local_projects.join("demo");
        fs::create_dir_all(&remote_project).unwrap();
        fs::create_dir_all(&local_project).unwrap();
        let remote_file = remote_project.join("session.jsonl");
        fs::write(&remote_file, b"remote-session\n").unwrap();

        let mut target = make_test_summary("restore-id", "demo", SessionSource::Claude);
        target.file_path = local_project.join("session.jsonl");
        let filter = FilterConfig {
            use_project_name_only: true,
            ..FilterConfig::default()
        };

        do_restore(
            &target,
            &remote_projects,
            &local_projects,
            &filter,
            &sync_repo,
        )
        .unwrap();

        assert_eq!(
            fs::read_to_string(target.file_path).unwrap(),
            "remote-session\n"
        );
    }

    #[test]
    fn restore_rejects_remote_project_traversal() {
        let temp = tempfile::tempdir().unwrap();
        let sync_repo = temp.path().join("remote");
        let remote_projects = sync_repo.join("projects");
        let local_projects = temp.path().join("home/.claude/projects");
        fs::create_dir_all(&remote_projects).unwrap();
        fs::create_dir_all(&local_projects).unwrap();
        let mut target = make_test_summary("restore-id", "..", SessionSource::Claude);
        target.file_path = local_projects.join("safe/session.jsonl");
        let filter = FilterConfig {
            use_project_name_only: true,
            ..FilterConfig::default()
        };

        let error = do_restore(
            &target,
            &remote_projects,
            &local_projects,
            &filter,
            &sync_repo,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("project") || error.contains("path"));
    }

    #[cfg(unix)]
    #[test]
    fn restore_rejects_remote_and_destination_symlink_escape() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let sync_repo = temp.path().join("remote");
        let remote_projects = sync_repo.join("projects");
        let local_projects = temp.path().join("home/.claude/projects");
        let outside_source = temp.path().join("outside-source.jsonl");
        let outside_destination = temp.path().join("outside-destination.jsonl");
        fs::create_dir_all(remote_projects.join("demo")).unwrap();
        fs::create_dir_all(local_projects.join("demo")).unwrap();
        fs::write(&outside_source, b"secret\n").unwrap();
        fs::write(&outside_destination, b"keep\n").unwrap();

        let mut target = make_test_summary("restore-id", "demo", SessionSource::Claude);
        target.file_path = local_projects.join("demo/session.jsonl");
        let filter = FilterConfig {
            use_project_name_only: true,
            ..FilterConfig::default()
        };

        symlink(&outside_source, remote_projects.join("demo/session.jsonl")).unwrap();
        assert!(do_restore(
            &target,
            &remote_projects,
            &local_projects,
            &filter,
            &sync_repo,
        )
        .is_err());

        fs::remove_file(remote_projects.join("demo/session.jsonl")).unwrap();
        fs::write(remote_projects.join("demo/session.jsonl"), b"safe\n").unwrap();
        symlink(&outside_destination, &target.file_path).unwrap();
        assert!(do_restore(
            &target,
            &remote_projects,
            &local_projects,
            &filter,
            &sync_repo,
        )
        .is_err());
        assert_eq!(fs::read_to_string(outside_destination).unwrap(), "keep\n");
    }

    #[cfg(unix)]
    #[test]
    fn restore_rejects_projects_root_symlink_without_reading_external_file() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let sync_repo = temp.path().join("remote");
        let remote_projects = sync_repo.join("projects");
        let outside = temp.path().join("outside");
        let local_projects = temp.path().join("home/.claude/projects");
        fs::create_dir_all(&sync_repo).unwrap();
        fs::create_dir_all(outside.join("demo")).unwrap();
        fs::create_dir_all(&local_projects).unwrap();
        fs::write(outside.join("demo/session.jsonl"), b"secret\n").unwrap();
        symlink(&outside, &remote_projects).unwrap();

        let mut target = make_test_summary("restore-id", "demo", SessionSource::Claude);
        target.file_path = local_projects.join("demo/session.jsonl");
        let filter = FilterConfig {
            use_project_name_only: true,
            ..FilterConfig::default()
        };
        assert!(do_restore(
            &target,
            &remote_projects,
            &local_projects,
            &filter,
            &sync_repo,
        )
        .is_err());
        assert!(!target.file_path.exists());
    }

    #[test]
    fn test_restore_source_guard_rejects_non_claude_before_io() {
        for source in [SessionSourceFilter::Codex, SessionSourceFilter::Omp] {
            let error = handle_session_restore_with_source(Some("does-not-matter"), source)
                .unwrap_err()
                .to_string();
            assert!(error.contains("Restore is only supported for Claude sessions"));
        }

        assert!(ensure_restore_source_supported(SessionSourceFilter::All).is_ok());
        assert!(ensure_restore_source_supported(SessionSourceFilter::Claude).is_ok());
    }

    #[test]
    fn test_raw_session_paths_are_contained_by_injected_root() {
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path().join("projects");
        let project = root.join("project");
        fs::create_dir_all(&project).unwrap();
        let inside = project.join("session.jsonl");
        fs::write(&inside, "session\n").unwrap();
        assert!(ensure_path_within_root(&inside, &root).is_ok());

        let outside = temp_dir.path().join("outside.jsonl");
        fs::write(&outside, "outside\n").unwrap();
        let error = ensure_path_within_root(&outside, &root)
            .unwrap_err()
            .to_string();
        assert!(error.contains("Raw session mutation is only allowed inside Claude projects"));

        let traversal = project.join("..").join("project").join("session.jsonl");
        assert!(ensure_path_within_root(&traversal, &root).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn test_raw_session_paths_reject_symlink_escape() {
        use std::os::unix::fs::symlink;

        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path().join("projects");
        fs::create_dir_all(&root).unwrap();
        let outside = temp_dir.path().join("outside.jsonl");
        fs::write(&outside, "outside\n").unwrap();
        let link = root.join("escaped.jsonl");
        symlink(&outside, &link).unwrap();

        let error = ensure_path_within_root(&link, &root)
            .unwrap_err()
            .to_string();
        assert!(error.contains("Raw session mutation is only allowed inside Claude projects"));
    }

    #[test]
    fn test_raw_session_paths_reject_parent_escape_after_canonicalize() {
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path().join("projects");
        fs::create_dir_all(root.join("project")).unwrap();
        let outside = temp_dir.path().join("outside.jsonl");
        fs::write(&outside, "outside\n").unwrap();
        let escaped = root
            .join("project")
            .join("..")
            .join("..")
            .join("outside.jsonl");

        let error = ensure_path_within_root(&escaped, &root)
            .unwrap_err()
            .to_string();
        assert!(error.contains("Raw session mutation is only allowed inside Claude projects"));
    }

    #[test]
    fn test_public_delete_session_with_commit_rejects_external_claude_file_before_io() {
        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("external-session.jsonl");
        fs::write(&file_path, "original\n").unwrap();
        let mut session = make_test_summary("external-id", "project", SessionSource::Claude);
        session.file_path = file_path.clone();

        let error = delete_session_with_commit(&session, DeleteReason::Explicit)
            .unwrap_err()
            .to_string();
        assert!(error.contains("Raw session mutation is only allowed inside Claude projects"));
        assert_eq!(fs::read_to_string(&file_path).unwrap(), "original\n");
    }

    #[test]
    fn test_public_raw_mutation_wrappers_reject_external_paths() {
        let temp_dir = tempfile::tempdir().unwrap();
        let rename_path = temp_dir.path().join("rename-session.jsonl");
        fs::write(&rename_path, "original\n").unwrap();
        let delete_path = temp_dir.path().join("delete-session.jsonl");
        fs::write(&delete_path, "original\n").unwrap();

        #[allow(deprecated)]
        {
            let rename_error = rename_session(&rename_path, "id", "new title")
                .unwrap_err()
                .to_string();
            assert!(rename_error
                .contains("Raw session mutation is only allowed inside Claude projects"));

            let delete_error = delete_session(&delete_path).unwrap_err().to_string();
            assert!(delete_error
                .contains("Raw session mutation is only allowed inside Claude projects"));
        }

        assert_eq!(fs::read_to_string(&rename_path).unwrap(), "original\n");
        assert_eq!(fs::read_to_string(&delete_path).unwrap(), "original\n");
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn delete_session_rejects_sync_projects_root_symlink_before_local_delete() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let local_file = home.join(".claude/projects/project/session.jsonl");
        fs::create_dir_all(local_file.parent().unwrap()).unwrap();
        fs::write(&local_file, b"local").unwrap();
        let config = temp.path().join("config");
        fs::create_dir_all(&config).unwrap();
        let repo = temp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        let state = SyncState {
            sync_repo_path: repo.clone(),
            has_remote: false,
            is_cloned_repo: false,
            last_synced_commit: None,
        };
        fs::write(
            config.join("state.json"),
            serde_json::to_vec(&state).unwrap(),
        )
        .unwrap();
        let outside = temp.path().join("outside");
        let outside_file = outside.join("project/session.jsonl");
        fs::create_dir_all(outside_file.parent().unwrap()).unwrap();
        fs::write(&outside_file, b"external").unwrap();
        symlink(&outside, repo.join("projects")).unwrap();

        let old_home = std::env::var_os("HOME");
        let old_userprofile = std::env::var_os("USERPROFILE");
        let old_config = std::env::var_os(crate::config::CONFIG_DIR_ENV);
        std::env::set_var("HOME", &home);
        std::env::set_var("USERPROFILE", &home);
        std::env::set_var(crate::config::CONFIG_DIR_ENV, &config);
        struct EnvGuard {
            home: Option<std::ffi::OsString>,
            userprofile: Option<std::ffi::OsString>,
            config: Option<std::ffi::OsString>,
        }
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                match self.home.take() {
                    Some(value) => std::env::set_var("HOME", value),
                    None => std::env::remove_var("HOME"),
                }
                match self.userprofile.take() {
                    Some(value) => std::env::set_var("USERPROFILE", value),
                    None => std::env::remove_var("USERPROFILE"),
                }
                match self.config.take() {
                    Some(value) => std::env::set_var(crate::config::CONFIG_DIR_ENV, value),
                    None => std::env::remove_var(crate::config::CONFIG_DIR_ENV),
                }
            }
        }
        let _guard = EnvGuard {
            home: old_home,
            userprofile: old_userprofile,
            config: old_config,
        };

        let mut session = make_test_summary("delete-root-link", "project", SessionSource::Claude);
        session.file_path = local_file.clone();
        let error = delete_session_with_commit(&session, DeleteReason::Explicit).unwrap_err();

        assert!(error.to_string().contains("sync projects root"));
        assert_eq!(fs::read(&local_file).unwrap(), b"local");
        assert_eq!(fs::read(&outside_file).unwrap(), b"external");
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn batch_cleanup_rejects_sync_projects_root_symlink_before_local_delete() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let local_file = home.join(".claude/projects/project/session.jsonl");
        fs::create_dir_all(local_file.parent().unwrap()).unwrap();
        fs::write(&local_file, b"local").unwrap();
        let repo = temp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        let outside = temp.path().join("outside");
        let outside_file = outside.join("project/session.jsonl");
        fs::create_dir_all(outside_file.parent().unwrap()).unwrap();
        fs::write(&outside_file, b"external").unwrap();
        symlink(&outside, repo.join("projects")).unwrap();

        let old_home = std::env::var_os("HOME");
        let old_userprofile = std::env::var_os("USERPROFILE");
        std::env::set_var("HOME", &home);
        std::env::set_var("USERPROFILE", &home);
        struct EnvGuard {
            home: Option<std::ffi::OsString>,
            userprofile: Option<std::ffi::OsString>,
        }
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                match self.home.take() {
                    Some(value) => std::env::set_var("HOME", value),
                    None => std::env::remove_var("HOME"),
                }
                match self.userprofile.take() {
                    Some(value) => std::env::set_var("USERPROFILE", value),
                    None => std::env::remove_var("USERPROFILE"),
                }
            }
        }
        let _guard = EnvGuard {
            home: old_home,
            userprofile: old_userprofile,
        };

        let state = SyncState {
            sync_repo_path: repo,
            has_remote: false,
            is_cloned_repo: false,
            last_synced_commit: None,
        };
        let mut session = make_test_summary("batch-root-link", "project", SessionSource::Claude);
        session.file_path = local_file.clone();
        let error = remove_session_for_batch(
            &session,
            DeleteReason::Cleanup,
            &FilterConfig::default(),
            &state,
        )
        .unwrap_err();

        assert!(error.to_string().contains("sync projects root"));
        assert_eq!(fs::read(&local_file).unwrap(), b"local");
        assert_eq!(fs::read(&outside_file).unwrap(), b"external");
    }

    #[test]
    fn test_summary_rename_guard_rejects_external_claude_file() {
        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("external-session.jsonl");
        fs::write(&file_path, "original\n").unwrap();
        let mut session = make_test_summary("external-id", "project", SessionSource::Claude);
        session.file_path = file_path.clone();

        let error = rename_session_with_guard(&session, "new title")
            .unwrap_err()
            .to_string();
        assert!(error.contains("Raw session mutation is only allowed inside Claude projects"));
        assert_eq!(fs::read_to_string(&file_path).unwrap(), "original\n");
    }

    #[test]
    fn test_source_aware_and_legacy_public_api_signatures() {
        let _: fn(&str, &str, SessionSourceFilter) -> Result<()> =
            handle_session_rename_with_source;
        let _: fn(&str, bool, SessionSourceFilter) -> Result<()> =
            handle_session_delete_with_source;
        let _: fn(Option<&str>, SessionSourceFilter) -> Result<()> =
            handle_session_restore_with_source;
        #[allow(deprecated)]
        {
            let _: fn(&Path, &str, &str) -> Result<()> = rename_session;
            let _: fn(&Path) -> Result<()> = delete_session;
            let _: fn(&str, &str) -> Result<()> = handle_session_rename;
            let _: fn(&str, bool) -> Result<()> = handle_session_delete;
            let _: fn(Option<&str>) -> Result<()> = handle_session_restore;
        }
    }

    fn make_scan_fixture() -> (tempfile::TempDir, SessionRoots, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let claude_project = temp.path().join("claude/project-valid");
        let codex_sessions = temp.path().join("codex/sessions/2026");
        let omp_sessions = temp.path().join("omp/sessions");
        fs::create_dir_all(&claude_project).unwrap();
        fs::create_dir_all(&codex_sessions).unwrap();
        fs::create_dir_all(&omp_sessions).unwrap();

        fs::write(
            claude_project.join("valid.jsonl"),
            r#"{"type":"user","sessionId":"cc-1","cwd":"/tmp/demo","timestamp":"2026-08-02T00:00:00Z","message":{"role":"user","content":"hello"}}"#,
        )
        .unwrap();
        fs::write(claude_project.join("broken.jsonl"), [0xff, 0xfe, 0xfd]).unwrap();

        fs::write(
            codex_sessions.join("valid.jsonl"),
            concat!(
                r#"{"timestamp":"2026-08-02T00:00:00Z","type":"session_meta","payload":{"id":"cx-1","cwd":"/tmp/demo"}}"#, "\n",
                r#"{"timestamp":"2026-08-02T00:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]}}"#,
            ),
        )
        .unwrap();
        fs::write(codex_sessions.join("broken.jsonl"), [0xff, 0xfe, 0xfd]).unwrap();
        fs::write(
            temp.path().join("codex/history.jsonl"),
            r#"{"session_id":"cx-1","ts":1,"text":"history title"}"#,
        )
        .unwrap();

        fs::write(
            omp_sessions.join("2026-08-02T00-00-00Z_om-1.jsonl"),
            concat!(
                r#"{"type":"session","version":3,"id":"om-1","timestamp":"2026-08-02T00:00:00Z","cwd":"/tmp/demo","title":"OMP"}"#, "\n",
                r#"{"type":"message","timestamp":"2026-08-02T00:00:01Z","message":{"role":"user","content":[{"type":"text","text":"hello"}]}}"#,
            ),
        )
        .unwrap();
        fs::write(omp_sessions.join("broken.jsonl"), [0xff, 0xfe, 0xfd]).unwrap();

        let roots = SessionRoots {
            claude_projects: temp.path().join("claude"),
            codex_sessions: temp.path().join("codex/sessions"),
            codex_history: temp.path().join("codex/history.jsonl"),
            omp_sessions,
        };
        let config = temp.path().join("config");
        (temp, roots, config)
    }

    #[test]
    fn missing_candidate_metadata_warning_preserves_not_found_kind() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing.jsonl");
        let mut tracker = SourceScanTracker::default();
        let mut diagnostics = ScanDiagnostics::with_id("I-META-NOTFOUND");

        assert!(inspect_candidate_file(
            temp.path(),
            &missing,
            &FilterConfig::no_size_limit(),
            "claude",
            &mut tracker,
            &mut diagnostics,
        )
        .is_none());
        assert_eq!(diagnostics.warnings.len(), 1);
        assert_eq!(
            diagnostics.warnings[0].error_kind,
            ScanWarningErrorKind::NotFound
        );
    }

    #[test]
    fn walk_entry_error_preserves_not_found_kind() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing");
        let error = walkdir::WalkDir::new(&missing)
            .into_iter()
            .next()
            .expect("missing root should yield an error");
        let mut diagnostics = ScanDiagnostics::with_id("I-WALK-NOTFOUND");

        assert!(handle_walk_entry(error, "claude", &mut diagnostics).is_none());
        assert_eq!(diagnostics.warnings.len(), 1);
        assert_eq!(
            diagnostics.warnings[0].error_kind,
            ScanWarningErrorKind::NotFound
        );
    }

    #[test]
    fn codex_history_read_error_preserves_error_chain_kind() {
        let (temp, roots, config) = make_scan_fixture();
        let history_dir = temp.path().join("codex/history-dir");
        fs::create_dir(&history_dir).unwrap();
        let roots = SessionRoots {
            codex_history: history_dir,
            ..roots
        };

        let report = scan_all_session_summaries_with_roots(
            None,
            SessionSourceFilter::Codex,
            &roots,
            &config,
        )
        .unwrap();
        assert!(report.diagnostics.warnings.iter().any(|warning| {
            warning.operation == "read" && warning.error_kind == ScanWarningErrorKind::ReadFailed
        }));
    }

    #[test]
    fn scanner_surfaces_cache_revalidation_error_without_losing_summary() {
        let (temp, roots, config) = make_scan_fixture();
        scan_all_session_summaries_with_roots(None, SessionSourceFilter::Claude, &roots, &config)
            .unwrap();
        let path = temp.path().join("claude/project-valid/valid.jsonl");
        let key = canonical_utf8_key(&path).unwrap();
        fs::write(
            &path,
            r#"{"type":"user","sessionId":"cc-1","cwd":"/tmp/demo","timestamp":"2026-08-02T00:02:00Z","message":{"role":"user","content":"changed"}}"#,
        )
        .unwrap();

        crate::session_cache::set_test_revalidation_error_path(Some(PathBuf::from(&key)));
        let report = scan_all_session_summaries_with_roots(
            None,
            SessionSourceFilter::Claude,
            &roots,
            &config,
        )
        .unwrap();
        crate::session_cache::set_test_revalidation_error_path(None);

        assert!(!report.summaries.is_empty());
        assert!(report.diagnostics.cache_errors >= 1);
        assert!(report.diagnostics.warnings.iter().any(|warning| {
            warning.category == ScanWarningCategory::Cache
                && warning.error_kind == ScanWarningErrorKind::PermissionDenied
        }));
        assert!(SessionIndexCache::load(&config).entries.contains_key(&key));
    }

    #[test]
    fn scan_warning_message_is_none_for_clean_diagnostics() {
        let diagnostics = ScanDiagnostics::with_id("I-CLEAN001");
        assert_eq!(scan_warning_message(&diagnostics), None);
    }

    #[test]
    fn interactive_scan_warning_writer_only_emits_for_degraded_reports() {
        let clean = ScanDiagnostics::with_id("I-CLEAN001");
        let mut clean_messages = Vec::new();
        emit_scan_warning_to(&clean, |message| clean_messages.push(message.to_string()));
        assert!(clean_messages.is_empty());

        let mut degraded = ScanDiagnostics::with_id("I-DEGRADED1");
        degraded.record_warning(
            Some("claude"),
            "parse",
            ScanWarningCategory::Data,
            Some(Path::new("/private/fixture/broken.jsonl")),
            "invalid JSON",
        );
        let mut degraded_messages = Vec::new();
        emit_scan_warning_to(&degraded, |message| {
            degraded_messages.push(message.to_string())
        });
        assert_eq!(degraded_messages.len(), 1);
        assert!(degraded_messages[0].contains("I-DEGRADED1"));
        assert!(!degraded_messages[0].contains("/private/fixture"));
    }

    #[test]
    fn mutation_scan_aborts_when_diagnostics_are_degraded() {
        let clean = SessionScanResult {
            summaries: Vec::new(),
            diagnostics: ScanDiagnostics::with_id("I-CLEAN001"),
        };
        assert!(scan_summaries_for_mutation(clean).is_ok());

        let mut diagnostics = ScanDiagnostics::with_id("I-MUTATE01");
        diagnostics.record_warning(
            Some("claude"),
            "read",
            ScanWarningCategory::Io,
            None,
            "fixture failure",
        );
        let degraded = SessionScanResult {
            summaries: Vec::new(),
            diagnostics,
        };
        let error = scan_summaries_for_mutation(degraded).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("mutation aborted"));
        assert!(message.contains("I-MUTATE01"));
    }

    #[test]
    fn parser_io_errors_after_fingerprint_preserve_old_cache_for_all_sources() {
        let cases = [
            (
                "claude",
                SessionSourceFilter::Claude,
                PathBuf::from("claude/project-valid/valid.jsonl"),
            ),
            (
                "codex",
                SessionSourceFilter::Codex,
                PathBuf::from("codex/sessions/2026/valid.jsonl"),
            ),
            (
                "omp",
                SessionSourceFilter::Omp,
                PathBuf::from("omp/sessions/2026-08-02T00-00-00Z_om-1.jsonl"),
            ),
        ];

        for (source, filter, relative_path) in cases {
            let (temp, roots, config) = make_scan_fixture();
            scan_all_session_summaries_with_roots(None, filter, &roots, &config).unwrap();
            let path = temp.path().join(relative_path);
            let key = canonical_utf8_key(&path).unwrap();
            assert!(SessionIndexCache::load(&config).entries.contains_key(&key));

            fs::write(&path, b"changed after initial cache").unwrap();
            set_test_remove_before_parse(Some(path));
            let report =
                scan_all_session_summaries_with_roots(None, filter, &roots, &config).unwrap();

            assert!(report.summaries.is_empty(), "source={source}");
            assert_eq!(report.diagnostics.io_errors, 1, "source={source}");
            assert!(report
                .diagnostics
                .warnings
                .iter()
                .any(|warning| warning.error_kind == ScanWarningErrorKind::NotFound));
            assert!(SessionIndexCache::load(&config).entries.contains_key(&key));
        }
    }

    #[test]
    fn parser_io_errors_are_incomplete_and_do_not_schedule_cache_removal() {
        for source in ["claude", "codex", "omp"] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join(format!("{source}.jsonl"));
            let candidate = CandidateFile {
                path_key: Some(path.to_string_lossy().to_string()),
                file_size: 1,
                mtime_secs: 1,
                content_fingerprint: "fingerprint".to_string(),
            };
            let mut tracker = SourceScanTracker::default();
            tracker.begin(source);
            let mut delta = CacheDelta::default();
            let mut diagnostics = ScanDiagnostics::with_id(format!("I-PARSER-{source}"));
            let error = anyhow::Error::new(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "read failed with private path",
            ));

            handle_parser_error(
                source,
                &path,
                &candidate,
                &error,
                &mut tracker,
                &mut delta,
                &mut diagnostics,
            );

            assert!(delta.removals.is_empty(), "source={source}");
            assert!(!tracker.retention().completed_sources.contains(source));
            assert_eq!(diagnostics.warnings[0].category, ScanWarningCategory::Io);
            assert_eq!(
                diagnostics.warnings[0].error_kind,
                ScanWarningErrorKind::ReadFailed
            );
        }
    }

    #[test]
    fn scan_warning_message_is_aggregated_and_path_free_when_degraded() {
        let mut diagnostics = ScanDiagnostics::with_id("I-DEGRADED1");
        diagnostics.record_warning(
            Some("claude"),
            "parse",
            ScanWarningCategory::Data,
            Some(Path::new("/private/fixture/broken.jsonl")),
            "invalid JSON",
        );

        let message = scan_warning_message(&diagnostics).expect("degraded scan should warn");
        assert_eq!(
            message,
            "Session scan incomplete: 1 malformed, 0 I/O, 0 cache error. Diagnostic ID: I-DEGRADED1"
        );
        assert!(!message.contains("/private/fixture"));
    }

    #[test]
    fn attach_scan_diagnostics_preserves_business_fields_and_adds_contract() {
        let mut diagnostics = ScanDiagnostics::with_id("I-JSON0001");
        diagnostics.files_seen = 4;
        diagnostics.record_warning(
            Some("claude"),
            "parse",
            ScanWarningCategory::Data,
            None,
            "invalid JSON",
        );
        let payload = json!({
            "query": "needle",
            "session_results": [{"session_id": "session-1"}]
        });

        let attached = attach_scan_diagnostics(payload, &diagnostics);
        assert_eq!(attached["query"], "needle");
        assert_eq!(attached["session_results"][0]["session_id"], "session-1");
        assert_eq!(attached["schema_version"], 1);
        assert_eq!(attached["diagnostics"]["diagnostic_id"], "I-JSON0001");
        assert_eq!(attached["diagnostics"]["files_seen"], 4);
        assert_eq!(attached["diagnostics"]["malformed_files"], 1);
        assert_eq!(attached["diagnostics"]["warnings"][0]["category"], "data");
    }

    #[test]
    fn scan_report_counts_three_sources_and_malformed_files() {
        let (_temp, roots, config) = make_scan_fixture();
        let report =
            scan_all_session_summaries_with_roots(None, SessionSourceFilter::All, &roots, &config)
                .unwrap();

        assert_eq!(report.summaries.len(), 3);
        assert_eq!(report.diagnostics.files_seen, 6);
        assert_eq!(report.diagnostics.files_parsed, 3);
        assert_eq!(report.diagnostics.cache_misses, 6);
        assert_eq!(report.diagnostics.malformed_files, 3);
        assert!(report.diagnostics.bytes_considered > 0);
        assert!(report.diagnostics.elapsed_ms <= 60_000);
        assert!(report.diagnostics.degraded());
    }

    #[test]
    fn scan_report_uses_cache_only_for_successfully_parsed_files() {
        let (_temp, roots, config) = make_scan_fixture();
        let first =
            scan_all_session_summaries_with_roots(None, SessionSourceFilter::All, &roots, &config)
                .unwrap();
        assert_eq!(first.summaries.len(), 3);
        assert!(first.diagnostics.parsed_bytes >= 9);

        let second =
            scan_all_session_summaries_with_roots(None, SessionSourceFilter::All, &roots, &config)
                .unwrap();
        assert_eq!(second.summaries.len(), 3);
        assert_eq!(second.diagnostics.cache_hits, 3);
        assert_eq!(second.diagnostics.cache_misses, 3);
        assert_eq!(second.diagnostics.malformed_files, 3);
        assert_eq!(second.diagnostics.parsed_bytes, 9);
    }

    #[test]
    fn scan_report_reparses_partial_sessions_and_evicts_all_sources() {
        let temp = tempfile::tempdir().unwrap();
        let claude_project = temp.path().join("claude/project");
        let codex_root = temp.path().join("codex/sessions");
        let omp_root = temp.path().join("omp/sessions");
        fs::create_dir_all(&claude_project).unwrap();
        fs::create_dir_all(&codex_root).unwrap();
        fs::create_dir_all(&omp_root).unwrap();

        fs::write(
            claude_project.join("partial.jsonl"),
            concat!(
                "{malformed claude line\n",
                r#"{"type":"user","sessionId":"cc-partial","cwd":"/tmp/demo","timestamp":"2026-08-02T00:00:00Z","message":{"role":"user","content":"hello"}}"#,
                "\n"
            ),
        )
        .unwrap();
        fs::write(
            codex_root.join("partial.jsonl"),
            concat!(
                "{malformed codex line\n",
                r#"{"type":"session_meta","payload":{"id":"cx-partial","cwd":"/tmp/demo"}}"#,
                "\n",
                r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]}}"#,
                "\n"
            ),
        )
        .unwrap();
        fs::write(
            omp_root.join("partial_om-partial.jsonl"),
            concat!(
                "{malformed omp line\n",
                r#"{"type":"session","id":"om-partial","cwd":"/tmp/demo","title":"OMP"}"#,
                "\n",
                r#"{"type":"message","message":{"role":"user","content":[{"type":"text","text":"hello"}]}}"#,
                "\n"
            ),
        )
        .unwrap();
        fs::write(
            temp.path().join("codex/history.jsonl"),
            r#"{"session_id":"cx-partial","ts":1,"text":"Codex"}"#,
        )
        .unwrap();

        let roots = SessionRoots {
            claude_projects: temp.path().join("claude"),
            codex_sessions: codex_root,
            codex_history: temp.path().join("codex/history.jsonl"),
            omp_sessions: omp_root,
        };
        let config = temp.path().join("config");

        let first =
            scan_all_session_summaries_with_roots(None, SessionSourceFilter::All, &roots, &config)
                .unwrap();
        assert_eq!(first.summaries.len(), 3);
        assert_eq!(first.diagnostics.files_parsed, 3);
        assert_eq!(first.diagnostics.malformed_files, 3);
        assert_eq!(first.diagnostics.cache_hits, 0);
        assert_eq!(first.diagnostics.cache_misses, 3);
        assert!(first.diagnostics.degraded());
        let cache = SessionIndexCache::load(&config);
        assert!(cache.entries.is_empty());

        let second =
            scan_all_session_summaries_with_roots(None, SessionSourceFilter::All, &roots, &config)
                .unwrap();
        assert_eq!(second.summaries.len(), 3);
        assert_eq!(second.diagnostics.files_parsed, 3);
        assert_eq!(second.diagnostics.malformed_files, 3);
        assert_eq!(second.diagnostics.cache_hits, 0);
        assert_eq!(second.diagnostics.cache_misses, 3);
        assert!(SessionIndexCache::load(&config).entries.is_empty());
    }

    #[test]
    fn scan_report_evicts_existing_clean_entries_when_files_become_partial() {
        let (_temp, roots, config) = make_scan_fixture();
        let claude_file = roots.claude_projects.join("project-valid/valid.jsonl");
        let codex_file = roots.codex_sessions.join("2026/valid.jsonl");
        let omp_file = roots.omp_sessions.join("2026-08-02T00-00-00Z_om-1.jsonl");
        fs::remove_file(roots.claude_projects.join("project-valid/broken.jsonl")).unwrap();
        fs::remove_file(roots.codex_sessions.join("2026/broken.jsonl")).unwrap();
        fs::remove_file(roots.omp_sessions.join("broken.jsonl")).unwrap();

        let first =
            scan_all_session_summaries_with_roots(None, SessionSourceFilter::All, &roots, &config)
                .unwrap();
        assert_eq!(first.summaries.len(), 3);
        assert_eq!(first.diagnostics.cache_hits, 0);
        assert_eq!(first.diagnostics.cache_misses, 3);
        assert_eq!(SessionIndexCache::load(&config).entries.len(), 3);

        fs::write(
            &claude_file,
            concat!(
                "{malformed claude line\n",
                r#"{"type":"user","sessionId":"cc-1","cwd":"/tmp/demo","timestamp":"2026-08-02T00:00:00Z","message":{"role":"user","content":"hello"}}"#,
                "\n"
            ),
        )
        .unwrap();
        fs::write(
            &codex_file,
            concat!(
                "{malformed codex line\n",
                r#"{"type":"session_meta","payload":{"id":"cx-1","cwd":"/tmp/demo"}}"#,
                "\n",
                r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]}}"#,
                "\n"
            ),
        )
        .unwrap();
        fs::write(
            &omp_file,
            concat!(
                "{malformed omp line\n",
                r#"{"type":"session","version":3,"id":"om-1","timestamp":"2026-08-02T00:00:00Z","cwd":"/tmp/demo","title":"OMP"}"#,
                "\n",
                r#"{"type":"message","message":{"role":"user","content":[{"type":"text","text":"hello"}]}}"#,
                "\n"
            ),
        )
        .unwrap();

        let second =
            scan_all_session_summaries_with_roots(None, SessionSourceFilter::All, &roots, &config)
                .unwrap();
        assert_eq!(second.summaries.len(), 3);
        assert_eq!(second.diagnostics.cache_hits, 0);
        assert_eq!(second.diagnostics.cache_misses, 3);
        assert_eq!(second.diagnostics.files_parsed, 3);
        assert_eq!(second.diagnostics.malformed_files, 3);
        assert!(SessionIndexCache::load(&config).entries.is_empty());
    }

    #[test]
    fn scanner_rejects_same_size_and_mtime_content_changes() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("claude/project");
        fs::create_dir_all(&project).unwrap();
        let session_file = project.join("session.jsonl");
        let valid = br#"{"type":"user","sessionId":"cc-fingerprint","cwd":"/tmp/demo","timestamp":"2026-08-02T00:00:00Z","message":{"role":"user","content":"hello"}}"#;
        fs::write(&session_file, valid).unwrap();

        let roots = SessionRoots {
            claude_projects: temp.path().join("claude"),
            codex_sessions: temp.path().join("missing-codex"),
            codex_history: temp.path().join("missing-history.jsonl"),
            omp_sessions: temp.path().join("missing-omp"),
        };
        let config = temp.path().join("config");
        let first = scan_all_session_summaries_with_roots(
            None,
            SessionSourceFilter::Claude,
            &roots,
            &config,
        )
        .unwrap();
        assert_eq!(first.summaries.len(), 1);
        assert_eq!(first.diagnostics.cache_hits, 0);
        assert_eq!(first.diagnostics.cache_misses, 1);

        let original_modified = fs::metadata(&session_file).unwrap().modified().unwrap();
        let malformed = vec![b'{'; valid.len()];
        assert_eq!(malformed.len(), valid.len());
        fs::write(&session_file, malformed).unwrap();
        let file = fs::OpenOptions::new()
            .write(true)
            .open(&session_file)
            .unwrap();
        file.set_times(fs::FileTimes::new().set_modified(original_modified))
            .unwrap();

        let second = scan_all_session_summaries_with_roots(
            None,
            SessionSourceFilter::Claude,
            &roots,
            &config,
        )
        .unwrap();

        assert!(second.summaries.is_empty());
        assert_eq!(second.diagnostics.cache_hits, 0);
        assert_eq!(second.diagnostics.cache_misses, 1);
        assert!(second.diagnostics.degraded());
        assert!(SessionIndexCache::load(&config).entries.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn memory_search_rejects_root_directory_and_file_symlinks() {
        use std::os::unix::fs::symlink;

        for mode in ["root", "memory", "file"] {
            let temp = tempfile::tempdir().unwrap();
            let project = temp.path().join("project");
            let outside = temp.path().join("outside");
            let outside_memory = outside.join("memory");
            let outside_file = outside_memory.join("secret.md");
            fs::create_dir_all(&outside_memory).unwrap();
            fs::write(&outside_file, "needle must not be read").unwrap();

            match mode {
                "root" => symlink(&outside, &project).unwrap(),
                "memory" => {
                    fs::create_dir_all(&project).unwrap();
                    symlink(&outside_memory, project.join("memory")).unwrap();
                }
                "file" => {
                    fs::create_dir_all(project.join("memory")).unwrap();
                    symlink(&outside_file, project.join("memory/secret.md")).unwrap();
                }
                _ => unreachable!(),
            }

            let roots = vec![MemorySearchRoot {
                project: "project".to_string(),
                dir_path: project,
                source: "claude".to_string(),
            }];
            assert!(search_memory_files(&roots, &["needle"], 80).is_empty());
            assert_eq!(
                fs::read_to_string(&outside_file).unwrap(),
                "needle must not be read"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn scanner_rejects_session_source_root_symlinks() {
        use std::os::unix::fs::symlink;

        let (temp, roots, config) = make_scan_fixture();
        let aliased_root = temp.path().join("claude-alias");
        symlink(&roots.claude_projects, &aliased_root).unwrap();
        let first = scan_all_session_summaries_with_roots(
            None,
            SessionSourceFilter::Claude,
            &roots,
            &config,
        )
        .unwrap();
        assert_eq!(first.diagnostics.cache_misses, 2);

        let aliased_roots = SessionRoots {
            claude_projects: aliased_root,
            codex_sessions: roots.codex_sessions.clone(),
            codex_history: roots.codex_history.clone(),
            omp_sessions: roots.omp_sessions.clone(),
        };
        let second = scan_all_session_summaries_with_roots(
            None,
            SessionSourceFilter::Claude,
            &aliased_roots,
            &config,
        )
        .unwrap();
        assert!(second.summaries.is_empty());
        assert_eq!(second.diagnostics.cache_hits, 0);
        assert!(second.diagnostics.degraded());
        assert_eq!(second.diagnostics.io_errors, 1);
        // An incomplete source scan preserves the previously valid cache entry.
        assert_eq!(SessionIndexCache::load(&config).entries.len(), 1);
    }

    #[test]
    fn scan_report_does_not_warn_for_missing_unused_source_root() {
        let temp = tempfile::tempdir().unwrap();
        let roots = SessionRoots {
            claude_projects: temp.path().join("missing-claude"),
            codex_sessions: temp.path().join("missing-codex"),
            codex_history: temp.path().join("missing-history.jsonl"),
            omp_sessions: temp.path().join("missing-omp"),
        };
        let config = temp.path().join("config");
        let report = scan_all_session_summaries_with_roots(
            None,
            SessionSourceFilter::Claude,
            &roots,
            &config,
        )
        .unwrap();
        assert!(report.summaries.is_empty());
        assert_eq!(report.diagnostics.io_errors, 0);
        assert!(!report.diagnostics.degraded());
    }

    #[cfg(unix)]
    #[test]
    fn scanner_parses_non_utf8_filename_without_caching_or_reopening_lossy_path() {
        use std::os::unix::ffi::OsStringExt;

        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("claude/project");
        fs::create_dir_all(&project).unwrap();
        let filename = std::ffi::OsString::from_vec(b"bad-\xff.jsonl".to_vec());
        let file = project.join(&filename);
        let write_result = fs::write(
            &file,
            r#"{"type":"user","sessionId":"cc-nonutf8","cwd":"/tmp/demo","timestamp":"2026-08-02T00:00:00Z","message":{"role":"user","content":"hello"}}"#,
        );
        if let Err(error) = write_result {
            // Darwin rejects malformed UTF-8 directory entries at the VFS
            // boundary; Linux/other Unix platforms exercise the full case.
            if error.raw_os_error() == Some(92) {
                return;
            }
            panic!("failed to create non-UTF-8 fixture: {error}");
        }
        let roots = SessionRoots {
            claude_projects: temp.path().join("claude"),
            codex_sessions: temp.path().join("missing-codex"),
            codex_history: temp.path().join("missing-history.jsonl"),
            omp_sessions: temp.path().join("missing-omp"),
        };
        let config = temp.path().join("config");

        let report = scan_all_session_summaries_with_roots(
            None,
            SessionSourceFilter::Claude,
            &roots,
            &config,
        )
        .unwrap();
        assert_eq!(report.summaries.len(), 1);
        assert!(SessionIndexCache::load(&config).entries.is_empty());
    }

    #[test]
    fn source_filtered_scan_preserves_unselected_source_cache_entries() {
        let (_temp, roots, config) = make_scan_fixture();
        scan_all_session_summaries_with_roots(None, SessionSourceFilter::All, &roots, &config)
            .unwrap();

        scan_all_session_summaries_with_roots(None, SessionSourceFilter::Claude, &roots, &config)
            .unwrap();

        let cache = SessionIndexCache::load(&config);
        assert!(cache.entries.values().any(|entry| entry.source == "codex"));
        assert!(cache.entries.values().any(|entry| entry.source == "omp"));
    }

    #[test]
    fn complete_claude_scan_prunes_only_missing_claude_entries() {
        let (_temp, roots, config) = make_scan_fixture();
        scan_all_session_summaries_with_roots(None, SessionSourceFilter::All, &roots, &config)
            .unwrap();
        let removed = roots.claude_projects.join("project-valid/valid.jsonl");
        let removed_key = canonical_utf8_key(&removed).unwrap();
        fs::remove_file(&removed).unwrap();

        scan_all_session_summaries_with_roots(None, SessionSourceFilter::Claude, &roots, &config)
            .unwrap();

        let cache = SessionIndexCache::load(&config);
        assert!(!cache.entries.contains_key(&removed_key));
        assert!(cache.entries.values().any(|entry| entry.source == "codex"));
        assert!(cache.entries.values().any(|entry| entry.source == "omp"));
    }

    #[test]
    fn incomplete_selected_root_preserves_existing_source_entries() {
        let (temp, mut roots, config) = make_scan_fixture();
        scan_all_session_summaries_with_roots(None, SessionSourceFilter::All, &roots, &config)
            .unwrap();
        let existing_claude = roots.claude_projects.join("project-valid/valid.jsonl");
        let existing_key = canonical_utf8_key(&existing_claude).unwrap();
        let missing_root = temp.path().join("missing-claude");
        roots.claude_projects = missing_root;

        let report = scan_all_session_summaries_with_roots(
            None,
            SessionSourceFilter::Claude,
            &roots,
            &config,
        )
        .unwrap();

        assert!(report.summaries.is_empty());
        let cache = SessionIndexCache::load(&config);
        assert!(cache.entries.contains_key(&existing_key));
    }

    #[test]
    fn regular_file_selected_root_preserves_existing_source_entries() {
        let (temp, mut roots, config) = make_scan_fixture();
        scan_all_session_summaries_with_roots(None, SessionSourceFilter::All, &roots, &config)
            .unwrap();
        let existing_claude = roots.claude_projects.join("project-valid/valid.jsonl");
        let existing_key = canonical_utf8_key(&existing_claude).unwrap();
        let root_file = temp.path().join("claude-root-file");
        fs::write(&root_file, b"not a directory").unwrap();
        roots.claude_projects = root_file;

        scan_all_session_summaries_with_roots(None, SessionSourceFilter::Claude, &roots, &config)
            .unwrap();

        let cache = SessionIndexCache::load(&config);
        assert!(cache.entries.contains_key(&existing_key));
    }

    #[test]
    fn source_scan_tracker_retention_contains_only_complete_started_sources() {
        let mut tracker = SourceScanTracker::default();
        tracker.begin("claude");
        tracker.seen("claude", "claude-seen".to_string());
        tracker.begin("codex");
        tracker.seen("codex", "codex-seen".to_string());
        tracker.mark_incomplete("codex");

        let retention = tracker.retention();
        assert_eq!(
            retention.completed_sources,
            HashSet::from(["claude".to_string()])
        );
        assert_eq!(
            retention.seen_by_source.get("claude"),
            Some(&HashSet::from(["claude-seen".to_string()]))
        );
        assert_eq!(
            retention.seen_by_source.get("codex"),
            Some(&HashSet::from(["codex-seen".to_string()]))
        );
        assert!(!retention.seen_by_source.contains_key("omp"));
    }

    #[test]
    fn partial_claude_parse_removes_only_claude_entry_and_preserves_other_sources() {
        let (_temp, roots, config) = make_scan_fixture();
        scan_all_session_summaries_with_roots(None, SessionSourceFilter::All, &roots, &config)
            .unwrap();
        let changed = roots.claude_projects.join("project-valid/valid.jsonl");
        fs::write(
            &changed,
            concat!(
                "{malformed claude line\\n",
                r#"{"type":"user","sessionId":"cc-1","cwd":"/tmp/demo","timestamp":"2026-08-02T00:00:00Z","message":{"role":"user","content":"hello"}}"#,
                "\\n"
            ),
        )
        .unwrap();

        scan_all_session_summaries_with_roots(None, SessionSourceFilter::Claude, &roots, &config)
            .unwrap();

        let cache = SessionIndexCache::load(&config);
        assert!(!cache
            .entries
            .contains_key(&changed.to_string_lossy().to_string()));
        assert!(cache.entries.values().any(|entry| entry.source == "codex"));
        assert!(cache.entries.values().any(|entry| entry.source == "omp"));
    }

    #[test]
    fn source_filtered_merge_preserves_unrelated_latest_cache_entry() {
        let (_temp, roots, config) = make_scan_fixture();
        scan_all_session_summaries_with_roots(None, SessionSourceFilter::All, &roots, &config)
            .unwrap();
        let unrelated_key = "/tmp/unrelated-latest-cache-entry.jsonl".to_string();
        let mut cache = SessionIndexCache::load(&config);
        cache.entries.insert(
            unrelated_key.clone(),
            crate::session_cache::CachedEntry {
                file_size: 1,
                mtime_secs: 1,
                content_fingerprint: "latest".to_string(),
                source: "future".to_string(),
                session_id: "future-session".to_string(),
                title: "latest writer entry".to_string(),
                project_name: "future-project".to_string(),
                project_dir: "/tmp/future-project".to_string(),
                message_count: 1,
                user_message_count: 1,
                assistant_message_count: 0,
                first_timestamp: None,
                last_activity: None,
            },
        );
        cache.save_with_result(&config).unwrap();

        scan_all_session_summaries_with_roots(None, SessionSourceFilter::Claude, &roots, &config)
            .unwrap();

        assert!(SessionIndexCache::load(&config)
            .entries
            .contains_key(&unrelated_key));
    }

    #[test]
    fn scan_report_keeps_other_sources_when_claude_root_is_not_directory() {
        let (temp, mut roots, config) = make_scan_fixture();
        let claude_root_file = temp.path().join("claude-root-file");
        fs::write(&claude_root_file, b"not a directory").unwrap();
        roots.claude_projects = claude_root_file;

        let report =
            scan_all_session_summaries_with_roots(None, SessionSourceFilter::All, &roots, &config)
                .unwrap();

        assert_eq!(report.summaries.len(), 2);
        assert_eq!(report.diagnostics.io_errors, 1);
        assert!(report.diagnostics.degraded());
        assert!(report
            .diagnostics
            .warnings
            .iter()
            .any(|warning| warning.source.as_deref() == Some("claude")
                && warning.operation == "metadata"));
    }

    #[test]
    fn legacy_scan_wrapper_keeps_summary_only_signature() {
        let _: fn(Option<&str>, SessionSourceFilter) -> Result<Vec<SessionSummary>> =
            scan_all_session_summaries;
    }

    #[test]
    fn scan_report_surfaces_corrupt_cache_but_keeps_sessions() {
        let (_temp, roots, config) = make_scan_fixture();
        fs::create_dir_all(&config).unwrap();
        fs::write(config.join("session_index.json"), b"not-json").unwrap();

        let report =
            scan_all_session_summaries_with_roots(None, SessionSourceFilter::All, &roots, &config)
                .unwrap();
        assert_eq!(report.summaries.len(), 3);
        assert_eq!(report.diagnostics.cache_errors, 1);
        assert!(report.diagnostics.degraded());
    }

    #[test]
    fn scan_report_surfaces_cache_save_failure_without_losing_sessions() {
        let (temp, roots, _config) = make_scan_fixture();
        let config_file = temp.path().join("config-file");
        fs::write(&config_file, b"not a directory").unwrap();

        let report = scan_all_session_summaries_with_roots(
            None,
            SessionSourceFilter::All,
            &roots,
            &config_file,
        )
        .unwrap();
        assert_eq!(report.summaries.len(), 3);
        assert!(report.diagnostics.cache_errors >= 1);
        assert!(report
            .diagnostics
            .warnings
            .iter()
            .any(|warning| warning.operation == "merge"));
    }

    #[test]
    fn claude_duplicate_session_files_count_filter_and_invalid_per_file() {
        let temp = tempfile::tempdir().unwrap();
        let filtered_dir = temp.path().join("claude/project-filtered");
        let invalid_dir = temp.path().join("claude/project-invalid");
        fs::create_dir_all(&filtered_dir).unwrap();
        fs::create_dir_all(&invalid_dir).unwrap();
        fs::write(
            filtered_dir.join("same.jsonl"),
            r#"{"type":"user","sessionId":"same","cwd":"/tmp/other","timestamp":"2026-08-02T00:00:00Z","message":{"role":"user","content":"hello"}}"#,
        )
        .unwrap();
        fs::write(
            invalid_dir.join("same.jsonl"),
            r#"{"type":"user","sessionId":"same","cwd":"/tmp/wanted","timestamp":"2026-08-02T00:00:01Z","message":{"role":"user","content":""}}"#,
        )
        .unwrap();

        let roots = SessionRoots {
            claude_projects: temp.path().join("claude"),
            codex_sessions: temp.path().join("missing-codex"),
            codex_history: temp.path().join("missing-history.jsonl"),
            omp_sessions: temp.path().join("missing-omp"),
        };
        let report = scan_all_session_summaries_with_roots(
            Some("wanted"),
            SessionSourceFilter::Claude,
            &roots,
            &temp.path().join("config"),
        )
        .unwrap();

        assert!(report.summaries.is_empty());
        assert_eq!(report.diagnostics.files_seen, 2);
        assert_eq!(report.diagnostics.files_parsed, 2);
        assert_eq!(report.diagnostics.files_skipped, 2);
    }

    #[test]
    fn cache_mtime_failure_is_not_replaced_with_zero() {
        use std::time::{Duration, SystemTime};

        assert!(cache_mtime_from_modified(Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "metadata unavailable",
        )))
        .is_none());
        assert!(
            cache_mtime_from_modified(Ok(SystemTime::UNIX_EPOCH - Duration::from_secs(1)))
                .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn scan_report_records_unreadable_nested_directory_and_continues() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("claude");
        let visible = root.join("visible");
        let protected = root.join("protected");
        fs::create_dir_all(&visible).unwrap();
        fs::create_dir_all(&protected).unwrap();
        fs::write(
            visible.join("valid.jsonl"),
            r#"{"type":"user","sessionId":"cc-visible","cwd":"/tmp/demo","timestamp":"2026-08-02T00:00:00Z","message":{"role":"user","content":"hello"}}"#,
        )
        .unwrap();
        let original_mode = fs::metadata(&protected).unwrap().permissions().mode();
        fs::set_permissions(&protected, fs::Permissions::from_mode(0o000)).unwrap();

        // Root-owned test processes can still read mode-000 directories. In that
        // environment this fixture cannot produce a stable permission failure.
        let permission_is_effective = fs::read_dir(&protected).is_err();
        if !permission_is_effective {
            fs::set_permissions(&protected, fs::Permissions::from_mode(original_mode)).unwrap();
            return;
        }

        let roots = SessionRoots {
            claude_projects: root,
            codex_sessions: temp.path().join("codex"),
            codex_history: temp.path().join("history.jsonl"),
            omp_sessions: temp.path().join("omp"),
        };
        let config = temp.path().join("config");
        let report = scan_all_session_summaries_with_roots(
            None,
            SessionSourceFilter::Claude,
            &roots,
            &config,
        )
        .unwrap();
        fs::set_permissions(&protected, fs::Permissions::from_mode(original_mode)).unwrap();

        assert_eq!(report.summaries.len(), 1);
        assert!(report.diagnostics.io_errors >= 1);
    }
}
