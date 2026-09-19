use crate::error::CoreError;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Arch {
    X86,
    X86_64,
    Arm,
    Arm64,
    Wasm32,
    Riscv64,
}

impl FromStr for Arch {
    type Err = CoreError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "x86" | "i686" | "i386" => Ok(Arch::X86),
            "x86_64" | "amd64" | "x64" => Ok(Arch::X86_64),
            "arm" | "armv7" | "armv7a" => Ok(Arch::Arm),
            "arm64" | "aarch64" => Ok(Arch::Arm64),
            "wasm32" => Ok(Arch::Wasm32),
            "riscv64" => Ok(Arch::Riscv64),
            other => Err(CoreError::TargetParse(format!("Unknown architecture: {other}"))),
        }
    }
}

impl fmt::Display for Arch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Arch::X86 => write!(f, "x86"),
            Arch::X86_64 => write!(f, "x86_64"),
            Arch::Arm => write!(f, "arm"),
            Arch::Arm64 => write!(f, "aarch64"),
            Arch::Wasm32 => write!(f, "wasm32"),
            Arch::Riscv64 => write!(f, "riscv64"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Os {
    Windows,
    Linux,
    Macos,
    Ios,
    Android,
    Emscripten,
}

impl FromStr for Os {
    type Err = CoreError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "windows" | "win32" | "win" | "mingw32" | "mingw" => Ok(Os::Windows),
            "linux" => Ok(Os::Linux),
            "macos" | "darwin" | "osx" => Ok(Os::Macos),
            "ios" => Ok(Os::Ios),
            "android" => Ok(Os::Android),
            "emscripten" => Ok(Os::Emscripten),
            other => Err(CoreError::TargetParse(format!("Unknown OS: {other}"))),
        }
    }
}

impl fmt::Display for Os {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Os::Windows => write!(f, "windows"),
            Os::Linux => write!(f, "linux"),
            Os::Macos => write!(f, "macos"),
            Os::Ios => write!(f, "ios"),
            Os::Android => write!(f, "android"),
            Os::Emscripten => write!(f, "emscripten"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Environment {
    Gnu,
    Musl,
    Msvc,
    AndroidEabi,
    None,
}

impl FromStr for Environment {
    type Err = CoreError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.starts_with("gnu") {
            Ok(Environment::Gnu)
        } else if s.starts_with("musl") {
            Ok(Environment::Musl)
        } else if s.starts_with("msvc") {
            Ok(Environment::Msvc)
        } else if s.starts_with("androideabi") || s.starts_with("android") {
            Ok(Environment::AndroidEabi)
        } else {
            // Empty, "none" or unrecognised (e.g. "elf", "sim") environments.
            Ok(Environment::None)
        }
    }
}

impl fmt::Display for Environment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Environment::Gnu => write!(f, "gnu"),
            Environment::Musl => write!(f, "musl"),
            Environment::Msvc => write!(f, "msvc"),
            Environment::AndroidEabi => write!(f, "android"),
            Environment::None => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TargetTriple {
    pub raw: String,
    pub arch: Arch,
    pub vendor: String,
    pub os: Os,
    pub env: Environment,
}

impl TargetTriple {
    pub fn host() -> Self {
        #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
        return Self::from_str("x86_64-pc-windows-msvc").unwrap();

        #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
        return Self::from_str("aarch64-pc-windows-msvc").unwrap();

        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        return Self::from_str("x86_64-unknown-linux-gnu").unwrap();

        #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
        return Self::from_str("aarch64-unknown-linux-gnu").unwrap();

        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        return Self::from_str("aarch64-apple-darwin").unwrap();

        #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
        return Self::from_str("x86_64-apple-darwin").unwrap();

        #[cfg(not(any(
            all(target_os = "windows", any(target_arch = "x86_64", target_arch = "aarch64")),
            all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")),
            all(target_os = "macos", any(target_arch = "x86_64", target_arch = "aarch64"))
        )))]
        return Self {
            raw: "unknown".into(),
            arch: Arch::X86_64,
            vendor: "unknown".into(),
            os: Os::Linux,
            env: Environment::Gnu,
        };
    }

    pub fn matches_platform_filter(&self, filter: &str) -> bool {
        let (negated, rule) = if let Some(stripped) = filter.strip_prefix('!') {
            (true, stripped.to_lowercase())
        } else {
            (false, filter.to_lowercase())
        };

        let matches = match rule.as_str() {
            "windows" | "win" => self.os == Os::Windows,
            "linux" => self.os == Os::Linux,
            "macos" | "darwin" | "osx" => self.os == Os::Macos,
            "ios" => self.os == Os::Ios,
            "android" => self.os == Os::Android,
            "emscripten" => self.os == Os::Emscripten,
            "x86_64" => self.arch == Arch::X86_64,
            "aarch64" | "arm64" => self.arch == Arch::Arm64,
            "arm" => self.arch == Arch::Arm,
            "x86" => self.arch == Arch::X86,
            "wasm32" => self.arch == Arch::Wasm32,
            "riscv64" => self.arch == Arch::Riscv64,
            "msvc" => self.env == Environment::Msvc,
            "gnu" => self.env == Environment::Gnu,
            "musl" => self.env == Environment::Musl,
            triple if triple == self.raw.to_lowercase() => true,
            _ => false,
        };

        if negated {
            !matches
        } else {
            matches
        }
    }

    pub fn matches_all_filters(&self, filters: &[String]) -> bool {
        if filters.is_empty() {
            return true;
        }

        let mut has_positive = false;
        let mut positive_matched = false;

        for f in filters {
            if f.starts_with('!') {
                if !self.matches_platform_filter(f) {
                    return false;
                }
            } else {
                has_positive = true;
                if self.matches_platform_filter(f) {
                    positive_matched = true;
                }
            }
        }

        if has_positive {
            positive_matched
        } else {
            true
        }
    }
}

impl FromStr for TargetTriple {
    type Err = CoreError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = s.split('-').collect();
        let arch = if !parts.is_empty() {
            Arch::from_str(parts[0])?
        } else {
            return Err(CoreError::TargetParse("Empty triple".into()));
        };

        let (vendor, os_str, env_str) = match parts.len() {
            1 => ("unknown".into(), "unknown", ""),
            2 => ("unknown".into(), parts[1], ""),
            3 => {
                if parts[1] == "apple" {
                    ("apple".into(), parts[2], "")
                } else if parts[1] == "linux" {
                    ("unknown".into(), parts[1], parts[2])
                } else {
                    (parts[1].into(), parts[2], "")
                }
            }
            4 => (parts[1].into(), parts[2], parts[3]),
            _ => (parts[1].into(), parts[2], parts[3]),
        };

        let mut env = Environment::from_str(env_str)?;
        let os_lower = os_str.to_lowercase();
        let mut os = if env == Environment::AndroidEabi || s.to_lowercase().contains("android") {
            Os::Android
        } else {
            Os::from_str(os_str).map_err(|_| {
                CoreError::TargetParse(format!(
                    "Unknown OS '{os_str}' in target triple '{s}' (supported: windows, linux, darwin/macos, ios, android, emscripten)"
                ))
            })?
        };
        if os_lower.starts_with("mingw") {
            os = Os::Windows;
            env = Environment::Gnu;
        }

        Ok(Self {
            raw: s.to_string(),
            arch,
            vendor,
            os,
            env,
        })
    }
}

impl fmt::Display for TargetTriple {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.raw)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
pub enum BuildType {
    Debug,
    #[default]
    Release,
    RelWithDebInfo,
    MinSizeRel,
}

impl FromStr for BuildType {
    type Err = CoreError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let lower = s.to_lowercase();
        match lower.as_str() {
            "debug" => Ok(BuildType::Debug),
            "release" => Ok(BuildType::Release),
            "relwithdebinfo" | "relwithdeb" => Ok(BuildType::RelWithDebInfo),
            "minsizerel" | "minsize" => Ok(BuildType::MinSizeRel),
            _ => Err(CoreError::Manifest(format!("Unknown build type: {s}"))),
        }
    }
}

impl fmt::Display for BuildType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BuildType::Debug => write!(f, "Debug"),
            BuildType::Release => write!(f, "Release"),
            BuildType::RelWithDebInfo => write!(f, "RelWithDebInfo"),
            BuildType::MinSizeRel => write!(f, "MinSizeRel"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum VCRuntime {
    #[default]
    #[serde(rename = "MultiThreadedDLL", alias = "dynamic")]
    MultiThreadedDLL,
    #[serde(rename = "MultiThreadedDebugDLL", alias = "dynamic_debug")]
    MultiThreadedDebugDLL,
    #[serde(rename = "MultiThreaded", alias = "static")]
    MultiThreaded,
    #[serde(rename = "MultiThreadedDebug", alias = "static_debug")]
    MultiThreadedDebug,
}

impl FromStr for VCRuntime {
    type Err = CoreError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "MultiThreadedDLL" | "dynamic" | "/MD" => Ok(VCRuntime::MultiThreadedDLL),
            "MultiThreadedDebugDLL" | "dynamic_debug" | "/MDd" => Ok(VCRuntime::MultiThreadedDebugDLL),
            "MultiThreaded" | "static" | "/MT" => Ok(VCRuntime::MultiThreaded),
            "MultiThreadedDebug" | "static_debug" | "/MTd" => Ok(VCRuntime::MultiThreadedDebug),
            other => Err(CoreError::Manifest(format!("Unknown MSVC runtime library: {other}"))),
        }
    }
}

impl VCRuntime {
    pub fn as_cmake_value(&self) -> &'static str {
        match self {
            VCRuntime::MultiThreadedDLL => "MultiThreadedDLL",
            VCRuntime::MultiThreadedDebugDLL => "MultiThreadedDebugDLL",
            VCRuntime::MultiThreaded => "MultiThreaded",
            VCRuntime::MultiThreadedDebug => "MultiThreadedDebug",
        }
    }

    pub fn is_static(&self) -> bool {
        matches!(self, VCRuntime::MultiThreaded | VCRuntime::MultiThreadedDebug)
    }

    pub fn is_debug(&self) -> bool {
        matches!(self, VCRuntime::MultiThreadedDebugDLL | VCRuntime::MultiThreadedDebug)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CxxStdlib {
    #[default]
    #[serde(alias = "default")]
    PlatformDefault,
    #[serde(alias = "libc++")]
    Libcxx,
    #[serde(alias = "libstdc++")]
    Libstdcxx,
    Custom,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> TargetTriple {
        s.parse().unwrap()
    }

    #[test]
    fn parses_common_triples() {
        let w = t("x86_64-pc-windows-msvc");
        assert_eq!((w.arch, w.os, w.env), (Arch::X86_64, Os::Windows, Environment::Msvc));

        let l = t("aarch64-unknown-linux-gnu");
        assert_eq!((l.arch, l.os, l.env), (Arch::Arm64, Os::Linux, Environment::Gnu));

        let a = t("aarch64-linux-android");
        assert_eq!((a.os, a.env), (Os::Android, Environment::AndroidEabi));

        let a7 = t("armv7a-linux-androideabi");
        assert_eq!((a7.arch, a7.os), (Arch::Arm, Os::Android));

        let m = t("aarch64-apple-darwin");
        assert_eq!((m.os, m.vendor.as_str()), (Os::Macos, "apple"));

        let e = t("wasm32-unknown-emscripten");
        assert_eq!(e.os, Os::Emscripten);
    }

    #[test]
    fn parses_mingw_triples_as_windows_gnu() {
        for s in ["x86_64-w64-mingw32", "i686-w64-mingw32", "x86_64-pc-windows-gnu"] {
            let tr = t(s);
            assert_eq!(tr.os, Os::Windows, "{s}");
            assert_eq!(tr.env, Environment::Gnu, "{s}");
        }
    }

    #[test]
    fn rejects_unknown_os() {
        assert!("x86_64-pc-widnows-msvc".parse::<TargetTriple>().is_err());
        assert!("x86_64".parse::<TargetTriple>().is_err());
        assert!("sparc-unknown-linux-gnu".parse::<TargetTriple>().is_err());
    }

    #[test]
    fn platform_filters() {
        let w = t("x86_64-pc-windows-msvc");
        assert!(w.matches_all_filters(&["windows".into()]));
        assert!(!w.matches_all_filters(&["linux".into(), "android".into()]));
        assert!(!w.matches_all_filters(&["!windows".into()]));
        assert!(w.matches_all_filters(&["msvc".into()]));
        assert!(w.matches_all_filters(&[]));
    }

    #[test]
    fn cxx_stdlib_aliases() {
        #[derive(Deserialize)]
        struct W {
            v: CxxStdlib,
        }
        let p = |s: &str| toml::from_str::<W>(&format!("v = \"{s}\"")).unwrap().v;
        assert_eq!(p("libc++"), CxxStdlib::Libcxx);
        assert_eq!(p("libstdc++"), CxxStdlib::Libstdcxx);
        assert_eq!(p("libcxx"), CxxStdlib::Libcxx);
        assert_eq!(p("default"), CxxStdlib::PlatformDefault);
    }
}
