//! Session management handlers
//!
//! Provides interactive session management for Claude Code conversations.
//! Supports listing, viewing, renaming, and deleting sessions with a
//! hierarchical navigation interface.

use anyhow::{Context, Result};
use colored::Colorize;
use inquire::{Confirm, Select, Text};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};

use crate::codex::{
    codex_history_path, codex_sessions_dir, discover_codex_sessions, load_codex_history_titles,
    CodexSession,
};
use crate::config::ConfigManager;
use crate::filter::FilterConfig;
use crate::parser::ConversationSession;
use crate::sync::discovery::{claude_projects_dir, discover_sessions, extract_project_name};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionSourceFilter {
    All,
    Claude,
    Codex,
}

impl SessionSourceFilter {
    fn includes_claude(self) -> bool {
        matches!(self, Self::All | Self::Claude)
    }

    fn includes_codex(self) -> bool {
        matches!(self, Self::All | Self::Codex)
    }
}

fn source_label(source: &str) -> &str {
    match source {
        "claude" => "CC",
        "codex" => "CX",
        _ => "??",
    }
}

fn memory_dir_name_for_source(source: &str) -> &'static str {
    match source {
        "codex" => ".memory",
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
            source: "claude".to_string(),
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
            source: "codex".to_string(),
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
enum SessionMenuChoice {
    Select(SessionSummary),
    Search,
    Cleanup,
    SwitchProject,
    Exit,
}

/// Menu choice for session actions
enum ActionChoice {
    OpenClaude,
    ViewDetails,
    Rename,
    Delete,
    Back,
}

// ============================================================================
// Core Functions
// ============================================================================

/// Scan all projects and return summaries
pub fn scan_all_projects() -> Result<Vec<ProjectSummary>> {
    let claude_dir = claude_projects_dir()?;

    if !claude_dir.exists() {
        return Ok(Vec::new());
    }

    let mut projects = Vec::new();
    // Use a filter with no file size limit for session listing
    let mut filter = FilterConfig::default();
    filter.max_file_size_bytes = u64::MAX;

    for entry in fs::read_dir(&claude_dir)? {
        let entry = entry?;
        let path = entry.path();

        if !path.is_dir() {
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

        // Scan sessions in this project
        let sessions = discover_sessions(&path, &filter).unwrap_or_default();

        if sessions.is_empty() {
            continue;
        }

        // Get project name from session's cwd field (more accurate than directory name)
        // Fall back to extract_project_name if no cwd is available
        let project_name = sessions
            .iter()
            .find_map(|s| s.project_name().map(|n| n.to_string()))
            .unwrap_or_else(|| extract_project_name(dir_name).to_string());

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

/// Scan sessions for a specific project, returns (valid_sessions, filtered_count)
pub fn scan_project_sessions_with_filtered(
    project: &ProjectSummary,
) -> Result<(Vec<SessionSummary>, usize)> {
    // Use a filter with no file size limit for session listing
    let mut filter = FilterConfig::default();
    filter.max_file_size_bytes = u64::MAX;
    let sessions = discover_sessions(&project.dir_path, &filter)?;

    let all_summaries: Vec<SessionSummary> = sessions
        .iter()
        .map(|s| SessionSummary::from_session(s, &project.name, &project.dir_path))
        .collect();

    let total_count = all_summaries.len();

    let mut valid_summaries: Vec<SessionSummary> = all_summaries
        .into_iter()
        .filter(|s| is_valid_session_summary(s))
        .collect();

    // Sort by last activity (most recent first)
    valid_summaries.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));

    let filtered_count = total_count - valid_summaries.len();
    Ok((valid_summaries, filtered_count))
}

/// Scan sessions for a specific project
pub fn scan_project_sessions(project: &ProjectSummary) -> Result<Vec<SessionSummary>> {
    let (sessions, _) = scan_project_sessions_with_filtered(project)?;
    Ok(sessions)
}

fn scan_codex_session_summaries() -> Result<Vec<SessionSummary>> {
    let sessions_dir = codex_sessions_dir()?;
    let history_path = codex_history_path()?;
    let titles = load_codex_history_titles(&history_path).unwrap_or_default();
    let sessions = discover_codex_sessions(&sessions_dir)?;

    let mut summaries: Vec<SessionSummary> = sessions
        .iter()
        .map(|session| {
            let project_name = session.project_name().unwrap_or("codex");
            let title = session.title(titles.get(&session.session_id).map(String::as_str));
            SessionSummary::from_codex_session(session, project_name, title)
        })
        .filter(|s| is_valid_session_summary(s))
        .collect();

    summaries.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));
    Ok(summaries)
}

fn scan_all_session_summaries(
    project_filter: Option<&str>,
    source: SessionSourceFilter,
) -> Result<Vec<SessionSummary>> {
    let mut summaries = Vec::new();

    if source.includes_claude() {
        let projects = scan_all_projects()?;
        for project in projects {
            if project_filter.is_some_and(|name| project.name != name) {
                continue;
            }
            summaries.extend(scan_project_sessions(&project)?);
        }
    }

    if source.includes_codex() {
        for session in scan_codex_session_summaries()? {
            if project_filter.is_some_and(|name| session.project_name != name) {
                continue;
            }
            summaries.push(session);
        }
    }

    summaries.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));
    Ok(summaries)
}

/// Get filtered (invalid) sessions for cleanup
pub fn get_filtered_sessions(project: &ProjectSummary) -> Result<Vec<SessionSummary>> {
    let mut filter = FilterConfig::default();
    filter.max_file_size_bytes = u64::MAX;
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

/// Rename a session by appending a custom-title entry (same as Claude Code official behavior)
pub fn rename_session(file_path: &Path, session_id: &str, new_title: &str) -> Result<()> {
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

/// Delete a session file
pub fn delete_session(file_path: &Path) -> Result<()> {
    fs::remove_file(file_path)
        .with_context(|| format!("Failed to delete file: {}", file_path.display()))?;
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

/// Show session selection menu for a project
fn show_session_menu(
    project: &ProjectSummary,
    sessions: &[SessionSummary],
    filtered_count: usize,
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
    let cleanup_option = if filtered_count > 0 {
        format!("Cleanup [{}]", filtered_count)
    } else {
        "Cleanup [0]".to_string()
    };
    let switch_option = "Switch project".to_string();
    let exit_option = "Exit".to_string();

    let mut options: Vec<String> = Vec::with_capacity(sessions.len() + 4);
    options.push(search_option.clone());

    let has_mixed_sources = sessions.iter().any(|s| s.source != sessions[0].source);
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

    options.push(cleanup_option.clone());
    options.push(switch_option.clone());
    options.push(exit_option.clone());

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
            } else if selected == cleanup_option {
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

    let is_codex = session.source == "codex";
    let mut options = Vec::new();
    if !is_codex {
        options.push("Open in Claude");
    }
    options.push("View details");
    if !is_codex {
        options.push("Rename session");
    }
    options.push("Delete session");
    options.push("Back to session list");

    let selection = Select::new("Choose an action:", options.clone())
        .with_help_message("Use arrow keys to navigate, Enter to select")
        .prompt();

    match selection {
        Ok(selected) => match selected {
            "Open in Claude" => Ok(ActionChoice::OpenClaude),
            "View details" => Ok(ActionChoice::ViewDetails),
            "Rename session" => Ok(ActionChoice::Rename),
            "Delete session" => Ok(ActionChoice::Delete),
            _ => Ok(ActionChoice::Back),
        },
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

/// Open session in Claude Code by executing `claude --resume {session_id}`
/// Returns: Ok(true) = executed command, Ok(false) = cancelled
fn open_in_claude(session: &SessionSummary) -> Result<bool> {
    // Get project path from session's cwd field
    let project_path = if let Ok(conv) = ConversationSession::from_file(&session.file_path) {
        conv.cwd().map(|s| s.to_string())
    } else {
        None
    };

    // Build default command
    let default_cmd = if let Some(ref path) = project_path {
        format!("cd \"{}\" && claude --resume {}", path, session.session_id)
    } else {
        format!("claude --resume {}", session.session_id)
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

            // Prepend source ~/.zshrc to load shell functions (e.g., cc-auto)
            let full_cmd = if cfg!(target_os = "windows") {
                cmd.clone()
            } else {
                format!("source ~/.zshrc && {}", cmd)
            };

            // Execute the command via zsh
            let status = if cfg!(target_os = "windows") {
                std::process::Command::new("cmd")
                    .arg("/C")
                    .arg(&full_cmd)
                    .status()
                    .with_context(|| format!("Failed to execute command: {}", cmd))?
            } else {
                std::process::Command::new("zsh")
                    .arg("-c")
                    .arg(&full_cmd)
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
fn rename_session_interactive(session: &mut SessionSummary) -> Result<()> {
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
                return Ok(());
            }

            if title == session.title {
                println!("{}", "Title unchanged.".yellow());
                return Ok(());
            }

            rename_session(&session.file_path, &session.session_id, &title)?;
            session.title = title.clone();

            println!();
            println!("{} Title updated successfully!", "SUCCESS:".green().bold());
            println!();
        }
        Err(_) => {
            println!("{}", "Rename cancelled.".yellow());
        }
    }

    Ok(())
}

/// Interactive delete session
fn delete_session_interactive(session: &SessionSummary) -> Result<bool> {
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
            delete_session(&session.file_path)?;
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
            let mut deleted_count = 0;
            for session in &filtered_sessions {
                if let Err(e) = delete_session(&session.file_path) {
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
    let mut all_sessions = scan_all_session_summaries(None, source)?;
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
        detect_current_project()?
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

            match show_session_menu(project, &sessions, filtered_count)? {
                SessionMenuChoice::Select(session) => {
                    let mut session = session;
                    let mut deleted = false;
                    loop {
                        match show_action_menu(&session)? {
                            ActionChoice::OpenClaude => {
                                if open_in_claude(&session)? {
                                    return Ok(());
                                }
                            }
                            ActionChoice::ViewDetails => {
                                show_session_details(&session)?;
                            }
                            ActionChoice::Rename => {
                                rename_session_interactive(&mut session)?;
                            }
                            ActionChoice::Delete => {
                                if delete_session_interactive(&session)? {
                                    deleted = true;
                                    break;
                                }
                            }
                            ActionChoice::Back => {
                                break;
                            }
                        }
                    }
                    if deleted {
                        all_sessions = scan_all_session_summaries(None, source)?;
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
                            match show_search_results(&results, &keyword)? {
                                SessionMenuChoice::Select(session) => {
                                    let mut session = session;
                                    loop {
                                        match show_action_menu(&session)? {
                                            ActionChoice::OpenClaude => {
                                                open_in_claude(&session)?;
                                                return Ok(());
                                            }
                                            ActionChoice::ViewDetails => {
                                                show_session_details(&session)?;
                                            }
                                            ActionChoice::Rename => {
                                                rename_session_interactive(&mut session)?;
                                            }
                                            ActionChoice::Delete => {
                                                if delete_session_interactive(&session)? {
                                                    break;
                                                }
                                            }
                                            ActionChoice::Back => {
                                                break;
                                            }
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
                SessionMenuChoice::Cleanup => {
                    if let Some(claude_project) = scan_all_projects()?
                        .iter()
                        .find(|p| p.name == project.name)
                    {
                        cleanup_sessions_interactive(claude_project)?;
                    } else {
                        println!(
                            "{}",
                            "Cleanup is only available for Claude sessions.".yellow()
                        );
                    }
                    all_sessions = scan_all_session_summaries(None, source)?;
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
            all_sessions = scan_all_session_summaries(None, source)?;
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
    let sessions = scan_all_session_summaries(project_filter, source)?;

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
    let sessions = scan_all_session_summaries(None, source)?;

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
            for j in (i + 1)..lines.len() {
                let next = lines[j].trim();
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

    let mut sessions = scan_all_session_summaries(None, source)?;

    if let Some(ref cutoff) = since_cutoff {
        sessions.retain(|s| is_after_cutoff(s.last_activity.as_deref(), cutoff));
    }

    if sessions.is_empty() {
        if json_output {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "total_projects": 0,
                    "projects": []
                }))?
            );
        } else {
            println!("{}", "No projects found.".yellow());
        }
        return Ok(());
    }

    let mut overviews = Vec::new();
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
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "total_projects": overviews.len(),
                "projects": overviews,
            }))?
        );
    } else {
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

/// Show session details (non-interactive), with optional drill-down flags
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
    let sessions = scan_all_session_summaries(None, source)?;

    if let Some(session) = sessions.iter().find(|s| s.session_id == session_id) {
        // If no drill-down flags and not json, use interactive view
        if session.source == "claude"
            && tail.is_none()
            && head.is_none()
            && around.is_none()
            && !json
        {
            show_session_details(session)?;
            return Ok(());
        }

        // Drill-down mode: parse and filter messages
        // JSON or --full uses full content (no truncation); terminal uses simplified
        let messages = collect_display_messages_for_summary(session, json || full);

        if messages.is_empty() {
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&serde_json::json!({
                        "source": session.source,
                        "session_id": session.session_id,
                        "project": session.project_name,
                        "title": session.title,
                        "message_count": 0,
                        "messages": []
                    }))?
                );
            } else {
                println!("(No messages found)");
            }
            return Ok(());
        }

        // Determine slice range
        let total = messages.len();
        let (start, end, showing) = if let Some(keyword) = around {
            let keyword_lower = keyword.to_lowercase();
            let pos = messages
                .iter()
                .position(|m| m.content.to_lowercase().contains(&keyword_lower))
                .unwrap_or(0);
            let s = pos.saturating_sub(num);
            let e = (pos + num + 1).min(total);
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

            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "source": session.source,
                    "session_id": session.session_id,
                    "project": session.project_name,
                    "title": session.title,
                    "message_count": session.message_count,
                    "showing": showing,
                    "messages": json_msgs,
                }))?
            );
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

        return Ok(());
    }

    anyhow::bail!("Session not found: {}", session_id)
}

// ============================================================================
// Search functionality
// ============================================================================

#[derive(Debug, Clone, serde::Serialize)]
struct SearchMatch {
    role: String,
    snippet: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum MatchMode {
    And, // 0 — sorted first
    Or,  // 1 — sorted after AND
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

impl Default for MatchMode {
    fn default() -> Self {
        MatchMode::And
    }
}

/// A processed message ready for display
struct DisplayMessage {
    index: usize,
    role: String,
    timestamp: Option<String>,
    content: String,
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
    parts.extend(texts.drain(..));
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
        let memory_dir = memory_dir_for_source(&root.dir_path, &root.source);
        if !memory_dir.is_dir() {
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
    let mut all_sessions = scan_all_session_summaries(project_filter, source)?;
    let memory_roots = memory_search_roots_from_sessions(&all_sessions);
    if all_sessions.is_empty() && memory_roots.is_empty() {
        if json_output {
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "query": query_display,
                    "total_matches": 0,
                    "memory_results": [],
                    "session_results": [],
                }))?
            );
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

        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "query": query_display,
                "total_matches": total_matches,
                "memory_results": memory_results,
                "session_results": session_json,
            }))?
        );
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
            if multi_keyword {
                if prev_mode.map_or(true, |m| m != &result.match_mode) {
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
            if multi_keyword {
                if prev_mode.map_or(true, |m| m != &result.match_mode) {
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

/// Rename session (non-interactive)
pub fn handle_session_rename(session_id: &str, new_title: &str) -> Result<()> {
    let projects = scan_all_projects()?;

    for project in &projects {
        let sessions = scan_project_sessions(project)?;

        if let Some(session) = sessions.iter().find(|s| s.session_id == session_id) {
            rename_session(&session.file_path, session_id, new_title)?;
            println!(
                "{} Session renamed successfully!",
                "SUCCESS:".green().bold()
            );
            return Ok(());
        }
    }

    anyhow::bail!("Session not found: {}", session_id)
}

/// Delete session (non-interactive)
pub fn handle_session_delete(session_id: &str, force: bool) -> Result<()> {
    let projects = scan_all_projects()?;

    for project in &projects {
        let sessions = scan_project_sessions(project)?;

        if let Some(session) = sessions.iter().find(|s| s.session_id == session_id) {
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

            delete_session(&session.file_path)?;
            println!(
                "{} Session deleted successfully!",
                "SUCCESS:".green().bold()
            );
            return Ok(());
        }
    }

    anyhow::bail!("Session not found: {}", session_id)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
