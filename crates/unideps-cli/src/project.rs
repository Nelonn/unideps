use crate::util::{resolve_relative, warn};
use anyhow::{Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use unideps_core::graph::{DependencyGraph, DependencyNode, NodeKind};
use unideps_core::manifest::{LocalConfig, Manifest, TargetSysrootConfig};
use unideps_core::strategy::StrategyEngine;
use unideps_core::target::TargetTriple;
use unideps_recipe::engine::LuaRecipeEngine;
use unideps_recipe::recipe::Recipe;

pub const SUPPORTED_STRATEGIES: &[&str] = &["cmake-install"];

pub struct Project {
    pub manifest_dir: PathBuf,
    pub manifest: Manifest,
    pub local: Option<LocalConfig>,
}

impl Project {
    pub fn load(manifest_path: &Path) -> Result<Self> {
        Self::load_with(manifest_path, true)
    }

    /// Manifest of a dependency's source tree. Storage, tools and resource limits
    /// belong to the outer project, so a `.local.toml` shipped in the source is ignored.
    pub fn load_nested(manifest_path: &Path) -> Result<Self> {
        Self::load_with(manifest_path, false)
    }

    fn load_with(manifest_path: &Path, read_local: bool) -> Result<Self> {
        if !manifest_path.exists() {
            anyhow::bail!("Manifest file not found: {}", manifest_path.display());
        }
        let manifest_path = dunce_abs(manifest_path);
        let manifest_dir = manifest_path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
        let manifest = Manifest::from_file(&manifest_path)?;
        let local_path = manifest_dir.join(".local.toml");
        let local = if read_local && local_path.exists() {
            Some(LocalConfig::from_file(&local_path)?)
        } else {
            None
        };
        StrategyEngine::validate(&manifest.strategy).map_err(anyhow::Error::msg)?;
        let project = Self {
            manifest_dir,
            manifest,
            local,
        };
        project.warn_unsupported();
        Ok(project)
    }

    fn warn_unsupported(&self) {
        if !self.manifest.registries.is_empty() {
            warn("[registries] are not supported yet and are ignored");
        }
        let Some(ref local) = self.local else { return };
        if let Some(ref c) = local.cache
            && (c.remote_url.is_some() || c.auth_token.is_some() || c.upload.is_some() || c.local_dir.is_some())
        {
            warn(".local.toml: [cache] settings are not supported yet and are ignored");
        }
        if let Some(ref r) = local.resources
            && r.max_memory_gb.is_some()
        {
            warn(".local.toml: resources.max_memory_gb is not supported yet and is ignored");
        }
        if let Some(ref s) = local.storage
            && (s.scratch_mode.is_some() || s.cache_sources.is_some())
        {
            warn(".local.toml: storage.scratch_mode / storage.cache_sources are not supported yet and are ignored");
        }
    }

    pub fn base_dir(&self, cli: Option<&Path>) -> PathBuf {
        resolve_base_dir(cli, self.local.as_ref(), &self.manifest_dir)
    }

    pub fn scratch_dir(&self) -> Option<PathBuf> {
        let dir = self.local.as_ref()?.storage.as_ref()?.scratch_dir.as_ref()?;
        Some(resolve_relative(&self.manifest_dir, dir))
    }

    /// Build directories are scratch space: they are removed after a successful build
    /// unless `storage.keep_build_dirs` in `.local.toml` asks to keep them.
    pub fn keep_build_dirs(&self) -> bool {
        self.local
            .as_ref()
            .and_then(|l| l.storage.as_ref())
            .and_then(|s| s.keep_build_dirs)
            .unwrap_or(false)
    }

    pub fn max_jobs(&self) -> Option<usize> {
        self.local.as_ref()?.resources.as_ref()?.max_jobs
    }

    pub fn local_tool_path(&self, name: &str) -> Option<PathBuf> {
        let p = self.local.as_ref()?.tools.get(name)?;
        Some(resolve_relative(&self.manifest_dir, p))
    }

    pub fn target_config(&self, target: &TargetTriple) -> TargetSysrootConfig {
        let mut cfg = self.manifest.targets.get(&target.raw).cloned().unwrap_or_default();
        if let Some(local) = self.local.as_ref().and_then(|l| l.targets.get(&target.raw)) {
            let l = local.clone();
            cfg.sysroot = l.sysroot.or(cfg.sysroot);
            cfg.toolchain_file = l.toolchain_file.or(cfg.toolchain_file);
            cfg.c_compiler = l.c_compiler.or(cfg.c_compiler);
            cfg.cxx_compiler = l.cxx_compiler.or(cfg.cxx_compiler);
            cfg.asm_compiler = l.asm_compiler.or(cfg.asm_compiler);
            cfg.compiler = l.compiler.or(cfg.compiler);
        }
        let dir = &self.manifest_dir;
        // Paths with separators are relative to the manifest; bare names are looked up on PATH.
        let fix = |p: Option<PathBuf>| {
            p.map(|p| {
                if p.components().count() > 1 { resolve_relative(dir, &p) } else { p }
            })
        };
        cfg.toolchain_file = cfg.toolchain_file.map(|p| resolve_relative(dir, &p));
        cfg.c_compiler = fix(cfg.c_compiler);
        cfg.cxx_compiler = fix(cfg.cxx_compiler);
        cfg
    }

    /// Unfiltered: edges are added by [`connect_graph`] after platform/condition filtering.
    pub fn build_graph(&self) -> Result<DependencyGraph> {
        let mut graph = DependencyGraph::new();

        for (tool_name, tool_req) in &self.manifest.tools {
            let mut node = DependencyNode::from_host_tool(tool_name, tool_req);
            node.path = node.path.map(|p| resolve_relative(&self.manifest_dir, &p));
            if self.local_tool_path(tool_name).is_none() {
                validate_source(&node)?;
            }
            graph.try_add_node(node)?;
        }

        for (dep_name, dep_spec) in &self.manifest.dependencies {
            let details = dep_spec.to_details();
            let mut node = DependencyNode::from_target_dep(dep_name, dep_spec);
            node.path = node.path.map(|p| resolve_relative(&self.manifest_dir, &p));
            node.patches = node.patches.iter().map(|p| resolve_relative(&self.manifest_dir, p)).collect();

            if let Some(ref recipe_path) = details.recipe {
                let recipe_full_path = resolve_relative(&self.manifest_dir, recipe_path);
                let recipe = load_recipe(&recipe_full_path, node.version.as_deref())
                    .with_context(|| format!("Dependency '{dep_name}'"))?;
                apply_recipe(&mut node, recipe, details.strategy.is_none(), &recipe_full_path);
            }

            if let Some(ov) = self.manifest.overrides.get(dep_name) {
                node.apply_override(ov);
                self.resolve_override_paths(&mut node, ov);
            }
            if let Some(ov) = self.local.as_ref().and_then(|lc| lc.overrides.get(dep_name)) {
                node.apply_override(ov);
                self.resolve_override_paths(&mut node, ov);
            }

            validate_source(&node)?;
            graph.try_add_node(node)?;
        }

        for name in self.manifest.overrides.keys() {
            if !self.manifest.dependencies.contains_key(name) {
                warn(format!("[overrides.{name}] does not match any dependency"));
            }
        }

        Ok(graph)
    }

    fn resolve_override_paths(&self, node: &mut DependencyNode, ov: &unideps_core::manifest::DependencyDetails) {
        if ov.path.is_some() {
            node.path = node.path.as_ref().map(|p| resolve_relative(&self.manifest_dir, p));
        }
        if !ov.patches.is_empty() {
            node.patches = node.patches.iter().map(|p| resolve_relative(&self.manifest_dir, p)).collect();
        }
    }
}

fn dunce_abs(p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().map(|cwd| cwd.join(p)).unwrap_or_else(|_| p.to_path_buf())
    }
}

pub fn resolve_base_dir(cli: Option<&Path>, local: Option<&LocalConfig>, manifest_dir: &Path) -> PathBuf {
    if let Some(p) = cli {
        return dunce_abs(p);
    }
    for var in ["UNIDEPS_DIR", "UNIDEPS_BASE_DIR"] {
        if let Ok(v) = std::env::var(var)
            && !v.trim().is_empty()
        {
            return PathBuf::from(v);
        }
    }
    if let Some(base) = local.and_then(|l| l.storage.as_ref()).and_then(|s| s.base_dir.as_ref()) {
        return resolve_relative(manifest_dir, base);
    }
    if let Ok(home) = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")) {
        return PathBuf::from(home).join(".unideps");
    }
    manifest_dir.join(".unideps")
}

fn validate_source(node: &DependencyNode) -> Result<()> {
    if node.git.is_none() && node.path.is_none() {
        anyhow::bail!(
            "{} '{}' has no source: set `git` or `path` (or a `recipe` providing one). \
             Version-only specs like `{} = \"1.0\"` need registries, which are not supported yet",
            capitalize(node.kind.describe()),
            node.name,
            node.name
        );
    }
    if !SUPPORTED_STRATEGIES.contains(&node.strategy.as_str()) {
        anyhow::bail!(
            "'{}' uses unknown strategy '{}' (supported: {})",
            node.name,
            node.strategy,
            SUPPORTED_STRATEGIES.join(", ")
        );
    }
    Ok(())
}

/// Checks that local source directories and patch files exist. Done per node at
/// build time so that packages filtered out for this target may reference paths
/// that only exist on other machines.
pub fn check_files(node: &DependencyNode) -> Result<()> {
    if let Some(ref p) = node.path
        && !p.is_dir()
    {
        anyhow::bail!("Source path of '{}' does not exist: {}", node.name, p.display());
    }
    for patch in &node.patches {
        if !patch.is_file() {
            anyhow::bail!("Patch file of '{}' not found: {}", node.name, patch.display());
        }
    }
    Ok(())
}

fn capitalize(s: &str) -> String {
    let s = s.strip_prefix("a ").unwrap_or(s);
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

fn load_recipe(path: &Path, version: Option<&str>) -> Result<Recipe> {
    if !path.exists() {
        anyhow::bail!("Recipe file not found: {}", path.display());
    }
    if path.extension().is_some_and(|ext| ext == "lua") {
        LuaRecipeEngine::new()?.load_recipe_from_file(path, version)
    } else {
        Recipe::from_toml_file(path)
    }
}

/// Manifest values always win over the recipe.
fn apply_recipe(node: &mut DependencyNode, rec: Recipe, strategy_unset: bool, recipe_path: &Path) {
    let recipe_dir = recipe_path.parent().unwrap_or(Path::new("."));
    if node.version.is_none() {
        node.version = rec.version.clone();
    }
    if strategy_unset && let Some(s) = rec.strategy.clone() {
        node.strategy = s;
    }
    for ig in &rec.abi_ignores {
        if !node.abi_ignores.contains(ig) {
            node.abi_ignores.push(ig.clone());
        }
    }
    for (k, v) in rec.cmake_options() {
        node.cmake_options.entry(k).or_insert(v);
    }
    if node.import_name.is_none() {
        node.import_name = rec.import_name;
    }
    if node.include_dirs.is_empty() {
        node.include_dirs = rec.include_dirs;
    }
    if node.libraries.is_empty() {
        node.libraries = rec.libraries;
    }
    if node.binaries.is_empty() {
        node.binaries = rec.binaries;
    }
    if node.dependencies.is_empty() {
        node.dependencies = rec.dependencies;
    }
    if node.tools.is_empty() {
        node.tools = rec.tools;
    }
    if node.patches.is_empty() {
        node.patches = rec.patches.iter().map(|p| resolve_relative(recipe_dir, p)).collect();
    }
    if node.git.is_none() && node.path.is_none() {
        if let Some(git) = rec.source.git {
            node.git = Some(git);
            node.tag = node.tag.take().or(rec.source.tag);
            node.branch = node.branch.take().or(rec.source.branch);
            node.commit = node.commit.take().or(rec.source.commit);
        } else if let Some(p) = rec.source.path {
            node.path = Some(resolve_relative(recipe_dir, &p));
        }
    }
}

/// References to undeclared packages are errors (typos); references to packages
/// filtered out for this target or configuration are dropped.
pub fn connect_graph(
    graph: &mut DependencyGraph,
    target: &TargetTriple,
    options: &BTreeMap<String, String>,
) -> Result<()> {
    let declared: BTreeSet<String> = graph.indices.keys().cloned().collect();
    graph.filter_conditions(options);
    graph.filter_platforms(target);

    let edges: Vec<_> = graph
        .graph
        .node_indices()
        .map(|idx| {
            let n = &graph.graph[idx];
            (idx, n.name.clone(), n.kind.clone(), n.dependencies.clone(), n.tools.clone())
        })
        .collect();

    for (idx, name, kind, deps, tools) in edges {
        for (dep, is_tool) in deps.iter().map(|d| (d, false)).chain(tools.iter().map(|t| (t, true))) {
            if dep == &name {
                anyhow::bail!("'{name}' depends on itself");
            }
            match graph.indices.get(dep) {
                Some(&dep_idx) => {
                    let dep_kind = &graph.graph[dep_idx].kind;
                    if is_tool && *dep_kind != NodeKind::HostTool {
                        anyhow::bail!("'{name}' lists '{dep}' in `tools`, but '{dep}' is not declared under [tools]");
                    }
                    if kind == NodeKind::HostTool && *dep_kind == NodeKind::TargetDependency {
                        anyhow::bail!("Tool '{name}' cannot depend on target dependency '{dep}'");
                    }
                    graph.add_dependency(idx, dep_idx);
                }
                None if declared.contains(dep) => {}
                None => anyhow::bail!(
                    "'{name}' depends on '{dep}', which is not declared in [dependencies] or [tools]"
                ),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project_with(toml: &str) -> (tempfile::TempDir, Result<Project>) {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("unideps.toml");
        std::fs::write(&path, toml).unwrap();
        let p = Project::load(&path);
        (temp, p)
    }

    #[test]
    fn version_only_dependency_is_rejected() {
        let (_t, p) = project_with("[dependencies]\nfmt = \"10.2.1\"\n");
        let err = p.unwrap().build_graph().unwrap_err().to_string();
        assert!(err.contains("has no source"), "{err}");
    }

    #[test]
    fn unknown_strategy_is_rejected() {
        let (_t, p) = project_with("[dependencies.a]\ngit = \"u\"\nstrategy = \"meson\"\n");
        let err = p.unwrap().build_graph().unwrap_err().to_string();
        assert!(err.contains("unknown strategy"), "{err}");
    }

    #[test]
    fn build_dirs_are_removed_unless_local_toml_opts_out() {
        let (_t, p) = project_with("");
        assert!(!p.unwrap().keep_build_dirs());

        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("unideps.toml"), "").unwrap();
        std::fs::write(temp.path().join(".local.toml"), "[storage]
keep_build_dirs = true
").unwrap();
        let p = Project::load(&temp.path().join("unideps.toml")).unwrap();
        assert!(p.keep_build_dirs());
    }

    #[test]
    fn broken_local_toml_is_an_error() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("unideps.toml"), "").unwrap();
        std::fs::write(temp.path().join(".local.toml"), "[storage\n").unwrap();
        assert!(Project::load(&temp.path().join("unideps.toml")).is_err());
    }

    #[test]
    fn nested_manifest_ignores_local_toml() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("unideps.toml"), "").unwrap();
        std::fs::write(temp.path().join(".local.toml"), "[storage
").unwrap();
        let p = Project::load_nested(&temp.path().join("unideps.toml")).unwrap();
        assert!(p.local.is_none());
    }

    #[test]
    fn dependency_and_tool_name_collision_is_rejected() {
        let (_t, p) = project_with("[tools.nasm]\ngit = \"u\"\n[dependencies.nasm]\ngit = \"u\"\n");
        assert!(p.unwrap().build_graph().is_err());
    }

    #[test]
    fn unknown_dependency_reference_is_rejected_but_filtered_one_is_not() {
        let (_t, p) = project_with(
            "[dependencies.a]\ngit = \"u\"\ndependencies = [\"b\"]\n\
             [dependencies.b]\ngit = \"u\"\nplatforms = [\"android\"]\n\
             [dependencies.c]\ngit = \"u\"\ndependencies = [\"typo\"]\n",
        );
        let p = p.unwrap();
        let linux: TargetTriple = "x86_64-unknown-linux-gnu".parse().unwrap();

        let mut g = p.build_graph().unwrap();
        let err = connect_graph(&mut g, &linux, &BTreeMap::new()).unwrap_err().to_string();
        assert!(err.contains("'typo'"), "{err}");
    }

    #[test]
    fn filtered_dependency_reference_is_dropped() {
        let (_t, p) = project_with(
            "[dependencies.a]\ngit = \"u\"\ndependencies = [\"b\"]\n\
             [dependencies.b]\ngit = \"u\"\nplatforms = [\"android\"]\n",
        );
        let p = p.unwrap();
        let linux: TargetTriple = "x86_64-unknown-linux-gnu".parse().unwrap();
        let mut g = p.build_graph().unwrap();
        connect_graph(&mut g, &linux, &BTreeMap::new()).unwrap();
        assert!(g.indices.contains_key("a"));
        assert!(!g.indices.contains_key("b"));
    }

    #[test]
    fn scoped_strategy_without_package_is_rejected() {
        let (_t, p) = project_with("[[strategy]]\nscope = \"cascade\"\nshared = true\n");
        assert!(p.is_err());
    }

    #[test]
    fn relative_paths_resolve_against_manifest_dir() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("vendor/lib")).unwrap();
        std::fs::write(temp.path().join("fix.patch"), "").unwrap();
        std::fs::write(
            temp.path().join("unideps.toml"),
            "[dependencies.lib]\npath = \"vendor/lib\"\npatches = [\"fix.patch\"]\n",
        )
        .unwrap();
        let p = Project::load(&temp.path().join("unideps.toml")).unwrap();
        let g = p.build_graph().unwrap();
        let node = &g.graph[g.indices["lib"]];
        assert!(node.path.as_ref().unwrap().is_absolute());
        assert!(node.patches[0].is_absolute());
    }

    #[test]
    fn toml_recipe_fills_source_and_options() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("recipes")).unwrap();
        std::fs::write(temp.path().join("recipes/p.patch"), "").unwrap();
        std::fs::write(
            temp.path().join("recipes/ogg.toml"),
            "name = \"ogg\"\npatches = [\"p.patch\"]\n[source]\ngit = \"https://x/ogg.git\"\ntag = \"v1\"\n[options]\nBUILD_TESTING = false\n",
        )
        .unwrap();
        std::fs::write(temp.path().join("unideps.toml"), "[dependencies.ogg]\nrecipe = \"recipes/ogg.toml\"\n").unwrap();
        let p = Project::load(&temp.path().join("unideps.toml")).unwrap();
        let g = p.build_graph().unwrap();
        let node = &g.graph[g.indices["ogg"]];
        assert_eq!(node.git.as_deref(), Some("https://x/ogg.git"));
        assert_eq!(node.tag.as_deref(), Some("v1"));
        assert_eq!(node.cmake_options["BUILD_TESTING"], "OFF");
        assert!(node.patches[0].ends_with("recipes/p.patch"));
    }

    #[test]
    fn broken_recipe_is_an_error() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("r.lua"), "package = {").unwrap();
        std::fs::write(temp.path().join("unideps.toml"), "[dependencies.x]\nrecipe = \"r.lua\"\n").unwrap();
        let p = Project::load(&temp.path().join("unideps.toml")).unwrap();
        assert!(p.build_graph().is_err());
    }
}
