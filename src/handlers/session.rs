//! Session management handlers
//!
//! Provides interactive session management for Claude Code conversations.
//! Supports listing, viewing, renaming, and deleting sessions with a
//! hierarchical navigation interface.

use anyhow::{Context, Result};
use colored::Colorize;
use inquire::{Confirm, Select, Text};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

use crate::filter::FilterConfig;
use crate::parser::ConversationSession;
use crate::sync::discovery::{claude_projects_dir, discover_sessions, extract_project_name};

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
    /// Create a SessionSummary from a ConversationSession
    pub fn from_session(session: &ConversationSession, project_name: &str, project_dir: &Path) -> Self {
        let file_size = fs::metadata(&session.file_path)
            .map(|m| m.len())
            .unwrap_or(0);

        let user_count = session
            .entries
            .iter()
            .filter(|e| e.entry_type == "user")
            .count();
        let assistant_count = session
            .entries
            .iter()
            .filter(|e| e.entry_type == "assistant")
            .count();

        SessionSummary {
            session_id: session.session_id.clone(),
            title: session.title().unwrap_or_else(|| "(No title)".to_string()),
            project_name: project_name.to_string(),
            project_dir: project_dir.to_path_buf(),
            file_path: PathBuf::from(&session.file_path),
            message_count: session.message_count(),
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
    SwitchProject,
    Exit,
}

/// Menu choice for session actions
enum ActionChoice {
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
    let filter = FilterConfig::default();

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
        let valid_session_count = sessions
            .iter()
            .filter(|s| s.message_count() > 0 && s.title().is_some())
            .count();

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

/// Scan sessions for a specific project
pub fn scan_project_sessions(project: &ProjectSummary) -> Result<Vec<SessionSummary>> {
    let filter = FilterConfig::default();
    let sessions = discover_sessions(&project.dir_path, &filter)?;

    let mut summaries: Vec<SessionSummary> = sessions
        .iter()
        .map(|s| SessionSummary::from_session(s, &project.name, &project.dir_path))
        // Filter out empty sessions (no messages) and sessions without real titles
        .filter(|s| s.message_count > 0 && s.title != "(No title)")
        .collect();

    // Sort by last activity (most recent first)
    summaries.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));

    Ok(summaries)
}

/// Detect if current directory corresponds to a Claude project
pub fn detect_current_project() -> Result<Option<ProjectSummary>> {
    let cwd = std::env::current_dir()?;
    let project_name = cwd
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();

    if project_name.is_empty() {
        return Ok(None);
    }

    let projects = scan_all_projects()?;

    // Find project matching current directory name
    Ok(projects.into_iter().find(|p| p.name == project_name))
}

/// Rename a session by modifying the first user message content
pub fn rename_session(file_path: &Path, new_title: &str) -> Result<()> {
    let content = fs::read_to_string(file_path)
        .with_context(|| format!("Failed to read file: {}", file_path.display()))?;

    let mut modified_lines = Vec::new();
    let mut found_user = false;

    for line in content.lines() {
        if line.trim().is_empty() {
            modified_lines.push(line.to_string());
            continue;
        }

        if !found_user {
            if let Ok(mut entry) = serde_json::from_str::<Value>(line) {
                if entry.get("type") == Some(&json!("user")) {
                    // Modify message.content
                    if let Some(msg) = entry.get_mut("message") {
                        msg["content"] = json!(new_title);
                    }
                    modified_lines.push(serde_json::to_string(&entry)?);
                    found_user = true;
                    continue;
                }
            }
        }
        modified_lines.push(line.to_string());
    }

    if !found_user {
        anyhow::bail!("No user message found in session");
    }

    fs::write(file_path, modified_lines.join("\n") + "\n")
        .with_context(|| format!("Failed to write file: {}", file_path.display()))?;

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
            format!(
                "{:<30} {:>3} sessions  {}",
                p.name, p.session_count, time
            )
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

    let mut options: Vec<String> = sessions
        .iter()
        .map(|s| {
            format!(
                "{:<40} {:>3} msgs  {}",
                s.display_title(40),
                s.message_count,
                s.relative_time()
            )
        })
        .collect();

    options.push("Switch project".to_string());
    options.push("Exit".to_string());

    let selection = Select::new("Select a session:", options.clone())
        .with_help_message("Use arrow keys to navigate, Enter to select")
        .prompt();

    match selection {
        Ok(selected) => {
            if selected == "Exit" {
                Ok(SessionMenuChoice::Exit)
            } else if selected == "Switch project" {
                Ok(SessionMenuChoice::SwitchProject)
            } else if let Some(idx) = options.iter().position(|o| o == &selected) {
                if idx < sessions.len() {
                    Ok(SessionMenuChoice::Select(sessions[idx].clone()))
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

/// Show action menu for a selected session
fn show_action_menu(session: &SessionSummary) -> Result<ActionChoice> {
    println!();
    println!(
        "{} {}",
        "Selected:".cyan().bold(),
        session.display_title(60).bold()
    );
    println!();

    let options = vec![
        "View details",
        "Rename session",
        "Delete session",
        "Back to session list",
    ];

    let selection = Select::new("Choose an action:", options.clone())
        .with_help_message("Use arrow keys to navigate, Enter to select")
        .prompt();

    match selection {
        Ok(selected) => match selected {
            "View details" => Ok(ActionChoice::ViewDetails),
            "Rename session" => Ok(ActionChoice::Rename),
            "Delete session" => Ok(ActionChoice::Delete),
            _ => Ok(ActionChoice::Back),
        },
        Err(_) => Ok(ActionChoice::Back),
    }
}

/// Show session details
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

    println!();
    println!("{}", "=".repeat(60).cyan());
    println!();

    // Wait for user input
    let _ = Text::new("Press Enter to continue...")
        .with_help_message("")
        .prompt();

    Ok(())
}

/// Interactive rename session
fn rename_session_interactive(session: &mut SessionSummary) -> Result<()> {
    println!();
    println!("{} {}", "Current title:".dimmed(), session.title);
    println!();

    let new_title = Text::new("Enter new title:")
        .with_initial_value(&session.title)
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

            rename_session(&session.file_path, &title)?;
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

// ============================================================================
// Main Entry Point
// ============================================================================

/// Main interactive session management handler
pub fn handle_session_interactive(project_filter: Option<&str>) -> Result<()> {
    // Check if running in interactive terminal
    if !atty::is(atty::Stream::Stdout) {
        anyhow::bail!("Interactive mode requires a terminal. Use subcommands for non-interactive use.");
    }

    println!();
    println!("{}", "Session Manager".cyan().bold());
    println!("{}", "=".repeat(40).cyan());

    // Load all projects
    let mut projects = scan_all_projects()?;

    if projects.is_empty() {
        println!("{}", "No Claude Code projects found.".yellow());
        println!(
            "{}",
            "Run Claude Code in a project directory first.".dimmed()
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
            // Show sessions for this project
            let sessions = scan_project_sessions(project)?;

            match show_session_menu(project, &sessions)? {
                SessionMenuChoice::Select(session) => {
                    // Enter session action loop
                    let mut session = session;
                    loop {
                        match show_action_menu(&session)? {
                            ActionChoice::ViewDetails => {
                                show_session_details(&session)?;
                            }
                            ActionChoice::Rename => {
                                rename_session_interactive(&mut session)?;
                            }
                            ActionChoice::Delete => {
                                if delete_session_interactive(&session)? {
                                    // Session deleted, break to refresh list
                                    break;
                                }
                            }
                            ActionChoice::Back => {
                                break;
                            }
                        }
                    }
                }
                SessionMenuChoice::SwitchProject => {
                    current_project = None;
                }
                SessionMenuChoice::Exit => {
                    break;
                }
            }
        } else {
            // Show project list
            // Refresh projects list
            projects = scan_all_projects()?;

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
pub fn handle_session_list(project_filter: Option<&str>, show_ids: bool) -> Result<()> {
    let projects = scan_all_projects()?;

    let filtered_projects: Vec<_> = if let Some(name) = project_filter {
        projects.into_iter().filter(|p| p.name == name).collect()
    } else {
        projects
    };

    if filtered_projects.is_empty() {
        if project_filter.is_some() {
            println!("{}", "No matching project found.".yellow());
        } else {
            println!("{}", "No projects found.".yellow());
        }
        return Ok(());
    }

    for project in &filtered_projects {
        println!();
        println!(
            "{} {} ({} sessions)",
            "Project:".cyan().bold(),
            project.name.bold(),
            project.session_count
        );
        println!("{}", "-".repeat(60));

        let sessions = scan_project_sessions(project)?;

        for session in sessions {
            if show_ids {
                println!(
                    "{} | {} | {} msgs | {}",
                    session.session_id.dimmed(),
                    session.display_title(40),
                    session.message_count,
                    session.relative_time()
                );
            } else {
                println!(
                    "{} | {} msgs | {}",
                    session.display_title(50),
                    session.message_count,
                    session.relative_time()
                );
            }
        }
    }

    Ok(())
}

/// Show session details (non-interactive)
pub fn handle_session_show(session_id: &str) -> Result<()> {
    let projects = scan_all_projects()?;

    for project in &projects {
        let sessions = scan_project_sessions(project)?;

        if let Some(session) = sessions.iter().find(|s| s.session_id == session_id) {
            show_session_details(session)?;
            return Ok(());
        }
    }

    anyhow::bail!("Session not found: {}", session_id)
}

/// Rename session (non-interactive)
pub fn handle_session_rename(session_id: &str, new_title: &str) -> Result<()> {
    let projects = scan_all_projects()?;

    for project in &projects {
        let sessions = scan_project_sessions(project)?;

        if let Some(session) = sessions.iter().find(|s| s.session_id == session_id) {
            rename_session(&session.file_path, new_title)?;
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
}
