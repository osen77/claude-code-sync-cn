//! Stable domain types shared by session scanning, cache, and maintenance.

use crate::codex::CodexSession;
use crate::omp::OmpSession;
use crate::parser::ConversationSession;
use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};

/// Identifies the system that produced a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct SessionIdentity {
    /// Source that produced the session.
    pub source: SessionSource,
    /// Source-local session identifier.
    pub session_id: String,
}

/// Filter selecting one or all session sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionSourceFilter {
    All,
    Claude,
    Codex,
    Omp,
}

impl SessionSourceFilter {
    pub(crate) fn includes(self, source: SessionSource) -> bool {
        matches!(
            (self, source),
            (Self::All, _)
                | (Self::Claude, SessionSource::Claude)
                | (Self::Codex, SessionSource::Codex)
                | (Self::Omp, SessionSource::Omp)
        )
    }

    pub(crate) fn includes_claude(self) -> bool {
        matches!(self, Self::All | Self::Claude)
    }

    pub(crate) fn includes_codex(self) -> bool {
        matches!(self, Self::All | Self::Codex)
    }

    pub(crate) fn includes_omp(self) -> bool {
        matches!(self, Self::All | Self::Omp)
    }
}

/// Project summary for listing.
#[derive(Debug, Clone)]
pub struct ProjectSummary {
    pub name: String,
    pub dir_path: PathBuf,
    pub session_count: usize,
    pub last_activity: Option<String>,
}

/// Session summary for listing and operations.
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
    pub has_custom_title: bool,
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
            has_custom_title: session.has_custom_title(),
        }
    }

    /// Get a truncated title for display (Unicode-safe).
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

    /// Format relative time for display.
    pub fn relative_time(&self) -> String {
        self.last_activity
            .as_ref()
            .map(|ts| format_relative_time(ts))
            .unwrap_or_else(|| "Unknown".to_string())
    }

    pub(crate) fn source_kind(&self) -> Result<SessionSource> {
        SessionSource::try_from(self.source.as_str())
    }

    pub(crate) fn identity(&self) -> Result<SessionIdentity> {
        Ok(SessionIdentity {
            source: self.source_kind()?,
            session_id: self.session_id.clone(),
        })
    }

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
            has_custom_title: false,
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
            has_custom_title: false,
        }
    }
}

/// Format a timestamp as relative time (e.g., "Today", "Yesterday", "3 days ago").
pub(crate) fn format_relative_time(timestamp: &str) -> String {
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
