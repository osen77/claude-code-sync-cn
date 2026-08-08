use anyhow::{Context, Result};
use fs4::FileExt;
use serde::Serialize;
#[cfg(test)]
use std::cell::Cell;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use tempfile::NamedTempFile;

pub(crate) struct FileLock {
    file: File,
}

impl FileLock {
    pub(crate) fn acquire(lock_path: &Path) -> Result<Self> {
        let parent = lock_path.parent().context("lock path has no parent")?;
        std::fs::create_dir_all(parent)?;
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(lock_path)?;
        set_private_permissions(lock_path)?;
        FileExt::lock(&file)?;
        Ok(Self { file })
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

pub(crate) fn persist_json_atomic<T: Serialize>(target: &Path, value: &T) -> Result<()> {
    #[cfg(test)]
    {
        let call = PERSIST_CALLS.with(|count| {
            let next = count.get().saturating_add(1);
            count.set(next);
            next
        });
        if FAIL_ON_PERSIST_CALL.with(|value| value.get() == Some(call)) {
            anyhow::bail!("test atomic persist failure");
        }
    }
    let parent = target.parent().context("JSON target has no parent")?;
    std::fs::create_dir_all(parent)?;
    let bytes = serde_json::to_vec(value)?;
    let mut temp = NamedTempFile::new_in(parent)?;
    set_private_permissions(temp.path())?;
    temp.write_all(&bytes)?;
    temp.flush()?;
    temp.as_file().sync_all()?;
    temp.persist(target).map_err(|error| error.error)?;
    sync_parent_directory(parent)?;
    Ok(())
}

/// Synchronize a directory after an atomic child replacement or removal.
///
/// Unix supports directory fsync through a read-only directory handle. Windows
/// and other platforms keep the operation explicit but use the platform's
/// available atomic rename semantics; the helper remains a testable boundary.
pub(crate) fn sync_parent_directory(parent: &Path) -> Result<()> {
    #[cfg(test)]
    if FORCE_PARENT_SYNC_FAILURE.with(Cell::get) {
        anyhow::bail!("test parent directory sync failure");
    }

    #[cfg(unix)]
    {
        File::open(parent)
            .with_context(|| format!("failed to open directory for sync: {}", parent.display()))?
            .sync_all()
            .with_context(|| format!("failed to sync directory: {}", parent.display()))?;
    }
    #[cfg(windows)]
    {
        let _ = parent;
        // Rust's portable std API does not expose a directory fsync contract on
        // Windows; callers still receive errors from all file operations.
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = parent;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn test_fail_persist_on_call(call: usize) {
    PERSIST_CALLS.with(|count| count.set(0));
    FAIL_ON_PERSIST_CALL.with(|value| value.set(Some(call)));
}

#[cfg(test)]
pub(crate) fn test_clear_persist_failures() {
    PERSIST_CALLS.with(|count| count.set(0));
    FAIL_ON_PERSIST_CALL.with(|value| value.set(None));
}

#[cfg(test)]
pub(crate) fn test_force_parent_sync_failure(enabled: bool) {
    FORCE_PARENT_SYNC_FAILURE.with(|value| value.set(enabled));
}

#[cfg(unix)]
fn set_private_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
thread_local! {
    static PERSIST_CALLS: Cell<usize> = const { Cell::new(0) };
    static FAIL_ON_PERSIST_CALL: Cell<Option<usize>> = const { Cell::new(None) };
    static FORCE_PARENT_SYNC_FAILURE: Cell<bool> = const { Cell::new(false) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use tempfile::tempdir;

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct Payload {
        value: u32,
    }

    #[test]
    fn persist_json_atomic_replaces_with_complete_json() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("state.json");
        persist_json_atomic(&target, &Payload { value: 1 }).unwrap();
        persist_json_atomic(&target, &Payload { value: 2 }).unwrap();
        let loaded: Payload = serde_json::from_slice(&std::fs::read(target).unwrap()).unwrap();
        assert_eq!(loaded, Payload { value: 2 });
    }

    #[test]
    fn parent_sync_failure_is_returned_after_atomic_replace() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("state.json");
        test_force_parent_sync_failure(true);
        let result = persist_json_atomic(&target, &Payload { value: 1 });
        test_force_parent_sync_failure(false);
        assert!(result.is_err());
        let loaded: Payload = serde_json::from_slice(&std::fs::read(target).unwrap()).unwrap();
        assert_eq!(loaded, Payload { value: 1 });
    }

    #[test]
    fn file_lock_serializes_two_writers() {
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("state.lock");
        let first = FileLock::acquire(&lock_path).unwrap();
        let path = lock_path.clone();
        let waiter = std::thread::spawn(move || FileLock::acquire(&path).unwrap());
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(!waiter.is_finished());
        drop(first);
        drop(waiter.join().unwrap());
    }
}
