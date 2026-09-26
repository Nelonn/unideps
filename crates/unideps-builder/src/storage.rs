use std::path::{Path, PathBuf};

pub struct StorageManager {
    base_dir: PathBuf,
    scratch_override: Option<PathBuf>,
}

/// Written last: a prefix without it is left over from a crashed build or unpack.
pub const INSTALL_MARKER: &str = ".unideps-complete";

impl StorageManager {
    pub fn new(base_dir: PathBuf) -> Self {
        Self {
            base_dir,
            scratch_override: None,
        }
    }

    pub fn with_scratch_dir(mut self, scratch: Option<PathBuf>) -> Self {
        self.scratch_override = scratch;
        self
    }

    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.base_dir.join("cache")
    }

    pub fn installed_dir(&self) -> PathBuf {
        self.base_dir.join("installed")
    }

    pub fn scratch_dir(&self) -> PathBuf {
        self.scratch_override
            .clone()
            .unwrap_or_else(|| self.base_dir.join("scratch"))
    }

    pub fn sources_dir(&self) -> PathBuf {
        self.base_dir.join("sources")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.base_dir.join("logs")
    }

    /// What each package's own `unideps.toml` resolved to; see `crate::resolution`.
    pub fn resolved_dir(&self) -> PathBuf {
        self.base_dir.join("resolved")
    }

    pub fn locks_dir(&self) -> PathBuf {
        self.base_dir.join("locks")
    }

    pub fn package_lock_path(&self, dir_name: &str) -> PathBuf {
        self.locks_dir().join(format!("{dir_name}.lock"))
    }

    /// Guards one checkout directory, so that two runs never fetch into it at once.
    pub fn source_lock_path(&self, dir_name: &str) -> PathBuf {
        self.locks_dir().join(format!("src-{dir_name}.lock"))
    }

    pub fn global_build_lock_path(&self) -> PathBuf {
        self.locks_dir().join("build.lock")
    }

    pub fn is_installed(install_dir: &Path) -> bool {
        install_dir.join(INSTALL_MARKER).is_file()
    }

    pub fn mark_installed(install_dir: &Path) -> std::io::Result<()> {
        std::fs::write(install_dir.join(INSTALL_MARKER), b"")
    }

    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(self.cache_dir())?;
        std::fs::create_dir_all(self.installed_dir())?;
        std::fs::create_dir_all(self.sources_dir())?;
        std::fs::create_dir_all(self.scratch_dir())?;
        std::fs::create_dir_all(self.logs_dir())?;
        std::fs::create_dir_all(self.locks_dir())?;
        std::fs::create_dir_all(self.resolved_dir())?;
        Ok(())
    }
}
