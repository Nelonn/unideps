use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;

pub struct PatchApplier;

impl PatchApplier {
    pub fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
        let walker = walkdir::WalkDir::new(src)
            .into_iter()
            .filter_entry(|e| e.depth() != 1 || e.file_name() != ".git");
        for entry in walker {
            let entry = entry?;
            let rel_path = entry.path().strip_prefix(src)?;
            if rel_path.as_os_str().is_empty() {
                continue;
            }
            let target_path = dst.join(rel_path);
            let ft = entry.file_type();
            if ft.is_dir() {
                std::fs::create_dir_all(&target_path)?;
            } else if ft.is_file() {
                if let Some(parent) = target_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::copy(entry.path(), &target_path)
                    .with_context(|| format!("Failed to copy {}", entry.path().display()))?;
            } else if ft.is_symlink() {
                if let Some(parent) = target_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                Self::copy_symlink(entry.path(), &target_path)?;
            }
        }
        Ok(())
    }

    #[cfg(unix)]
    fn copy_symlink(src: &Path, dst: &Path) -> Result<()> {
        let target = std::fs::read_link(src)?;
        std::os::unix::fs::symlink(target, dst)?;
        Ok(())
    }

    #[cfg(not(unix))]
    fn copy_symlink(src: &Path, dst: &Path) -> Result<()> {
        // Symlinks usually need elevated rights on Windows; copy the pointed-to content.
        if src.is_dir() {
            Self::copy_dir_recursive(src, dst)
        } else if src.is_file() {
            std::fs::copy(src, dst)?;
            Ok(())
        } else {
            Ok(())
        }
    }

    pub fn apply_patch(target_dir: &Path, patch_path: &Path) -> Result<()> {
        let patch_canonical = dunce::canonicalize(patch_path)
            .with_context(|| format!("Patch file not found: {}", patch_path.display()))?;

        let mut errors = Vec::new();

        let mut git = Command::new("git");
        git.current_dir(target_dir)
            .args(["apply", "--ignore-whitespace", "--whitespace=nowarn", "-p1"])
            .arg(&patch_canonical);
        match git.output() {
            Ok(out) if out.status.success() => return Ok(()),
            Ok(out) => errors.push(format!("git apply: {}", String::from_utf8_lossy(&out.stderr).trim())),
            Err(e) => errors.push(format!("git apply: {e}")),
        }

        let mut patch = Command::new("patch");
        patch
            .current_dir(target_dir)
            .args(["-p1", "--forward", "--batch", "-i"])
            .arg(&patch_canonical);
        match patch.output() {
            Ok(out) if out.status.success() => return Ok(()),
            Ok(out) => errors.push(format!(
                "patch: {}{}",
                String::from_utf8_lossy(&out.stdout).trim(),
                String::from_utf8_lossy(&out.stderr).trim()
            )),
            Err(e) => errors.push(format!("patch: {e}")),
        }

        anyhow::bail!("Failed to apply patch {}:\n  {}", patch_path.display(), errors.join("\n  "));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_fixture(dir: &Path) -> std::path::PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("test.txt"), b"Line 1\nLine 2\nLine 3\n").unwrap();
        let patch_path = dir.parent().unwrap().join("change.patch");
        let patch_content = b"--- a/test.txt\n+++ b/test.txt\n@@ -1,3 +1,3 @@\n Line 1\n-Line 2\n+Line 2 Patched\n Line 3\n";
        std::fs::write(&patch_path, patch_content).unwrap();
        patch_path
    }

    #[test]
    fn test_apply_patch_success() {
        let temp = tempfile::tempdir().unwrap();
        let target_dir = temp.path().join("src");
        let patch_path = write_fixture(&target_dir);

        PatchApplier::apply_patch(&target_dir, &patch_path).unwrap();

        let result = std::fs::read_to_string(target_dir.join("test.txt")).unwrap();
        assert!(result.contains("Line 2 Patched"));
    }

    #[test]
    fn test_apply_patch_inside_foreign_repository() {
        let temp = tempfile::tempdir().unwrap();
        let out = Command::new("git").current_dir(temp.path()).args(["init", "--quiet"]).output().unwrap();
        assert!(out.status.success());
        let target_dir = temp.path().join("storage/sources/pkg");
        let patch_path = write_fixture(&target_dir);

        PatchApplier::apply_patch(&target_dir, &patch_path).unwrap();

        let result = std::fs::read_to_string(target_dir.join("test.txt")).unwrap();
        assert!(result.contains("Line 2 Patched"), "patch was silently skipped");
    }

    #[test]
    fn test_apply_patch_failure_reports_error() {
        let temp = tempfile::tempdir().unwrap();
        let target_dir = temp.path().join("src");
        let patch_path = write_fixture(&target_dir);
        std::fs::write(target_dir.join("test.txt"), b"something else\n").unwrap();
        assert!(PatchApplier::apply_patch(&target_dir, &patch_path).is_err());
    }

    #[test]
    fn test_copy_dir_recursive() {
        let temp = tempfile::tempdir().unwrap();
        let src = temp.path().join("src");
        let dst = temp.path().join("dst");

        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::create_dir_all(src.join(".git")).unwrap();
        std::fs::create_dir_all(src.join(".github")).unwrap();
        std::fs::write(src.join("file1.txt"), b"file1").unwrap();
        std::fs::write(src.join("sub/file2.txt"), b"file2").unwrap();
        std::fs::write(src.join(".git/HEAD"), b"x").unwrap();
        std::fs::write(src.join(".github/ci.yml"), b"y").unwrap();

        PatchApplier::copy_dir_recursive(&src, &dst).unwrap();

        assert_eq!(std::fs::read(dst.join("file1.txt")).unwrap(), b"file1");
        assert_eq!(std::fs::read(dst.join("sub/file2.txt")).unwrap(), b"file2");
        assert!(!dst.join(".git").exists());
        assert!(dst.join(".github/ci.yml").exists());
    }
}
