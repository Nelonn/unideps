use crate::recipe::Recipe;
use anyhow::{Context, Result};
use mlua::{Lua, LuaOptions, StdLib, Table, Value};
use std::path::{Path, PathBuf};

/// Recipes may come from third parties, so they run without `os`, `io`, `require`,
/// `dofile` or `loadfile` and cannot touch the filesystem or spawn processes.
pub struct LuaRecipeEngine {
    lua: Lua,
}

impl LuaRecipeEngine {
    pub fn new() -> Result<Self> {
        let lua = Lua::new_with(
            StdLib::TABLE | StdLib::STRING | StdLib::MATH | StdLib::UTF8,
            LuaOptions::default(),
        )?;
        let globals = lua.globals();
        for name in ["dofile", "loadfile", "load", "require", "collectgarbage"] {
            globals.set(name, Value::Nil)?;
        }
        Ok(Self { lua })
    }

    pub fn load_recipe_from_str(&self, code: &str, requested_version: Option<&str>) -> Result<Recipe> {
        self.lua.load(code).exec()?;
        let pkg_table: Table = self
            .lua
            .globals()
            .get::<Option<Table>>("package")?
            .context("recipe must define a global `package` table")?;

        let name: String = pkg_table.get("name").context("recipe `package.name` must be a string")?;
        let latest: Option<String> = pkg_table.get("latest")?;
        let version: Option<String> = match requested_version {
            Some(v) => Some(v.to_string()),
            None => pkg_table.get::<Option<String>>("version")?.or_else(|| latest.clone()),
        };

        let mut recipe = Recipe {
            name,
            version: version.clone(),
            latest,
            strategy: pkg_table.get("strategy")?,
            abi_ignores: pkg_table.get::<Option<Vec<String>>>("abi_ignores")?.unwrap_or_default(),
            import_name: pkg_table.get("import_name")?,
            ..Default::default()
        };

        let source: Option<Table> = match pkg_table.get::<Value>("source")? {
            Value::Function(f) => Some(f.call(version.clone().unwrap_or_default())?),
            Value::Table(t) => Some(t),
            Value::Nil => None,
            other => anyhow::bail!("recipe `package.source` must be a table or function, got {}", other.type_name()),
        };
        if let Some(src) = source {
            recipe.source.git = src.get("git")?;
            recipe.source.tag = src.get("tag")?;
            recipe.source.branch = src.get("branch")?;
            recipe.source.commit = src.get("commit")?;
            recipe.source.url = src.get("url")?;
            recipe.source.hash = src.get("hash")?;
            recipe.source.path = src.get::<Option<String>>("path")?.map(PathBuf::from);
        }

        recipe.dependencies = self.name_list(&pkg_table, "dependencies")?;
        recipe.tools = self.name_list(&pkg_table, "tools")?;

        recipe.include_dirs = Self::path_list(&pkg_table, "include_dirs", "includes")?;
        recipe.libraries = Self::path_list(&pkg_table, "libraries", "lib")?;
        recipe.binaries = Self::path_list(&pkg_table, "binaries", "bin")?;
        recipe.patches = Self::path_list(&pkg_table, "patches", "patch")?;

        if let Some(opts) = pkg_table.get::<Option<Table>>("options")? {
            for pair in opts.pairs::<String, Value>() {
                let (k, v) = pair?;
                let tv = match v {
                    Value::Boolean(b) => toml::Value::Boolean(b),
                    Value::Integer(i) => toml::Value::Integer(i),
                    Value::Number(n) => toml::Value::Float(n),
                    Value::String(s) => toml::Value::String(s.to_str()?.to_string()),
                    other => anyhow::bail!("recipe option '{k}' has unsupported type {}", other.type_name()),
                };
                recipe.options.insert(k, tv);
            }
        }

        Ok(recipe)
    }

    /// Accepts either a list or a function returning a list; entries are
    /// strings or tables with a `name` field.
    fn name_list(&self, pkg: &Table, key: &str) -> Result<Vec<String>> {
        let list: Option<Table> = match pkg.get::<Value>(key)? {
            Value::Function(f) => Some(f.call(self.lua.create_table()?)?),
            Value::Table(t) => Some(t),
            Value::Nil => None,
            other => anyhow::bail!("recipe `package.{key}` must be a list or function, got {}", other.type_name()),
        };
        let mut out = Vec::new();
        if let Some(list) = list {
            for val in list.sequence_values::<Value>() {
                match val? {
                    Value::String(s) => out.push(s.to_str()?.to_string()),
                    Value::Table(t) => out.push(t.get::<String>("name").with_context(|| format!("entry in `{key}` needs a `name`"))?),
                    other => anyhow::bail!("unsupported entry type {} in `package.{key}`", other.type_name()),
                }
            }
        }
        Ok(out)
    }

    fn path_list(table: &Table, key: &str, alt_key: &str) -> Result<Vec<PathBuf>> {
        let mut value = table.get::<Value>(key)?;
        if value.is_nil() {
            value = table.get::<Value>(alt_key)?;
        }
        match value {
            Value::Nil => Ok(Vec::new()),
            Value::String(s) => Ok(vec![PathBuf::from(s.to_str()?.to_string())]),
            Value::Table(t) => t
                .sequence_values::<String>()
                .map(|v| Ok(PathBuf::from(v?)))
                .collect(),
            other => anyhow::bail!("recipe `package.{key}` must be a string or list, got {}", other.type_name()),
        }
    }

    pub fn load_recipe_from_file(&self, path: impl AsRef<Path>, requested_version: Option<&str>) -> Result<Recipe> {
        let path_ref = path.as_ref();
        let content = std::fs::read_to_string(path_ref)
            .with_context(|| format!("Failed to read recipe file: {}", path_ref.display()))?;
        self.load_recipe_from_str(&content, requested_version)
            .with_context(|| format!("Failed to evaluate recipe {}", path_ref.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_full_recipe() {
        let code = r#"
package = {
    name = "ogg",
    latest = "1.3.5",
    import_name = "Ogg",
    source = function(v) return { git = "https://github.com/xiph/ogg.git", tag = "v" .. v } end,
    dependencies = { "zlib", { name = "png" } },
    tools = function(opts) return { "nasm" } end,
    libraries = "lib/ogg",
    patches = { "fix.patch" },
    options = { BUILD_TESTING = false, LEVEL = 3 },
}
"#;
        let engine = LuaRecipeEngine::new().unwrap();
        let r = engine.load_recipe_from_str(code, None).unwrap();
        assert_eq!(r.name, "ogg");
        assert_eq!(r.version.as_deref(), Some("1.3.5"));
        assert_eq!(r.source.tag.as_deref(), Some("v1.3.5"));
        assert_eq!(r.dependencies, vec!["zlib", "png"]);
        assert_eq!(r.tools, vec!["nasm"]);
        assert_eq!(r.libraries, vec![PathBuf::from("lib/ogg")]);
        assert_eq!(r.patches, vec![PathBuf::from("fix.patch")]);
        assert_eq!(r.options["BUILD_TESTING"], toml::Value::Boolean(false));
        assert_eq!(r.options["LEVEL"], toml::Value::Integer(3));
    }

    #[test]
    fn sandbox_blocks_os_and_io() {
        let engine = LuaRecipeEngine::new().unwrap();
        assert!(engine.load_recipe_from_str("os.execute('echo hi')", None).is_err());
        let engine = LuaRecipeEngine::new().unwrap();
        assert!(engine.load_recipe_from_str("io.open('x', 'w')", None).is_err());
        let engine = LuaRecipeEngine::new().unwrap();
        assert!(engine.load_recipe_from_str("require('os')", None).is_err());
    }

    #[test]
    fn missing_package_is_an_error() {
        let engine = LuaRecipeEngine::new().unwrap();
        let err = engine.load_recipe_from_str("x = 1", None).unwrap_err();
        assert!(err.to_string().contains("package"));
    }
}
