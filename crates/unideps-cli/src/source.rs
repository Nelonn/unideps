use anyhow::{Context, Result};
use std::path::PathBuf;
use unideps_builder::git::{GitRef, GitSource};
use unideps_builder::patch::PatchApplier;
use unideps_builder::storage::StorageManager;
use unideps_core::graph::DependencyNode;
use unideps_core::hash::HashCalculator;

pub struct Sources<'a> {
    pub storage: &'a StorageManager,
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
        let head = GitSource::fetch_and_checkout(&url, &dir, git_ref(node), node.shallow)
            .with_context(|| format!("Failed to fetch '{}'", node.name))?;
        node.resolved_commit = Some(head);
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
        let head = GitSource::fetch_and_checkout(url, &dir, git_ref(node), node.shallow)
            .with_context(|| format!("Failed to fetch '{}'", node.name))?;
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
