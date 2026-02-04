//! Setup wizard handler
//!
//! Provides an interactive setup wizard for first-time configuration.
//! This is a simplified, user-friendly alternative to the `init` command.

use anyhow::{Context, Result};
use colored::Colorize;
use inquire::{Confirm, Select, Text};
use std::path::PathBuf;
use std::process::Command;

use crate::config::ConfigManager;
use crate::filter::FilterConfig;
use crate::scm;
use crate::sync;

/// Sync mode options
#[derive(Debug, Clone)]
enum SyncMode {
    MultiDevice,
    SingleDevice,
}

impl std::fmt::Display for SyncMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncMode::MultiDevice => write!(f, "多设备同步 (推荐) - 支持不同电脑同步同一项目"),
            SyncMode::SingleDevice => write!(f, "单设备备份 - 仅本机备份，使用完整路径"),
        }
    }
}

/// Repository source options
#[derive(Debug, Clone)]
enum RepoSource {
    Existing,
    CreateNew,
}

impl std::fmt::Display for RepoSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RepoSource::Existing => write!(f, "使用已有仓库 - 输入仓库地址"),
            RepoSource::CreateNew => write!(f, "创建新仓库 - 自动在 GitHub 创建"),
        }
    }
}

/// Check if gh CLI is installed
fn is_gh_installed() -> bool {
    Command::new("gh")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Check if gh is authenticated
fn is_gh_authenticated() -> bool {
    Command::new("gh")
        .args(["auth", "status"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Get current OS type
fn get_os() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "unknown"
    }
}

/// Install gh CLI based on OS
fn install_gh_cli() -> Result<()> {
    let os = get_os();

    println!("{}", "📦 正在安装 GitHub CLI (gh)...".cyan());
    println!();

    let (cmd, args): (&str, Vec<&str>) = match os {
        "macos" => {
            println!("{}", "   使用 Homebrew 安装...".cyan());
            // Check if brew is installed
            if !Command::new("brew").arg("--version").output().map(|o| o.status.success()).unwrap_or(false) {
                return Err(anyhow::anyhow!(
                    "未安装 Homebrew。请先安装: /bin/bash -c \"$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)\""
                ));
            }
            ("brew", vec!["install", "gh"])
        }
        "linux" => {
            // Try to detect package manager
            if Command::new("apt-get").arg("--version").output().map(|o| o.status.success()).unwrap_or(false) {
                println!("{}", "   使用 apt 安装...".cyan());
                // Need to add GitHub's apt repository first
                println!("{}", "   添加 GitHub APT 源...".cyan());

                let add_key = Command::new("sh")
                    .args(["-c", "curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg | sudo dd of=/usr/share/keyrings/githubcli-archive-keyring.gpg"])
                    .status();

                if add_key.is_err() {
                    return Err(anyhow::anyhow!("添加 GitHub GPG key 失败"));
                }

                let add_repo = Command::new("sh")
                    .args(["-c", "echo \"deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main\" | sudo tee /etc/apt/sources.list.d/github-cli.list > /dev/null"])
                    .status();

                if add_repo.is_err() {
                    return Err(anyhow::anyhow!("添加 GitHub APT 源失败"));
                }

                // Update and install
                let _ = Command::new("sudo").args(["apt-get", "update"]).status();
                ("sudo", vec!["apt-get", "install", "-y", "gh"])
            } else if Command::new("dnf").arg("--version").output().map(|o| o.status.success()).unwrap_or(false) {
                println!("{}", "   使用 dnf 安装...".cyan());
                ("sudo", vec!["dnf", "install", "-y", "gh"])
            } else if Command::new("pacman").arg("--version").output().map(|o| o.status.success()).unwrap_or(false) {
                println!("{}", "   使用 pacman 安装...".cyan());
                ("sudo", vec!["pacman", "-S", "--noconfirm", "github-cli"])
            } else {
                return Err(anyhow::anyhow!(
                    "未检测到支持的包管理器。请手动安装 gh: https://github.com/cli/cli#installation"
                ));
            }
        }
        "windows" => {
            // Try winget first, then scoop
            if Command::new("winget").arg("--version").output().map(|o| o.status.success()).unwrap_or(false) {
                println!("{}", "   使用 winget 安装...".cyan());
                ("winget", vec!["install", "--id", "GitHub.cli", "-e"])
            } else if Command::new("scoop").arg("--version").output().map(|o| o.status.success()).unwrap_or(false) {
                println!("{}", "   使用 scoop 安装...".cyan());
                ("scoop", vec!["install", "gh"])
            } else {
                return Err(anyhow::anyhow!(
                    "未检测到 winget 或 scoop。请手动安装 gh: https://github.com/cli/cli#installation"
                ));
            }
        }
        _ => {
            return Err(anyhow::anyhow!(
                "不支持的操作系统。请手动安装 gh: https://github.com/cli/cli#installation"
            ));
        }
    };

    let status = Command::new(cmd)
        .args(&args)
        .status()
        .context("执行安装命令失败")?;

    if !status.success() {
        return Err(anyhow::anyhow!("gh CLI 安装失败"));
    }

    println!("{}", "✓ GitHub CLI 安装成功".green());
    Ok(())
}

/// Authenticate with GitHub using web browser
fn authenticate_gh() -> Result<()> {
    println!();
    println!("{}", "🔐 需要登录 GitHub 账号".cyan().bold());
    println!("{}", "   将打开浏览器进行认证，请在浏览器中完成登录。".cyan());
    println!();

    let status = Command::new("gh")
        .args(["auth", "login", "--web", "--git-protocol", "https"])
        .status()
        .context("启动 gh auth login 失败")?;

    if !status.success() {
        return Err(anyhow::anyhow!("GitHub 认证失败"));
    }

    println!("{}", "✓ GitHub 认证成功".green());
    Ok(())
}

/// Create a new GitHub repository
fn create_github_repo(repo_name: &str, private: bool) -> Result<String> {
    println!();
    println!("{}", format!("📦 正在创建仓库 {}...", repo_name).cyan());

    let mut args = vec!["repo", "create", repo_name, "--clone=false", "--source=."];
    if private {
        args.push("--private");
    } else {
        args.push("--public");
    }

    // Get the repo URL using gh repo create
    let output = Command::new("gh")
        .args(["repo", "create", repo_name, if private { "--private" } else { "--public" }, "--clone=false"])
        .output()
        .context("创建仓库失败")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("创建仓库失败: {}", stderr));
    }

    // Get the repo URL
    let output = Command::new("gh")
        .args(["repo", "view", repo_name, "--json", "url", "-q", ".url"])
        .output()
        .context("获取仓库 URL 失败")?;

    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();

    if url.is_empty() {
        // Fallback: construct URL from repo name
        let username_output = Command::new("gh")
            .args(["api", "user", "-q", ".login"])
            .output()
            .context("获取用户名失败")?;
        let username = String::from_utf8_lossy(&username_output.stdout).trim().to_string();
        return Ok(format!("https://github.com/{}/{}.git", username, repo_name));
    }

    println!("{}", "✓ 仓库创建成功".green());
    Ok(format!("{}.git", url))
}

/// Ensure gh CLI is installed and authenticated
fn ensure_gh_ready() -> Result<()> {
    // Check if gh is installed
    if !is_gh_installed() {
        println!();
        println!("{}", "⚠️  未检测到 GitHub CLI (gh)".yellow());

        let install = Confirm::new("是否自动安装 GitHub CLI?")
            .with_default(true)
            .with_help_message("需要 gh CLI 来创建仓库和进行认证")
            .prompt()
            .unwrap_or(false);

        if install {
            install_gh_cli()?;
        } else {
            return Err(anyhow::anyhow!(
                "需要 GitHub CLI。请手动安装: https://github.com/cli/cli#installation"
            ));
        }
    }

    // Check if authenticated
    if !is_gh_authenticated() {
        authenticate_gh()?;
    } else {
        println!("{}", "✓ GitHub CLI 已认证".green());
    }

    Ok(())
}

/// Handle clone failure with helpful guidance
fn handle_clone_failure(error: &anyhow::Error, remote_url: &str) -> Result<()> {
    let error_msg = error.to_string().to_lowercase();

    println!();
    println!("{}", "❌ 克隆仓库失败".red().bold());
    println!();

    if error_msg.contains("authentication") || error_msg.contains("auth") || error_msg.contains("permission") || error_msg.contains("403") || error_msg.contains("401") {
        // Authentication error
        println!("{}", "💡 这可能是认证问题。解决方案:".yellow());
        println!();
        println!("   {} 使用 GitHub CLI 网页认证 (推荐)", "方式一:".cyan());
        println!("      运行: gh auth login --web");
        println!();
        println!("   {} 使用 Personal Access Token", "方式二:".cyan());
        println!("      1. 访问 https://github.com/settings/tokens");
        println!("      2. 创建 token (需要 repo 权限)");
        println!("      3. 使用格式: https://<token>@github.com/user/repo.git");
        println!();

        let retry_auth = Confirm::new("是否使用 GitHub CLI 进行网页认证?")
            .with_default(true)
            .prompt()
            .unwrap_or(false);

        if retry_auth {
            ensure_gh_ready()?;
            return Ok(()); // Signal to retry clone
        }
    } else if error_msg.contains("not found") || error_msg.contains("404") || error_msg.contains("does not exist") {
        // Repository not found
        println!("{}", "💡 仓库不存在。解决方案:".yellow());
        println!();
        println!("   1. 检查仓库地址是否正确");
        println!("   2. 确认仓库是否已创建");
        println!("   3. 如果是私有仓库，请确认有访问权限");
        println!();
        println!("   {}", format!("   当前地址: {}", remote_url).cyan());
        println!();

        let create_new = Confirm::new("是否创建新仓库?")
            .with_default(true)
            .prompt()
            .unwrap_or(false);

        if create_new {
            return Err(anyhow::anyhow!("REPO_NOT_FOUND_CREATE_NEW"));
        }
    } else {
        // Generic error
        println!("   错误信息: {}", error);
        println!();
        println!("{}", "💡 可能的原因:".yellow());
        println!("   - 网络连接问题");
        println!("   - 仓库地址不正确");
        println!("   - 没有访问权限");
    }

    Err(anyhow::anyhow!("克隆失败，请解决上述问题后重试"))
}

/// Run the interactive setup wizard
pub fn handle_setup(skip_sync: bool) -> Result<()> {
    println!();
    println!(
        "{}",
        "🔧 Claude Code Sync 配置向导".cyan().bold()
    );
    println!("{}", "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".cyan());
    println!();

    // Step 1: Select sync mode
    let sync_mode = Select::new(
        "选择同步模式:",
        vec![SyncMode::MultiDevice, SyncMode::SingleDevice],
    )
    .with_help_message("多设备模式允许在不同电脑间同步相同项目名的对话")
    .prompt()
    .context("取消选择同步模式")?;

    let use_project_name_only = matches!(sync_mode, SyncMode::MultiDevice);

    // Check if existing config has different mode
    if let Ok(existing_config) = crate::filter::FilterConfig::load() {
        if existing_config.use_project_name_only != use_project_name_only {
            println!();
            println!("{}", "⚠️  检测到同步模式变更".yellow().bold());
            println!("{}", "─".repeat(50).dimmed());

            let old_mode = if existing_config.use_project_name_only {
                "多设备同步"
            } else {
                "单设备备份"
            };
            let new_mode = if use_project_name_only {
                "多设备同步"
            } else {
                "单设备备份"
            };

            println!("当前配置: {} → 新选择: {}", old_mode.cyan(), new_mode.green());
            println!();
            println!(
                "{}",
                "切换模式可能导致同步仓库中出现混合目录格式。".yellow()
            );
            println!(
                "{}",
                "建议在切换后手动清理旧格式的目录以避免数据重复。".yellow()
            );
            println!("{}", "─".repeat(50).dimmed());
            println!();

            let confirm = Confirm::new("确认切换模式？")
                .with_default(true)
                .prompt()
                .context("取消确认")?;

            if !confirm {
                return Err(anyhow::anyhow!("用户取消配置"));
            }
        }
    }

    println!();

    // Step 2: Select repository source
    let repo_source = Select::new(
        "仓库来源:",
        vec![RepoSource::Existing, RepoSource::CreateNew],
    )
    .with_help_message("选择使用已有仓库还是创建新仓库")
    .prompt()
    .context("取消选择仓库来源")?;

    let remote_url = match repo_source {
        RepoSource::CreateNew => {
            // Ensure gh is ready
            ensure_gh_ready()?;

            println!();

            let repo_name = Text::new("新仓库名称:")
                .with_default("claude-code-history")
                .with_help_message("将在你的 GitHub 账号下创建此仓库")
                .prompt()
                .context("取消输入仓库名称")?;

            let private = Confirm::new("设为私有仓库?")
                .with_default(true)
                .with_help_message("私有仓库只有你能访问，推荐用于存储对话历史")
                .prompt()
                .unwrap_or(true);

            create_github_repo(&repo_name, private)?
        }
        RepoSource::Existing => {
            println!();

            Text::new("远程仓库地址:")
                .with_placeholder("https://github.com/username/claude-code-history.git")
                .with_help_message("Git 仓库地址，用于备份和同步对话历史")
                .prompt()
                .context("取消输入远程仓库地址")?
        }
    };

    // Validate URL
    if !is_valid_git_url(&remote_url) {
        return Err(anyhow::anyhow!(
            "无效的 Git URL。必须以 'https://', 'http://', 'git@' 或 'ssh://' 开头"
        ));
    }

    println!();

    // Step 3: Get local directory (with default)
    let default_path = ConfigManager::default_repo_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "~/claude-history-backup".to_string());

    let local_path_str = Text::new("本地备份目录:")
        .with_default(&default_path)
        .with_help_message("对话历史将同步到此目录")
        .prompt()
        .context("取消输入本地目录")?;

    let local_path = expand_tilde(&local_path_str)?;

    println!();

    // Show configuration summary
    println!("{}", "📋 配置摘要".cyan().bold());
    println!("   {} {}", "模式:".cyan(), if use_project_name_only { "多设备同步" } else { "单设备备份" });
    println!("   {} {}", "远程:".cyan(), remote_url);
    println!("   {} {}", "本地:".cyan(), local_path.display());
    println!();

    // Confirm
    let confirm = Confirm::new("确认以上配置?")
        .with_default(true)
        .prompt()
        .context("取消确认")?;

    if !confirm {
        println!("{}", "已取消配置。".yellow());
        return Ok(());
    }

    println!();

    // Step 4: Clone repository (with retry logic)
    println!("{}", "📥 正在克隆仓库...".cyan());

    let clone_result = scm::clone(&remote_url, &local_path);

    if let Err(e) = clone_result {
        let handle_result = handle_clone_failure(&e, &remote_url);

        match handle_result {
            Ok(()) => {
                // Retry clone after authentication
                println!();
                println!("{}", "📥 重新尝试克隆...".cyan());
                scm::clone(&remote_url, &local_path).context("重试克隆仍然失败")?;
            }
            Err(ref retry_err) if retry_err.to_string() == "REPO_NOT_FOUND_CREATE_NEW" => {
                // User wants to create new repo
                ensure_gh_ready()?;

                let repo_name = Text::new("新仓库名称:")
                    .with_default("claude-code-history")
                    .prompt()
                    .context("取消输入仓库名称")?;

                let private = Confirm::new("设为私有仓库?")
                    .with_default(true)
                    .prompt()
                    .unwrap_or(true);

                let new_url = create_github_repo(&repo_name, private)?;

                println!();
                println!("{}", "📥 克隆新仓库...".cyan());
                scm::clone(&new_url, &local_path).context("克隆新仓库失败")?;

                // Update remote_url for later use
                // Note: we continue with new_url
            }
            Err(e) => return Err(e),
        }
    }

    println!("{}", "✓ 仓库克隆成功".green());

    // Step 5: Initialize sync state
    sync::init_from_onboarding(&local_path, Some(&remote_url), true)
        .context("初始化同步状态失败")?;

    // Step 6: Save filter configuration
    let filter_config = FilterConfig {
        use_project_name_only,
        sync_subdirectory: "projects".to_string(),
        ..Default::default()
    };
    filter_config.save().context("保存配置失败")?;

    println!("{}", "✓ 配置已保存".green());
    println!();

    // Step 7: Optional initial sync
    if !skip_sync {
        let do_sync = Confirm::new("是否立即同步?")
            .with_default(true)
            .with_help_message("将本地对话历史推送到远程仓库")
            .prompt()
            .unwrap_or(false);

        if do_sync {
            println!();
            println!("{}", "🔄 正在同步...".cyan());

            match sync::sync_bidirectional(
                None,
                None,
                false,
                false,
                crate::VerbosityLevel::Normal,
            ) {
                Ok(()) => {
                    println!("{}", "✓ 同步完成".green());
                }
                Err(e) => {
                    println!("{} {}", "⚠️  同步时出现问题:".yellow(), e);
                    println!("{}", "   可以稍后使用 'claude-code-sync sync' 重试".yellow());
                }
            }
        }
    }

    // Step 8: Configure auto-sync (hooks + wrapper)
    println!();
    let setup_auto_sync = Confirm::new("是否配置自动同步？")
        .with_default(true)
        .with_help_message("启动时自动拉取，退出时自动推送，无需手动执行命令")
        .prompt()
        .unwrap_or(false);

    if setup_auto_sync {
        println!();
        println!("{}", "🔧 正在配置自动同步...".cyan());

        // Install hooks
        match crate::handlers::hooks::handle_hooks_install() {
            Ok(()) => {}
            Err(e) => {
                println!("{} {}", "⚠️  Hooks 安装失败:".yellow(), e);
            }
        }

        // Install wrapper
        match crate::handlers::wrapper::handle_wrapper_install(false) {
            Ok(wrapper_path) => {
                println!();
                println!("{}", "✓ 自动同步已配置".green());
                println!();
                println!("{}", "使用方式:".cyan());
                println!(
                    "   使用 {} 启动 Claude Code（替代 claude 命令）",
                    "claude-sync".bold()
                );
                println!("   或添加别名: alias claude='{}'", wrapper_path.display());
            }
            Err(e) => {
                println!("{} {}", "⚠️  Wrapper 安装失败:".yellow(), e);
            }
        }
    }

    // Step 9: Configure config sync (settings.json, CLAUDE.md, etc.)
    println!();
    let sync_config = Confirm::new("是否同步配置文件？")
        .with_default(true)
        .with_help_message("同步 settings.json、CLAUDE.md 等配置到远程仓库")
        .prompt()
        .unwrap_or(true);

    // Update filter config with config sync settings
    let mut filter_config = FilterConfig::load().unwrap_or_default();
    filter_config.config_sync.enabled = sync_config;

    if sync_config {
        // Let user choose what to sync
        println!();
        println!("{}", "选择需要同步的配置项:".cyan());

        filter_config.config_sync.sync_settings = Confirm::new("  同步 settings.json (权限、模型配置)?")
            .with_default(true)
            .prompt()
            .unwrap_or(true);

        filter_config.config_sync.sync_claude_md = Confirm::new("  同步 CLAUDE.md (用户指令)?")
            .with_default(true)
            .prompt()
            .unwrap_or(true);

        filter_config.config_sync.sync_hooks = Confirm::new("  同步 hooks (钩子脚本)?")
            .with_default(false)
            .with_help_message("注意: hooks 路径可能不跨平台兼容")
            .prompt()
            .unwrap_or(false);

        filter_config.config_sync.sync_skills_list = Confirm::new("  同步 skills/plugins 列表?")
            .with_default(true)
            .with_help_message("仅同步列表，需要在每台设备手动安装")
            .prompt()
            .unwrap_or(true);
    }

    filter_config.save().context("保存配置同步设置失败")?;
    println!("{}", "✓ 配置同步设置已保存".green());

    println!();
    println!("{}", "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".green());
    println!("{}", "🎉 配置完成！".green().bold());
    println!();

    if setup_auto_sync {
        println!("{}", "自动同步已启用，使用 claude-sync 启动即可。".cyan());
        println!();
        println!("{}", "管理命令:".cyan());
        println!("   {} - 查看自动同步状态", "claude-code-sync automate --status".bold());
        println!("   {} - 卸载自动同步", "claude-code-sync automate --uninstall".bold());
    } else {
        println!("{}", "常用命令:".cyan());
        println!("   {} - 双向同步", "claude-code-sync sync".bold());
        println!("   {} - 推送到远程", "claude-code-sync push".bold());
        println!("   {} - 拉取到本地", "claude-code-sync pull".bold());
        println!("   {} - 查看状态", "claude-code-sync status".bold());
        println!();
        println!("{}", "提示: 运行 'claude-code-sync automate' 可配置自动同步".dimmed());
    }
    println!();

    Ok(())
}

/// Validate git URL format
fn is_valid_git_url(url: &str) -> bool {
    url.starts_with("https://")
        || url.starts_with("http://")
        || url.starts_with("git@")
        || url.starts_with("ssh://")
}

/// Expand tilde in path
fn expand_tilde(path: &str) -> Result<PathBuf> {
    if path.starts_with("~/") || path == "~" {
        let home = dirs::home_dir().context("无法获取用户主目录")?;
        if path == "~" {
            Ok(home)
        } else {
            Ok(home.join(&path[2..]))
        }
    } else {
        Ok(PathBuf::from(path))
    }
}
