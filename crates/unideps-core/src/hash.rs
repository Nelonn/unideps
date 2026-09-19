use crate::graph::{DependencyNode, NodeKind};
use crate::target::{Environment, TargetTriple};
use std::path::Path;
use blake3::Hasher;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageMeta {
    pub name: String,
    pub version: Option<String>,
    pub build_id: String,
    pub target: String,
    pub shared: bool,
    pub cxx_runtime: String,
    pub cxx_std: String,
    pub cxx_stdlib: String,
    pub build_info: BuildInfoMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildInfoMeta {
    pub host_triple: String,
    pub timestamp: String,
    pub compiler: Option<String>,
    pub glibc_version: Option<String>,
}

pub struct HashCalculator;

/// Bump when default build behaviour changes to invalidate previously cached builds.
const BUILD_DEFAULTS_VERSION: &[u8] = b":build_defaults:v2";

impl HashCalculator {
    pub fn short_hash(data: &str) -> String {
        Self::short_hash_bytes(data.as_bytes())
    }

    pub fn short_hash_bytes(data: &[u8]) -> String {
        blake3::hash(data).to_hex()[..16].to_string()
    }

    /// Sorted so the result does not depend on directory iteration order.
    pub fn hash_dir_contents(dir: &Path) -> std::io::Result<String> {
        let mut hasher = Hasher::new();
        let walker = walkdir::WalkDir::new(dir)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|e| e.file_name() != ".git");
        for entry in walker {
            let entry = entry.map_err(std::io::Error::other)?;
            let rel = entry.path().strip_prefix(dir).unwrap_or(entry.path());
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            if entry.file_type().is_file() {
                hasher.update(b":file:");
                hasher.update(rel_str.as_bytes());
                hasher.update(b"\0");
                let mut f = std::fs::File::open(entry.path())?;
                std::io::copy(&mut f, &mut hasher)?;
            } else if entry.file_type().is_symlink() {
                hasher.update(b":link:");
                hasher.update(rel_str.as_bytes());
                hasher.update(b"\0");
                if let Ok(target) = std::fs::read_link(entry.path()) {
                    hasher.update(target.to_string_lossy().as_bytes());
                }
            }
        }
        Ok(hasher.finalize().to_hex()[..16].to_string())
    }

    pub fn calculate_source_id(node: &DependencyNode) -> String {
        let mut hasher = Hasher::new();

        hasher.update(node.name.as_bytes());

        if let Some(ref v) = node.version {
            hasher.update(b":v:");
            hasher.update(v.as_bytes());
        }
        if let Some(ref url) = node.git {
            hasher.update(b":git:");
            hasher.update(url.as_bytes());
        }
        if let Some(ref commit) = node.commit {
            hasher.update(b":commit:");
            hasher.update(commit.as_bytes());
        } else if let Some(ref tag) = node.tag {
            hasher.update(b":tag:");
            hasher.update(tag.as_bytes());
        } else if let Some(ref branch) = node.branch {
            hasher.update(b":branch:");
            hasher.update(branch.as_bytes());
        }
        if node.commit.is_none()
            && node.tag.is_none()
            && let Some(ref resolved) = node.resolved_commit
        {
            hasher.update(b":resolved:");
            hasher.update(resolved.as_bytes());
        }
        if let Some(ref path) = node.path {
            hasher.update(b":path:");
            match Self::hash_dir_contents(path) {
                Ok(h) => hasher.update(h.as_bytes()),
                Err(_) => hasher.update(path.to_string_lossy().as_bytes()),
            };
        }

        for patch in &node.patches {
            hasher.update(b":patch:");
            let patch_name = patch.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            hasher.update(patch_name.as_bytes());
            if let Ok(content) = std::fs::read(patch) {
                hasher.update(b":content:");
                hasher.update(&content);
            }
        }

        let hash = hasher.finalize();
        hash.to_hex()[..16].to_string()
    }

    pub fn calculate_build_id(
        node: &DependencyNode,
        target: &TargetTriple,
        host_target: &TargetTriple,
        dep_build_ids: &[&str],
    ) -> String {
        let source_id = node
            .source_id
            .clone()
            .unwrap_or_else(|| Self::calculate_source_id(node));

        let mut hasher = Hasher::new();
        hasher.update(BUILD_DEFAULTS_VERSION);
        hasher.update(b":source_id:");
        hasher.update(source_id.as_bytes());

        let effective_target = match node.kind {
            NodeKind::TargetDependency => target,
            NodeKind::HostTool => host_target,
        };

        hasher.update(b":target:");
        hasher.update(effective_target.raw.as_bytes());

        let ignored = |key: &str| node.abi_ignores.iter().any(|i| i == key);

        if !ignored("compiler") {
            if let Some(ref comp) = node.compiler {
                hasher.update(b":compiler:");
                hasher.update(comp.as_str().as_bytes());
            }
            if let Some(ref fp) = node.toolchain_fingerprint {
                hasher.update(b":toolchain:");
                hasher.update(fp.as_bytes());
            }
        }

        // The MSVC runtime only affects MSVC-ABI targets.
        if effective_target.env == Environment::Msvc && !ignored("vc_runtime") && !ignored("cxx_runtime") {
            hasher.update(b":vc_runtime:");
            hasher.update(format!("{:?}", node.config.vc_runtime).as_bytes());
        }

        if !ignored("cxx_std") {
            hasher.update(b":cxx_std:");
            hasher.update(node.config.cxx_std.as_bytes());
        }

        if !ignored("cxx_stdlib") {
            hasher.update(b":cxx_stdlib:");
            hasher.update(format!("{:?}", node.config.cxx_stdlib).as_bytes());
        }

        hasher.update(b":build_type:");
        hasher.update(format!("{:?}", node.config.build_type).as_bytes());

        let is_shared = node.shared.unwrap_or(node.config.shared);
        hasher.update(b":shared:");
        hasher.update(if is_shared { b"true" } else { b"false" });

        hasher.update(b":header_only:");
        hasher.update(if node.header_only { b"true" } else { b"false" });

        hasher.update(b":lto:");
        hasher.update(if node.config.lto { b"true" } else { b"false" });

        for flag in &node.config.flags {
            hasher.update(b":flag:");
            hasher.update(flag.as_bytes());
        }

        for (k, v) in &node.config.cmake_options {
            hasher.update(b":cmake_opt:");
            hasher.update(k.as_bytes());
            hasher.update(b"=");
            hasher.update(v.as_bytes());
        }

        for (k, v) in &node.cmake_options {
            hasher.update(b":node_opt:");
            hasher.update(k.as_bytes());
            hasher.update(b"=");
            hasher.update(v.as_bytes());
        }

        for dep_id in dep_build_ids {
            hasher.update(b":dep_id:");
            hasher.update(dep_id.as_bytes());
        }

        let hash = hasher.finalize();
        hash.to_hex()[..16].to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::CompilerType;
    use crate::manifest::{DependencyDetails, DependencySpec};

    #[test]
    fn test_compiler_separation_in_build_id() {
        let spec = DependencySpec::Detailed(DependencyDetails {
            version: Some("1.0.0".into()),
            ..Default::default()
        });
        let mut node_clang = DependencyNode::from_target_dep("foo", &spec);
        node_clang.compiler = Some(CompilerType::Clang);

        let mut node_msvc = DependencyNode::from_target_dep("foo", &spec);
        node_msvc.compiler = Some(CompilerType::Msvc);

        let mut node_gcc = DependencyNode::from_target_dep("foo", &spec);
        node_gcc.compiler = Some(CompilerType::Gcc);

        let target: TargetTriple = "x86_64-pc-windows-msvc".parse().unwrap();
        let host: TargetTriple = "x86_64-pc-windows-msvc".parse().unwrap();

        let id_clang = HashCalculator::calculate_build_id(&node_clang, &target, &host, &[]);
        let id_msvc = HashCalculator::calculate_build_id(&node_msvc, &target, &host, &[]);
        let id_gcc = HashCalculator::calculate_build_id(&node_gcc, &target, &host, &[]);

        assert_ne!(id_clang, id_msvc);
        assert_ne!(id_clang, id_gcc);
        assert_ne!(id_msvc, id_gcc);
    }

    #[test]
    fn test_patches_content_affects_hash() {
        let temp = tempfile::tempdir().unwrap();
        let patch_a = temp.path().join("fix.patch");
        let patch_b = temp.path().join("fix2.patch");

        std::fs::write(&patch_a, b"content A").unwrap();
        std::fs::write(&patch_b, b"content B").unwrap();

        let spec = DependencySpec::Detailed(DependencyDetails {
            version: Some("1.0.0".into()),
            ..Default::default()
        });

        let mut node1 = DependencyNode::from_target_dep("foo", &spec);
        node1.patches = vec![patch_a.clone()];

        let mut node2 = DependencyNode::from_target_dep("foo", &spec);
        node2.patches = vec![patch_b.clone()];

        let src_id1 = HashCalculator::calculate_source_id(&node1);
        let src_id2 = HashCalculator::calculate_source_id(&node2);
        assert_ne!(src_id1, src_id2);

        let target: TargetTriple = "x86_64-pc-windows-msvc".parse().unwrap();
        let host: TargetTriple = "x86_64-pc-windows-msvc".parse().unwrap();

        let build_id1 = HashCalculator::calculate_build_id(&node1, &target, &host, &[]);
        let build_id2 = HashCalculator::calculate_build_id(&node2, &target, &host, &[]);
        assert_ne!(build_id1, build_id2);

        std::fs::write(&patch_a, b"content Modified").unwrap();
        let src_id1_modified = HashCalculator::calculate_source_id(&node1);
        assert_ne!(src_id1, src_id1_modified);
    }

    fn node(spec: DependencyDetails) -> DependencyNode {
        DependencyNode::from_target_dep("foo", &DependencySpec::Detailed(spec))
    }

    #[test]
    fn test_git_url_affects_source_id() {
        let a = node(DependencyDetails { git: Some("https://a/x.git".into()), tag: Some("v1".into()), ..Default::default() });
        let b = node(DependencyDetails { git: Some("https://b/x.git".into()), tag: Some("v1".into()), ..Default::default() });
        assert_ne!(HashCalculator::calculate_source_id(&a), HashCalculator::calculate_source_id(&b));
    }

    #[test]
    fn test_resolved_commit_affects_branch_source_id() {
        let mut a = node(DependencyDetails { git: Some("u".into()), branch: Some("main".into()), ..Default::default() });
        let mut b = a.clone();
        a.resolved_commit = Some("1111".into());
        b.resolved_commit = Some("2222".into());
        assert_ne!(HashCalculator::calculate_source_id(&a), HashCalculator::calculate_source_id(&b));
    }

    #[test]
    fn test_path_contents_affect_source_id() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("a.c"), b"int a;").unwrap();
        let n = node(DependencyDetails { path: Some(temp.path().to_path_buf()), ..Default::default() });
        let before = HashCalculator::calculate_source_id(&n);
        std::fs::create_dir_all(temp.path().join(".git")).unwrap();
        std::fs::write(temp.path().join(".git/index"), b"ignored").unwrap();
        assert_eq!(before, HashCalculator::calculate_source_id(&n));
        std::fs::write(temp.path().join("a.c"), b"int b;").unwrap();
        assert_ne!(before, HashCalculator::calculate_source_id(&n));
    }

    #[test]
    fn test_vc_runtime_only_hashed_for_msvc() {
        let mut md = node(DependencyDetails::default());
        let mut mdd = md.clone();
        md.config.vc_runtime = crate::target::VCRuntime::MultiThreadedDLL;
        mdd.config.vc_runtime = crate::target::VCRuntime::MultiThreadedDebugDLL;
        let linux: TargetTriple = "x86_64-unknown-linux-gnu".parse().unwrap();
        let win: TargetTriple = "x86_64-pc-windows-msvc".parse().unwrap();
        let id = |n: &DependencyNode, t: &TargetTriple| HashCalculator::calculate_build_id(n, t, t, &[]);
        assert_eq!(id(&md, &linux), id(&mdd, &linux));
        assert_ne!(id(&md, &win), id(&mdd, &win));
    }

    #[test]
    fn test_toolchain_fingerprint_affects_build_id() {
        let mut a = node(DependencyDetails::default());
        let mut b = a.clone();
        a.toolchain_fingerprint = Some("gcc-11".into());
        b.toolchain_fingerprint = Some("gcc-13".into());
        let t: TargetTriple = "x86_64-unknown-linux-gnu".parse().unwrap();
        assert_ne!(
            HashCalculator::calculate_build_id(&a, &t, &t, &[]),
            HashCalculator::calculate_build_id(&b, &t, &t, &[])
        );
    }
}
