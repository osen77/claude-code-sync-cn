use anyhow::{Context, Result};
use colored::Colorize;
use inquire::Confirm;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::filter::FilterConfig;
use crate::history::{
    ConversationSummary, OperationHistory, OperationRecord, OperationType, SyncOperation,
};
use crate::interactive_conflict;
use crate::scm;
use crate::BINARY_NAME;

use super::discovery::{
    check_directory_structure_consistency, claude_projects_dir, discover_sessions,
    find_colliding_projects,
};
use super::state::SyncState;
use super::MAX_CONVERSATIONS_TO_DISPLAY;

/// Push local Claude Code history to sync repository
pub fn push_history(
    commit_message: Option<&str>,
    push_remote: bool,
    branch: Option<&str>,
    exclude_attachments: bool,
    sync_config: bool,
    interactive: bool,
    verbosity: crate::VerbosityLevel,
) -> Result<()> {
    use crate::VerbosityLevel;

    if verbosity != VerbosityLevel::Quiet {
        println!("{}", "Pushing Claude Code history...".cyan().bold());
    }

    let state = SyncState::load()?;
    let repo = scm::open(&state.sync_repo_path)?;
    let mut filter = FilterConfig::load()?;

    // Override exclude_attachments if specified in command
    if exclude_attachments {
        filter.exclude_attachments = true;
    }

    // Set up LFS if enabled
    if filter.enable_lfs {
        if verbosity != VerbosityLevel::Quiet {
            println!("  {} Git LFS...", "Configuring".cyan());
        }
        scm::lfs::setup(&state.sync_repo_path, &filter.lfs_patterns)
            .context("Failed to set up Git LFS")?;
    }

    let claude_dir = claude_projects_dir()?;

    // Check directory structure consistency before pushing
    let projects_dir = state.sync_repo_path.join(&filter.sync_subdirectory);
    if projects_dir.exists() {
        let structure_check =
            check_directory_structure_consistency(&projects_dir, filter.use_project_name_only);

        if !structure_check.is_consistent {
            if let Some(warning) = &structure_check.warning {
                println!();
                println!("{}", "⚠️  目录结构不一致警告".yellow().bold());
                println!("{}", "─".repeat(50).dimmed());
                println!("{}", warning.yellow());
                println!();

                if interactive && interactive_conflict::is_interactive() {
                    let proceed = Confirm::new("是否继续推送？")
                        .with_default(false)
                        .with_help_message("建议先清理目录结构再继续")
                        .prompt()
                        .context("取消确认")?;

                    if !proceed {
                        println!("\n{}", "推送已取消。".yellow());
                        println!(
                            "提示：使用 '{}' 可以切换同步模式",
                            format!("{} config --use-project-name-only <true|false>", BINARY_NAME).cyan()
                        );
                        return Ok(());
                    }
                } else if verbosity != VerbosityLevel::Quiet {
                    println!(
                        "{}",
                        "使用 --interactive 选项可以在不一致时选择是否继续".dimmed()
                    );
                }
            }
        }
    }

    // Get the current branch name for operation record
    let branch_name = branch
        .map(|s| s.to_string())
        .or_else(|| repo.current_branch().ok())
        .unwrap_or_else(|| "main".to_string());

    // Discover all sessions
    println!("  {} conversation sessions...", "Discovering".cyan());
    let sessions = discover_sessions(&claude_dir, &filter)?;
    println!("  {} {} sessions", "Found".green(), sessions.len());

    // Check for project name collisions when using project-name-only mode
    if filter.use_project_name_only {
        let collisions = find_colliding_projects(&claude_dir);
        if !collisions.is_empty() {
            println!();
            println!(
                "{}",
                "Warning: Multiple projects map to the same name:".yellow().bold()
            );
            for (name, paths) in &collisions {
                println!("  {} -> {} locations:", name.cyan(), paths.len());
                for path in paths.iter().take(3) {
                    let display_path = path.file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("unknown");
                    println!("    - {}", display_path);
                }
                if paths.len() > 3 {
                    println!("    ... and {} more", paths.len() - 3);
                }
            }
            println!();
            println!(
                "{}",
                "Sessions from colliding projects will be merged into the same directory.".yellow()
            );
            println!();
        }
    }

    // ============================================================================
    // COPY SESSIONS AND TRACK CHANGES
    // ============================================================================
    // Note: projects_dir was already defined above for consistency check
    fs::create_dir_all(&projects_dir)?;

    // Discover existing sessions in sync repo to determine operation type
    println!("  {} sessions to sync repository...", "Copying".cyan());
    let existing_sessions = discover_sessions(&projects_dir, &filter)?;
    let existing_map: HashMap<_, _> = existing_sessions
        .iter()
        .map(|s| (s.session_id.clone(), s))
        .collect();

    // Track pushed conversations for operation record
    let mut pushed_conversations: Vec<ConversationSummary> = Vec::new();
    let mut added_count = 0;
    let mut modified_count = 0;
    let mut unchanged_count = 0;

    // Track sessions skipped due to missing cwd
    let mut skipped_no_cwd = 0;

    // Mapping from local project dir -> sync repo project dir (for memory sync)
    let mut project_dir_to_sync: HashMap<PathBuf, PathBuf> = HashMap::new();

    // Closure to compute the relative path for a session, respecting use_project_name_only
    let compute_relative_path =
        |session: &crate::parser::ConversationSession| -> Option<PathBuf> {
            if filter.use_project_name_only {
                let full_relative = Path::new(&session.file_path)
                    .strip_prefix(&claude_dir)
                    .unwrap_or(Path::new(&session.file_path));

                let filename = full_relative.file_name()?;
                let project_name = session.project_name()?;
                Some(PathBuf::from(project_name).join(filename))
            } else {
                Some(
                    Path::new(&session.file_path)
                        .strip_prefix(&claude_dir)
                        .unwrap_or(Path::new(&session.file_path))
                        .to_path_buf(),
                )
            }
        };

    for session in &sessions {
        let relative_path = match compute_relative_path(session) {
            Some(path) => path,
            None => {
                skipped_no_cwd += 1;
                log::debug!("Skipping session {} (no cwd)", session.session_id);
                continue;
            }
        };

        // Build project dir mapping for memory sync (amortized during session loop)
        if let Some(sync_project_dir) = relative_path.parent() {
            if !sync_project_dir.as_os_str().is_empty() {
                let local_project_dir = Path::new(&session.file_path)
                    .parent()
                    .unwrap_or(Path::new(""));
                project_dir_to_sync
                    .entry(local_project_dir.to_path_buf())
                    .or_insert_with(|| sync_project_dir.to_path_buf());
            }
        }

        let dest_path = projects_dir.join(&relative_path);

        // Determine operation type based on existing state
        let operation = if let Some(existing) = existing_map.get(&session.session_id) {
            if existing.content_hash() == session.content_hash() {
                unchanged_count += 1;
                SyncOperation::Unchanged
            } else {
                modified_count += 1;
                SyncOperation::Modified
            }
        } else {
            added_count += 1;
            SyncOperation::Added
        };

        // Write the session file
        session.write_to_file(&dest_path)?;

        // Track this session in pushed conversations
        let relative_path_str = relative_path.to_string_lossy().to_string();
        match ConversationSummary::new(
            session.session_id.clone(),
            relative_path_str.clone(),
            session.latest_timestamp(),
            session.message_count(),
            operation,
        ) {
            Ok(summary) => pushed_conversations.push(summary),
            Err(e) => log::warn!(
                "Failed to create summary for {}: {}",
                relative_path_str,
                e
            ),
        }
    }

    // ============================================================================
    // SHOW SUMMARY AND INTERACTIVE CONFIRMATION
    // ============================================================================
    if verbosity != VerbosityLevel::Quiet {
        println!();
        println!("{}", "Push Summary:".bold().cyan());
        println!("  {} Added: {}", "•".green(), added_count);
        println!("  {} Modified: {}", "•".yellow(), modified_count);
        println!("  {} Unchanged: {}", "•".dimmed(), unchanged_count);
        let total_with_cwd = sessions.len().saturating_sub(skipped_no_cwd);
        println!("  {} Skipped (no cwd): {}", "•".dimmed(), skipped_no_cwd);
        println!(
            "  {} Sessions (with project context): {}",
            "•".cyan(),
            total_with_cwd
        );
        println!();
    }

    // Show detailed file list in verbose mode
    if verbosity == VerbosityLevel::Verbose {
        println!("{}", "Files to be pushed:".bold());
        for (idx, session) in sessions.iter().enumerate().take(20) {
            let Some(relative_path) = compute_relative_path(session) else {
                continue;
            };

            let status = if let Some(existing) = existing_map.get(&session.session_id) {
                if existing.content_hash() == session.content_hash() {
                    "unchanged".dimmed()
                } else {
                    "modified".yellow()
                }
            } else {
                "new".green()
            };

            println!("  {}. {} [{}]", idx + 1, relative_path.display(), status);
        }
        if sessions.len() > 20 {
            println!("  ... and {} more", sessions.len() - 20);
        }
        println!();
    }

    // Interactive confirmation
    if interactive && interactive_conflict::is_interactive() {
        let confirm = Confirm::new("Do you want to proceed with pushing these changes?")
            .with_default(true)
            .with_help_message("This will commit and push to the sync repository")
            .prompt()
            .context("Failed to get confirmation")?;

        if !confirm {
            println!("\n{}", "Push cancelled.".yellow());
            return Ok(());
        }
    }

    // ============================================================================
    // SYNC DEVICE CONFIGURATION (if enabled)
    // ============================================================================
    if sync_config && filter.config_sync.enabled && filter.config_sync.push_with_config {
        if verbosity != VerbosityLevel::Quiet {
            println!();
            println!("  {} device configuration...", "Syncing".cyan());
        }

        // Use config_sync handler to push configuration files (no commit)
        match crate::handlers::config_sync::push_config_files(&filter.config_sync) {
            Ok(synced_files) => {
                if !synced_files.is_empty() {
                    if verbosity != VerbosityLevel::Quiet {
                        println!("  {} Device configuration synced:", "✓".green());
                        for file in &synced_files {
                            println!("    - {}", file.dimmed());
                        }
                    }
                } else if verbosity == VerbosityLevel::Verbose {
                    println!("  {} No configuration files to sync", "ℹ".dimmed());
                }
            }
            Err(e) => {
                log::warn!("Failed to sync device configuration: {}", e);
                if verbosity != VerbosityLevel::Quiet {
                    println!(
                        "  {} Failed to sync device configuration: {}",
                        "⚠".yellow(),
                        e
                    );
                }
            }
        }
    }

    // ============================================================================
    // REMOVE LOCALLY DELETED SESSIONS FROM SYNC REPO
    // ============================================================================
    // Compare sync repo files against local files to detect deletions.
    // Only remove files from sync repo project dirs that have a corresponding
    // local project dir — this prevents deleting sessions pushed by other devices.
    let mut deleted_from_repo = 0;

    {
        // Build a set of local session file names grouped by project dir name
        // (the encoded directory name under ~/.claude/projects/)
        let mut local_files_by_project: HashMap<String, std::collections::HashSet<String>> =
            HashMap::new();

        if let Ok(entries) = fs::read_dir(&claude_dir) {
            for entry in entries.filter_map(|e| e.ok()) {
                let local_project_dir = entry.path();
                if !local_project_dir.is_dir() {
                    continue;
                }
                let dir_name = local_project_dir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default()
                    .to_string();
                if dir_name.starts_with('.') {
                    continue;
                }

                let mut file_names = std::collections::HashSet::new();
                if let Ok(files) = fs::read_dir(&local_project_dir) {
                    for file in files.filter_map(|f| f.ok()) {
                        if let Some(name) = file.file_name().to_str() {
                            if name.ends_with(".jsonl") {
                                file_names.insert(name.to_string());
                            }
                        }
                    }
                }
                local_files_by_project.insert(dir_name, file_names);
            }
        }

        // Now scan sync repo project dirs and find files to remove.
        // For use_project_name_only mode, we need to map project names back to
        // local project dirs. We use the already-discovered sessions to build this mapping.
        if filter.use_project_name_only {
            // Build mapping: project_name -> set of local file names (union of all matching dirs)
            let mut local_files_by_name: HashMap<String, std::collections::HashSet<String>> =
                HashMap::new();
            // Track which project names have a local dir present
            let mut project_name_has_local: std::collections::HashSet<String> =
                std::collections::HashSet::new();

            for session in &sessions {
                if let Some(pname) = session.project_name() {
                    project_name_has_local.insert(pname.to_string());
                    let fname = Path::new(&session.file_path)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or_default()
                        .to_string();
                    local_files_by_name
                        .entry(pname.to_string())
                        .or_default()
                        .insert(fname);
                }
            }
            // Scan sync repo
            if let Ok(entries) = fs::read_dir(&projects_dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let sync_project_dir = entry.path();
                    if !sync_project_dir.is_dir() {
                        continue;
                    }
                    let project_name = sync_project_dir
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or_default()
                        .to_string();

                    // Only process projects that exist locally
                    if !project_name_has_local.contains(&project_name) {
                        continue;
                    }

                    let local_files = local_files_by_name
                        .get(&project_name)
                        .cloned()
                        .unwrap_or_default();

                    if let Ok(files) = fs::read_dir(&sync_project_dir) {
                        for file in files.filter_map(|f| f.ok()) {
                            let fname = file.file_name().to_string_lossy().to_string();
                            if fname.ends_with(".jsonl") && !local_files.contains(&fname) {
                                let file_path = file.path();
                                if let Err(e) = fs::remove_file(&file_path) {
                                    log::warn!("Failed to remove deleted session: {}", e);
                                } else {
                                    deleted_from_repo += 1;
                                    log::debug!("Removed deleted session: {}", file_path.display());
                                }
                            }
                        }
                    }
                }
            }
        } else {
            // Full-path mode: sync repo dir names match local dir names exactly
            if let Ok(entries) = fs::read_dir(&projects_dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let sync_project_dir = entry.path();
                    if !sync_project_dir.is_dir() {
                        continue;
                    }
                    let dir_name = sync_project_dir
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or_default()
                        .to_string();

                    // Only process dirs that exist locally
                    let Some(local_files) = local_files_by_project.get(&dir_name) else {
                        continue;
                    };

                    if let Ok(files) = fs::read_dir(&sync_project_dir) {
                        for file in files.filter_map(|f| f.ok()) {
                            let fname = file.file_name().to_string_lossy().to_string();
                            if fname.ends_with(".jsonl") && !local_files.contains(&fname) {
                                let file_path = file.path();
                                if let Err(e) = fs::remove_file(&file_path) {
                                    log::warn!("Failed to remove deleted session: {}", e);
                                } else {
                                    deleted_from_repo += 1;
                                    log::debug!("Removed deleted session: {}", file_path.display());
                                }
                            }
                        }
                    }
                }
            }
        }

        if deleted_from_repo > 0 && verbosity != VerbosityLevel::Quiet {
            println!(
                "  {} Removed {} deleted sessions from sync repo",
                "✓".green(),
                deleted_from_repo
            );
        }
    }

    // ============================================================================
    // SYNC AUTO MEMORY DIRECTORIES
    // ============================================================================
    if filter.auto_memory.enabled {
        if verbosity != VerbosityLevel::Quiet {
            println!();
            println!("  {} auto memory directories...", "Syncing".cyan());
        }

        // project_dir_to_sync was built during session loop above.
        // NOTE: We cannot use extract_project_name() because it splits by '-'
        // and fails for project names containing hyphens (e.g. "claude-openclaw").
        let mut synced_count = 0;
        // Collect local memory file names per sync project during copy,
        // so we can detect deletions without re-scanning directories.
        let mut local_memory_by_sync: HashMap<PathBuf, std::collections::HashSet<std::ffi::OsString>> =
            HashMap::new();
        for (local_dir, sync_project) in &project_dir_to_sync {
            let local_memory = local_dir.join("memory");
            if !local_memory.is_dir() {
                continue;
            }

            let dest_memory_dir = projects_dir.join(sync_project).join("memory");

            // Create destination directory
            if let Err(e) = fs::create_dir_all(&dest_memory_dir) {
                log::warn!(
                    "Failed to create memory directory for {}: {}",
                    sync_project.display(),
                    e
                );
                continue;
            }

            // Copy memory files and collect names for deletion detection
            let file_set = local_memory_by_sync
                .entry(sync_project.clone())
                .or_default();
            if let Ok(entries) = fs::read_dir(&local_memory) {
                for entry in entries.filter_map(|e| e.ok()) {
                    if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                        file_set.insert(entry.file_name());
                        let dest_file = dest_memory_dir.join(entry.file_name());
                        if let Err(e) = fs::copy(entry.path(), &dest_file) {
                            log::warn!("Failed to copy memory file: {}", e);
                        }
                    }
                }
            }

            synced_count += 1;
            if verbosity == VerbosityLevel::Verbose {
                println!(
                    "    {} {}",
                    "→".cyan(),
                    sync_project.join("memory").display()
                );
            }
        }

        if synced_count > 0 {
            if verbosity != VerbosityLevel::Quiet {
                println!("  {} Synced {} memory directories", "✓".green(), synced_count);
            }
        } else if verbosity == VerbosityLevel::Verbose {
            println!("  {} No memory directories found", "ℹ".dimmed());
        }

        // Remove remote memory files that no longer exist locally.
        // local_memory_by_sync was populated during the copy phase above.
        let mut deleted_memory_count = 0;
        for (sync_project, local_files) in &local_memory_by_sync {
            let remote_memory = projects_dir.join(sync_project).join("memory");
            if !remote_memory.is_dir() {
                continue;
            }

            if let Ok(entries) = fs::read_dir(&remote_memory) {
                for entry in entries.filter_map(|e| e.ok()) {
                    if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                        let file_name = entry.file_name();
                        if !local_files.contains(&file_name) {
                            if let Err(e) = fs::remove_file(entry.path()) {
                                log::warn!("Failed to remove deleted memory file: {}", e);
                            } else {
                                deleted_memory_count += 1;
                            }
                        }
                    }
                }
            }
        }

        if deleted_memory_count > 0 && verbosity != VerbosityLevel::Quiet {
            println!(
                "  {} Removed {} deleted memory files from sync repo",
                "✓".green(),
                deleted_memory_count
            );
        }
    }

    // ============================================================================
    // COMMIT AND PUSH CHANGES
    // ============================================================================
    repo.stage_all()?;

    let has_changes = repo.has_changes()?;
    if has_changes {
        // Get the current commit hash before making any changes
        // This allows us to undo the push later by resetting to this commit
        // Note: We don't create file snapshots for push - git already has history!
        // Undo push simply does `git reset` to this commit.
        // On a brand new repo with no commits, this will be None (no undo available for first push)
        let commit_before_push = repo.current_commit_hash().ok();

        if let Some(ref hash) = commit_before_push {
            if verbosity != VerbosityLevel::Quiet {
                println!(
                    "  {} Recorded commit {} for undo",
                    "✓".green(),
                    &hash[..8]
                );
            }
        } else if verbosity != VerbosityLevel::Quiet {
            println!(
                "  {} First push - no previous commit to undo to",
                "ℹ".cyan()
            );
        }

        let default_message = format!(
            "Sync {} sessions at {}",
            sessions.len(),
            chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
        );
        let message = commit_message.unwrap_or(&default_message);

        println!("  {} changes...", "Committing".cyan());
        repo.commit(message)?;
        println!("  {} Committed: {}", "✓".green(), message);

        // Push to remote if configured
        if push_remote && state.has_remote {
            println!("  {} to remote...", "Pushing".cyan());

            match repo.push("origin", &branch_name) {
                Ok(_) => println!("  {} Pushed to origin/{}", "✓".green(), branch_name),
                Err(e) => log::warn!("Failed to push: {}", e),
            }
        }

        // ============================================================================
        // CREATE AND SAVE OPERATION RECORD
        // ============================================================================
        let mut operation_record = OperationRecord::new(
            OperationType::Push,
            Some(branch_name.clone()),
            pushed_conversations.clone(),
        );

        // Store commit hash for undo (no file snapshot needed - git has history)
        // On first push (no prior commits), this will be None
        operation_record.commit_hash = commit_before_push;

        // Load operation history and add this operation
        let mut history = match OperationHistory::load() {
            Ok(h) => h,
            Err(e) => {
                log::warn!("Failed to load operation history: {}", e);
                log::info!("Creating new history...");
                OperationHistory::default()
            }
        };

        if let Err(e) = history.add_operation(operation_record) {
            log::warn!("Failed to save operation to history: {}", e);
            log::info!("Push completed successfully, but history was not updated.");
        }
    } else {
        println!("  {} No changes to commit", "Note:".yellow());
    }

    // ============================================================================
    // DISPLAY SUMMARY TO USER
    // ============================================================================
    println!("\n{}", "=== Push Summary ===".bold().cyan());

    // Show operation statistics
    let stats_msg = if deleted_from_repo > 0 {
        format!(
            "  {} Added    {} Modified    {} Deleted    {} Unchanged",
            format!("{added_count}").green(),
            format!("{modified_count}").cyan(),
            format!("{deleted_from_repo}").red(),
            format!("{unchanged_count}").dimmed(),
        )
    } else {
        format!(
            "  {} Added    {} Modified    {} Unchanged",
            format!("{added_count}").green(),
            format!("{modified_count}").cyan(),
            format!("{unchanged_count}").dimmed(),
        )
    };
    println!("{stats_msg}");
    println!();

    // Group conversations by project (top-level directory)
    let mut by_project: HashMap<String, Vec<&ConversationSummary>> = HashMap::new();
    for conv in &pushed_conversations {
        // Skip unchanged conversations in detailed output
        if conv.operation == SyncOperation::Unchanged {
            continue;
        }

        let project = conv
            .project_path
            .split('/')
            .next()
            .unwrap_or("unknown")
            .to_string();
        by_project.entry(project).or_default().push(conv);
    }

    // Display conversations grouped by project
    if !by_project.is_empty() {
        println!("{}", "Pushed Conversations:".bold());

        let mut projects: Vec<_> = by_project.keys().collect();
        projects.sort();

        for project in projects {
            let conversations = &by_project[project];
            println!("\n  {} {}/", "Project:".bold(), project.cyan());

            for conv in conversations.iter().take(MAX_CONVERSATIONS_TO_DISPLAY) {
                let operation_str = match conv.operation {
                    SyncOperation::Added => "ADD".green(),
                    SyncOperation::Modified => "MOD".cyan(),
                    SyncOperation::Conflict => "CONFLICT".yellow(),
                    SyncOperation::Unchanged => "---".dimmed(),
                };

                let timestamp_str = conv
                    .timestamp
                    .as_ref()
                    .and_then(|t| {
                        // Extract just the date portion for compact display
                        t.split('T').next()
                    })
                    .unwrap_or("unknown");

                println!(
                    "    {} {} ({}msg, {})",
                    operation_str,
                    conv.project_path,
                    conv.message_count,
                    timestamp_str.dimmed()
                );
            }

            if conversations.len() > MAX_CONVERSATIONS_TO_DISPLAY {
                println!(
                    "    {} ... and {} more conversations",
                    "...".dimmed(),
                    conversations.len() - MAX_CONVERSATIONS_TO_DISPLAY
                );
            }
        }
    }

    if verbosity == VerbosityLevel::Quiet {
        println!("Push complete");
    } else {
        println!("\n{}", "Push complete!".green().bold());
    }

    // Clean up old snapshots automatically
    if let Err(e) = crate::undo::cleanup_old_snapshots(None, false) {
        log::warn!("Failed to cleanup old snapshots: {}", e);
    }

    Ok(())
}
