//! What a package's own `unideps.toml` resolved to, remembered next to the installed
//! prefixes.
//!
//! Only the source of a package says whether it carries a nested `unideps.toml`, and only
//! a CMake configure of it (the option probe) says which of that manifest's dependencies
//! are enabled. Both feed the package's `build_id`, so without a record of the outcome
//! every run would have to fetch the source and probe the package again just to find out
//! that it is already installed -- the probe alone is a full configure per package per run.
//!
//! The record is keyed by the build id the package would have if its manifest pulled in
//! nothing (`plain_id`), which is known before the source is touched. The package's own
//! revision, patches, options, toolchain and dependencies are all part of that key.
//!
//! What the key does *not* cover is anything the nested packages are re-resolved from on
//! every run: a branch (or no ref at all) points at a different commit over time, and a
//! `path` source is hashed from its current contents. Freezing those in a record would
//! pin a package to a revision its manifest no longer asks for, so a resolution that
//! contains one is not recorded at all -- see `is_recordable`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use unideps_core::graph::DependencyNode;

use crate::storage::StorageManager;

/// Bump when the fields below change meaning; records written by an older unideps are
/// then ignored instead of misread.
const FORMAT_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resolution {
    version: u32,
    /// For reading the file by hand; the name is already in the file name.
    pub name: String,
    /// The build id the package really got, once its nested packages were known. Equal to
    /// the key when the source has no nested `unideps.toml`.
    pub build_id: String,
    /// Everything the package's own `unideps.toml` pulled in, transitively, in the order
    /// the build produced it. Empty when there is no nested manifest.
    pub nested: Vec<DependencyNode>,
}

/// Whether a resolution containing `node` may be remembered across runs, i.e. whether the
/// node is the same package every time the key is. A branch and the default HEAD are
/// re-resolved to whatever they point at now, and a `path` source is hashed from its
/// contents, so neither is fixed by the key; a package that pulls one in has to be resolved
/// again on every run, which is the cost of tracking a moving source.
pub fn is_recordable(node: &DependencyNode) -> bool {
    node.path.is_none() && (node.git.is_none() || node.commit.is_some() || node.tag.is_some())
}

impl Resolution {
    pub fn new(name: &str, build_id: &str, nested: Vec<DependencyNode>) -> Self {
        Self {
            version: FORMAT_VERSION,
            name: name.to_string(),
            build_id: build_id.to_string(),
            nested,
        }
    }

    fn path(storage: &StorageManager, name: &str, plain_id: &str) -> PathBuf {
        storage.resolved_dir().join(format!("{name}-{plain_id}.json"))
    }

    /// Whether every prefix this record names is still there. A package unideps installed
    /// has to be a complete install; a prefix outside the storage is one the manifest points
    /// at itself (a locally provided tool), where existing is all unideps ever asked of it.
    fn prefixes_are_intact(&self, storage: &StorageManager) -> bool {
        let installed = storage.installed_dir();
        let own = installed.join(format!("{}-{}", self.name, self.build_id));
        if !StorageManager::is_installed(&own) {
            return false;
        }
        self.nested.iter().all(|n| match n.install_prefix {
            Some(ref p) if p.starts_with(&installed) => StorageManager::is_installed(p),
            Some(ref p) => p.exists(),
            None => false,
        })
    }

    /// `None` whenever the record is missing, unreadable, written by another version, or
    /// points at a prefix that is no longer a complete install. Re-resolving is always
    /// correct, so nothing here is worth failing a build over.
    pub fn load(storage: &StorageManager, name: &str, plain_id: &str) -> Option<Self> {
        let raw = std::fs::read(Self::path(storage, name, plain_id)).ok()?;
        let resolution: Self = serde_json::from_slice(&raw).ok()?;
        if resolution.version != FORMAT_VERSION || resolution.name != name {
            return None;
        }
        // A prefix can be deleted by hand, and moving the storage leaves the recorded paths
        // behind; in both cases the record is simply rewritten by the run that re-resolves.
        resolution.prefixes_are_intact(storage).then_some(resolution)
    }

    /// Written through a temporary file so that a reader never sees half a record.
    pub fn store(&self, storage: &StorageManager, plain_id: &str) -> Result<()> {
        let dir = storage.resolved_dir();
        std::fs::create_dir_all(&dir)?;
        let path = Self::path(storage, &self.name, plain_id);
        let tmp = dir.join(format!(".{}-{plain_id}.{}.tmp", self.name, std::process::id()));
        let write = (|| -> Result<()> {
            std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
            replace(&tmp, &path)
        })();
        if write.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        write.with_context(|| format!("Failed to record the resolution of '{}'", self.name))
    }
}

/// `rename` refuses to overwrite on Windows, and a rewrite is the normal case here.
fn replace(from: &Path, to: &Path) -> Result<()> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    let _ = std::fs::remove_file(to);
    std::fs::rename(from, to).with_context(|| format!("Failed to move {} to {}", from.display(), to.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use unideps_core::manifest::{DependencyDetails, DependencySpec};

    fn node(name: &str, prefix: &Path) -> DependencyNode {
        let mut n = DependencyNode::from_target_dep(name, &DependencySpec::Detailed(DependencyDetails::default()));
        n.build_id = Some("b".repeat(16));
        n.install_prefix = Some(prefix.to_path_buf());
        n
    }

    fn install(dir: &Path) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        StorageManager::mark_installed(dir).unwrap();
        dir.to_path_buf()
    }

    fn detailed(d: DependencyDetails) -> DependencyNode {
        DependencyNode::from_target_dep("x", &DependencySpec::Detailed(d))
    }

    /// A moving source has to be re-resolved every run, so a resolution holding one must
    /// never be frozen into a record: it would pin the package to a revision its manifest
    /// no longer asks for.
    #[test]
    fn only_a_pinned_source_may_be_recorded() {
        let git = Some("https://example.invalid/x.git".to_string());
        let pinned_tag = detailed(DependencyDetails { git: git.clone(), tag: Some("v1".into()), ..Default::default() });
        let pinned_commit =
            detailed(DependencyDetails { git: git.clone(), commit: Some("a".repeat(40)), ..Default::default() });
        let branch = detailed(DependencyDetails { git: git.clone(), branch: Some("main".into()), ..Default::default() });
        let default_head = detailed(DependencyDetails { git: git.clone(), ..Default::default() });
        let local = detailed(DependencyDetails { path: Some(PathBuf::from("../x")), ..Default::default() });

        assert!(is_recordable(&pinned_tag));
        assert!(is_recordable(&pinned_commit));
        assert!(!is_recordable(&branch));
        assert!(!is_recordable(&default_head));
        assert!(!is_recordable(&local));
    }

    #[test]
    fn round_trips_the_nested_packages() {
        let temp = tempfile::tempdir().unwrap();
        let storage = StorageManager::new(temp.path().to_path_buf());
        storage.ensure_dirs().unwrap();
        install(&storage.installed_dir().join("outer-aaaa"));
        let inner = install(&storage.installed_dir().join("inner-bbbb"));

        let nested = vec![node("inner", &inner)];
        Resolution::new("outer", "aaaa", nested).store(&storage, "plain").unwrap();

        let loaded = Resolution::load(&storage, "outer", "plain").unwrap();
        assert_eq!(loaded.build_id, "aaaa");
        assert_eq!(loaded.nested.len(), 1);
        assert_eq!(loaded.nested[0].name, "inner");
        assert_eq!(loaded.nested[0].install_prefix.as_deref(), Some(inner.as_path()));
    }

    #[test]
    fn is_ignored_when_a_prefix_is_gone() {
        let temp = tempfile::tempdir().unwrap();
        let storage = StorageManager::new(temp.path().to_path_buf());
        storage.ensure_dirs().unwrap();
        install(&storage.installed_dir().join("outer-aaaa"));
        let inner = install(&storage.installed_dir().join("inner-bbbb"));

        Resolution::new("outer", "aaaa", vec![node("inner", &inner)])
            .store(&storage, "plain")
            .unwrap();
        assert!(Resolution::load(&storage, "outer", "plain").is_some());

        std::fs::remove_dir_all(&inner).unwrap();
        assert!(Resolution::load(&storage, "outer", "plain").is_none());
    }

    #[test]
    fn is_ignored_when_the_package_itself_is_incomplete() {
        let temp = tempfile::tempdir().unwrap();
        let storage = StorageManager::new(temp.path().to_path_buf());
        storage.ensure_dirs().unwrap();
        let own = install(&storage.installed_dir().join("outer-aaaa"));

        Resolution::new("outer", "aaaa", Vec::new()).store(&storage, "plain").unwrap();
        assert!(Resolution::load(&storage, "outer", "plain").is_some());

        std::fs::remove_file(own.join(crate::storage::INSTALL_MARKER)).unwrap();
        assert!(Resolution::load(&storage, "outer", "plain").is_none());
    }

    #[test]
    fn a_prefix_outside_the_storage_only_has_to_exist() {
        let temp = tempfile::tempdir().unwrap();
        let storage = StorageManager::new(temp.path().join("store"));
        storage.ensure_dirs().unwrap();
        install(&storage.installed_dir().join("outer-aaaa"));
        // e.g. a tool the project points at itself, which unideps never installed.
        let local = temp.path().join("local-tool");
        std::fs::create_dir_all(&local).unwrap();

        Resolution::new("outer", "aaaa", vec![node("tool", &local)])
            .store(&storage, "plain")
            .unwrap();
        assert!(Resolution::load(&storage, "outer", "plain").is_some());

        std::fs::remove_dir_all(&local).unwrap();
        assert!(Resolution::load(&storage, "outer", "plain").is_none());
    }

    #[test]
    fn a_rewrite_replaces_the_previous_record() {
        let temp = tempfile::tempdir().unwrap();
        let storage = StorageManager::new(temp.path().to_path_buf());
        storage.ensure_dirs().unwrap();
        install(&storage.installed_dir().join("outer-aaaa"));
        install(&storage.installed_dir().join("outer-cccc"));

        Resolution::new("outer", "aaaa", Vec::new()).store(&storage, "plain").unwrap();
        Resolution::new("outer", "cccc", Vec::new()).store(&storage, "plain").unwrap();
        assert_eq!(Resolution::load(&storage, "outer", "plain").unwrap().build_id, "cccc");
    }
}
