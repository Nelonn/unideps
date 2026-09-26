use crate::error::{CoreError, CoreResult};
use crate::manifest::{DependencySpec, ToolRequirement};
use crate::strategy::EffectiveConfig;
use crate::target::TargetTriple;
use petgraph::algo::toposort;
use serde::{Deserialize, Serialize};
use petgraph::graph::DiGraph;
pub use petgraph::Direction;
pub use petgraph::graph::NodeIndex;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeKind {
    TargetDependency,
    HostTool,
}

impl NodeKind {
    pub fn describe(&self) -> &'static str {
        match self {
            NodeKind::TargetDependency => "a dependency",
            NodeKind::HostTool => "a tool",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyNode {
    pub name: String,
    pub kind: NodeKind,
    pub version: Option<String>,
    pub git: Option<String>,
    pub tag: Option<String>,
    pub branch: Option<String>,
    pub commit: Option<String>,
    pub path: Option<PathBuf>,
    pub shallow: bool,
    pub strategy: String,
    pub shared: Option<bool>,
    pub header_only: bool,
    pub cmake_options: BTreeMap<String, String>,
    pub abi_ignores: Vec<String>,
    pub platforms: Vec<String>,
    pub dependencies: Vec<String>,
    pub tools: Vec<String>,
    pub import_name: Option<String>,
    pub include_dirs: Vec<PathBuf>,
    pub libraries: Vec<PathBuf>,
    pub binaries: Vec<PathBuf>,
    pub enabled_if: Option<String>,
    pub auto_import: bool,
    pub config: EffectiveConfig,
    pub source_id: Option<String>,
    pub build_id: Option<String>,
    pub install_prefix: Option<PathBuf>,
    pub compiler: Option<crate::compiler::CompilerType>,
    pub patches: Vec<PathBuf>,
    /// Commit a branch / default HEAD resolved to; branch names alone would never invalidate the cache.
    pub resolved_commit: Option<String>,
    pub toolchain_fingerprint: Option<String>,
}

impl DependencyNode {
    pub fn from_target_dep(name: &str, spec: &DependencySpec) -> Self {
        let d = spec.to_details();
        Self {
            name: name.to_string(),
            kind: NodeKind::TargetDependency,
            version: d.version,
            git: d.git,
            tag: d.tag,
            branch: d.branch,
            commit: d.commit,
            path: d.path,
            shallow: d.shallow.unwrap_or(true),
            strategy: d.strategy.unwrap_or_else(|| "cmake-install".into()),
            shared: d.shared,
            header_only: d.header_only.unwrap_or(false),
            cmake_options: d.cmake_options,
            abi_ignores: d.abi_ignores,
            platforms: d.platforms,
            dependencies: d.dependencies.iter().map(|s| s.name().to_string()).collect(),
            tools: d.tools,
            import_name: d.import_name,
            include_dirs: d.include_dirs,
            libraries: d.libraries,
            binaries: d.binaries,
            enabled_if: d.enabled_if,
            auto_import: d.auto_import.unwrap_or(false),
            config: EffectiveConfig::default(),
            source_id: None,
            build_id: None,
            install_prefix: None,
            compiler: None,
            patches: d.patches,
            resolved_commit: None,
            toolchain_fingerprint: None,
        }
    }

    pub fn apply_override(&mut self, details: &crate::manifest::DependencyDetails) {
        if let Some(ref v) = details.version {
            self.version = Some(v.clone());
        }
        // A source override replaces the whole source: switching to `path` drops `git`
        // and vice versa, and any ref (commit/tag/branch) replaces all previous refs.
        if details.git.is_some() || details.path.is_some() {
            self.git = details.git.clone();
            self.path = details.path.clone();
        }
        if details.commit.is_some() || details.tag.is_some() || details.branch.is_some() {
            self.commit = details.commit.clone();
            self.tag = details.tag.clone();
            self.branch = details.branch.clone();
        }
        if let Some(s) = details.shallow {
            self.shallow = s;
        }
        if let Some(ref s) = details.strategy {
            self.strategy = s.clone();
        }
        if let Some(sh) = details.shared {
            self.shared = Some(sh);
        }
        if let Some(h) = details.header_only {
            self.header_only = h;
        }
        for (k, v) in &details.cmake_options {
            self.cmake_options.insert(k.clone(), v.clone());
        }
        if !details.abi_ignores.is_empty() {
            self.abi_ignores.extend(details.abi_ignores.clone());
        }
        if !details.platforms.is_empty() {
            self.platforms = details.platforms.clone();
        }
        if !details.dependencies.is_empty() {
            self.dependencies = details.dependencies.iter().map(|s| s.name().to_string()).collect();
        }
        if !details.tools.is_empty() {
            self.tools = details.tools.clone();
        }
        if let Some(ref imp) = details.import_name {
            self.import_name = Some(imp.clone());
        }
        if !details.include_dirs.is_empty() {
            self.include_dirs = details.include_dirs.clone();
        }
        if !details.libraries.is_empty() {
            self.libraries = details.libraries.clone();
        }
        if !details.binaries.is_empty() {
            self.binaries = details.binaries.clone();
        }
        if let Some(ref e) = details.enabled_if {
            self.enabled_if = Some(e.clone());
        }
        if let Some(ai) = details.auto_import {
            self.auto_import = ai;
        }
        if !details.patches.is_empty() {
            self.patches = details.patches.clone();
        }
    }

    pub fn from_host_tool(name: &str, req: &ToolRequirement) -> Self {
        let d = req.to_details();
        Self {
            name: name.to_string(),
            kind: NodeKind::HostTool,
            version: d.version,
            git: d.git,
            tag: d.tag,
            branch: d.branch,
            commit: d.commit,
            path: d.path,
            shallow: true,
            strategy: d.strategy.unwrap_or_else(|| "cmake-install".into()),
            shared: Some(false),
            header_only: false,
            cmake_options: BTreeMap::new(),
            abi_ignores: vec!["vc_runtime".into()],
            platforms: Vec::new(),
            dependencies: Vec::new(),
            tools: Vec::new(),
            import_name: None,
            include_dirs: Vec::new(),
            libraries: Vec::new(),
            binaries: Vec::new(),
            enabled_if: None,
            auto_import: false,
            config: EffectiveConfig::default(),
            source_id: None,
            build_id: None,
            install_prefix: None,
            compiler: None,
            patches: Vec::new(),
            resolved_commit: None,
            toolchain_fingerprint: None,
        }
    }
}

#[derive(Debug)]
pub struct DependencyGraph {
    pub graph: DiGraph<DependencyNode, ()>,
    pub indices: HashMap<String, NodeIndex>,
}

impl Default for DependencyGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl DependencyGraph {
    pub fn new() -> Self {
        Self {
            graph: DiGraph::new(),
            indices: HashMap::new(),
        }
    }

    pub fn add_or_get_node(&mut self, node: DependencyNode) -> NodeIndex {
        if let Some(&idx) = self.indices.get(&node.name) {
            return idx;
        }
        let name = node.name.clone();
        let idx = self.graph.add_node(node);
        self.indices.insert(name, idx);
        idx
    }

    /// Dependencies and tools share one namespace, so a name may be declared only once.
    pub fn try_add_node(&mut self, node: DependencyNode) -> CoreResult<NodeIndex> {
        if let Some(&idx) = self.indices.get(&node.name) {
            let existing = &self.graph[idx];
            return Err(CoreError::Manifest(format!(
                "'{}' is declared twice (as {} and as {}); dependency and tool names must be unique",
                node.name,
                existing.kind.describe(),
                node.kind.describe()
            )));
        }
        Ok(self.add_or_get_node(node))
    }

    /// All nodes reachable from `idx` following edges in `dir`
    /// (`Outgoing` = transitive dependencies, `Incoming` = transitive dependents).
    pub fn reachable(&self, idx: NodeIndex, dir: Direction) -> BTreeSet<NodeIndex> {
        let mut seen = BTreeSet::new();
        let mut queue: VecDeque<NodeIndex> = self.graph.neighbors_directed(idx, dir).collect();
        while let Some(current) = queue.pop_front() {
            if seen.insert(current) {
                queue.extend(self.graph.neighbors_directed(current, dir));
            }
        }
        seen
    }

    pub fn add_dependency(&mut self, dependent: NodeIndex, dependency: NodeIndex) {
        self.graph.add_edge(dependent, dependency, ());
    }

    pub fn topological_order(&self) -> CoreResult<Vec<NodeIndex>> {
        match toposort(&self.graph, None) {
            Ok(order) => {
                let mut rev = order;
                rev.reverse();
                Ok(rev)
            }
            Err(cycle) => Err(CoreError::CycleDetected(format!(
                "Cycle involves node {:?}",
                self.graph.node_weight(cycle.node_id()).map(|n| &n.name)
            ))),
        }
    }

    pub fn filter_platforms(&mut self, target: &TargetTriple) {
        while let Some(idx) = self.graph.node_indices().find(|&idx| {
            let node = &self.graph[idx];
            if node.kind == NodeKind::HostTool {
                false
            } else {
                !target.matches_all_filters(&node.platforms)
            }
        }) {
            let name = self.graph[idx].name.clone();
            self.indices.remove(&name);
            self.graph.remove_node(idx);
            self.rebuild_indices();
        }
    }

    pub fn filter_conditions(&mut self, options: &BTreeMap<String, String>) {
        while let Some(idx) = self.graph.node_indices().find(|&idx| {
            let node = &self.graph[idx];
            if let Some(ref cond) = node.enabled_if {
                !Self::eval_condition(cond, options)
            } else {
                false
            }
        }) {
            let name = self.graph[idx].name.clone();
            self.indices.remove(&name);
            self.graph.remove_node(idx);
            self.rebuild_indices();
        }
    }

    fn rebuild_indices(&mut self) {
        self.indices.clear();
        for idx in self.graph.node_indices() {
            self.indices.insert(self.graph[idx].name.clone(), idx);
        }
    }

    pub fn eval_condition(cond: &str, options: &BTreeMap<String, String>) -> bool {
        let trimmed = cond.trim();
        if trimmed.is_empty() {
            return true;
        }

        if trimmed.contains("||") {
            return trimmed
                .split("||")
                .any(|part| Self::eval_condition(part, options));
        }

        if trimmed.contains("&&") {
            return trimmed
                .split("&&")
                .all(|part| Self::eval_condition(part, options));
        }

        if let Some(stripped) = trimmed.strip_prefix('!') {
            return !Self::eval_condition(stripped, options);
        }

        if let Some((k, v)) = trimmed.split_once('=') {
            let key = k.trim();
            let expected = v.trim().to_lowercase();
            if let Some(actual) = options.get(key) {
                actual.to_lowercase() == expected
            } else {
                expected == "off" || expected == "false" || expected == "0"
            }
        } else if let Some(val) = options.get(trimmed) {
            let lower = val.to_lowercase();
            lower == "on" || lower == "true" || lower == "1" || lower == "yes" || lower == "y"
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_topological_order() {
        let mut graph = DependencyGraph::new();
        let a = DependencyNode::from_target_dep("app", &crate::manifest::DependencySpec::Version("1".into()));
        let b = DependencyNode::from_target_dep("lib", &crate::manifest::DependencySpec::Version("1".into()));
        let a_idx = graph.add_or_get_node(a);
        let b_idx = graph.add_or_get_node(b);
        graph.add_dependency(a_idx, b_idx);
        let order = graph.topological_order().unwrap();
        assert_eq!(order, vec![b_idx, a_idx]);
    }

    #[test]
    fn test_override_replaces_source_and_ref() {
        use crate::manifest::{DependencyDetails, DependencySpec};
        let spec = DependencySpec::Detailed(DependencyDetails {
            git: Some("https://up/x.git".into()),
            commit: Some("abcdef1".into()),
            ..Default::default()
        });
        let mut node = DependencyNode::from_target_dep("x", &spec);
        node.apply_override(&DependencyDetails { tag: Some("v2".into()), ..Default::default() });
        assert_eq!(node.commit, None);
        assert_eq!(node.tag.as_deref(), Some("v2"));
        assert_eq!(node.git.as_deref(), Some("https://up/x.git"));

        node.apply_override(&DependencyDetails { path: Some("../x".into()), ..Default::default() });
        assert_eq!(node.git, None);
        assert!(node.path.is_some());
    }

    #[test]
    fn test_duplicate_names_rejected() {
        let mut graph = DependencyGraph::new();
        let dep = DependencyNode::from_target_dep("nasm", &crate::manifest::DependencySpec::Version("1".into()));
        let tool = DependencyNode::from_host_tool("nasm", &ToolRequirement::Version("2".into()));
        graph.try_add_node(dep).unwrap();
        assert!(graph.try_add_node(tool).is_err());
    }

    #[test]
    fn test_reachable() {
        let mut graph = DependencyGraph::new();
        let mk = |n: &str| DependencyNode::from_target_dep(n, &crate::manifest::DependencySpec::Version("1".into()));
        let a = graph.add_or_get_node(mk("a"));
        let b = graph.add_or_get_node(mk("b"));
        let c = graph.add_or_get_node(mk("c"));
        graph.add_dependency(a, b);
        graph.add_dependency(b, c);
        assert_eq!(graph.reachable(a, Direction::Outgoing), [b, c].into_iter().collect());
        assert_eq!(graph.reachable(c, Direction::Incoming), [a, b].into_iter().collect());
    }

    #[test]
    fn test_eval_condition() {
        let opts: BTreeMap<String, String> = [("A", "ON"), ("B", "OFF"), ("MODE", "Fast")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let ev = |c: &str| DependencyGraph::eval_condition(c, &opts);
        assert!(ev("A"));
        assert!(!ev("B"));
        assert!(!ev("MISSING"));
        assert!(ev("!B"));
        assert!(ev("A && !B"));
        assert!(ev("B || A"));
        assert!(ev("MODE=fast"));
        assert!(ev("MISSING=OFF"));
        assert!(!ev("A && B || MISSING"));
    }
}
