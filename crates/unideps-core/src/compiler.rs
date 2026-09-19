use std::fmt;
use std::path::Path;
use std::str::FromStr;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CompilerType {
    Clang,
    Msvc,
    Gcc,
    Custom(String),
}

impl CompilerType {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Clang => "clang",
            Self::Msvc => "msvc",
            Self::Gcc => "gcc",
            Self::Custom(s) => s.as_str(),
        }
    }

    pub fn detect_from_path(path: impl AsRef<Path>) -> Self {
        let p = path.as_ref();
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_lowercase();
        if stem.contains("clang") {
            Self::Clang
        } else if stem == "cl" || stem.starts_with("cl-") || stem.contains("msvc") {
            Self::Msvc
        } else if stem.contains("gcc") || stem.contains("g++") || stem.contains("mingw") {
            Self::Gcc
        } else if !stem.is_empty() {
            Self::Custom(stem)
        } else {
            Self::Custom(p.to_string_lossy().to_string())
        }
    }
}

impl fmt::Display for CompilerType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromStr for CompilerType {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let norm = s.trim().to_lowercase();
        match norm.as_str() {
            "clang" | "appleclang" | "apple-clang" | "clang-cl" => Ok(Self::Clang),
            "msvc" | "cl" => Ok(Self::Msvc),
            "gcc" | "gnu" | "g++" | "mingw" => Ok(Self::Gcc),
            _ => Ok(Self::Custom(norm)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compiler_type_from_str() {
        assert_eq!("clang".parse::<CompilerType>().unwrap(), CompilerType::Clang);
        assert_eq!("Clang".parse::<CompilerType>().unwrap(), CompilerType::Clang);
        assert_eq!("msvc".parse::<CompilerType>().unwrap(), CompilerType::Msvc);
        assert_eq!("MSVC".parse::<CompilerType>().unwrap(), CompilerType::Msvc);
        assert_eq!("gcc".parse::<CompilerType>().unwrap(), CompilerType::Gcc);
        assert_eq!("GNU".parse::<CompilerType>().unwrap(), CompilerType::Gcc);
    }

    #[test]
    fn test_compiler_type_detect_from_path() {
        assert_eq!(CompilerType::detect_from_path("C:/Program Files/LLVM/bin/clang.exe"), CompilerType::Clang);
        assert_eq!(CompilerType::detect_from_path("clang++"), CompilerType::Clang);
        assert_eq!(CompilerType::detect_from_path("C:/VC/bin/cl.exe"), CompilerType::Msvc);
        assert_eq!(CompilerType::detect_from_path("/usr/bin/x86_64-linux-gnu-gcc-11"), CompilerType::Gcc);
    }
}
