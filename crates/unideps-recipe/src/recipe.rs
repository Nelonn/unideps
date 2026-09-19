use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use unideps_core::manifest::{deserialize_path_or_vec, deserialize_string_or_vec};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Recipe {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub latest: Option<String>,
    #[serde(default)]
    pub strategy: Option<String>,
    #[serde(default)]
    pub source: RecipeSource,
    #[serde(default)]
    pub options: BTreeMap<String, toml::Value>,
    #[serde(default, deserialize_with = "deserialize_string_or_vec")]
    pub abi_ignores: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_string_or_vec")]
    pub dependencies: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_string_or_vec")]
    pub tools: Vec<String>,
    #[serde(default)]
    pub import_name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_path_or_vec")]
    pub include_dirs: Vec<PathBuf>,
    #[serde(default, deserialize_with = "deserialize_path_or_vec")]
    pub libraries: Vec<PathBuf>,
    #[serde(default, deserialize_with = "deserialize_path_or_vec")]
    pub binaries: Vec<PathBuf>,
    #[serde(default, deserialize_with = "deserialize_path_or_vec")]
    pub patches: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RecipeSource {
    #[serde(default)]
    pub git: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub commit: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub hash: Option<String>,
    #[serde(default)]
    pub path: Option<PathBuf>,
}

impl Recipe {
    pub fn from_toml_file(path: &std::path::Path) -> anyhow::Result<Self> {
        use anyhow::Context;
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read recipe file: {}", path.display()))?;
        toml::from_str(&content).with_context(|| format!("Invalid recipe {}", path.display()))
    }

    /// Recipe `options` as CMake definitions (`true` -> `ON`).
    pub fn cmake_options(&self) -> BTreeMap<String, String> {
        self.options
            .iter()
            .map(|(k, v)| {
                let s = match v {
                    toml::Value::Boolean(true) => "ON".to_string(),
                    toml::Value::Boolean(false) => "OFF".to_string(),
                    toml::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                (k.clone(), s)
            })
            .collect()
    }
}
