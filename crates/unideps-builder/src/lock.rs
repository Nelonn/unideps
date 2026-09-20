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

        FileExt::lock(&file)
            .with_context(|| format!("Failed to acquire exclusive lock on: {}", p.display()))?;

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
}
