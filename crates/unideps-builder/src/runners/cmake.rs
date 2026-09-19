use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use unideps_core::target::CxxStdlib;

pub struct CMakeRunner;

pub struct CMakeBuild<'a> {
    pub source_dir: &'a Path,
    pub build_dir: &'a Path,
    pub install_prefix: &'a Path,
    pub build_type: &'a str,
    pub shared: bool,
    pub msvc_runtime: Option<&'a str>,
    pub c_compiler: Option<&'a Path>,
    pub cxx_compiler: Option<&'a Path>,
    pub toolchain_file: Option<&'a Path>,
    pub definitions: &'a BTreeMap<String, String>,
    pub prefix_paths: &'a [PathBuf],
    pub host_tools_bins: &'a [PathBuf],
    pub log_dir: Option<&'a Path>,
    pub flags: &'a [String],
    pub cxx_std: &'a str,
    pub cxx_stdlib: CxxStdlib,
    pub lto: bool,
    pub jobs: Option<usize>,
    pub collect_pdbs: bool,
    /// Drop compiler/toolchain environment variables inherited from the consuming
    /// build (CMake exports e.g. `CC` of the target toolchain to child processes).
    /// Used for host tools so they are built with the host's default compiler.
    pub isolate_env: bool,
}

pub const TOOLCHAIN_ENV_VARS: &[&str] = &[
    "CC", "CXX", "CPP", "AS", "AR", "LD", "RC", "ASM", "OBJC", "OBJCXX", "CUDACXX",
    "CFLAGS", "CXXFLAGS", "CPPFLAGS", "LDFLAGS", "ASMFLAGS", "RCFLAGS",
    "CMAKE_TOOLCHAIN_FILE", "CMAKE_GENERATOR_PLATFORM", "CMAKE_GENERATOR_TOOLSET",
    "SDKROOT", "MACOSX_DEPLOYMENT_TARGET",
];

/// Maps `"17"`, `"c++17"`, `"gnu++17"` to (`CMAKE_CXX_STANDARD`, `CMAKE_CXX_EXTENSIONS`).
pub fn parse_cxx_std(value: &str) -> Result<Option<(String, bool)>> {
    let v = value.trim().to_lowercase();
    if v.is_empty() {
        return Ok(None);
    }
    let (num, ext) = if let Some(n) = v.strip_prefix("gnu++") {
        (n, true)
    } else if let Some(n) = v.strip_prefix("c++") {
        (n, false)
    } else {
        (v.as_str(), false)
    };
    let num = match num {
        "0x" => "11",
        "1y" => "14",
        "1z" => "17",
        "2a" => "20",
        "2b" => "23",
        "2c" => "26",
        other => other,
    };
    if num.is_empty() || !num.chars().all(|c| c.is_ascii_digit()) {
        anyhow::bail!("Invalid cxx_std '{value}': expected e.g. \"17\", \"c++20\" or \"gnu++17\"");
    }
    Ok(Some((num.to_string(), ext)))
}

fn norm(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

impl CMakeRunner {
    /// Separate from `build_and_install` so tests can inspect the arguments.
    pub fn configure_args(b: &CMakeBuild, src: &Path, bld: &Path, pfx: &Path) -> Result<Vec<String>> {
        let mut args = vec![
            "-S".to_string(),
            norm(src),
            "-B".to_string(),
            norm(bld),
            "-GNinja".to_string(),
            format!("-DCMAKE_BUILD_TYPE={}", b.build_type),
            format!("-DCMAKE_INSTALL_PREFIX={}", norm(pfx)),
            format!("-DBUILD_SHARED_LIBS={}", if b.shared { "ON" } else { "OFF" }),
        ];
        if let Some(rt) = b.msvc_runtime {
            args.push("-DCMAKE_POLICY_DEFAULT_CMP0091=NEW".into());
            args.push(format!("-DCMAKE_MSVC_RUNTIME_LIBRARY={rt}"));
        }
        if let Some(tf) = b.toolchain_file {
            args.push(format!("-DCMAKE_TOOLCHAIN_FILE={}", norm(tf)));
        } else {
            if let Some(cc) = b.c_compiler {
                args.push(format!("-DCMAKE_C_COMPILER={}", norm(cc)));
            }
            if let Some(cxx) = b.cxx_compiler {
                args.push(format!("-DCMAKE_CXX_COMPILER={}", norm(cxx)));
            }
        }

        if !b.prefix_paths.is_empty() {
            let joined: Vec<String> = b.prefix_paths.iter().map(|p| norm(p)).collect();
            let joined_str = joined.join(";");
            args.push(format!("-DCMAKE_PREFIX_PATH={joined_str}"));
            args.push(format!("-DCMAKE_FIND_ROOT_PATH={joined_str}"));
        }

        // Static deps usually end up linked into shared libs (-fPIC required on ELF).
        // MSVC ignores this; users can override via cmake_options.
        if !b.definitions.contains_key("CMAKE_POSITION_INDEPENDENT_CODE") {
            args.push("-DCMAKE_POSITION_INDEPENDENT_CODE=ON".into());
        }

        if let Some((std, ext)) = parse_cxx_std(b.cxx_std)? {
            if !b.definitions.contains_key("CMAKE_CXX_STANDARD") {
                args.push(format!("-DCMAKE_CXX_STANDARD={std}"));
            }
            if !b.definitions.contains_key("CMAKE_CXX_EXTENSIONS") {
                args.push(format!("-DCMAKE_CXX_EXTENSIONS={}", if ext { "ON" } else { "OFF" }));
            }
        }

        if b.lto && !b.definitions.contains_key("CMAKE_INTERPROCEDURAL_OPTIMIZATION") {
            args.push("-DCMAKE_POLICY_DEFAULT_CMP0069=NEW".into());
            args.push("-DCMAKE_INTERPROCEDURAL_OPTIMIZATION=ON".into());
        }

        let mut c_flags: Vec<String> = b.definitions.get("CMAKE_C_FLAGS").into_iter().cloned().collect();
        let mut cxx_flags: Vec<String> = b.definitions.get("CMAKE_CXX_FLAGS").into_iter().cloned().collect();
        c_flags.extend(b.flags.iter().cloned());
        cxx_flags.extend(b.flags.iter().cloned());
        match b.cxx_stdlib {
            CxxStdlib::Libcxx => cxx_flags.push("-stdlib=libc++".into()),
            CxxStdlib::Libstdcxx => cxx_flags.push("-stdlib=libstdc++".into()),
            CxxStdlib::PlatformDefault | CxxStdlib::Custom => {}
        }
        if !c_flags.is_empty() {
            args.push(format!("-DCMAKE_C_FLAGS={}", c_flags.join(" ")));
        }
        if !cxx_flags.is_empty() {
            args.push(format!("-DCMAKE_CXX_FLAGS={}", cxx_flags.join(" ")));
        }

        for (k, v) in b.definitions {
            if k == "CMAKE_C_FLAGS" || k == "CMAKE_CXX_FLAGS" {
                continue;
            }
            args.push(format!("-D{k}={v}"));
        }
        Ok(args)
    }

    pub fn build_and_install(b: &CMakeBuild) -> Result<()> {
        std::fs::create_dir_all(b.build_dir)?;
        std::fs::create_dir_all(b.install_prefix)?;

        let src = dunce::canonicalize(b.source_dir).unwrap_or_else(|_| b.source_dir.to_path_buf());
        let bld = dunce::canonicalize(b.build_dir).unwrap_or_else(|_| b.build_dir.to_path_buf());
        let pfx = dunce::canonicalize(b.install_prefix).unwrap_or_else(|_| b.install_prefix.to_path_buf());

        let mut cfg = Command::new("cmake");
        cfg.current_dir(&bld);
        cfg.args(Self::configure_args(b, &src, &bld, &pfx)?);
        if b.isolate_env {
            for var in TOOLCHAIN_ENV_VARS {
                cfg.env_remove(var);
            }
        }

        let mut pkg_config_paths = Vec::new();
        for p in b.prefix_paths {
            for sub in [p.join("lib").join("pkgconfig"), p.join("share").join("pkgconfig")] {
                if sub.exists() {
                    pkg_config_paths.push(sub);
                }
            }
        }
        if !pkg_config_paths.is_empty() {
            if let Some(existing) = std::env::var_os("PKG_CONFIG_PATH") {
                pkg_config_paths.extend(std::env::split_paths(&existing));
            }
            if let Ok(new_val) = std::env::join_paths(pkg_config_paths) {
                cfg.env("PKG_CONFIG_PATH", new_val);
            }
        }

        let path_env = if b.host_tools_bins.is_empty() {
            None
        } else {
            let mut paths = b.host_tools_bins.to_vec();
            if let Some(existing_path) = std::env::var_os("PATH") {
                paths.extend(std::env::split_paths(&existing_path));
            }
            std::env::join_paths(paths).ok()
        };
        if let Some(ref p) = path_env {
            cfg.env("PATH", p);
        }

        let log_destination = b.log_dir.map(PathBuf::from).unwrap_or_else(|| bld.clone());
        std::fs::create_dir_all(&log_destination)?;

        Self::run_logged(cfg, &log_destination.join("configure.log"), "configure")?;

        let mut bld_cmd = Command::new("cmake");
        bld_cmd.current_dir(&bld);
        if b.isolate_env {
            for var in TOOLCHAIN_ENV_VARS {
                bld_cmd.env_remove(var);
            }
        }
        bld_cmd.args(["--build", "."]);
        bld_cmd.args(["--config", b.build_type]);
        if let Some(jobs) = b.jobs {
            bld_cmd.args(["--parallel", &jobs.to_string()]);
        }
        if let Some(ref p) = path_env {
            bld_cmd.env("PATH", p);
        }
        Self::run_logged(bld_cmd, &log_destination.join("build.log"), "build")?;

        let mut inst_cmd = Command::new("cmake");
        inst_cmd.current_dir(&bld);
        inst_cmd.args(["--install", "."]);
        inst_cmd.args(["--config", b.build_type]);
        let inst_log_path = log_destination.join("install.log");
        Self::run_logged(inst_cmd, &inst_log_path, "install")?;

        if b.collect_pdbs {
            Self::auto_install_pdbs(&bld, &pfx, Some(&inst_log_path))?;
        }

        Ok(())
    }

    fn run_logged(mut cmd: Command, log_path: &Path, step: &str) -> Result<()> {
        cmd.env_remove("MAKEFLAGS");
        let mut log = std::fs::File::create(log_path)?;
        std::io::Write::write_all(&mut log, format!("Command: {cmd:?}\n\n").as_bytes())?;
        let err = log.try_clone()?;
        cmd.stdout(log);
        cmd.stderr(err);
        let status = cmd
            .status()
            .with_context(|| format!("Failed to execute cmake {step} (is cmake on PATH?)"))?;
        if !status.success() {
            let tail = std::fs::read_to_string(log_path)
                .map(|s| {
                    let lines: Vec<&str> = s.lines().collect();
                    lines[lines.len().saturating_sub(20)..].join("\n")
                })
                .unwrap_or_default();
            anyhow::bail!("CMake {step} step failed. Full log: {}\n{tail}", log_path.display());
        }
        Ok(())
    }

    pub fn auto_install_pdbs(build_dir: &Path, install_prefix: &Path, log_path: Option<&Path>) -> Result<()> {
        if !build_dir.exists() || !install_prefix.exists() {
            return Ok(());
        }

        let has_ext = |p: &Path, exts: &[&str]| {
            p.extension()
                .and_then(|s| s.to_str())
                .is_some_and(|e| exts.iter().any(|x| e.eq_ignore_ascii_case(x)))
        };

        let bld_pdbs: Vec<PathBuf> = walkdir::WalkDir::new(build_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file() && has_ext(e.path(), &["pdb"]))
            .map(|e| e.path().to_path_buf())
            .collect();

        if bld_pdbs.is_empty() {
            return Ok(());
        }

        let installed_binaries: Vec<PathBuf> = walkdir::WalkDir::new(install_prefix)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file() && has_ext(e.path(), &["lib", "dll", "exe", "a"]))
            .map(|e| e.path().to_path_buf())
            .collect();

        let mut log_file = log_path.and_then(|p| std::fs::OpenOptions::new().append(true).open(p).ok());
        let mut log = |src: &Path, dst: &Path| {
            if let Some(ref mut lf) = log_file {
                use std::io::Write;
                let _ = writeln!(lf, "Auto-installed PDB: {} -> {}", src.display(), dst.display());
            }
        };

        for bin_path in &installed_binaries {
            let bin_dir = bin_path.parent().unwrap_or(install_prefix);
            let bin_stem = bin_path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();

            let mut candidate_pdbs: Vec<PathBuf> = Vec::new();

            if let Ok(data) = std::fs::read(bin_path) {
                for emb in Self::extract_embedded_pdbs(&data) {
                    let emb_path = Path::new(&emb);
                    if emb_path.is_file() {
                        candidate_pdbs.push(emb_path.to_path_buf());
                    } else if let Some(fname) = emb_path.file_name().and_then(|f| f.to_str()) {
                        candidate_pdbs.extend(
                            bld_pdbs
                                .iter()
                                .filter(|bp| {
                                    bp.file_name()
                                        .and_then(|f| f.to_str())
                                        .is_some_and(|n| n.eq_ignore_ascii_case(fname))
                                })
                                .cloned(),
                        );
                    }
                }
            }

            for bp in &bld_pdbs {
                let bp_stem = bp.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
                let matches = bp_stem.eq_ignore_ascii_case(bin_stem)
                    || bin_stem.strip_prefix("lib").is_some_and(|s| bp_stem.eq_ignore_ascii_case(s))
                    || (bin_stem.len() > 1
                        && bin_stem.ends_with('d')
                        && bp_stem.eq_ignore_ascii_case(&bin_stem[..bin_stem.len() - 1]))
                    || bin_stem.strip_suffix("-static").is_some_and(|s| bp_stem.eq_ignore_ascii_case(s));
                if matches {
                    candidate_pdbs.push(bp.clone());
                }
            }

            candidate_pdbs.sort();
            candidate_pdbs.dedup();

            for src_pdb in candidate_pdbs {
                let Some(fname) = src_pdb.file_name().and_then(|f| f.to_str()) else {
                    continue;
                };
                let dest_pdb = bin_dir.join(fname);
                if !dest_pdb.exists() && std::fs::copy(&src_pdb, &dest_pdb).is_ok() {
                    log(&src_pdb, &dest_pdb);
                }

                // vc140.pdb .. vc143.pdb are per-directory compiler PDBs, not per-binary.
                let lower = fname.to_ascii_lowercase();
                let is_compiler_pdb = lower.starts_with("vc1") && lower.len() == "vc140.pdb".len();
                let stem_pdb = bin_dir.join(format!("{bin_stem}.pdb"));
                if !stem_pdb.exists() && !is_compiler_pdb && std::fs::copy(&src_pdb, &stem_pdb).is_ok() {
                    log(&src_pdb, &stem_pdb);
                }
            }
        }

        Ok(())
    }

    fn extract_embedded_pdbs(data: &[u8]) -> Vec<String> {
        let mut results = Vec::new();
        let mut i = 0;
        let len = data.len();
        while i + 4 <= len {
            if data[i] == b'.' && data[i + 1..i + 4].eq_ignore_ascii_case(b"pdb") {
                let end = i + 4;
                let next_byte = data.get(end).copied().unwrap_or(0);
                if matches!(next_byte, 0 | b'\r' | b'\n' | b'"' | b'\'' | b' ') {
                    let mut start = i;
                    while start > 0 {
                        let b = data[start - 1];
                        if matches!(b, b'"' | b'\'' | b';' | b'|' | b'<' | b'>') || b < 32 {
                            break;
                        }
                        start -= 1;
                    }
                    if start < i
                        && let Ok(s) = std::str::from_utf8(&data[start..end])
                        && let Some(norm) = Self::normalize_extracted_path(s)
                    {
                        results.push(norm);
                    }
                }
                i += 4;
            } else {
                i += 1;
            }
        }
        results
    }

    fn normalize_extracted_path(s: &str) -> Option<String> {
        let trimmed = s.trim();
        if trimmed.is_empty() || !trimmed.to_ascii_lowercase().ends_with(".pdb") {
            return None;
        }
        if let Some(pos) = trimmed.rfind(":\\").or_else(|| trimmed.rfind(":/"))
            && pos >= 1
            && trimmed.as_bytes()[pos - 1].is_ascii_alphabetic()
        {
            return Some(trimmed[pos - 1..].to_string());
        }
        if let Some(last) = trimmed.split_whitespace().last()
            && last.to_ascii_lowercase().ends_with(".pdb")
        {
            return Some(last.to_string());
        }
        Some(trimmed.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_embedded_pdbs() {
        let data = b"some prefix data C:\\dev\\build\\mylib.pdb\0 middle data another.PDB\0 suffix";
        let pdbs = CMakeRunner::extract_embedded_pdbs(data);
        assert_eq!(pdbs.len(), 2);
        assert_eq!(pdbs[0], "C:\\dev\\build\\mylib.pdb");
        assert_eq!(pdbs[1], "another.PDB");
    }

    #[test]
    fn test_auto_install_pdbs() {
        let temp = tempfile::tempdir().unwrap();
        let bld = temp.path().join("build");
        let pfx = temp.path().join("install");
        std::fs::create_dir_all(bld.join("CMakeFiles/mylib.dir")).unwrap();
        std::fs::create_dir_all(pfx.join("lib")).unwrap();

        let pdb_path = bld.join("CMakeFiles/mylib.dir/mylib.pdb");
        std::fs::write(&pdb_path, b"test-pdb-content").unwrap();

        let unused_pdb_path = bld.join("unused.pdb");
        std::fs::write(&unused_pdb_path, b"unused-pdb-content").unwrap();

        let lib_path = pfx.join("lib/mylib.lib");
        let pdb_str = pdb_path.to_string_lossy();
        let mut lib_content = Vec::new();
        lib_content.extend_from_slice(b"dummy lib header\0");
        lib_content.extend_from_slice(pdb_str.as_bytes());
        lib_content.push(0);
        std::fs::write(&lib_path, lib_content).unwrap();

        CMakeRunner::auto_install_pdbs(&bld, &pfx, None).unwrap();

        let installed_pdb = pfx.join("lib/mylib.pdb");
        assert!(installed_pdb.exists());
        assert_eq!(std::fs::read(&installed_pdb).unwrap(), b"test-pdb-content");
        assert!(!pfx.join("lib/unused.pdb").exists());

        std::fs::write(&installed_pdb, b"existing-content").unwrap();
        CMakeRunner::auto_install_pdbs(&bld, &pfx, None).unwrap();
        assert_eq!(std::fs::read(&installed_pdb).unwrap(), b"existing-content");
    }

    fn opts<'a>(defs: &'a BTreeMap<String, String>, flags: &'a [String]) -> CMakeBuild<'a> {
        CMakeBuild {
            source_dir: Path::new("src"),
            build_dir: Path::new("bld"),
            install_prefix: Path::new("pfx"),
            build_type: "Release",
            shared: false,
            msvc_runtime: None,
            c_compiler: None,
            cxx_compiler: None,
            toolchain_file: None,
            definitions: defs,
            prefix_paths: &[],
            host_tools_bins: &[],
            log_dir: None,
            flags,
            cxx_std: "",
            cxx_stdlib: CxxStdlib::PlatformDefault,
            lto: false,
            jobs: None,
            collect_pdbs: false,
            isolate_env: false,
        }
    }

    fn args_of(b: &CMakeBuild) -> Vec<String> {
        CMakeRunner::configure_args(b, Path::new("s"), Path::new("b"), Path::new("p")).unwrap()
    }

    #[test]
    fn test_flags_std_stdlib_and_lto_reach_cmake() {
        let mut defs = BTreeMap::new();
        defs.insert("CMAKE_CXX_FLAGS".to_string(), "-DUSER".to_string());
        let flags = vec!["-O2".to_string()];
        let mut b = opts(&defs, &flags);
        b.cxx_std = "gnu++17";
        b.cxx_stdlib = CxxStdlib::Libcxx;
        b.lto = true;
        let args = args_of(&b);
        assert!(args.contains(&"-DCMAKE_CXX_STANDARD=17".to_string()));
        assert!(args.contains(&"-DCMAKE_CXX_EXTENSIONS=ON".to_string()));
        assert!(args.contains(&"-DCMAKE_INTERPROCEDURAL_OPTIMIZATION=ON".to_string()));
        assert!(args.contains(&"-DCMAKE_C_FLAGS=-O2".to_string()));
        assert!(args.contains(&"-DCMAKE_CXX_FLAGS=-DUSER -O2 -stdlib=libc++".to_string()));
        assert_eq!(args.iter().filter(|a| a.starts_with("-DCMAKE_CXX_FLAGS=")).count(), 1);
    }

    #[test]
    fn test_defaults_do_not_force_standard() {
        let defs = BTreeMap::new();
        let args = args_of(&opts(&defs, &[]));
        assert!(!args.iter().any(|a| a.contains("CMAKE_CXX_STANDARD")));
        assert!(!args.iter().any(|a| a.contains("CMAKE_CXX_FLAGS")));
    }

    #[test]
    fn test_parse_cxx_std() {
        assert_eq!(parse_cxx_std("20").unwrap(), Some(("20".into(), false)));
        assert_eq!(parse_cxx_std("c++2a").unwrap(), Some(("20".into(), false)));
        assert_eq!(parse_cxx_std("").unwrap(), None);
        assert!(parse_cxx_std("latest").is_err());
    }
}
