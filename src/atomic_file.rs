use anyhow::{Context, Result};
use fs4::FileExt;
use serde::Serialize;
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
    let parent = target.parent().context("JSON target has no parent")?;
    std::fs::create_dir_all(parent)?;
    let bytes = serde_json::to_vec(value)?;
    let mut temp = NamedTempFile::new_in(parent)?;
    set_private_permissions(temp.path())?;
    temp.write_all(&bytes)?;
    temp.flush()?;
    temp.as_file().sync_all()?;
    temp.persist(target).map_err(|error| error.error)?;
    Ok(())
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
