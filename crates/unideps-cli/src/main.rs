mod build;
mod project;
mod source;
mod util;

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use unideps_builder::git::GitSource;
use unideps_builder::lock::FileLock;
use unideps_builder::runners::cmake::ACTIVE_ENV_VAR;
use unideps_builder::storage::StorageManager;

#[derive(Parser, Debug)]
#[command(name = "unideps", version, about = "UniDeps C/C++ Dependency Orchestrator")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Fetch, build and install all dependencies, then generate the CMake targets file
    Build(Box<build::BuildArgs>),
    /// Download (git) sources of all dependencies and tools without building
    Fetch {
        #[arg(short, long, default_value = "unideps.toml")]
        manifest: PathBuf,
        #[arg(long)]
        base_dir: Option<PathBuf>,
    },
    /// Remove scratch build directories (sources, installs and cache are kept)
    Clean {
        #[arg(short, long, default_value = "unideps.toml")]
        manifest: PathBuf,
        #[arg(long)]
        base_dir: Option<PathBuf>,
    },
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if std::env::var_os(ACTIVE_ENV_VAR).is_some_and(|v| !v.is_empty()) {
        anyhow::bail!(
            "unideps was started from inside another unideps run ({ACTIVE_ENV_VAR} is set); it would wait forever \
             for the build lock. Nested unideps.toml files are built by the outer run: in the dependency's \
             CMakeLists.txt use `unideps_setup()` from cmake/unideps.cmake instead of calling `unideps` directly"
        );
    }
    match cli.command {
        Commands::Build(args) => build::run(*args),
        Commands::Fetch { manifest, base_dir } => build::fetch(&manifest, base_dir.as_deref()),
        Commands::Clean { manifest, base_dir } => {
            let storage = if manifest.exists() {
                let p = project::Project::load(&manifest)?;
                StorageManager::new(p.base_dir(base_dir.as_deref())).with_scratch_dir(p.scratch_dir())
            } else {
                let cwd = std::env::current_dir()?;
                StorageManager::new(project::resolve_base_dir(base_dir.as_deref(), None, &cwd))
            };
            let _lock = FileLock::acquire(storage.global_build_lock_path())?;
            GitSource::remove_dir_all_force(storage.scratch_dir())?;
            println!("UniDeps scratch directory cleaned: {}", storage.scratch_dir().display());
            // Probe results used to be cached; they are scratch space now, so drop the leftovers.
            if let Ok(entries) = std::fs::read_dir(storage.cache_dir()) {
                for entry in entries.flatten() {
                    if entry.path().extension().is_some_and(|e| e == "opts") {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
            Ok(())
        }
    }
}
