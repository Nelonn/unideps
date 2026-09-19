use anyhow::{Context, Result};
use std::path::Path;
use std::process::{Command, Output};

/// Priority when a manifest sets several: commit > tag > branch > default HEAD.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitRef<'a> {
    Commit(&'a str),
    Tag(&'a str),
    Branch(&'a str),
    DefaultHead,
}

impl<'a> GitRef<'a> {
    pub fn from_parts(commit: Option<&'a str>, tag: Option<&'a str>, branch: Option<&'a str>) -> Self {
        if let Some(c) = commit {
            GitRef::Commit(c)
        } else if let Some(t) = tag {
            GitRef::Tag(t)
        } else if let Some(b) = branch {
            GitRef::Branch(b)
        } else {
            GitRef::DefaultHead
        }
    }

    /// Branches and the default HEAD move over time and must be re-resolved on every run.
    pub fn is_floating(&self) -> bool {
        matches!(self, GitRef::Branch(_) | GitRef::DefaultHead)
    }

    fn describe(&self) -> String {
        match self {
            GitRef::Commit(c) => format!("commit {c}"),
            GitRef::Tag(t) => format!("tag {t}"),
            GitRef::Branch(b) => format!("branch {b}"),
            GitRef::DefaultHead => "default branch".into(),
        }
    }
}

/// Written last, so an interrupted checkout or submodule update is detected and resumed.
const CHECKOUT_MARKER: &str = "unideps-checkout";

pub struct GitSource;

impl GitSource {
    /// Returns the checked-out commit. A missing ref is an error, never a silent
    /// fallback to another revision.
    pub fn fetch_and_checkout(url: &str, dest: impl AsRef<Path>, git_ref: GitRef, shallow: bool) -> Result<String> {
        let dest_path = dest.as_ref();

        if let GitRef::Commit(c) = git_ref
            && (c.len() < 7 || !c.chars().all(|ch| ch.is_ascii_hexdigit()))
        {
            anyhow::bail!("Invalid commit '{c}' for {url}: expected at least 7 hex characters");
        }

        if dest_path.exists() {
            match Self::existing_checkout_state(dest_path, url, git_ref) {
                CheckoutState::UpToDate(head) => return Ok(head),
                CheckoutState::Reusable => {
                    return Self::sync(dest_path, url, git_ref, shallow);
                }
                CheckoutState::Invalid => Self::remove_dir_all_force(dest_path)?,
            }
        }

        std::fs::create_dir_all(dest_path)?;
        Self::run(dest_path, &["init", "--quiet"], "git init")?;
        Self::run(dest_path, &["remote", "add", "origin", url], "git remote add")?;

        match Self::sync(dest_path, url, git_ref, shallow) {
            Ok(head) => Ok(head),
            Err(e) => {
                let _ = Self::remove_dir_all_force(dest_path);
                Err(e)
            }
        }
    }

    fn existing_checkout_state(dest: &Path, url: &str, git_ref: GitRef) -> CheckoutState {
        if !dest.join(".git").is_dir() {
            return CheckoutState::Invalid;
        }
        let remote_matches = Self::capture(dest, &["remote", "get-url", "origin"])
            .map(|u| u == url)
            .unwrap_or(false);
        if !remote_matches {
            return CheckoutState::Invalid;
        }
        let Ok(head) = Self::get_head_commit(dest) else {
            return CheckoutState::Invalid;
        };
        let marker = std::fs::read_to_string(dest.join(".git").join(CHECKOUT_MARKER)).unwrap_or_default();
        if marker.trim() != head {
            // Interrupted run: resume in place instead of re-cloning.
            return CheckoutState::Reusable;
        }
        let matches = match git_ref {
            GitRef::Commit(c) => head.starts_with(&c.to_lowercase()),
            GitRef::Tag(t) => Self::capture(dest, &["rev-parse", &format!("refs/tags/{t}^{{commit}}")])
                .map(|tc| tc == head)
                .unwrap_or(false),
            GitRef::Branch(_) | GitRef::DefaultHead => false,
        };
        if matches {
            CheckoutState::UpToDate(head)
        } else {
            CheckoutState::Reusable
        }
    }

    fn sync(dest: &Path, url: &str, git_ref: GitRef, shallow: bool) -> Result<String> {
        let depth: &[&str] = if shallow { &["--depth", "1"] } else { &[] };
        let fetch = |refspec: &str| -> Result<Output> {
            let mut args = vec!["fetch", "--quiet", "--force"];
            args.extend_from_slice(depth);
            args.extend_from_slice(&["origin", refspec]);
            Self::output(dest, &args)
        };

        let target = match git_ref {
            GitRef::Tag(t) => {
                let out = fetch(&format!("+refs/tags/{t}:refs/tags/{t}"))?;
                Self::check(&out, &format!("Failed to fetch tag '{t}' from {url}"))?;
                format!("refs/tags/{t}^{{commit}}")
            }
            GitRef::Branch(b) => {
                let out = fetch(&format!("+refs/heads/{b}:refs/remotes/origin/{b}"))?;
                Self::check(&out, &format!("Failed to fetch branch '{b}' from {url}"))?;
                format!("refs/remotes/origin/{b}")
            }
            GitRef::DefaultHead => {
                let out = fetch("HEAD")?;
                Self::check(&out, &format!("Failed to fetch default branch from {url}"))?;
                "FETCH_HEAD".to_string()
            }
            GitRef::Commit(c) => {
                // Servers that allow fetching by SHA (GitHub, GitLab) take the fast path;
                // otherwise fetch everything and look for the commit locally.
                let direct = fetch(c)?;
                if !direct.status.success() {
                    let mut args = vec!["fetch", "--quiet", "--tags"];
                    if dest.join(".git").join("shallow").exists() {
                        args.push("--unshallow");
                    }
                    args.extend_from_slice(&["origin", "+refs/heads/*:refs/remotes/origin/*"]);
                    let full = Self::output(dest, &args)?;
                    Self::check(&full, &format!("Failed to fetch {url}"))?;
                }
                format!("{c}^{{commit}}")
            }
        };

        let resolved = Self::capture(dest, &["rev-parse", "--verify", "--quiet", &target]).map_err(|_| {
            anyhow::anyhow!("{} not found in {url}", git_ref.describe())
        })?;

        Self::run(dest, &["reset", "--quiet", "--hard", &resolved], "git reset --hard")?;
        // reset --hard keeps untracked files, e.g. submodules deleted upstream;
        // a single -f would skip them because they are nested repositories.
        Self::run(dest, &["clean", "-ffdxq"], "git clean")?;
        Self::update_submodules(dest, shallow)?;

        let head = Self::get_head_commit(dest)?;
        if head != resolved {
            anyhow::bail!("Checkout of {} in {} ended at {head}, expected {resolved}", git_ref.describe(), dest.display());
        }
        std::fs::write(dest.join(".git").join(CHECKOUT_MARKER), &head)?;
        Ok(head)
    }

    fn update_submodules(dest: &Path, shallow: bool) -> Result<()> {
        if !dest.join(".gitmodules").exists() {
            return Ok(());
        }
        // update alone keeps using the URLs recorded at init time.
        Self::run(dest, &["submodule", "sync", "--quiet", "--recursive"], "git submodule sync")?;

        if Self::try_update_submodules(dest, shallow)? {
            return Self::clean_submodules(dest);
        }
        // Cached module repositories in .git/modules keep their old remote when a
        // submodule is re-added from a different URL (sync skips uninitialised ones),
        // so the pinned commit cannot be found. Re-clone all submodules from scratch.
        GitSource::remove_dir_all_force(dest.join(".git").join("modules"))?;
        let _ = Self::output(dest, &["submodule", "deinit", "--all", "--force", "--quiet"]);
        Self::run(dest, &["submodule", "sync", "--quiet", "--recursive"], "git submodule sync")?;
        if Self::try_update_submodules(dest, shallow)? {
            return Self::clean_submodules(dest);
        }
        Self::run(dest, &["submodule", "update", "--init", "--recursive", "--force"], "git submodule update")?;
        Self::clean_submodules(dest)
    }

    /// Shallow first when requested; servers that refuse fetching unadvertised SHAs
    /// cannot serve a shallow submodule pinned to a non-tip commit, so retry in full.
    fn try_update_submodules(dest: &Path, shallow: bool) -> Result<bool> {
        let base = ["submodule", "update", "--init", "--recursive", "--force"];
        if shallow {
            let mut args = base.to_vec();
            args.extend_from_slice(&["--depth", "1"]);
            if Self::output(dest, &args)?.status.success() {
                return Ok(true);
            }
        }
        Ok(Self::output(dest, &base)?.status.success())
    }

    fn clean_submodules(dest: &Path) -> Result<()> {
        Self::run(
            dest,
            &["submodule", "foreach", "--quiet", "--recursive", "git", "clean", "-ffdxq"],
            "git clean in submodules",
        )
    }

    pub fn get_head_commit(repo_path: impl AsRef<Path>) -> Result<String> {
        Self::capture(repo_path.as_ref(), &["rev-parse", "HEAD"])
    }

    fn command(dir: &Path, args: &[&str]) -> Command {
        let mut cmd = Command::new("git");
        // Tests use local-path submodules, which git blocks by default since 2.38.1.
        #[cfg(test)]
        cmd.args(["-c", "protocol.file.allow=always"]);
        cmd.current_dir(dir)
            .args(args)
            // Runs inside a CMake configure with no terminal; a prompt would hang it.
            .env("GIT_TERMINAL_PROMPT", "0");
        // If dest/.git is missing or broken, git would walk up into an enclosing
        // repository (the user's project when storage lives inside it) and
        // `reset --hard` / `clean -ffdx` would destroy it.
        if let Some(parent) = dir.parent() {
            cmd.env("GIT_CEILING_DIRECTORIES", parent);
        }
        cmd
    }

    fn output(dir: &Path, args: &[&str]) -> Result<Output> {
        Self::command(dir, args)
            .output()
            .with_context(|| format!("Failed to run git {}", args.join(" ")))
    }

    fn check(out: &Output, what: &str) -> Result<()> {
        if out.status.success() {
            Ok(())
        } else {
            anyhow::bail!("{what}: {}", String::from_utf8_lossy(&out.stderr).trim())
        }
    }

    fn run(dir: &Path, args: &[&str], what: &str) -> Result<()> {
        let out = Self::output(dir, args)?;
        Self::check(&out, &format!("{what} failed in {}", dir.display()))
    }

    fn capture(dir: &Path, args: &[&str]) -> Result<String> {
        let out = Self::output(dir, args)?;
        Self::check(&out, &format!("git {}", args.join(" ")))?;
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    pub fn remove_dir_all_force(path: impl AsRef<Path>) -> Result<()> {
        let p = path.as_ref();
        if !p.exists() {
            return Ok(());
        }
        // Git marks object files read-only, which makes remove_dir_all fail on Windows.
        #[cfg(windows)]
        for entry in walkdir::WalkDir::new(p).into_iter().filter_map(|e| e.ok()) {
            if let Ok(metadata) = entry.path().symlink_metadata() {
                let mut perms = metadata.permissions();
                if perms.readonly() {
                    #[allow(clippy::permissions_set_readonly_false)]
                    perms.set_readonly(false);
                    let _ = std::fs::set_permissions(entry.path(), perms);
                }
            }
        }
        std::fs::remove_dir_all(p).with_context(|| format!("Failed to remove directory {}", p.display()))?;
        Ok(())
    }
}

enum CheckoutState {
    UpToDate(String),
    Reusable,
    Invalid,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Upstream repo with: commit c1 tagged v1, commit c2 on branch main (default).
    fn upstream(root: &Path) -> (String, String, String) {
        let up = root.join("upstream");
        std::fs::create_dir_all(&up).unwrap();
        git(&up, &["init", "--quiet", "-b", "main"]);
        std::fs::write(up.join("f.txt"), "one").unwrap();
        git(&up, &["add", "."]);
        git(&up, &["commit", "--quiet", "-m", "c1"]);
        git(&up, &["tag", "v1"]);
        let c1 = git(&up, &["rev-parse", "HEAD"]);
        std::fs::write(up.join("f.txt"), "two").unwrap();
        git(&up, &["commit", "--quiet", "-am", "c2"]);
        let c2 = git(&up, &["rev-parse", "HEAD"]);
        let url = up.to_string_lossy().replace('\\', "/");
        (url, c1, c2)
    }

    #[test]
    fn checks_out_tag_branch_and_commit() {
        let temp = tempfile::tempdir().unwrap();
        let (url, c1, c2) = upstream(temp.path());

        let tag_dir = temp.path().join("tag");
        assert_eq!(GitSource::fetch_and_checkout(&url, &tag_dir, GitRef::Tag("v1"), true).unwrap(), c1);
        assert_eq!(std::fs::read_to_string(tag_dir.join("f.txt")).unwrap(), "one");
        assert_eq!(GitSource::fetch_and_checkout(&url, &tag_dir, GitRef::Tag("v1"), true).unwrap(), c1);

        let br_dir = temp.path().join("branch");
        assert_eq!(GitSource::fetch_and_checkout(&url, &br_dir, GitRef::Branch("main"), true).unwrap(), c2);

        let c_dir = temp.path().join("commit");
        assert_eq!(GitSource::fetch_and_checkout(&url, &c_dir, GitRef::Commit(&c1[..10]), false).unwrap(), c1);
        assert_eq!(std::fs::read_to_string(c_dir.join("f.txt")).unwrap(), "one");
    }

    #[test]
    fn missing_ref_is_an_error_not_a_fallback() {
        let temp = tempfile::tempdir().unwrap();
        let (url, _, _) = upstream(temp.path());
        let dest = temp.path().join("x");
        assert!(GitSource::fetch_and_checkout(&url, &dest, GitRef::Tag("v9.9.9"), true).is_err());
        assert!(GitSource::fetch_and_checkout(&url, &dest, GitRef::Branch("nope"), true).is_err());
        assert!(GitSource::fetch_and_checkout(&url, &dest, GitRef::Commit("deadbeefdeadbeef"), true).is_err());
        assert!(GitSource::fetch_and_checkout(&url, &dest, GitRef::Commit("xyz"), true).is_err());
        assert!(!dest.exists());
    }

    #[test]
    fn branch_is_refreshed_when_upstream_moves() {
        let temp = tempfile::tempdir().unwrap();
        let (url, _, c2) = upstream(temp.path());
        let dest = temp.path().join("br");
        assert_eq!(GitSource::fetch_and_checkout(&url, &dest, GitRef::Branch("main"), true).unwrap(), c2);

        let up = temp.path().join("upstream");
        std::fs::write(up.join("f.txt"), "three").unwrap();
        git(&up, &["commit", "--quiet", "-am", "c3"]);
        let c3 = git(&up, &["rev-parse", "HEAD"]);

        assert_eq!(GitSource::fetch_and_checkout(&url, &dest, GitRef::Branch("main"), true).unwrap(), c3);
        assert_eq!(std::fs::read_to_string(dest.join("f.txt")).unwrap(), "three");
    }

    #[test]
    fn different_remote_url_triggers_fresh_clone() {
        let temp = tempfile::tempdir().unwrap();
        let (url, c1, _) = upstream(temp.path());
        let fork = temp.path().join("fork");
        git(temp.path(), &["clone", "--quiet", &url, "fork"]);
        std::fs::write(fork.join("f.txt"), "fork").unwrap();
        git(&fork, &["commit", "--quiet", "-am", "fork change"]);
        git(&fork, &["tag", "-f", "v1"]);
        let fork_url = fork.to_string_lossy().replace('\\', "/");

        let dest = temp.path().join("dest");
        assert_eq!(GitSource::fetch_and_checkout(&url, &dest, GitRef::Tag("v1"), true).unwrap(), c1);
        let fork_head = GitSource::fetch_and_checkout(&fork_url, &dest, GitRef::Tag("v1"), true).unwrap();
        assert_ne!(fork_head, c1);
        assert_eq!(std::fs::read_to_string(dest.join("f.txt")).unwrap(), "fork");
    }

    #[test]
    fn submodules_follow_the_superproject() {
        let temp = tempfile::tempdir().unwrap();
        let url_of = |p: &Path| p.to_string_lossy().replace('\\', "/");

        // Non-tip pins: a shallow submodule fetch cannot simply take the branch head.
        let sub = temp.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        git(&sub, &["init", "--quiet", "-b", "main"]);
        let mut sub_commits = Vec::new();
        for v in ["s1", "s2", "s3"] {
            std::fs::write(sub.join("v.txt"), v).unwrap();
            git(&sub, &["add", "."]);
            git(&sub, &["commit", "--quiet", "-m", v]);
            sub_commits.push(git(&sub, &["rev-parse", "HEAD"]));
        }

        let sup = temp.path().join("super");
        std::fs::create_dir_all(&sup).unwrap();
        git(&sup, &["init", "--quiet", "-b", "main"]);
        git(&sup, &["-c", "protocol.file.allow=always", "submodule", "add", "--quiet", &url_of(&sub), "third_party/sub"]);
        git(&sup.join("third_party/sub"), &["checkout", "--quiet", &sub_commits[0]]);
        git(&sup, &["add", "."]);
        git(&sup, &["commit", "--quiet", "-m", "pin s1"]);

        let dest = temp.path().join("checkout");
        let read_sub = || std::fs::read_to_string(dest.join("third_party/sub/v.txt")).unwrap();

        GitSource::fetch_and_checkout(&url_of(&sup), &dest, GitRef::Branch("main"), true).unwrap();
        assert_eq!(read_sub(), "s1");

        git(&sup.join("third_party/sub"), &["checkout", "--quiet", &sub_commits[1]]);
        git(&sup, &["commit", "--quiet", "-am", "pin s2"]);
        GitSource::fetch_and_checkout(&url_of(&sup), &dest, GitRef::Branch("main"), true).unwrap();
        assert_eq!(read_sub(), "s2");

        git(&sup, &["rm", "--quiet", "third_party/sub"]);
        git(&sup, &["commit", "--quiet", "-m", "drop sub"]);
        GitSource::fetch_and_checkout(&url_of(&sup), &dest, GitRef::Branch("main"), true).unwrap();
        assert!(!dest.join("third_party/sub/v.txt").exists(), "stale submodule left behind");

        // Same path, new URL: the cached module repo in .git/modules still points at `sub`.
        let fork = temp.path().join("fork");
        git(temp.path(), &["clone", "--quiet", &url_of(&sub), "fork"]);
        std::fs::write(fork.join("v.txt"), "fork").unwrap();
        git(&fork, &["commit", "--quiet", "-am", "fork"]);
        git(&sup, &["-c", "protocol.file.allow=always", "submodule", "add", "--quiet", "--force", &url_of(&fork), "third_party/sub"]);
        // `add --force` reuses the cached module repo; point it at the fork's commit explicitly.
        let sup_sub = sup.join("third_party/sub");
        git(&sup_sub, &["fetch", "--quiet", &url_of(&fork), "main"]);
        git(&sup_sub, &["checkout", "--quiet", "FETCH_HEAD"]);
        git(&sup, &["add", "third_party/sub"]);
        git(&sup, &["commit", "--quiet", "-am", "use fork"]);
        assert!(std::fs::read_to_string(sup.join(".gitmodules")).unwrap().contains("fork"));
        GitSource::fetch_and_checkout(&url_of(&sup), &dest, GitRef::Branch("main"), true).unwrap();
        assert_eq!(read_sub(), "fork");
    }

    #[test]
    fn test_remove_dir_all_force() {
        let temp = tempfile::tempdir().unwrap();
        let sub = temp.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let file = sub.join("dummy.txt");
        std::fs::write(&file, "hello").unwrap();

        assert!(file.exists());
        GitSource::remove_dir_all_force(&sub).unwrap();
        assert!(!sub.exists());
    }
}
