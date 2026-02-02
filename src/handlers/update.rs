//! Self-update functionality
//!
//! Provides automatic update checking and self-update capabilities.
//! Downloads prebuilt binaries from GitHub Releases.

use anyhow::{Context, Result};
use colored::Colorize;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

/// GitHub repository for releases
const GITHUB_REPO: &str = "osen77/claude-code-sync-cn";

/// Timeout for HTTP requests (in seconds)
const REQUEST_TIMEOUT_SECS: u64 = 10;

/// Get current version from Cargo.toml
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Fetch the latest version from GitHub API
pub fn fetch_latest_version() -> Result<String> {
    let url = format!("https://api.github.com/repos/{}/releases/latest", GITHUB_REPO);

    // Use curl command to avoid adding heavy dependencies
    let output = Command::new("curl")
        .args([
            "-fsSL",
            "--max-time",
            &REQUEST_TIMEOUT_SECS.to_string(),
            "-H",
            "Accept: application/vnd.github.v3+json",
            "-H",
            "User-Agent: claude-code-sync",
            &url,
        ])
        .output()
        .context("Failed to execute curl")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("Failed to fetch release info: {}", stderr));
    }

    let response = String::from_utf8_lossy(&output.stdout);

    // Parse tag_name from JSON response
    // Simple parsing without serde_json dependency
    for line in response.lines() {
        if line.contains("\"tag_name\"") {
            if let Some(start) = line.find(": \"") {
                let rest = &line[start + 3..];
                if let Some(end) = rest.find('"') {
                    return Ok(rest[..end].to_string());
                }
            }
        }
    }

    Err(anyhow::anyhow!("Could not parse version from response"))
}

/// Compare version strings (v0.1.2 vs v0.1.1)
pub fn is_newer(latest: &str, current: &str) -> bool {
    let parse = |s: &str| -> Vec<u32> {
        s.trim_start_matches('v')
            .split('.')
            .filter_map(|p| p.split('-').next()) // Handle pre-release versions like 0.1.2-beta
            .filter_map(|p| p.parse().ok())
            .collect()
    };

    let latest_parts = parse(latest);
    let current_parts = parse(current);

    latest_parts > current_parts
}

/// Check for available updates
/// Returns Some(new_version) if an update is available, None otherwise
pub fn check_for_update() -> Result<Option<String>> {
    let current = current_version();
    let latest = fetch_latest_version()?;

    if is_newer(&latest, current) {
        Ok(Some(latest))
    } else {
        Ok(None)
    }
}

/// Check for updates silently (for startup check)
/// Swallows errors to avoid disrupting normal operation
pub fn check_for_update_silent() -> Option<String> {
    // Set a shorter timeout for background check
    let url = format!("https://api.github.com/repos/{}/releases/latest", GITHUB_REPO);

    let output = Command::new("curl")
        .args([
            "-fsSL",
            "--max-time",
            "5", // Shorter timeout for background check
            "-H",
            "Accept: application/vnd.github.v3+json",
            "-H",
            "User-Agent: claude-code-sync",
            &url,
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let response = String::from_utf8_lossy(&output.stdout);

    // Parse tag_name
    for line in response.lines() {
        if line.contains("\"tag_name\"") {
            if let Some(start) = line.find(": \"") {
                let rest = &line[start + 3..];
                if let Some(end) = rest.find('"') {
                    let latest = &rest[..end];
                    let current = current_version();

                    if is_newer(latest, current) {
                        return Some(latest.to_string());
                    }
                }
            }
        }
    }

    None
}

/// Get the asset name for the current platform
fn get_asset_name() -> Result<String> {
    let os = if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        return Err(anyhow::anyhow!("Unsupported operating system"));
    };

    let arch = if cfg!(target_arch = "x86_64") {
        "x64"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        return Err(anyhow::anyhow!("Unsupported architecture"));
    };

    let name = if cfg!(target_os = "windows") {
        format!("claude-code-sync-{}-{}.exe", os, arch)
    } else {
        format!("claude-code-sync-{}-{}", os, arch)
    };

    Ok(name)
}

/// Download a file using curl
fn download_file(url: &str, dest: &PathBuf) -> Result<()> {
    println!("{}", format!("   {}", url).cyan());

    let status = Command::new("curl")
        .args([
            "-fSL",
            "--progress-bar",
            "-o",
            dest.to_str().unwrap(),
            url,
        ])
        .status()
        .context("Failed to execute curl")?;

    if !status.success() {
        return Err(anyhow::anyhow!("Download failed"));
    }

    Ok(())
}

/// Download and replace the current binary
fn download_and_replace(version: &str) -> Result<()> {
    let current_exe = std::env::current_exe().context("Failed to get current executable path")?;
    let asset_name = get_asset_name()?;

    let url = format!(
        "https://github.com/{}/releases/download/{}/{}",
        GITHUB_REPO, version, asset_name
    );

    println!("{}", "📥 正在下载...".cyan());

    // Download to temp file
    let temp_path = current_exe.with_extension("new");
    download_file(&url, &temp_path)?;

    println!("{}", "✓ 下载完成".green());

    // Replace binary
    println!("{}", "📦 正在更新...".cyan());

    #[cfg(windows)]
    {
        // On Windows, rename the running executable first
        let old_path = current_exe.with_extension("old");

        // Remove old backup if exists
        let _ = fs::remove_file(&old_path);

        // Rename current to old
        fs::rename(&current_exe, &old_path).context("Failed to rename current executable")?;

        // Rename new to current
        fs::rename(&temp_path, &current_exe).context("Failed to install new executable")?;

        println!("{}", "✓ 更新完成".green());
        println!();
        println!(
            "{}",
            "注意: 旧版本已保存为 .old 文件，可手动删除".yellow()
        );
    }

    #[cfg(not(windows))]
    {
        // On Unix, we can replace directly
        fs::rename(&temp_path, &current_exe).context("Failed to install new executable")?;

        // Set executable permission
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&current_exe, fs::Permissions::from_mode(0o755))
                .context("Failed to set executable permission")?;
        }

        println!("{}", "✓ 更新完成".green());
    }

    Ok(())
}

/// Handle the update command
pub fn handle_update(check_only: bool) -> Result<()> {
    let current = current_version();

    println!();
    println!("{}", "🔄 检查更新".cyan().bold());
    println!("   {} v{}", "当前版本:".cyan(), current);

    let latest = match fetch_latest_version() {
        Ok(v) => v,
        Err(e) => {
            println!("{} {}", "❌ 检查更新失败:".red(), e);
            return Err(e);
        }
    };

    println!("   {} {}", "最新版本:".cyan(), latest);
    println!();

    if !is_newer(&latest, current) {
        println!("{}", "✓ 已是最新版本".green());
        return Ok(());
    }

    println!(
        "{}",
        format!("💡 发现新版本: {} → {}", current, latest)
            .yellow()
            .bold()
    );
    println!();

    if check_only {
        println!("{}", "运行 'claude-code-sync update' 进行更新".cyan());
        return Ok(());
    }

    // Confirm update
    print!("{}", "是否立即更新? [Y/n] ".cyan());
    std::io::stdout().flush()?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;

    if input.trim().to_lowercase() == "n" {
        println!("{}", "已取消更新".yellow());
        return Ok(());
    }

    println!();

    // Perform update
    download_and_replace(&latest)?;

    println!();
    println!("{}", "🎉 更新成功！".green().bold());
    println!("   新版本: {}", latest);
    println!();

    Ok(())
}

/// Print update notification (for startup check)
pub fn print_update_notification(new_version: &str) {
    let current = current_version();
    eprintln!();
    eprintln!(
        "{}",
        format!(
            "💡 发现新版本 {} (当前 v{})",
            new_version, current
        )
        .yellow()
    );
    eprintln!(
        "{}",
        "   运行 'claude-code-sync update' 更新".yellow()
    );
    eprintln!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_newer() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("v0.1.2", "0.1.1"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(is_newer("v0.1.10", "v0.1.9"));

        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("v0.1.0", "0.2.0"));
        assert!(!is_newer("0.1.1", "0.1.2"));
    }

    #[test]
    fn test_is_newer_with_prerelease() {
        // Pre-release versions should compare by main version only
        assert!(is_newer("v0.2.0-beta", "0.1.0"));
        assert!(!is_newer("v0.1.0-beta", "0.1.0"));
    }

    #[test]
    fn test_get_asset_name() {
        let name = get_asset_name().unwrap();
        // Should contain os and arch
        assert!(name.contains("darwin") || name.contains("linux") || name.contains("windows"));
        assert!(name.contains("x64") || name.contains("arm64"));
    }

    #[test]
    fn test_current_version() {
        let version = current_version();
        // Should be a valid semver
        assert!(version.split('.').count() >= 2);
    }
}
