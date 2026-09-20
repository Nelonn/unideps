use anyhow::Result;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use unideps_core::target::{BuildType, VCRuntime};

/// CMake passes `BUILD_SHARED_LIBS` and options as `ON`/`OFF`, which clap's bool rejects.
pub fn parse_bool(s: &str) -> Result<bool, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "1" | "on" | "true" | "yes" | "y" => Ok(true),
        "0" | "off" | "false" | "no" | "n" | "" => Ok(false),
        other => Err(format!("expected ON/OFF, TRUE/FALSE, YES/NO or 1/0, got '{other}'")),
    }
}

/// `CMAKE_MSVC_RUNTIME_LIBRARY` is usually a generator expression such as
/// `MultiThreaded$<$<CONFIG:Debug>:Debug>DLL`; expand it for the build type.
pub fn resolve_msvc_runtime(raw: &str, build_type: BuildType) -> Result<VCRuntime> {
    let config = build_type.to_string();
    let mut out = String::new();
    let mut rest = raw.trim();
    const OPEN: &str = "$<$<CONFIG:";
    while let Some(pos) = rest.find(OPEN) {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + OPEN.len()..];
        let (configs, tail) = after
            .split_once(">:")
            .ok_or_else(|| anyhow::anyhow!("Malformed generator expression in MSVC runtime '{raw}'"))?;
        let (value, tail) = tail
            .split_once('>')
            .ok_or_else(|| anyhow::anyhow!("Malformed generator expression in MSVC runtime '{raw}'"))?;
        if configs.split(',').any(|c| c.trim().eq_ignore_ascii_case(&config)) {
            out.push_str(value);
        }
        rest = tail;
    }
    out.push_str(rest);
    if out.contains("$<") {
        anyhow::bail!("Unsupported generator expression in MSVC runtime '{raw}'");
    }
    Ok(out.parse::<VCRuntime>()?)
}

/// Expands `${NAME}` (a variable of the consuming CMake project, given as
/// `--cmake-args=-DNAME=...`) and `$ENV{NAME}` in a `cmake_options` value. Anything else,
/// including a `$` not followed by `{`, is kept as is. Unknown names are errors: passing
/// the literal text on to CMake would silently configure the package wrongly.
pub fn expand_vars(value: &str, vars: &BTreeMap<String, String>) -> Result<String> {
    let mut out = String::new();
    let mut rest = value;
    while let Some(pos) = rest.find('$') {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];
        let (from_env, body) = match tail.strip_prefix("$ENV{") {
            Some(b) => (true, b),
            None => match tail.strip_prefix("${") {
                Some(b) => (false, b),
                None => {
                    out.push('$');
                    rest = &tail[1..];
                    continue;
                }
            },
        };
        let Some((name, after)) = body.split_once('}') else {
            anyhow::bail!("Unterminated variable reference in '{value}'");
        };
        let resolved = if from_env { std::env::var(name).ok() } else { vars.get(name).cloned() };
        match resolved {
            Some(v) => out.push_str(&v),
            None if from_env => anyhow::bail!("Environment variable '{name}' (used in '{value}') is not set"),
            None => anyhow::bail!(
                "Variable '{name}' (used in '{value}') is not defined: set it in CMake before unideps_setup() or pass --cmake-args=-D{name}=<value>"
            ),
        }
        rest = after;
    }
    out.push_str(rest);
    Ok(out)
}

pub fn resolve_relative(base: &Path, p: &Path) -> PathBuf {
    if p.is_absolute() { p.to_path_buf() } else { base.join(p) }
}

/// Part of the build id, so upgrading the compiler or editing the toolchain file
/// invalidates cached builds.
pub fn toolchain_fingerprint(
    c_compiler: Option<&Path>,
    cxx_compiler: Option<&Path>,
    toolchain_file: Option<&Path>,
) -> Option<String> {
    let mut parts = Vec::new();
    for (label, compiler) in [("cc", c_compiler), ("cxx", cxx_compiler)] {
        let Some(compiler) = compiler else { continue };
        let resolved = if compiler.exists() {
            Some(compiler.to_path_buf())
        } else {
            which::which(compiler).ok()
        };
        let meta = resolved.as_ref().and_then(|p| std::fs::metadata(p).ok());
        let mtime = meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let len = meta.map(|m| m.len()).unwrap_or(0);
        let shown = resolved.unwrap_or_else(|| compiler.to_path_buf());
        parts.push(format!("{label}={}|{len}|{mtime}", shown.display()));
    }
    if let Some(tf) = toolchain_file {
        let content = std::fs::read(tf).unwrap_or_default();
        parts.push(format!(
            "toolchain={}|{}",
            tf.display(),
            unideps_core::hash::HashCalculator::short_hash_bytes(&content)
        ));
    }
    if parts.is_empty() {
        None
    } else {
        Some(unideps_core::hash::HashCalculator::short_hash(&parts.join(";")))
    }
}

/// Accepts both `-DKEY=VALUE` and `-DKEY:TYPE=VALUE`.
pub fn parse_define(arg: &str) -> Option<(String, String)> {
    let stripped = arg.strip_prefix("-D")?;
    let (k, v) = stripped.split_once('=')?;
    let key = k.split_once(':').map(|(k, _)| k).unwrap_or(k);
    if key.is_empty() {
        return None;
    }
    Some((key.to_string(), v.to_string()))
}

pub fn warn(msg: impl std::fmt::Display) {
    eprintln!("warning: {msg}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bool_parsing_accepts_cmake_spellings() {
        for t in ["ON", "on", "TRUE", "1", "YES", "y"] {
            assert_eq!(parse_bool(t), Ok(true), "{t}");
        }
        for f in ["OFF", "false", "0", "NO", "n", ""] {
            assert_eq!(parse_bool(f), Ok(false), "{f}");
        }
        assert!(parse_bool("maybe").is_err());
    }

    #[test]
    fn msvc_runtime_generator_expressions() {
        let genex = "MultiThreaded$<$<CONFIG:Debug>:Debug>DLL";
        assert_eq!(resolve_msvc_runtime(genex, BuildType::Debug).unwrap(), VCRuntime::MultiThreadedDebugDLL);
        assert_eq!(resolve_msvc_runtime(genex, BuildType::Release).unwrap(), VCRuntime::MultiThreadedDLL);
        let static_genex = "MultiThreaded$<$<CONFIG:Debug,RelWithDebInfo>:Debug>";
        assert_eq!(
            resolve_msvc_runtime(static_genex, BuildType::RelWithDebInfo).unwrap(),
            VCRuntime::MultiThreadedDebug
        );
        assert_eq!(resolve_msvc_runtime("MultiThreaded", BuildType::Debug).unwrap(), VCRuntime::MultiThreaded);
        assert!(resolve_msvc_runtime("$<IF:$<CONFIG:Debug>,a,b>", BuildType::Debug).is_err());
        assert!(resolve_msvc_runtime("Bogus", BuildType::Debug).is_err());
    }

    #[test]
    fn define_parsing() {
        assert_eq!(parse_define("-DFOO=ON"), Some(("FOO".into(), "ON".into())));
        assert_eq!(parse_define("-DFOO:BOOL=OFF"), Some(("FOO".into(), "OFF".into())));
        assert_eq!(parse_define("-DPATH=a=b"), Some(("PATH".into(), "a=b".into())));
        assert_eq!(parse_define("--foo"), None);
    }

    #[test]
    fn variables_are_expanded() {
        let vars = BTreeMap::from([("SKIA_DIR".to_string(), "C:/skia".to_string())]);
        assert_eq!(expand_vars("${SKIA_DIR}", &vars).unwrap(), "C:/skia");
        assert_eq!(expand_vars("-I${SKIA_DIR}/include;${SKIA_DIR}", &vars).unwrap(), "-IC:/skia/include;C:/skia");
        assert_eq!(expand_vars("plain $ and $x and 5$", &vars).unwrap(), "plain $ and $x and 5$");
        assert_eq!(expand_vars("", &vars).unwrap(), "");
    }

    #[test]
    fn unknown_or_broken_variables_are_errors() {
        let vars = BTreeMap::new();
        let err = expand_vars("${SKIA_DIR}", &vars).unwrap_err().to_string();
        assert!(err.contains("SKIA_DIR") && err.contains("--cmake-args"), "{err}");
        assert!(expand_vars("$ENV{UNIDEPS_SURELY_UNSET_VARIABLE}", &vars).is_err());
        assert!(expand_vars("${SKIA_DIR", &vars).is_err());
    }

    #[test]
    fn fingerprint_changes_with_toolchain_content() {
        let temp = tempfile::tempdir().unwrap();
        let tf = temp.path().join("tc.cmake");
        std::fs::write(&tf, "set(A 1)").unwrap();
        let a = toolchain_fingerprint(None, None, Some(&tf));
        std::fs::write(&tf, "set(A 2)").unwrap();
        let b = toolchain_fingerprint(None, None, Some(&tf));
        assert_ne!(a, b);
        assert_eq!(toolchain_fingerprint(None, None, None), None);
    }
}
