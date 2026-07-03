//! Time-boxed "delete unlock" window.
//!
//! When active, `ccs push` treats locally-missing sessions as intentional
//! deletions and prunes them from the sync repo (same as `--prune`, but with
//! NO tombstone). The window expires passively — there is no background
//! process; every consumer re-checks the stored expiry timestamp.

use crate::config::ConfigManager;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Serialize, Deserialize)]
struct UnlockState {
    /// Absolute expiry in unix seconds (timezone-independent).
    expires_at: u64,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Remaining seconds until expiry, or `None` if already expired.
/// Pure function — no IO — so expiry logic is unit-testable in isolation.
fn remaining_at(expires_at: u64, now: u64) -> Option<u64> {
    if now < expires_at {
        Some(expires_at - now)
    } else {
        None
    }
}

fn state_path() -> Result<PathBuf> {
    ConfigManager::delete_unlock_path()
}

/// Open (or extend) the window for `minutes`. Overwrites any existing state,
/// so calling again simply renews the deadline. Returns the expiry unix ts.
pub fn unlock(minutes: u64) -> Result<u64> {
    let expires_at = now_secs() + minutes * 60;
    ConfigManager::ensure_config_dir()?;
    let path = state_path()?;
    let json = serde_json::to_string(&UnlockState { expires_at })?;
    std::fs::write(&path, json)
        .with_context(|| format!("Failed to write delete-unlock state: {}", path.display()))?;
    Ok(expires_at)
}

/// Close the window. Idempotent: a missing file is treated as success.
pub fn disable() -> Result<()> {
    let path = state_path()?;
    if path.exists() {
        std::fs::remove_file(&path)
            .with_context(|| format!("Failed to remove delete-unlock state: {}", path.display()))?;
    }
    Ok(())
}

/// Remaining seconds if the window is active, else `None` (expired/absent).
pub fn status() -> Result<Option<u64>> {
    let path = state_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path)?;
    let state: UnlockState = serde_json::from_str(&content)?;
    Ok(remaining_at(state.expires_at, now_secs()))
}

/// Fail-safe active check for push consumption. ANY error (missing/corrupt/
/// unreadable state) resolves to `false` so push falls back to protection.
pub fn is_active() -> bool {
    matches!(status(), Ok(Some(_)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CONFIG_DIR_ENV;
    use serial_test::serial;
    use std::env;
    use tempfile::TempDir;

    fn with_temp_config(f: impl FnOnce()) {
        let tmp = TempDir::new().unwrap();
        env::set_var(CONFIG_DIR_ENV, tmp.path());
        f();
        env::remove_var(CONFIG_DIR_ENV);
    }

    #[test]
    fn test_remaining_at_active() {
        assert_eq!(remaining_at(100, 40), Some(60));
    }

    #[test]
    fn test_remaining_at_expired() {
        assert_eq!(remaining_at(100, 100), None);
        assert_eq!(remaining_at(100, 150), None);
    }

    #[test]
    #[serial]
    fn test_unlock_then_status_roundtrip() {
        with_temp_config(|| {
            unlock(15).unwrap();
            let remaining = status().unwrap().expect("window should be active");
            // 15 minutes = 900s; allow a little slack for test execution.
            assert!(remaining > 890 && remaining <= 900, "remaining={remaining}");
            assert!(is_active());
        });
    }

    #[test]
    #[serial]
    fn test_disable_clears_window() {
        with_temp_config(|| {
            unlock(15).unwrap();
            disable().unwrap();
            assert_eq!(status().unwrap(), None);
            assert!(!is_active());
        });
    }

    #[test]
    #[serial]
    fn test_absent_file_is_inactive() {
        with_temp_config(|| {
            assert_eq!(status().unwrap(), None);
            assert!(!is_active());
        });
    }

    #[test]
    #[serial]
    fn test_corrupt_file_is_failsafe_inactive() {
        with_temp_config(|| {
            ConfigManager::ensure_config_dir().unwrap();
            std::fs::write(ConfigManager::delete_unlock_path().unwrap(), "not json {{").unwrap();
            // status() surfaces the parse error, but is_active() must be fail-safe.
            assert!(status().is_err());
            assert!(!is_active());
        });
    }
}
