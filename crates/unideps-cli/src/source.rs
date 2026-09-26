use anyhow::{Context, Result};
use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use unideps_builder::git::{GitRef, GitSource};
use unideps_builder::lock::FileLock;
use unideps_builder::patch::PatchApplier;
use unideps_builder::storage::StorageManager;
use unideps_core::graph::DependencyNode;
use unideps_core::hash::HashCalculator;

pub struct Sources<'a> {
    pub storage: &'a StorageManager,
    /// Checkouts this run has already brought to the wanted revision. A floating ref is
    /// resolved before hashing and used again when the package is built; without this the
    /// whole fetch / reset / submodule walk would run a second time for every such package.
    synced: RefCell<HashSet<PathBuf>>,
}

impl<'a> Sources<'a> {
    pub fn new(storage: &'a StorageManager) -> Self {
        Self {
            storage,
            synced: RefCell::new(HashSet::new()),
        }
    }
}

fn git_ref(node: &DependencyNode) -> GitRef<'_> {
    GitRef::from_parts(node.commit.as_deref(), node.tag.as_deref(), node.branch.as_deref())
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

impl Sources<'_> {
    /// Includes a hash of the URL so that forks with the same name and ref never
    /// share a checkout.
    pub fn checkout_dir(&self, node: &DependencyNode, url: &str) -> PathBuf {
        let ref_part = match git_ref(node) {
            GitRef::Commit(c) => sanitize(&c.chars().take(12).collect::<String>()),
            GitRef::Tag(t) => sanitize(t),
            GitRef::Branch(b) => format!("branch-{}", sanitize(b)),
            GitRef::DefaultHead => "head".to_string(),
        };
        let url_hash = &HashCalculator::short_hash(url)[..8];
        self.storage.sources_dir().join(format!("{}-{ref_part}-{url_hash}", node.name))
    }

    /// Serialises the runs that share a storage directory on the level of one checkout,
    /// instead of making them wait for each other's whole build.
    fn lock_checkout(&self, dir: &Path) -> Result<FileLock> {
        let name = dir.file_name().unwrap_or_default().to_string_lossy().into_owned();
        FileLock::acquire(self.storage.source_lock_path(&name))
    }

    /// Brings `dir` to `git_ref`, at most once per run.
    fn checkout(&self, node: &DependencyNode, url: &str, dir: &Path) -> Result<String> {
        let _lock = self.lock_checkout(dir)?;
        let head = GitSource::fetch_and_checkout(url, dir, git_ref(node), node.shallow)
            .with_context(|| format!("Failed to fetch '{}'", node.name))?;
        self.synced.borrow_mut().insert(dir.to_path_buf());
        Ok(head)
    }

    /// Floating refs must be fetched before hashing so the commit becomes part of
    /// `source_id`. Tags and commits are fetched only when building, so cache hits
    /// need no network access.
    pub fn resolve_floating_ref(&self, node: &mut DependencyNode) -> Result<()> {
        if node.path.is_some() {
            return Ok(());
        }
        let Some(url) = node.git.clone() else { return Ok(()) };
        if !git_ref(node).is_floating() {
            return Ok(());
        }
        let dir = self.checkout_dir(node, &url);
        println!("[FETCH] {} ({url})", node.name);
        node.resolved_commit = Some(self.checkout(node, &url, &dir)?);
        Ok(())
    }

    pub fn base_source(&self, node: &DependencyNode) -> Result<PathBuf> {
        if let Some(ref p) = node.path {
            return Ok(p.clone());
        }
        let url = node
            .git
            .as_deref()
            .with_context(|| format!("'{}' has no git URL or path", node.name))?;
        let dir = self.checkout_dir(node, url);
        // Already brought up to date by `resolve_floating_ref`; repeating the whole fetch,
        // reset and submodule walk would only reproduce the tree that is already there. The
        // checkout is shared with other runs, so what it holds is still worth confirming.
        let head = if self.synced.borrow().contains(&dir) {
            GitSource::get_head_commit(&dir).with_context(|| format!("Failed to read the checkout of '{}'", node.name))?
        } else {
            self.checkout(node, url, &dir)?
        };
        if let Some(ref expected) = node.resolved_commit
            && &head != expected
        {
            anyhow::bail!("Source of '{}' changed while building (expected {expected}, got {head})", node.name);
        }
        Ok(dir)
    }

    pub fn prepared_source(&self, node: &DependencyNode, source_id: &str) -> Result<PathBuf> {
        let base = self.base_source(node)?;
        if node.patches.is_empty() {
            return Ok(base);
        }
        let patched = self.storage.sources_dir().join(format!("{}-{source_id}", node.name));
        let marker = patched.join(".unideps-patched");
        if marker.exists() {
            return Ok(patched);
        }
        // Another run may be building the same patched tree; re-check once it is done.
        let _lock = self.lock_checkout(&patched)?;
        if marker.exists() {
            return Ok(patched);
        }
        let tmp = self
            .storage
            .sources_dir()
            .join(format!("tmp-{}-{source_id}-{}", node.name, std::process::id()));
        let _ = GitSource::remove_dir_all_force(&tmp);
        std::fs::create_dir_all(&tmp)?;
        let result = (|| -> Result<()> {
            PatchApplier::copy_dir_recursive(&base, &tmp)?;
            for patch_path in &node.patches {
                PatchApplier::apply_patch(&tmp, patch_path)?;
            }
            std::fs::write(tmp.join(".unideps-patched"), b"")?;
            Ok(())
        })();
        if let Err(e) = result {
            let _ = GitSource::remove_dir_all_force(&tmp);
            return Err(e.context(format!("Failed to prepare patched source for '{}'", node.name)));
        }
        GitSource::remove_dir_all_force(&patched)?;
        if std::fs::rename(&tmp, &patched).is_err() {
            PatchApplier::copy_dir_recursive(&tmp, &patched)?;
            let _ = GitSource::remove_dir_all_force(&tmp);
        }
        Ok(patched)
    }
}
