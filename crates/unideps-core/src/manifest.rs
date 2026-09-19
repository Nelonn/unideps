use crate::error::{CoreError, CoreResult};
use crate::target::{BuildType, CxxStdlib, VCRuntime};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Manifest {
    #[serde(default)]
    pub package: Option<PackageSection>,
    #[serde(default)]
    pub project: Option<ProjectSection>,
    #[serde(default)]
    pub presets: BTreeMap<String, Preset>,
    #[serde(default)]
    pub registries: BTreeMap<String, RegistryConfig>,
    #[serde(default)]
    pub tools: BTreeMap<String, ToolRequirement>,
    #[serde(default)]
    pub dependencies: BTreeMap<String, DependencySpec>,
    #[serde(default)]
    pub strategy: Vec<StrategyRule>,
    #[serde(default)]
    pub overrides: BTreeMap<String, DependencyDetails>,
    #[serde(default)]
    pub targets: BTreeMap<String, TargetSysrootConfig>,
}

impl Manifest {
    pub fn from_file(path: impl AsRef<Path>) -> CoreResult<Self> {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path)?;
        content
            .parse()
            .map_err(|e: CoreError| CoreError::Manifest(format!("{}: {e}", path.display())))
    }
}

impl std::str::FromStr for Manifest {
    type Err = CoreError;

    fn from_str(s: &str) -> CoreResult<Self> {
        Ok(toml::from_str(s)?)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PackageSection {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub latest: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectSection {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Preset {
    #[serde(default)]
    pub build_type: Option<BuildType>,
    #[serde(default, alias = "cxx_runtime")]
    pub vc_runtime: Option<VCRuntime>,
    #[serde(default)]
    pub cxx_std: Option<String>,
    #[serde(default)]
    pub cxx_stdlib: Option<CxxStdlib>,
    #[serde(default)]
    pub cxx_stdlib_package: Option<String>,
    #[serde(default)]
    pub lto: Option<LtoSetting>,
    #[serde(default, deserialize_with = "deserialize_string_or_vec")]
    pub flags: Vec<String>,
}

/// `lto = true`, `lto = false` or `lto = "thin" | "full" | "on" | "off"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LtoSetting {
    Bool(bool),
    Mode(String),
}

impl LtoSetting {
    pub fn enabled(&self) -> CoreResult<bool> {
        match self {
            LtoSetting::Bool(b) => Ok(*b),
            LtoSetting::Mode(m) => match m.to_lowercase().as_str() {
                "on" | "true" | "thin" | "full" | "fat" => Ok(true),
                "off" | "false" | "none" => Ok(false),
                other => Err(CoreError::Manifest(format!("Unknown lto mode: {other}"))),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryConfig {
    pub git: String,
    #[serde(default = "default_branch")]
    pub branch: String,
}

fn default_branch() -> String {
    "main".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolRequirement {
    Version(String),
    Detailed(ToolDetails),
}

impl ToolRequirement {
    pub fn to_details(&self) -> ToolDetails {
        match self {
            ToolRequirement::Version(v) => ToolDetails {
                version: Some(v.clone()),
                ..Default::default()
            },
            ToolRequirement::Detailed(d) => d.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolDetails {
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub git: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub commit: Option<String>,
    #[serde(default)]
    pub path: Option<PathBuf>,
    #[serde(default)]
    pub strategy: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
#[allow(clippy::large_enum_variant)]
pub enum DependencySpec {
    Version(String),
    Detailed(DependencyDetails),
}

impl DependencySpec {
    pub fn to_details(&self) -> DependencyDetails {
        match self {
            DependencySpec::Version(v) => DependencyDetails {
                version: Some(v.clone()),
                ..Default::default()
            },
            DependencySpec::Detailed(d) => d.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DependencyDetails {
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub git: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub commit: Option<String>,
    #[serde(default)]
    pub path: Option<PathBuf>,
    #[serde(default)]
    pub shallow: Option<bool>,
    #[serde(default)]
    pub strategy: Option<String>,
    #[serde(default)]
    pub shared: Option<bool>,
    #[serde(default)]
    pub header_only: Option<bool>,
    #[serde(default)]
    pub cmake_options: BTreeMap<String, String>,
    #[serde(default, deserialize_with = "deserialize_string_or_vec")]
    pub abi_ignores: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_string_or_vec")]
    pub platforms: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_subdep_or_vec")]
    pub dependencies: Vec<SubDependency>,
    #[serde(default, deserialize_with = "deserialize_string_or_vec")]
    pub tools: Vec<String>,
    #[serde(default)]
    pub recipe: Option<PathBuf>,
    #[serde(default)]
    pub import_name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_path_or_vec")]
    pub include_dirs: Vec<PathBuf>,
    #[serde(default, deserialize_with = "deserialize_path_or_vec")]
    pub libraries: Vec<PathBuf>,
    #[serde(default, deserialize_with = "deserialize_path_or_vec")]
    pub binaries: Vec<PathBuf>,
    #[serde(default)]
    pub enabled_if: Option<String>,
    #[serde(default)]
    pub auto_import: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_path_or_vec")]
    pub patches: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SubDependency {
    Simple(String),
    Detailed {
        name: String,
        #[serde(default)]
        version: Option<String>,
        #[serde(default)]
        options: BTreeMap<String, toml::Value>,
    },
}

impl SubDependency {
    pub fn name(&self) -> &str {
        match self {
            SubDependency::Simple(s) => s,
            SubDependency::Detailed { name, .. } => name,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum StrategyScope {
    #[default]
    Exact,
    Cascade,
    Global,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StrategyRule {
    #[serde(default)]
    pub package: Option<String>,
    #[serde(default)]
    pub scope: StrategyScope,
    #[serde(default)]
    pub build_type: Option<BuildType>,
    #[serde(default)]
    pub shared: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_string_or_vec")]
    pub flags: Vec<String>,
    #[serde(default, alias = "cxx_runtime")]
    pub vc_runtime: Option<VCRuntime>,
    #[serde(default)]
    pub cxx_std: Option<String>,
    #[serde(default)]
    pub cxx_stdlib: Option<CxxStdlib>,
    #[serde(default)]
    pub lto: Option<bool>,
    #[serde(default)]
    pub cmake_options: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TargetSysrootConfig {
    #[serde(default)]
    pub sysroot: Option<PathBuf>,
    #[serde(default)]
    pub toolchain_file: Option<PathBuf>,
    #[serde(default)]
    pub c_compiler: Option<PathBuf>,
    #[serde(default)]
    pub cxx_compiler: Option<PathBuf>,
    #[serde(default)]
    pub asm_compiler: Option<PathBuf>,
    #[serde(default)]
    pub compiler: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LocalConfig {
    #[serde(default)]
    pub storage: Option<LocalStorageConfig>,
    #[serde(default)]
    pub resources: Option<LocalResourcesConfig>,
    #[serde(default)]
    pub cache: Option<LocalCacheConfig>,
    #[serde(default)]
    pub tools: BTreeMap<String, PathBuf>,
    #[serde(default)]
    pub targets: BTreeMap<String, TargetSysrootConfig>,
    #[serde(default)]
    pub overrides: BTreeMap<String, DependencyDetails>,
}

impl LocalConfig {
    pub fn from_file(path: impl AsRef<Path>) -> CoreResult<Self> {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path)?;
        toml::from_str(&content)
            .map_err(|e| CoreError::Manifest(format!("{}: {e}", path.display())))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LocalStorageConfig {
    #[serde(default)]
    pub base_dir: Option<PathBuf>,
    #[serde(default)]
    pub scratch_mode: Option<String>,
    #[serde(default)]
    pub scratch_dir: Option<PathBuf>,
    #[serde(default)]
    pub keep_build_dirs: Option<bool>,
    #[serde(default)]
    pub cache_sources: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LocalResourcesConfig {
    #[serde(default)]
    pub max_jobs: Option<usize>,
    #[serde(default)]
    pub max_memory_gb: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LocalCacheConfig {
    #[serde(default)]
    pub remote_url: Option<String>,
    #[serde(default)]
    pub auth_token: Option<String>,
    #[serde(default)]
    pub upload: Option<bool>,
    #[serde(default)]
    pub local_dir: Option<PathBuf>,
}

pub fn deserialize_path_or_vec<'de, D>(deserializer: D) -> Result<Vec<PathBuf>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct PathOrVec;

    impl<'de> serde::de::Visitor<'de> for PathOrVec {
        type Value = Vec<PathBuf>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a path string or a list of path strings")
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(vec![PathBuf::from(value)])
        }

        fn visit_seq<S>(self, mut seq: S) -> Result<Self::Value, S::Error>
        where
            S: serde::de::SeqAccess<'de>,
        {
            let mut vec = Vec::new();
            while let Some(elem) = seq.next_element::<PathBuf>()? {
                vec.push(elem);
            }
            Ok(vec)
        }
    }

    deserializer.deserialize_any(PathOrVec)
}

pub fn deserialize_string_or_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct StringOrVec;

    impl<'de> serde::de::Visitor<'de> for StringOrVec {
        type Value = Vec<String>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a string or a list of strings")
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(vec![value.to_string()])
        }

        fn visit_seq<S>(self, mut seq: S) -> Result<Self::Value, S::Error>
        where
            S: serde::de::SeqAccess<'de>,
        {
            let mut vec = Vec::new();
            while let Some(elem) = seq.next_element::<String>()? {
                vec.push(elem);
            }
            Ok(vec)
        }
    }

    deserializer.deserialize_any(StringOrVec)
}

pub fn deserialize_subdep_or_vec<'de, D>(deserializer: D) -> Result<Vec<SubDependency>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct SubDepOrVec;

    impl<'de> serde::de::Visitor<'de> for SubDepOrVec {
        type Value = Vec<SubDependency>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a sub-dependency string, table, or a list of them")
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(vec![SubDependency::Simple(value.to_string())])
        }

        fn visit_map<M>(self, map: M) -> Result<Self::Value, M::Error>
        where
            M: serde::de::MapAccess<'de>,
        {
            let dep = SubDependency::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
            Ok(vec![dep])
        }

        fn visit_seq<S>(self, mut seq: S) -> Result<Self::Value, S::Error>
        where
            S: serde::de::SeqAccess<'de>,
        {
            let mut vec = Vec::new();
            while let Some(elem) = seq.next_element::<SubDependency>()? {
                vec.push(elem);
            }
            Ok(vec)
        }
    }

    deserializer.deserialize_any(SubDepOrVec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_manifest_string_libraries_and_auto_import() {
        let toml_str = r#"
[dependencies.libyuv]
git = "https://chromium.googlesource.com/libyuv/libyuv"
commit = "2dd4257364d39c38d79465c4ddc4b93137fe729b"
strategy = "cmake-install"
auto_import = true
libraries = "lib/yuv"
enabled_if = "OPENMEDIA_EXAMPLES"
"#;
        let manifest = toml_str.parse::<Manifest>().unwrap();
        let dep = manifest.dependencies.get("libyuv").unwrap();
        let details = dep.to_details();

        assert_eq!(details.auto_import, Some(true));
        assert_eq!(details.libraries, vec![PathBuf::from("lib/yuv")]);
        assert_eq!(details.strategy.as_deref(), Some("cmake-install"));
        assert_eq!(details.enabled_if.as_deref(), Some("OPENMEDIA_EXAMPLES"));
    }

    #[test]
    fn test_manifest_empty_table_dep() {
        let toml_str = r#"
[dependencies.empty]
"#;
        let manifest = toml_str.parse::<Manifest>().unwrap();
        let dep = manifest.dependencies.get("empty").unwrap();
        let details = dep.to_details();
        assert_eq!(details.auto_import, None);
    }

    #[test]
    fn test_preset_lto_and_stdlib() {
        let toml_str = r#"
[presets.rel]
build_type = "Release"
cxx_stdlib = "libc++"
lto = "thin"

[presets.dbg]
lto = false
"#;
        let manifest = toml_str.parse::<Manifest>().unwrap();
        let rel = &manifest.presets["rel"];
        assert_eq!(rel.cxx_stdlib, Some(CxxStdlib::Libcxx));
        assert!(rel.lto.as_ref().unwrap().enabled().unwrap());
        assert!(!manifest.presets["dbg"].lto.as_ref().unwrap().enabled().unwrap());
    }
}
