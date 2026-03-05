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

use crate::filter::FilterConfig;
use crate::parser::ConversationSession;
use crate::sync::discovery::{claude_projects_dir, discover_sessions, extract_project_name};
use crate::config::ConfigManager;

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
    pub fn from_session(session: &ConversationSession, project_name: &str, project_dir: &Path) -> Self {
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
        let valid_session_count = sessions
            .iter()
            .filter(|s| is_valid_session(s))
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

/// Check if a ConversationSession is valid (has messages and a real title)
fn is_valid_session(session: &ConversationSession) -> bool {
    session.message_count() > 0 && session.title().is_some()
}

/// Check if a SessionSummary is valid (has messages and a real title)
fn is_valid_session_summary(summary: &SessionSummary) -> bool {
    summary.message_count > 0 && summary.title != "(No title)"
}

/// Scan sessions for a specific project, returns (valid_sessions, filtered_count)
pub fn scan_project_sessions_with_filtered(project: &ProjectSummary) -> Result<(Vec<SessionSummary>, usize)> {
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

    for (i, s) in sessions.iter().enumerate() {
        options.push(format!(
            "[{:>2}] {:<40} {:>3} msgs  {}",
            i + 1,
            s.display_title(40),
            s.message_count,
            s.relative_time()
        ));
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
fn search_sessions(sessions: &[SessionSummary], keyword: &str) -> Vec<(SessionSummary, Vec<String>)> {
    search_sessions_full(sessions, keyword, 60, true)
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
    let start = if end == total { end.saturating_sub(max_len) } else { start };

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
        .map(|(i, (s, _))| {
            format!(
                "[{:>2}] {}",
                i + 1,
                s.display_title(50),
            )
        })
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

    let options = vec![
        "Open in Claude",
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

    if let Ok(conv) = ConversationSession::from_file(&session.file_path) {
        let messages = collect_display_messages(&conv);

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

                let role_label = if m.role == "user" {
                    "[User]".green().bold()
                } else {
                    "[Claude]".blue().bold()
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
    let content = serde_json::to_string_pretty(data)
        .with_context(|| "Failed to serialize user data")?;
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
                            println!("{} Failed to clear saved command: {}", "WARNING:".yellow(), e);
                        } else {
                            println!("{} Saved command cleared, using default next time", "INFO:".cyan());
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
            let (sessions, filtered_count) = scan_project_sessions_with_filtered(project)?;

            match show_session_menu(project, &sessions, filtered_count)? {
                SessionMenuChoice::Select(session) => {
                    // Enter session action loop
                    let mut session = session;
                    loop {
                        match show_action_menu(&session)? {
                            ActionChoice::OpenClaude => {
                                if open_in_claude(&session)? {
                                    // Executed command, exit program
                                    return Ok(());
                                }
                                // Cancelled, continue to action menu
                            }
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
                                _ => {} // Back to session list
                            }
                        }
                    }
                }
                SessionMenuChoice::Cleanup => {
                    cleanup_sessions_interactive(project)?;
                    // Continue to refresh the session list
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

        for (i, session) in sessions.iter().enumerate() {
            if show_ids {
                println!(
                    "[{:>2}] {} | {} | {} msgs | {}",
                    i + 1,
                    session.session_id.dimmed(),
                    session.display_title(40),
                    session.message_count,
                    session.relative_time()
                );
            } else {
                println!(
                    "[{:>2}] {} | {} msgs | {}",
                    i + 1,
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
pub fn handle_session_projects() -> Result<()> {
    let mut projects = scan_all_projects()?;

    if projects.is_empty() {
        println!("{}", "No projects found.".yellow());
        return Ok(());
    }

    // Sort by last activity (most recent first)
    projects.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));

    println!(
        "{} ({} projects)",
        "Projects".cyan().bold(),
        projects.len()
    );
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

/// Show session details (non-interactive), with optional drill-down flags
pub fn handle_session_show(
    session_id: &str,
    tail: Option<usize>,
    head: Option<usize>,
    around: Option<&str>,
    num: usize,
    json: bool,
) -> Result<()> {
    let projects = scan_all_projects()?;

    for project in &projects {
        let sessions = scan_project_sessions(project)?;

        if let Some(session) = sessions.iter().find(|s| s.session_id == session_id) {
            // If no drill-down flags and not json, use interactive view
            if tail.is_none() && head.is_none() && around.is_none() && !json {
                show_session_details(session)?;
                return Ok(());
            }

            // Drill-down mode: parse and filter messages
            let conv = ConversationSession::from_file(&session.file_path)?;
            let messages = collect_display_messages(&conv);

            if messages.is_empty() {
                if json {
                    println!(
                        "{}",
                        serde_json::to_string(&serde_json::json!({
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
                    "--- {} | {} | {} | {} msgs | showing {} ---",
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

#[derive(Debug, Clone)]
struct SessionSearchResult {
    summary: SessionSummary,
    matches: Vec<SearchMatch>,
    score: f64,
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
fn collect_display_messages(conv: &ConversationSession) -> Vec<DisplayMessage> {
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
                if let Some(text) = ConversationSession::extract_display_content(msg, true) {
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
                } else if let Some(text) =
                    ConversationSession::extract_display_content(msg, false)
                {
                    assistant_texts.push(text);
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
        anyhow::bail!("Invalid duration: '{}'. Use format like '1d', '3h', '1w'", since);
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
    projects: &[ProjectSummary],
    keyword: &str,
    context_chars: usize,
) -> Vec<MemorySearchResult> {
    let keyword_lower = keyword.to_lowercase();
    let mut results = Vec::new();

    for project in projects {
        let memory_dir = project.dir_path.join("memory");
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

            // Collect match snippets
            let mut matches = Vec::new();
            for line in content.lines() {
                if line.to_lowercase().contains(&keyword_lower) {
                    let snippet = extract_match_snippet(line, &keyword_lower, context_chars);
                    matches.push(MemoryMatch { snippet });
                }
            }

            if !matches.is_empty() {
                results.push(MemorySearchResult {
                    project: project.name.clone(),
                    file: format!("memory/{}", file_name),
                    matches,
                });
            }
        }
    }

    results
}

/// Search sessions across projects (both user and assistant messages)
fn search_sessions_full(
    sessions: &[SessionSummary],
    keyword: &str,
    context_chars: usize,
    user_only: bool,
) -> Vec<SessionSearchResult> {
    let keyword_lower = keyword.to_lowercase();
    let mut results = Vec::new();

    for session in sessions {
        let Ok(conv) = ConversationSession::from_file(&session.file_path) else {
            continue;
        };

        let mut matches = Vec::new();
        const MAX_MATCHES_PER_SESSION: usize = 20;

        for entry in &conv.entries {
            if matches.len() >= MAX_MATCHES_PER_SESSION {
                break;
            }

            let is_user = entry.entry_type == "user";
            let is_assistant = entry.entry_type == "assistant";

            if !is_user && !is_assistant {
                continue;
            }

            if user_only && is_assistant {
                continue;
            }

            if ConversationSession::is_tool_result_entry(entry) {
                continue;
            }

            if let Some(msg) = entry.message.as_ref() {
                let text = if is_user {
                    ConversationSession::extract_user_text(msg)
                } else {
                    ConversationSession::extract_display_content(msg, false)
                };

                if let Some(text) = text {
                    if text.to_lowercase().contains(&keyword_lower) {
                        let snippet =
                            extract_match_snippet(&text, &keyword_lower, context_chars);
                        matches.push(SearchMatch {
                            role: if is_user {
                                "user".to_string()
                            } else {
                                "assistant".to_string()
                            },
                            snippet,
                        });
                    }
                }
            }
        }

        if !matches.is_empty() {
            let recency_score = calculate_recency_score(session.last_activity.as_deref());
            let match_score = (matches.len() as f64).ln_1p();
            let score = recency_score * 0.6 + match_score * 0.4;

            results.push(SessionSearchResult {
                summary: session.clone(),
                matches,
                score,
            });
        }
    }

    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    results
}

/// Handle `ccs session search` command
pub fn handle_session_search(
    keyword: &str,
    project_filter: Option<&str>,
    since: Option<&str>,
    context_chars: usize,
    limit: usize,
    user_only: bool,
    json_output: bool,
) -> Result<()> {
    // 1. Parse time filter
    let cutoff = if let Some(since_str) = since {
        Some(parse_duration_filter(since_str)?)
    } else {
        None
    };

    // 2. Scan and filter projects
    let projects = scan_all_projects()?;
    let filtered_projects: Vec<_> = if let Some(name) = project_filter {
        projects.into_iter().filter(|p| p.name == name).collect()
    } else {
        projects
    };

    if filtered_projects.is_empty() {
        if json_output {
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "query": keyword,
                    "total_matches": 0,
                    "memory_results": [],
                    "session_results": [],
                }))?
            );
        } else {
            println!("[0 results | query: \"{}\"]", keyword);
        }
        return Ok(());
    }

    // 3. Search memory files (no time filter - memory is persistent knowledge)
    let memory_results = search_memory_files(&filtered_projects, keyword, context_chars);

    // 4. Collect sessions with time filter
    let mut all_sessions = Vec::new();
    for project in &filtered_projects {
        let sessions = scan_project_sessions(project)?;
        for session in sessions {
            if let Some(ref cutoff_dt) = cutoff {
                if let Some(ref ts) = session.last_activity {
                    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(ts) {
                        if dt.with_timezone(&chrono::Utc) < *cutoff_dt {
                            continue;
                        }
                    }
                } else {
                    continue;
                }
            }
            all_sessions.push(session);
        }
    }

    // 5. Search sessions
    let session_results = search_sessions_full(&all_sessions, keyword, context_chars, user_only);

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
                    "project": r.summary.project_name,
                    "title": r.summary.title,
                    "last_activity": r.summary.last_activity,
                    "message_count": r.summary.message_count,
                    "matches": r.matches,
                })
            })
            .collect();

        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "query": keyword,
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
        println!("[0 results | query: \"{}\"]", keyword);
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
            keyword
        );
    } else if memory_match_count > 0 {
        println!(
            "[{} matches in memory | query: \"{}\"]",
            memory_match_count, keyword
        );
    } else {
        println!(
            "[{} matches in {} sessions | query: \"{}\"]",
            session_match_count,
            session_results.len(),
            keyword
        );
    }
    println!();

    let mut shown = 0;

    // Memory results first
    if !memory_results.is_empty() {
        if is_tty {
            println!("{}", "=== Memory ===".cyan().bold());
        } else {
            println!("=== Memory ===");
        }
        for result in &memory_results {
            if shown >= limit {
                break;
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
        for result in &session_results {
            if shown >= limit {
                break;
            }
            let time_str = result
                .summary
                .last_activity
                .as_ref()
                .map(|t| format_compact_relative_time(t))
                .unwrap_or_else(|| "?".to_string());

            let header = format!(
                "--- {} | {} | {} | {} | {} msgs ---",
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
