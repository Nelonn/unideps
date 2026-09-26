use crate::project::{Project, check_files, connect_graph};
use crate::source::Sources;
use crate::util::{expand_dep_refs, expand_vars, parse_bool, parse_define, resolve_msvc_runtime, toolchain_fingerprint, warn};
use anyhow::{Context, Result};
use clap::Args;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use unideps_builder::git::GitSource;
use unideps_builder::lock::FileLock;
use unideps_builder::resolution::{Resolution, is_recordable};
use unideps_builder::runners::cmake::{CMakeBuild, CMakeRunner};
use unideps_builder::storage::StorageManager;
use unideps_cache::packager::Packager;
use unideps_cmake::generator::CMakeGenerator;
use unideps_core::compiler::CompilerType;
use unideps_core::graph::{DependencyGraph, DependencyNode, Direction, NodeIndex, NodeKind};
use unideps_core::hash::HashCalculator;
use unideps_core::manifest::Preset;
use unideps_core::strategy::{EffectiveConfig, StrategyEngine};
use unideps_core::target::{Arch, BuildType, Environment, Os, TargetTriple, VCRuntime};

#[derive(Args, Debug, Default)]
pub struct BuildArgs {
    /// Path to unideps.toml
    #[arg(short, long, default_value = "unideps.toml")]
    pub manifest: PathBuf,

    /// Target triple (default: host)
    #[arg(short, long)]
    pub target: Option<String>,

    /// Preset from [presets]
    #[arg(short, long)]
    pub preset: Option<String>,

    /// Debug, Release, RelWithDebInfo or MinSizeRel
    #[arg(long)]
    pub build_type: Option<String>,

    /// Default linkage for dependencies that do not set `shared` (ON/OFF, true/false)
    #[arg(long, value_parser = parse_bool)]
    pub default_shared: Option<bool>,

    /// MSVC runtime (MultiThreaded[Debug][DLL]; `$<$<CONFIG:..>:..>` expressions are expanded)
    #[arg(long, allow_hyphen_values = true)]
    pub msvc_runtime: Option<String>,

    #[arg(long)]
    pub c_compiler: Option<PathBuf>,

    #[arg(long)]
    pub cxx_compiler: Option<PathBuf>,

    /// Compiler family: clang, msvc, gcc or a custom name
    #[arg(long)]
    pub compiler: Option<String>,

    #[arg(long)]
    pub toolchain_file: Option<PathBuf>,

    /// `-DKEY=VALUE` options: evaluated by `enabled_if`; ANDROID_* ones are forwarded to builds
    #[arg(long, allow_hyphen_values = true)]
    pub cmake_args: Vec<String>,

    /// Where to write the generated CMake targets file
    #[arg(long)]
    pub generate_targets: Option<PathBuf>,

    /// Storage directory (default: $UNIDEPS_DIR, .local.toml storage.base_dir, ~/.unideps)
    #[arg(long)]
    pub base_dir: Option<PathBuf>,
}

fn detect_default_compiler(target: &TargetTriple) -> CompilerType {
    match (target.os, target.env) {
        (Os::Android | Os::Ios | Os::Macos | Os::Emscripten, _) => CompilerType::Clang,
        (Os::Windows, Environment::Gnu) => CompilerType::Gcc,
        (Os::Windows, _) => CompilerType::Msvc,
        _ => CompilerType::Gcc,
    }
}

fn android_abi(arch: Arch) -> &'static str {
    match arch {
        Arch::Arm64 => "arm64-v8a",
        Arch::Arm => "armeabi-v7a",
        Arch::X86_64 => "x86_64",
        Arch::X86 => "x86",
        _ => "arm64-v8a",
    }
}

struct BuildContext<'a> {
    project: &'a Project,
    target: TargetTriple,
    host: TargetTriple,
    strategy: StrategyEngine,
    storage: &'a StorageManager,
    /// Shared for the whole run so that a checkout is brought up to date only once.
    sources: &'a Sources<'a>,
    c_compiler: Option<PathBuf>,
    cxx_compiler: Option<PathBuf>,
    toolchain_file: Option<PathBuf>,
    target_compiler: CompilerType,
    host_compiler: CompilerType,
    target_base: EffectiveConfig,
    default_shared: Option<bool>,
    forwarded: BTreeMap<String, String>,
    /// Variables of the consuming CMake project (`--cmake-args`), for `${NAME}` in `cmake_options`.
    vars: BTreeMap<String, String>,
}

/// Names of the packages whose nested `unideps.toml` is being built, outermost first,
/// with their `source_id`s. Seeing a `source_id` twice means the manifests depend on
/// each other.
type NestedStack = Vec<(String, String)>;

/// A built package together with everything its nested `unideps.toml` pulled in.
struct Built {
    node: DependencyNode,
    nested: Vec<DependencyNode>,
}

impl<'a> BuildContext<'a> {
    /// Same target and toolchain, but the strategy rules of another manifest.
    fn with_strategy(&self, strategy: StrategyEngine) -> BuildContext<'a> {
        BuildContext {
            project: self.project,
            target: self.target.clone(),
            host: self.host.clone(),
            strategy,
            storage: self.storage,
            sources: self.sources,
            c_compiler: self.c_compiler.clone(),
            cxx_compiler: self.cxx_compiler.clone(),
            toolchain_file: self.toolchain_file.clone(),
            target_compiler: self.target_compiler.clone(),
            host_compiler: self.host_compiler.clone(),
            target_base: self.target_base.clone(),
            default_shared: self.default_shared,
            forwarded: self.forwarded.clone(),
            vars: self.vars.clone(),
        }
    }

    fn is_native(&self) -> bool {
        self.target.raw == self.host.raw && self.toolchain_file.is_none()
    }

    fn triple_for(&self, node: &DependencyNode) -> &TargetTriple {
        match node.kind {
            NodeKind::TargetDependency => &self.target,
            NodeKind::HostTool => &self.host,
        }
    }

    /// Host tools are built for the host with the host's default toolchain. They only
    /// reuse the project's compilers when not cross-compiling.
    fn compilers_for(&self, node: &DependencyNode) -> (Option<&Path>, Option<&Path>, Option<&Path>) {
        match node.kind {
            NodeKind::TargetDependency => (
                self.c_compiler.as_deref(),
                self.cxx_compiler.as_deref(),
                self.toolchain_file.as_deref(),
            ),
            NodeKind::HostTool if self.is_native() => (self.c_compiler.as_deref(), self.cxx_compiler.as_deref(), None),
            NodeKind::HostTool => (None, None, None),
        }
    }

    fn configure_node(&self, node: &mut DependencyNode, dependents: &[&str]) -> Result<()> {
        let base = match node.kind {
            NodeKind::TargetDependency => {
                if node.shared.is_none() {
                    node.shared = self.default_shared;
                }
                // Stored in node options so they are hashed: ANDROID_STL changes the ABI.
                for (k, v) in &self.forwarded {
                    node.cmake_options.entry(k.clone()).or_insert_with(|| v.clone());
                }
                if self.target.os == Os::Android && !node.cmake_options.contains_key("ANDROID_ABI") {
                    node.cmake_options
                        .insert("ANDROID_ABI".into(), android_abi(self.target.arch).into());
                }
                node.compiler = Some(self.target_compiler.clone());
                self.target_base.clone()
            }
            NodeKind::HostTool => {
                node.compiler = Some(if self.is_native() {
                    self.target_compiler.clone()
                } else {
                    self.host_compiler.clone()
                });
                EffectiveConfig::default()
            }
        };
        node.config = self.strategy.resolve_for_node(&node.name, dependents, &base);
        let (cc, cxx, tf) = self.compilers_for(node);
        node.toolchain_fingerprint = toolchain_fingerprint(cc, cxx, tf);

        // After the rules are applied and before anything is hashed: the expanded values
        // are what the package is built with.
        // All problems of a package are reported together, not one per run.
        let mut errors: Vec<String> = Vec::new();
        for options in [&mut node.cmake_options, &mut node.config.cmake_options] {
            for (key, value) in options.iter_mut() {
                match expand_vars(value, &self.vars) {
                    Ok(expanded) => *value = expanded,
                    Err(e) => errors.push(format!("In cmake_options.{key} of '{}': {e:#}", node.name)),
                }
            }
        }
        if !errors.is_empty() {
            anyhow::bail!("{}", errors.join("\n"));
        }
        Ok(())
    }
}

pub fn run(args: BuildArgs) -> Result<()> {
    let host = TargetTriple::host();
    let target: TargetTriple = match args.target {
        Some(ref t) => t.parse()?,
        None => host.clone(),
    };
    println!("Target triple: {target}");
    println!("Host triple: {host}");

    let project = Project::load(&args.manifest)?;
    let m = &project.manifest;

    let preset: Option<&Preset> = match args.preset.as_deref() {
        Some(name) => Some(m.presets.get(name).with_context(|| {
            let known: Vec<_> = m.presets.keys().map(String::as_str).collect();
            format!("Unknown preset '{name}' (available: {})", known.join(", "))
        })?),
        None => None,
    };

    let mut options: BTreeMap<String, String> = BTreeMap::new();
    for arg in &args.cmake_args {
        match parse_define(arg) {
            Some((k, v)) => {
                options.insert(k, v);
            }
            None => warn(format!("ignoring --cmake-args value '{arg}' (expected -DKEY=VALUE)")),
        }
    }

    let build_type = if let Some(ref bt) = args.build_type {
        bt.parse::<BuildType>()?
    } else if let Some(bt) = options.get("CMAKE_BUILD_TYPE") {
        bt.parse::<BuildType>()?
    } else if let Some(bt) = preset.and_then(|p| p.build_type) {
        bt
    } else {
        BuildType::Release
    };

    let mut target_base = EffectiveConfig {
        build_type,
        ..Default::default()
    };
    if let Some(p) = preset {
        if let Some(ref std) = p.cxx_std {
            target_base.cxx_std = std.clone();
        }
        if let Some(sl) = p.cxx_stdlib {
            target_base.cxx_stdlib = sl;
        }
        if let Some(ref lto) = p.lto {
            target_base.lto = lto.enabled()?;
        }
        target_base.add_flags(&p.flags);
        if p.cxx_stdlib_package.is_some() {
            warn("presets: cxx_stdlib_package is not supported yet and is ignored");
        }
    }
    target_base.vc_runtime = if let Some(ref raw) = args.msvc_runtime {
        resolve_msvc_runtime(raw, build_type).context("Invalid --msvc-runtime")?
    } else if let Some(rt) = preset.and_then(|p| p.vc_runtime) {
        rt
    } else if build_type == BuildType::Debug {
        VCRuntime::MultiThreadedDebugDLL
    } else {
        VCRuntime::MultiThreadedDLL
    };

    let target_cfg = project.target_config(&target);
    let c_compiler = args.c_compiler.clone().or(target_cfg.c_compiler);
    let cxx_compiler = args.cxx_compiler.clone().or(target_cfg.cxx_compiler);
    let toolchain_file = args
        .toolchain_file
        .clone()
        .or_else(|| options.get("CMAKE_TOOLCHAIN_FILE").map(PathBuf::from))
        .or(target_cfg.toolchain_file);
    if let Some(ref tf) = toolchain_file
        && !tf.is_file()
    {
        anyhow::bail!("Toolchain file not found: {}", tf.display());
    }

    let target_compiler = if let Some(c) = args.compiler.as_deref().or(target_cfg.compiler.as_deref()) {
        c.parse::<CompilerType>().unwrap_or_else(|never| match never {})
    } else if let Some(ref cc) = c_compiler {
        CompilerType::detect_from_path(cc)
    } else if let Some(ref cxx) = cxx_compiler {
        CompilerType::detect_from_path(cxx)
    } else {
        detect_default_compiler(&target)
    };
    println!("Compiler: {target_compiler}");

    let forwarded: BTreeMap<String, String> = options
        .iter()
        .filter(|(k, _)| k.starts_with("ANDROID_") || k.starts_with("CMAKE_ANDROID_"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    let mut graph = project.build_graph()?;
    connect_graph(&mut graph, &target, &options)?;
    // Fail on cycles before touching the storage.
    graph.topological_order()?;

    let storage = StorageManager::new(project.base_dir(args.base_dir.as_deref())).with_scratch_dir(project.scratch_dir());

    let sources = Sources::new(&storage);
    let ctx = BuildContext {
        project: &project,
        host_compiler: detect_default_compiler(&host),
        target,
        host,
        strategy: StrategyEngine::new(&m.strategy),
        storage: &storage,
        sources: &sources,
        c_compiler,
        cxx_compiler,
        toolchain_file,
        target_compiler,
        target_base,
        default_shared: args.default_shared,
        forwarded,
        vars: options,
    };

    // Before the first (possibly long) build: report configuration errors of all packages.
    preflight(&ctx, &graph)?;

    storage.ensure_dirs()?;
    println!("Storage: {}", storage.base_dir().display());
    // Runs no longer wait for each other: each package is locked while it is installed
    // (see `build_node`). The shared lock only keeps `unideps clean` from pulling the
    // scratch directories out from under a build in progress.
    let _run_lock = FileLock::acquire_shared(storage.global_build_lock_path())?;

    let built_nodes = build_project(&ctx, &mut graph, &mut NestedStack::new())?;

    let refs: Vec<&DependencyNode> = built_nodes
        .iter()
        .filter(|n| n.kind == NodeKind::TargetDependency)
        .collect();
    let cmake_content = CMakeGenerator::generate_targets_cmake(&refs);
    let cmake_path = match args.generate_targets {
        Some(out_file) => {
            if let Some(parent) = out_file.parent() {
                std::fs::create_dir_all(parent)?;
            }
            out_file
        }
        None => ctx.storage.installed_dir().join("unideps.cmake"),
    };
    std::fs::write(&cmake_path, cmake_content)?;

    println!("UniDeps successfully prepared environment: {}", cmake_path.display());
    Ok(())
}

/// Configures every node without building it, so that mistakes in the manifest (an
/// undefined `${NAME}` in `cmake_options`, ...) are reported before anything is built,
/// all at once. Packages behind nested manifests are checked when they are reached.
fn preflight(ctx: &BuildContext, graph: &DependencyGraph) -> Result<()> {
    let mut errors: Vec<String> = Vec::new();
    for idx in graph.graph.node_indices() {
        let mut node = graph.graph[idx].clone();
        let dependents: Vec<&str> = graph
            .reachable(idx, Direction::Incoming)
            .into_iter()
            .map(|i| graph.graph[i].name.as_str())
            .collect();
        if let Err(e) = ctx.configure_node(&mut node, &dependents) {
            errors.push(format!("{e:#}"));
        }
    }
    if !errors.is_empty() {
        anyhow::bail!("{}", errors.join("\n"));
    }
    Ok(())
}

/// Builds every node of `graph` in dependency order. The result lists the packages
/// pulled in by nested manifests first, then the graph's own nodes.
fn build_project(ctx: &BuildContext, graph: &mut DependencyGraph, stack: &mut NestedStack) -> Result<Vec<DependencyNode>> {
    let mut nested: Vec<DependencyNode> = Vec::new();
    let mut direct: Vec<DependencyNode> = Vec::new();
    for idx in graph.topological_order()? {
        let built = build_node(ctx, graph, idx, stack)?;
        graph.graph[idx] = built.node.clone();
        nested.extend(built.nested);
        direct.push(built.node);
    }
    Ok(merge_nested(nested, direct))
}

/// A package the project (or an outer manifest) declares itself wins over the same
/// name coming from a nested manifest; between nested ones the first wins. Each
/// package appears once so that the generated targets do not clash.
fn merge_nested(nested: Vec<DependencyNode>, direct: Vec<DependencyNode>) -> Vec<DependencyNode> {
    let mut kept: HashMap<String, Option<PathBuf>> = direct
        .iter()
        .map(|n| (n.name.clone(), n.install_prefix.clone()))
        .collect();
    let show = |p: &Option<PathBuf>| p.as_deref().map_or_else(String::new, |p| p.display().to_string());
    let mut merged: Vec<DependencyNode> = Vec::new();
    for node in nested {
        match kept.get(&node.name) {
            Some(prefix) if *prefix == node.install_prefix => {}
            Some(prefix) => warn(format!(
                "'{}' is required by a nested unideps.toml as {}, but {} is already used; keeping the latter",
                node.name,
                show(&node.install_prefix),
                show(prefix),
            )),
            None => {
                kept.insert(node.name.clone(), node.install_prefix.clone());
                merged.push(node);
            }
        }
    }
    merged.extend(direct);
    merged
}

/// Dependencies declared by the `unideps.toml` inside a dependency's source tree.
struct Nested {
    nodes: Vec<DependencyNode>,
    /// Content of the targets file that the dependency's own `unideps_setup()` includes.
    targets_cmake: String,
}

/// Builds the dependencies of `<src_dir>/unideps.toml`, if there is one. This runs in the
/// current process; a package that is already installed never gets here, because its
/// `Resolution` record already says what this would produce.
fn build_nested(
    ctx: &BuildContext,
    node: &DependencyNode,
    dep_nodes: &[&DependencyNode],
    src_dir: &Path,
    source_id: &str,
    stack: &mut NestedStack,
) -> Result<Option<Nested>> {
    let manifest = src_dir.join("unideps.toml");
    if node.kind != NodeKind::TargetDependency || !manifest.is_file() {
        return Ok(None);
    }
    if stack.iter().any(|(_, id)| id == source_id) {
        let chain: Vec<&str> = stack.iter().map(|(n, _)| n.as_str()).chain([node.name.as_str()]).collect();
        anyhow::bail!("Nested unideps.toml files depend on each other: {}", chain.join(" -> "));
    }
    println!("[NESTED] {} ({})", node.name, manifest.display());

    let project = Project::load_nested(&manifest)?;
    let mut graph = project.build_graph()?;
    // `enabled_if` refers to the options of the package itself, i.e. its own `option()`
    // defaults overridden by what it is configured with, not to the options of this project.
    let mut options = BTreeMap::new();
    if graph.graph.node_weights().any(|n| n.enabled_if.is_some()) {
        options = probe_options(ctx, node, dep_nodes, src_dir)?;
    }
    options.extend(node.config.cmake_options.iter().map(|(k, v)| (k.clone(), v.clone())));
    options.extend(node.cmake_options.iter().map(|(k, v)| (k.clone(), v.clone())));
    connect_graph(&mut graph, &ctx.target, &options)?;

    let nested_ctx = ctx.with_strategy(StrategyEngine::new(&project.manifest.strategy));
    preflight(&nested_ctx, &graph).with_context(|| format!("In the nested unideps.toml of '{}'", node.name))?;
    stack.push((node.name.clone(), source_id.to_string()));
    let built = build_project(&nested_ctx, &mut graph, stack);
    stack.pop();
    let nodes = built.with_context(|| format!("In the nested unideps.toml of '{}'", node.name))?;

    let targets: Vec<&DependencyNode> = nodes.iter().filter(|n| n.kind == NodeKind::TargetDependency).collect();
    let targets_cmake = CMakeGenerator::generate_targets_cmake(&targets);
    Ok(Some(Nested { nodes, targets_cmake }))
}

/// Options a package declares in its CMake before it reaches `unideps_setup()`. They are
/// not known without running CMake, so the package is configured once in a scratch
/// directory (its `unideps_setup()` writes them out and stops the configure). The result
/// is scratch space too: it is never stored in the cache, so a probe always reflects the
/// current sources and options. What it leads to is remembered instead, by
/// `unideps_builder::resolution`, so an installed package is never probed again.
fn probe_options(
    ctx: &BuildContext,
    node: &DependencyNode,
    dep_nodes: &[&DependencyNode],
    src_dir: &Path,
) -> Result<BTreeMap<String, String>> {
    let dep_ids: Vec<&str> = dep_nodes.iter().filter_map(|d| d.build_id.as_deref()).collect();
    let key = HashCalculator::calculate_build_id(node, &ctx.target, &ctx.host, &dep_ids);
    let name = format!("{}-probe-{key}", node.name);

    println!("[PROBE] {} ({key})", node.name);
    // The pid keeps a parallel run from deleting the scratch directory of this one.
    let scratch = ctx
        .storage
        .scratch_dir()
        .join(format!("{name}-{}", std::process::id()));
    GitSource::remove_dir_all_force(&scratch)?;
    std::fs::create_dir_all(&scratch)?;
    let out = std::path::absolute(scratch.join("options.txt"))?;
    // The install prefix names the log directory, so this is where the warning below points.
    let res = run_cmake(
        ctx,
        dep_nodes,
        node,
        src_dir,
        &scratch.join("build"),
        &scratch.join(&name),
        CmakeMode::Probe(&out),
    );
    let content = match res {
        Ok(()) if out.is_file() => Some(std::fs::read_to_string(&out)?),
        Ok(()) => None,
        Err(e) => {
            if !ctx.project.keep_build_dirs() {
                let _ = GitSource::remove_dir_all_force(&scratch);
            }
            return Err(e);
        }
    };
    if !ctx.project.keep_build_dirs() {
        let _ = GitSource::remove_dir_all_force(&scratch);
    }

    let Some(content) = content else {
        let log = ctx.storage.logs_dir().join(&name).join("configure.log");
        warn(format!(
            "could not read the options of '{}', so its optional dependencies (`enabled_if`) are treated as disabled. Does its cmake/unideps.cmake support nested builds (update it) and does the configure reach unideps_setup()? Log: {}",
            node.name,
            log.display()
        ));
        return Ok(BTreeMap::new());
    };

    Ok(content
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect())
}

fn build_node(ctx: &BuildContext, graph: &DependencyGraph, idx: NodeIndex, stack: &mut NestedStack) -> Result<Built> {
    let deps = graph.reachable(idx, Direction::Outgoing);
    let dependents: Vec<&str> = graph
        .reachable(idx, Direction::Incoming)
        .into_iter()
        .map(|i| graph.graph[i].name.as_str())
        .collect();

    let mut node = graph.graph[idx].clone();
    check_files(&node)?;
    ctx.configure_node(&mut node, &dependents)?;

    if node.kind == NodeKind::HostTool
        && let Some(local) = ctx.project.local_tool_path(&node.name)
    {
        if !local.exists() {
            anyhow::bail!("Local tool path for '{}' does not exist: {}", node.name, local.display());
        }
        println!("[LOCAL] {} ({})", node.name, local.display());
        node.build_id = Some(HashCalculator::short_hash(&local.to_string_lossy()));
        node.install_prefix = Some(local);
        return Ok(Built { node, nested: Vec::new() });
    }

    let sources = ctx.sources;
    sources.resolve_floating_ref(&mut node)?;

    let source_id = HashCalculator::calculate_source_id(&node);
    node.source_id = Some(source_id.clone());

    let mut dep_build_ids: Vec<&str> = deps.iter().filter_map(|&d| graph.graph[d].build_id.as_deref()).collect();

    // What the package's own `unideps.toml` pulled in the last time is remembered under the
    // build id it would have without it, which is known here. That makes an installed
    // package a cache hit before its source is fetched and, for a package with a nested
    // manifest, before it is configured once just to read its options back out.
    let plain_id = HashCalculator::calculate_build_id(&node, &ctx.target, &ctx.host, &dep_build_ids);
    if let Some(resolution) = Resolution::load(ctx.storage, &node.name, &plain_id) {
        println!("[CACHE HIT] {} ({})", node.name, resolution.build_id);
        node.install_prefix = Some(
            ctx.storage
                .installed_dir()
                .join(format!("{}-{}", node.name, resolution.build_id)),
        );
        node.build_id = Some(resolution.build_id);
        return Ok(Built { node, nested: resolution.nested });
    }

    // The nested dependencies are part of this package's build, so they have to be
    // known (and built) before its build id can be computed. This needs the source
    // even when the package itself is cached; an existing checkout is reused offline.
    let src_dir = sources.prepared_source(&node, &source_id)?;
    let direct_deps: Vec<&DependencyNode> = deps.iter().map(|&d| &graph.graph[d]).collect();
    let nested = build_nested(ctx, &node, &direct_deps, &src_dir, &source_id, stack)?;
    let nested_nodes: &[DependencyNode] = nested.as_ref().map_or(&[], |n| n.nodes.as_slice());

    dep_build_ids.extend(nested_nodes.iter().filter_map(|n| n.build_id.as_deref()));
    let build_id = HashCalculator::calculate_build_id(&node, &ctx.target, &ctx.host, &dep_build_ids);
    node.build_id = Some(build_id.clone());

    let dir_name = format!("{}-{build_id}", node.name);
    let install_dir = ctx.storage.installed_dir().join(&dir_name);
    node.install_prefix = Some(install_dir.clone());
    let archive_name = format!("{dir_name}.tar.zst");
    let cached_archive = ctx.storage.cache_dir().join(&archive_name);
    // A nested package that is re-resolved on every run (a branch, a `path` source) is not
    // this package's forever, and a tool the project points at a local path is not unideps'
    // to remember either; a resolution holding one keeps being worked out from scratch.
    // Recording the rest is best effort: the only loss is that the next run resolves again.
    let recordable = nested_nodes
        .iter()
        .all(|n| is_recordable(n) && ctx.project.local_tool_path(&n.name).is_none());
    let done = |node: DependencyNode| {
        let built = Built { node, nested: nested_nodes.to_vec() };
        if recordable {
            let record = Resolution::new(&built.node.name, &build_id, built.nested.clone());
            if let Err(e) = record.store(ctx.storage, &plain_id) {
                warn(format!("{e:#}"));
            }
        }
        built
    };

    if StorageManager::is_installed(&install_dir) {
        println!("[CACHE HIT] {} ({build_id})", node.name);
        return Ok(done(node));
    }

    // Only the run that actually installs this package holds a lock, and only for as long
    // as it takes; the others wait for this package instead of for the whole build.
    let _package_lock = FileLock::acquire(ctx.storage.package_lock_path(&dir_name))?;
    if StorageManager::is_installed(&install_dir) {
        println!("[CACHE HIT] {} ({build_id})", node.name);
        return Ok(done(node));
    }

    GitSource::remove_dir_all_force(&install_dir)?;

    if cached_archive.exists() {
        println!("[UNPACK] {} ({build_id}) from cache: {archive_name}", node.name);
        match Packager::unpack_zst_to_dir(&cached_archive, &install_dir) {
            Ok(()) => {
                StorageManager::mark_installed(&install_dir)?;
                return Ok(done(node));
            }
            Err(e) => {
                warn(format!("discarding corrupted cache archive {}: {e:#}", cached_archive.display()));
                let _ = std::fs::remove_file(&cached_archive);
                GitSource::remove_dir_all_force(&install_dir)?;
            }
        }
    }

    println!("[BUILD] {} ({build_id})", node.name);
    let build_scratch = ctx.storage.scratch_dir().join(&dir_name);

    if node.header_only {
        install_headers(&node, &src_dir, &install_dir)?;
    } else {
        if !src_dir.join("CMakeLists.txt").exists() {
            anyhow::bail!(
                "CMakeLists.txt not found in source directory of '{}': {}",
                node.name,
                src_dir.display()
            );
        }
        let nested_targets = match nested {
            Some(ref n) => {
                std::fs::create_dir_all(&build_scratch)?;
                let file = std::path::absolute(build_scratch.join("unideps_targets.cmake"))?;
                std::fs::write(&file, &n.targets_cmake)?;
                Some(file)
            }
            None => None,
        };
        let dep_nodes: Vec<&DependencyNode> = direct_deps.iter().copied().chain(nested_nodes).collect();
        run_cmake(
            ctx,
            &dep_nodes,
            &node,
            &src_dir,
            &build_scratch.join("build"),
            &install_dir,
            CmakeMode::Build { nested_targets: nested_targets.as_deref() },
        )?;
    }

    Packager::pack_dir_to_zst(&install_dir, &cached_archive)?;
    StorageManager::mark_installed(&install_dir)?;

    if !ctx.project.keep_build_dirs() {
        let _ = GitSource::remove_dir_all_force(&build_scratch);
    }
    Ok(done(node))
}

enum CmakeMode<'a> {
    Build { nested_targets: Option<&'a Path> },
    /// Only configure; `unideps_setup()` of the package writes its options to the file.
    Probe(&'a Path),
}

fn run_cmake(
    ctx: &BuildContext,
    dep_nodes: &[&DependencyNode],
    node: &DependencyNode,
    src_dir: &Path,
    bld_dir: &Path,
    install_dir: &Path,
    mode: CmakeMode,
) -> Result<()> {
    // A nested package can also be a direct dependency: pass its prefix once.
    let mut seen = BTreeSet::new();
    let dep_nodes: Vec<&DependencyNode> = dep_nodes
        .iter()
        .copied()
        .filter(|d| d.install_prefix.as_ref().is_none_or(|p| seen.insert(p.clone())))
        .collect();

    let dep_prefixes: Vec<PathBuf> = dep_nodes
        .iter()
        .filter(|d| d.kind == NodeKind::TargetDependency)
        .filter_map(|d| d.install_prefix.clone())
        .collect();

    let mut host_tools_bins: Vec<PathBuf> = Vec::new();
    for d in dep_nodes.iter().filter(|d| d.kind == NodeKind::HostTool) {
        if let Some(ref p) = d.install_prefix {
            let bin_dir = p.join("bin");
            if bin_dir.exists() {
                host_tools_bins.push(bin_dir);
            }
            host_tools_bins.push(p.clone());
        }
    }

    let mut definitions = node.cmake_options.clone();
    for (k, v) in &node.config.cmake_options {
        definitions.insert(k.clone(), v.clone());
    }
    let prefix_of = |name: &str| {
        let dep = dep_nodes.iter().find(|d| d.name == name)?;
        Some(dep.install_prefix.as_ref()?.to_string_lossy().replace('\\', "/"))
    };
    for (key, value) in definitions.iter_mut() {
        *value = expand_dep_refs(value, &prefix_of)
            .with_context(|| format!("In cmake_options.{key} of '{}'", node.name))?;
    }
    for d in dep_nodes.iter().filter(|d| d.kind == NodeKind::TargetDependency) {
        for (k, v) in dep_root_vars(d) {
            definitions.entry(k).or_insert(v);
        }
    }
    // Read by `unideps_setup()` inside the package instead of running unideps. Not part
    // of the build id: the file only lists prefixes that are already hashed.
    let (var, file) = match mode {
        CmakeMode::Build { nested_targets } => ("UNIDEPS_NESTED_TARGETS", nested_targets),
        CmakeMode::Probe(file) => ("UNIDEPS_NESTED_PROBE", Some(file)),
    };
    if let Some(file) = file {
        definitions.insert(var.into(), file.to_string_lossy().replace('\\', "/"));
    }

    let triple = ctx.triple_for(node);
    let msvc_runtime = (triple.env == Environment::Msvc).then(|| node.config.vc_runtime.as_cmake_value());
    let (c_compiler, cxx_compiler, toolchain_file) = ctx.compilers_for(node);
    let build_type = node.config.build_type.to_string();
    let log_dir = ctx
        .storage
        .logs_dir()
        .join(install_dir.file_name().unwrap_or_default());

    let build = CMakeBuild {
        source_dir: src_dir,
        build_dir: bld_dir,
        install_prefix: install_dir,
        build_type: &build_type,
        shared: node.shared.unwrap_or(node.config.shared),
        msvc_runtime,
        c_compiler,
        cxx_compiler,
        toolchain_file,
        definitions: &definitions,
        prefix_paths: &dep_prefixes,
        host_tools_bins: &host_tools_bins,
        log_dir: Some(&log_dir),
        flags: &node.config.flags,
        cxx_std: &node.config.cxx_std,
        cxx_stdlib: node.config.cxx_stdlib,
        lto: node.config.lto,
        jobs: ctx.project.max_jobs(),
        collect_pdbs: triple.os == Os::Windows,
        isolate_env: node.kind == NodeKind::HostTool && !ctx.is_native(),
    };
    match mode {
        CmakeMode::Build { .. } => CMakeRunner::build_and_install(&build),
        CmakeMode::Probe(_) => CMakeRunner::configure_only(&build).map(drop),
    }
    .with_context(|| format!("Failed to build '{}'", node.name))
}

/// Keeps the relative layout because the generator resolves `include_dirs` against
/// the install prefix.
fn install_headers(node: &DependencyNode, src_dir: &Path, install_dir: &Path) -> Result<()> {
    let dirs = if node.include_dirs.is_empty() {
        vec![PathBuf::from("include")]
    } else {
        node.include_dirs.clone()
    };
    std::fs::create_dir_all(install_dir)?;
    for rel in dirs {
        if rel.is_absolute() {
            anyhow::bail!("include_dirs of header-only '{}' must be relative: {}", node.name, rel.display());
        }
        let from = src_dir.join(&rel);
        if !from.is_dir() {
            anyhow::bail!(
                "Header-only '{}': '{}' not found in source; set `include_dirs` to the header directory",
                node.name,
                rel.display()
            );
        }
        unideps_builder::patch::PatchApplier::copy_dir_recursive(&from, &install_dir.join(&rel))?;
    }
    Ok(())
}

/// `<Name>_ROOT`, `<Name>_INCLUDE_DIR(S)` and `<Name>_LIBRARY/LIBRARIES` hints so that
/// classic Find modules of dependent packages locate this dependency.
fn dep_root_vars(dep: &DependencyNode) -> Vec<(String, String)> {
    let mut vars = Vec::new();
    let Some(ref prefix) = dep.install_prefix else { return vars };
    let norm = |p: &Path| p.to_string_lossy().replace('\\', "/");

    let mut names = vec![dep.name.clone(), dep.name.to_uppercase()];
    if let Some(ref imp) = dep.import_name {
        names.push(imp.clone());
        names.push(imp.to_uppercase());
    }
    names.dedup();

    let prefix_str = norm(prefix);
    let inc_dir = prefix.join("include");
    let inc_str = inc_dir.exists().then(|| norm(&inc_dir));

    let mut found_lib = dep
        .libraries
        .iter()
        .find_map(|lib| CMakeGenerator::resolve_library_path(prefix, lib))
        .map(|p| norm(&p));
    if found_lib.is_none()
        && let Ok(entries) = std::fs::read_dir(prefix.join("lib"))
    {
        let is_lib = |f: &str| f.ends_with(".a") || f.ends_with(".lib") || f.ends_with(".so") || f.ends_with(".dylib");
        let mut candidates: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.file_name().is_some_and(|f| is_lib(&f.to_string_lossy().to_lowercase())))
            .collect();
        candidates.sort();
        let wanted: Vec<String> = names.iter().map(|n| n.to_lowercase()).collect();
        let by_name = candidates.iter().find(|p| {
            let f = p.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
            wanted.iter().any(|w| f.contains(w.trim_start_matches("lib")))
        });
        found_lib = by_name.or(candidates.first()).map(|p| norm(p));
    }

    for n in &names {
        vars.push((format!("{n}_ROOT"), prefix_str.clone()));
        if let Some(ref inc) = inc_str {
            vars.push((format!("{n}_INCLUDE_DIR"), inc.clone()));
            vars.push((format!("{n}_INCLUDE_DIRS"), inc.clone()));
        }
        if let Some(ref lib) = found_lib {
            vars.push((format!("{n}_LIBRARY"), lib.clone()));
            vars.push((format!("{n}_LIBRARIES"), lib.clone()));
        }
    }
    vars
}

pub fn fetch(manifest: &Path, base_dir: Option<&Path>) -> Result<()> {
    let project = Project::load(manifest)?;
    let storage = StorageManager::new(project.base_dir(base_dir));
    storage.ensure_dirs()?;
    let _run_lock = FileLock::acquire_shared(storage.global_build_lock_path())?;
    let sources = Sources::new(&storage);
    fetch_project(&project, &sources, &mut BTreeSet::new())?;
    println!("All sources fetched");
    Ok(())
}

/// Also follows the `unideps.toml` files found in the fetched sources.
fn fetch_project(project: &Project, sources: &Sources, visited: &mut BTreeSet<PathBuf>) -> Result<()> {
    let graph = project.build_graph()?;
    for idx in graph.graph.node_indices() {
        let mut node = graph.graph[idx].clone();
        if node.git.is_none() || node.path.is_some() || project.local_tool_path(&node.name).is_some() {
            continue;
        }
        sources.resolve_floating_ref(&mut node)?;
        let dir = sources.base_source(&node)?;
        println!("[SOURCE] {} -> {}", node.name, dir.display());

        let manifest = dir.join("unideps.toml");
        if node.kind == NodeKind::TargetDependency && manifest.is_file() && visited.insert(dir) {
            let nested = Project::load_nested(&manifest)?;
            fetch_project(&nested, sources, visited)
                .with_context(|| format!("In the nested unideps.toml of '{}'", node.name))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use unideps_core::manifest::{DependencyDetails, DependencySpec};

    fn node(name: &str, prefix: &str) -> DependencyNode {
        let mut n = DependencyNode::from_target_dep(name, &DependencySpec::Detailed(DependencyDetails::default()));
        n.install_prefix = Some(PathBuf::from(prefix));
        n
    }

    fn names(nodes: &[DependencyNode]) -> Vec<(&str, &str)> {
        nodes
            .iter()
            .map(|n| (n.name.as_str(), n.install_prefix.as_deref().and_then(Path::to_str).unwrap_or("")))
            .collect()
    }

    #[test]
    fn nested_packages_come_first() {
        let merged = merge_nested(vec![node("b", "/b")], vec![node("a", "/a")]);
        assert_eq!(names(&merged), [("b", "/b"), ("a", "/a")]);
    }

    #[test]
    fn declared_package_wins_over_nested_one() {
        let merged = merge_nested(vec![node("b", "/nested/b")], vec![node("b", "/own/b")]);
        assert_eq!(names(&merged), [("b", "/own/b")]);
    }

    #[test]
    fn first_nested_package_wins_and_identical_ones_are_merged() {
        let nested = vec![node("b", "/b1"), node("b", "/b2"), node("c", "/c"), node("c", "/c")];
        let merged = merge_nested(nested, Vec::new());
        assert_eq!(names(&merged), [("b", "/b1"), ("c", "/c")]);
    }
}
