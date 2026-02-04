//! Claude Code hooks management
//!
//! This module handles installation and management of Claude Code hooks
//! for automatic synchronization.

use anyhow::{Context, Result};
use colored::Colorize;
use serde_json::{json, Value};
use std::path::PathBuf;

/// Identifier for hooks installed by claude-code-sync
const HOOK_MARKER_COMMENT: &str = "claude-code-sync";

/// Get the path to Claude settings file
fn claude_settings_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Cannot find home directory")?;
    Ok(home.join(".claude").join("settings.json"))
}

/// Get the hooks configuration to install
fn get_hooks_config() -> Value {
    json!({
        "SessionStart": [
            {
                "hooks": [
                    {
                        "type": "command",
                        "command": "claude-code-sync hook-session-start",
                        "timeout": 60,
                        "statusMessage": "Syncing conversation history..."
                    }
                ]
            }
        ],
        "Stop": [
            {
                "hooks": [
                    {
                        "type": "command",
                        "command": "claude-code-sync hook-stop",
                        "timeout": 60
                    }
                ]
            }
        ],
        "UserPromptSubmit": [
            {
                "hooks": [
                    {
                        "type": "command",
                        "command": "claude-code-sync hook-new-project-check",
                        "timeout": 30
                    }
                ]
            }
        ]
    })
}

/// Check if a hook array contains a claude-code-sync hook
fn contains_our_hook(hooks_array: &[Value], command_pattern: &str) -> bool {
    hooks_array.iter().any(|group| {
        group
            .get("hooks")
            .and_then(|h| h.as_array())
            .map(|hooks| {
                hooks.iter().any(|hook| {
                    hook.get("command")
                        .and_then(|c| c.as_str())
                        .map(|cmd| cmd.contains(command_pattern))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    })
}

/// Install hooks to ~/.claude/settings.json
pub fn handle_hooks_install() -> Result<()> {
    let settings_path = claude_settings_path()?;

    println!(
        "{}",
        "Installing Claude Code hooks...".cyan().bold()
    );

    // Read existing settings or create new
    let mut settings: Value = if settings_path.exists() {
        let content = std::fs::read_to_string(&settings_path)?;
        serde_json::from_str(&content).unwrap_or(json!({}))
    } else {
        json!({})
    };

    // Ensure hooks object exists
    if settings.get("hooks").is_none() {
        settings["hooks"] = json!({});
    }

    let hooks_to_add = get_hooks_config();
    let hooks_obj = settings
        .get_mut("hooks")
        .and_then(|v| v.as_object_mut())
        .context("Failed to access hooks object")?;

    // Merge each hook type
    for (event_name, new_hooks) in hooks_to_add.as_object().unwrap() {
        let new_hooks_array = new_hooks.as_array().unwrap();

        if let Some(existing) = hooks_obj.get_mut(event_name) {
            // Check if our hook already exists
            if let Some(existing_array) = existing.as_array() {
                if contains_our_hook(existing_array, HOOK_MARKER_COMMENT) {
                    println!(
                        "  {} {} hook already installed",
                        "!".yellow(),
                        event_name
                    );
                    continue;
                }
            }

            // Append our hooks to existing array
            if let Some(existing_array) = existing.as_array_mut() {
                for hook in new_hooks_array {
                    existing_array.push(hook.clone());
                }
                println!("  {} {} hook added", "✓".green(), event_name);
            }
        } else {
            // Create new hook array
            hooks_obj.insert(event_name.clone(), new_hooks.clone());
            println!("  {} {} hook installed", "✓".green(), event_name);
        }
    }

    // Write back
    std::fs::create_dir_all(settings_path.parent().unwrap())?;
    std::fs::write(&settings_path, serde_json::to_string_pretty(&settings)?)?;

    println!(
        "\n{} Hooks installed to {}",
        "✓".green(),
        settings_path.display()
    );

    Ok(())
}

/// Uninstall hooks from ~/.claude/settings.json
pub fn handle_hooks_uninstall() -> Result<()> {
    let settings_path = claude_settings_path()?;

    if !settings_path.exists() {
        println!("{}", "No settings file found, nothing to uninstall.".yellow());
        return Ok(());
    }

    println!(
        "{}",
        "Removing Claude Code hooks...".cyan().bold()
    );

    let content = std::fs::read_to_string(&settings_path)?;
    let mut settings: Value = serde_json::from_str(&content)?;

    if let Some(hooks_obj) = settings.get_mut("hooks").and_then(|v| v.as_object_mut()) {
        let mut removed_count = 0;

        // Remove our hooks from each event type (including legacy SessionEnd)
        for event_name in &["SessionStart", "Stop", "SessionEnd", "UserPromptSubmit"] {
            if let Some(hooks_array) = hooks_obj.get_mut(*event_name).and_then(|v| v.as_array_mut())
            {
                let original_len = hooks_array.len();

                // Filter out our hooks
                hooks_array.retain(|group| {
                    !group
                        .get("hooks")
                        .and_then(|h| h.as_array())
                        .map(|hooks| {
                            hooks.iter().any(|hook| {
                                hook.get("command")
                                    .and_then(|c| c.as_str())
                                    .map(|cmd| cmd.contains(HOOK_MARKER_COMMENT))
                                    .unwrap_or(false)
                            })
                        })
                        .unwrap_or(false)
                });

                if hooks_array.len() < original_len {
                    removed_count += original_len - hooks_array.len();
                    println!("  {} Removed {} hook", "✓".green(), event_name);
                }

                // Remove empty arrays
                if hooks_array.is_empty() {
                    hooks_obj.remove(*event_name);
                }
            }
        }

        if removed_count == 0 {
            println!("{}", "No claude-code-sync hooks found to remove.".yellow());
        } else {
            // Write back
            std::fs::write(&settings_path, serde_json::to_string_pretty(&settings)?)?;
            println!("\n{} {} hook(s) removed", "✓".green(), removed_count);
        }
    } else {
        println!("{}", "No hooks configured, nothing to uninstall.".yellow());
    }

    Ok(())
}

/// Show current hooks configuration status
pub fn handle_hooks_show() -> Result<()> {
    let settings_path = claude_settings_path()?;

    println!("{}", "Claude Code Hooks Status".cyan().bold());
    println!("Settings file: {}", settings_path.display());
    println!();

    if !settings_path.exists() {
        println!("{}", "No settings file found.".yellow());
        println!();
        println!("Run '{}' to install hooks.", "claude-code-sync hooks install".cyan());
        return Ok(());
    }

    let content = std::fs::read_to_string(&settings_path)?;
    let settings: Value = serde_json::from_str(&content)?;

    let hooks_installed = if let Some(hooks_obj) = settings.get("hooks").and_then(|v| v.as_object())
    {
        let mut found = Vec::new();

        // Check SessionStart
        if let Some(hooks_array) = hooks_obj.get("SessionStart").and_then(|v| v.as_array()) {
            if contains_our_hook(hooks_array, "claude-code-sync hook-session-start") {
                found.push("SessionStart");
            }
        }

        // Check Stop
        if let Some(hooks_array) = hooks_obj.get("Stop").and_then(|v| v.as_array()) {
            if contains_our_hook(hooks_array, "claude-code-sync hook-stop") {
                found.push("Stop");
            }
        }

        // Check UserPromptSubmit
        if let Some(hooks_array) = hooks_obj.get("UserPromptSubmit").and_then(|v| v.as_array()) {
            if contains_our_hook(hooks_array, "claude-code-sync hook-new-project-check") {
                found.push("UserPromptSubmit");
            }
        }

        found
    } else {
        Vec::new()
    };

    if hooks_installed.is_empty() {
        println!("{}", "claude-code-sync hooks: NOT installed".yellow());
        println!();
        println!("Run '{}' to install hooks.", "claude-code-sync hooks install".cyan());
    } else {
        println!("{}", "claude-code-sync hooks: INSTALLED".green());
        println!();
        println!("Installed hooks:");
        for hook in &hooks_installed {
            let description = match *hook {
                "SessionStart" => "Pull on startup (IDE support)",
                "Stop" => "Push after each response",
                "UserPromptSubmit" => "New project detection",
                _ => "",
            };
            println!("  {} {} ({})", "•".green(), hook.cyan(), description);
        }

        if hooks_installed.len() < 3 {
            println!();
            println!(
                "{}",
                "Note: Some hooks are missing. Run 'claude-code-sync hooks install' to reinstall."
                    .yellow()
            );
        }
    }

    Ok(())
}

/// Handle the hook-new-project-check command
/// This is called by the UserPromptSubmit hook to detect new projects
/// Reads JSON from stdin, outputs JSON to stdout
pub fn handle_new_project_check() -> Result<()> {
    use crate::sync::discovery::{claude_projects_dir, find_local_project_by_name};

    // Read hook input from stdin
    let input: Value = serde_json::from_reader(std::io::stdin())
        .context("Failed to read hook input from stdin")?;

    let cwd = match input.get("cwd").and_then(|v| v.as_str()) {
        Some(cwd) => cwd,
        None => {
            // No cwd provided, silently exit
            return Ok(());
        }
    };

    // Extract project name from cwd (handle both Unix and Windows paths)
    let project_name = cwd
        .split(&['/', '\\'])
        .filter(|s| !s.is_empty())
        .last()
        .unwrap_or("unknown");

    let claude_dir = match claude_projects_dir() {
        Ok(dir) => dir,
        Err(_) => return Ok(()), // Silently exit if we can't find the projects dir
    };

    // Check if local project directory exists
    let has_local_project = find_local_project_by_name(&claude_dir, project_name).is_some();

    if !has_local_project {
        // This is a new project, try to pull from remote
        log::info!("New project detected: {}", project_name);

        // Execute pull quietly - we use a separate process to avoid blocking
        // and to ensure clean error handling
        let pull_result = std::process::Command::new("claude-code-sync")
            .args(["pull", "--quiet"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();

        if pull_result.is_ok() {
            // Check if we now have a local project after pull
            if find_local_project_by_name(&claude_dir, project_name).is_some() {
                // Found remote history, notify user via hook output
                let output = json!({
                    "additionalContext": format!(
                        "Detected remote conversation history for project '{}'. \
                         It has been pulled. Consider running /clear or restarting \
                         Claude Code to load the history.",
                        project_name
                    )
                });
                println!("{}", serde_json::to_string(&output)?);
            }
        }
    }

    Ok(())
}

/// Handle the hook-stop command
/// This is called by the Stop hook after each AI response to push history
/// Reads JSON from stdin
pub fn handle_stop() -> Result<()> {
    use std::io::Write;

    // Log hook execution for debugging
    if let Ok(home) = std::env::var("HOME") {
        let debug_log = std::path::PathBuf::from(&home)
            .join("Library/Application Support/claude-code-sync/hook-debug.log");
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&debug_log)
        {
            let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
            let _ = writeln!(file, "[{}] Stop hook executed", timestamp);
        }
    }

    // Read hook input from stdin (required by Claude Code hooks)
    let _input: Value = serde_json::from_reader(std::io::stdin())
        .unwrap_or(json!({}));

    // Execute push quietly after each response
    let push_result = std::process::Command::new("claude-code-sync")
        .args(["push", "--quiet"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    // Log result
    if let Ok(home) = std::env::var("HOME") {
        let debug_log = std::path::PathBuf::from(&home)
            .join("Library/Application Support/claude-code-sync/hook-debug.log");
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&debug_log)
        {
            let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
            match &push_result {
                Ok(status) => {
                    let _ = writeln!(file, "[{}] Stop push completed: exit code {}", timestamp, status);
                }
                Err(e) => {
                    let _ = writeln!(file, "[{}] Stop push failed: {}", timestamp, e);
                }
            }
        }
    }

    // Also sync config if enabled
    if let Ok(filter) = crate::filter::FilterConfig::load() {
        if filter.config_sync.enabled {
            let _ = super::config_sync::handle_config_push(&filter.config_sync);
        }
    }

    Ok(())
}

/// Debounce interval for SessionStart pull (in seconds)
/// Extra protection layer to prevent duplicate pulls
const SESSION_START_DEBOUNCE_SECS: u64 = 300; // 5 minutes

/// Count running Claude Code processes
/// Uses ps + grep to detect Claude Code native-binary processes
fn count_claude_processes() -> usize {
    let output = std::process::Command::new("sh")
        .args(["-c", "ps aux | grep 'native-binary/claude' | grep -v grep | wc -l"])
        .output();

    match output {
        Ok(out) => {
            String::from_utf8_lossy(&out.stdout)
                .trim()
                .parse()
                .unwrap_or(0)
        }
        Err(_) => 0 // If detection fails, assume first start
    }
}

/// Handle the hook-session-start command
/// This is called by the SessionStart hook to pull latest history
/// Reads JSON from stdin, outputs JSON to stdout
///
/// Uses triple-condition detection to only pull on first startup:
/// 1. Process count = 1 (no other Claude instances)
/// 2. source = "startup" (not resume/compact)
/// 3. Debounce not active (extra protection)
pub fn handle_session_start() -> Result<()> {
    use std::io::Write;

    // Read hook input from stdin (required by Claude Code hooks)
    let input: Value = serde_json::from_reader(std::io::stdin())
        .unwrap_or(json!({}));

    // Extract source field
    let source = input
        .get("source")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    // Count Claude Code processes
    let process_count = count_claude_processes();
    let is_first_instance = process_count <= 1;
    let is_startup = source == "startup";

    // Get timestamp file path for debouncing
    let timestamp_file = crate::config::ConfigManager::config_dir()
        .map(|d| d.join("last-session-pull"));

    // Check debounce
    let debounce_active = if let Ok(ref ts_path) = timestamp_file {
        if ts_path.exists() {
            if let Ok(metadata) = std::fs::metadata(ts_path) {
                if let Ok(modified) = metadata.modified() {
                    let elapsed = std::time::SystemTime::now()
                        .duration_since(modified)
                        .unwrap_or_default();
                    elapsed.as_secs() < SESSION_START_DEBOUNCE_SECS
                } else {
                    false
                }
            } else {
                false
            }
        } else {
            false
        }
    } else {
        false
    };

    // Log hook execution with all conditions
    if let Ok(home) = std::env::var("HOME") {
        let debug_log = std::path::PathBuf::from(&home)
            .join("Library/Application Support/claude-code-sync/hook-debug.log");
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&debug_log)
        {
            let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
            let _ = writeln!(
                file,
                "[{}] SessionStart (source: {}, processes: {}, debounce: {})",
                timestamp, source, process_count, debounce_active
            );
        }
    }

    // Triple-condition check: first instance + startup + no debounce
    if !is_first_instance {
        if let Ok(home) = std::env::var("HOME") {
            let debug_log = std::path::PathBuf::from(&home)
                .join("Library/Application Support/claude-code-sync/hook-debug.log");
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&debug_log)
            {
                let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
                let _ = writeln!(file, "[{}] pull skipped (other instances: {})", timestamp, process_count);
            }
        }
        return Ok(());
    }

    if !is_startup {
        if let Ok(home) = std::env::var("HOME") {
            let debug_log = std::path::PathBuf::from(&home)
                .join("Library/Application Support/claude-code-sync/hook-debug.log");
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&debug_log)
            {
                let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
                let _ = writeln!(file, "[{}] pull skipped (source: {} != startup)", timestamp, source);
            }
        }
        return Ok(());
    }

    if debounce_active {
        if let Ok(home) = std::env::var("HOME") {
            let debug_log = std::path::PathBuf::from(&home)
                .join("Library/Application Support/claude-code-sync/hook-debug.log");
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&debug_log)
            {
                let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
                let _ = writeln!(file, "[{}] pull skipped (debounce active)", timestamp);
            }
        }
        return Ok(());
    }

    // Update timestamp file before pull
    if let Ok(ref ts_path) = timestamp_file {
        let _ = std::fs::write(ts_path, "");
    }

    // Execute pull quietly (first start confirmed)
    let pull_result = std::process::Command::new("claude-code-sync")
        .args(["pull", "--quiet"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    // Log result
    if let Ok(home) = std::env::var("HOME") {
        let debug_log = std::path::PathBuf::from(&home)
            .join("Library/Application Support/claude-code-sync/hook-debug.log");
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&debug_log)
        {
            let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
            match &pull_result {
                Ok(status) => {
                    let _ = writeln!(file, "[{}] SessionStart pull completed: exit code {}", timestamp, status);
                }
                Err(e) => {
                    let _ = writeln!(file, "[{}] SessionStart pull failed: {}", timestamp, e);
                }
            }
        }
    }

    // If pull succeeded and we got new content, we could notify the user
    // But for SessionStart, we just silently sync - the user will see the history
    if let Err(e) = &pull_result {
        log::debug!("SessionStart pull failed: {}", e);
    }

    // Auto-apply CLAUDE.md after pull
    if let Ok(filter) = crate::filter::FilterConfig::load() {
        if filter.config_sync.enabled && filter.config_sync.auto_apply_claude_md {
            let _ = super::config_sync::auto_apply_claude_md(&filter.config_sync);
        }
    }

    // Exit successfully - no output needed for SessionStart unless we want to add context
    Ok(())
}

/// Check if hooks are installed
pub fn are_hooks_installed() -> Result<bool> {
    let settings_path = claude_settings_path()?;

    if !settings_path.exists() {
        return Ok(false);
    }

    let content = std::fs::read_to_string(&settings_path)?;
    let settings: Value = serde_json::from_str(&content)?;

    if let Some(hooks_obj) = settings.get("hooks").and_then(|v| v.as_object()) {
        // Check all required hooks
        let has_session_start = hooks_obj
            .get("SessionStart")
            .and_then(|v| v.as_array())
            .map(|arr| contains_our_hook(arr, "claude-code-sync hook-session-start"))
            .unwrap_or(false);

        let has_stop = hooks_obj
            .get("Stop")
            .and_then(|v| v.as_array())
            .map(|arr| contains_our_hook(arr, "claude-code-sync hook-stop"))
            .unwrap_or(false);

        let has_prompt_submit = hooks_obj
            .get("UserPromptSubmit")
            .and_then(|v| v.as_array())
            .map(|arr| contains_our_hook(arr, "claude-code-sync hook-new-project-check"))
            .unwrap_or(false);

        Ok(has_session_start && has_stop && has_prompt_submit)
    } else {
        Ok(false)
    }
}
