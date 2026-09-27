use anyhow::{Context, Result};
use fs4::FileExt;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

/// The lock file is intentionally never deleted: removing it on release lets a
/// waiter lock the unlinked inode while a newcomer locks a freshly created file,
/// so two processes would hold "the same" lock at once.
pub struct FileLock {
    file: Option<File>,
    path: PathBuf,
}

impl FileLock {
    pub fn acquire(path: impl AsRef<Path>) -> Result<Self> {
        Self::open(path, true)
    }

    /// Any number of holders at once, but never together with an exclusive one. Used by a
    /// build to keep `unideps clean` out while still running next to other builds.
    pub fn acquire_shared(path: impl AsRef<Path>) -> Result<Self> {
        Self::open(path, false)
    }

    fn open(path: impl AsRef<Path>, exclusive: bool) -> Result<Self> {
        let p = path.as_ref().to_path_buf();
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&p)
            .with_context(|| format!("Failed to open lock file: {}", p.display()))?;

        let kind = if exclusive { "exclusive" } else { "shared" };
        let res = if exclusive { FileExt::lock(&file) } else { FileExt::lock_shared(&file) };
        res.with_context(|| format!("Failed to acquire {kind} lock on: {}", p.display()))?;

        Ok(Self {
            file: Some(file),
            path: p,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            let _ = FileExt::unlock(&file);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::StorageManager;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn lock_is_mutually_exclusive_and_reusable() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("locks/test.lock");
        let inside = Arc::new(AtomicUsize::new(0));
        let max_inside = Arc::new(AtomicUsize::new(0));

        let handles: Vec<_> = (0..6)
            .map(|_| {
                let path = path.clone();
                let inside = inside.clone();
                let max_inside = max_inside.clone();
                std::thread::spawn(move || {
                    for _ in 0..5 {
                        let _lock = FileLock::acquire(&path).unwrap();
                        let now = inside.fetch_add(1, Ordering::SeqCst) + 1;
                        max_inside.fetch_max(now, Ordering::SeqCst);
                        std::thread::sleep(std::time::Duration::from_millis(2));
                        inside.fetch_sub(1, Ordering::SeqCst);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(max_inside.load(Ordering::SeqCst), 1);
        assert!(path.exists());
    }

    #[test]
    fn shared_locks_do_not_exclude_each_other() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("locks/shared.lock");
        let a = FileLock::acquire_shared(&path).unwrap();
        let b = FileLock::acquire_shared(&path).unwrap();
        drop((a, b));
        // ... but an exclusive one still waits for them, which `acquire` here can only show
        // by succeeding once they are gone.
        let _excl = FileLock::acquire(&path).unwrap();
    }

    #[test]
    fn build_slot_blocks_other_builds_but_not_other_package_locks() {
        let temp = tempfile::tempdir().unwrap();
        let storage = StorageManager::new(temp.path().to_path_buf());
        let slot = storage.package_build_slot_lock_path();
        let _first_package = FileLock::acquire(storage.package_lock_path("first-id")).unwrap();
        let first_build = FileLock::acquire(&slot).unwrap();

        let second_package = storage.package_lock_path("second-id");
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (built_tx, built_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _package = FileLock::acquire(second_package).unwrap();
            ready_tx.send(()).unwrap();
            let _build = FileLock::acquire(slot).unwrap();
            built_tx.send(()).unwrap();
        });

        ready_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        let blocked = built_rx.recv_timeout(std::time::Duration::from_millis(100)).is_err();
        drop(first_build);
        built_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        worker.join().unwrap();
        assert!(blocked, "another package entered the build slot while it was occupied");
    }
}
