use crate::manifest::{StrategyRule, StrategyScope};
use crate::target::{BuildType, CxxStdlib, VCRuntime};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectiveConfig {
    pub build_type: BuildType,
    pub shared: bool,
    pub vc_runtime: VCRuntime,
    /// C++ standard (`"17"`, `"c++20"`, `"gnu++17"`). Empty means "let the package decide".
    pub cxx_std: String,
    pub cxx_stdlib: CxxStdlib,
    pub lto: bool,
    pub flags: Vec<String>,
    pub cmake_options: BTreeMap<String, String>,
}

impl Default for EffectiveConfig {
    fn default() -> Self {
        Self {
            build_type: BuildType::Release,
            shared: false,
            vc_runtime: VCRuntime::MultiThreadedDLL,
            cxx_std: String::new(),
            cxx_stdlib: CxxStdlib::PlatformDefault,
            lto: false,
            flags: Vec::new(),
            cmake_options: BTreeMap::new(),
        }
    }
}

impl EffectiveConfig {
    pub fn apply_rule(&mut self, rule: &StrategyRule) {
        if let Some(bt) = rule.build_type {
            self.build_type = bt;
        }
        if let Some(sh) = rule.shared {
            self.shared = sh;
        }
        if let Some(rt) = rule.vc_runtime {
            self.vc_runtime = rt;
        }
        if let Some(ref std) = rule.cxx_std {
            self.cxx_std = std.clone();
        }
        if let Some(sl) = rule.cxx_stdlib {
            self.cxx_stdlib = sl;
        }
        if let Some(lto) = rule.lto {
            self.lto = lto;
        }
        self.add_flags(&rule.flags);
        for (k, v) in &rule.cmake_options {
            self.cmake_options.insert(k.clone(), v.clone());
        }
    }

    pub fn add_flags(&mut self, flags: &[String]) {
        for flag in flags {
            if !self.flags.contains(flag) {
                self.flags.push(flag.clone());
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct StrategyEngine {
    pub global_rules: Vec<StrategyRule>,
    pub cascade_rules: BTreeMap<String, Vec<StrategyRule>>,
    pub exact_rules: BTreeMap<String, Vec<StrategyRule>>,
}

impl StrategyEngine {
    pub fn new(rules: &[StrategyRule]) -> Self {
        let mut engine = Self::default();
        for r in rules {
            match r.scope {
                StrategyScope::Global => {
                    engine.global_rules.push(r.clone());
                }
                StrategyScope::Cascade => {
                    if let Some(ref pkg) = r.package {
                        engine.cascade_rules.entry(pkg.clone()).or_default().push(r.clone());
                    }
                }
                StrategyScope::Exact => {
                    if let Some(ref pkg) = r.package {
                        engine.exact_rules.entry(pkg.clone()).or_default().push(r.clone());
                    }
                }
            }
        }
        engine
    }

    /// Rules without a `package` are only meaningful for the `global` scope.
    pub fn validate(rules: &[StrategyRule]) -> Result<(), String> {
        for r in rules {
            if r.scope != StrategyScope::Global && r.package.is_none() {
                return Err(format!(
                    "[[strategy]] rule with scope = \"{:?}\" requires a `package` field",
                    r.scope
                )
                .to_lowercase());
            }
        }
        Ok(())
    }

    /// `dependents` lists every package that (transitively) depends on `package_name`;
    /// their `cascade` rules apply to it. Precedence: global < cascade from dependents
    /// < own cascade < exact.
    pub fn resolve_for_node(
        &self,
        package_name: &str,
        dependents: &[&str],
        base: &EffectiveConfig,
    ) -> EffectiveConfig {
        let mut resolved = base.clone();

        for g in &self.global_rules {
            resolved.apply_rule(g);
        }

        for dependent in dependents {
            if let Some(cascades) = self.cascade_rules.get(*dependent) {
                for c in cascades {
                    resolved.apply_rule(c);
                }
            }
        }

        if let Some(cascades) = self.cascade_rules.get(package_name) {
            for c in cascades {
                resolved.apply_rule(c);
            }
        }

        if let Some(exacts) = self.exact_rules.get(package_name) {
            for e in exacts {
                resolved.apply_rule(e);
            }
        }

        resolved
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(package: Option<&str>, scope: StrategyScope, shared: bool) -> StrategyRule {
        StrategyRule {
            package: package.map(Into::into),
            scope,
            shared: Some(shared),
            ..Default::default()
        }
    }

    #[test]
    fn cascade_applies_to_transitive_dependencies() {
        let engine = StrategyEngine::new(&[rule(Some("app_lib"), StrategyScope::Cascade, true)]);
        let base = EffectiveConfig::default();

        assert!(engine.resolve_for_node("zlib", &["png", "app_lib"], &base).shared);
        assert!(engine.resolve_for_node("app_lib", &[], &base).shared);
        assert!(!engine.resolve_for_node("zlib", &["other"], &base).shared);
    }

    #[test]
    fn exact_beats_cascade_beats_global() {
        let engine = StrategyEngine::new(&[
            rule(None, StrategyScope::Global, true),
            rule(Some("parent"), StrategyScope::Cascade, false),
            rule(Some("child"), StrategyScope::Exact, true),
        ]);
        let base = EffectiveConfig::default();
        assert!(engine.resolve_for_node("unrelated", &[], &base).shared);
        assert!(!engine.resolve_for_node("other_child", &["parent"], &base).shared);
        assert!(engine.resolve_for_node("child", &["parent"], &base).shared);
    }

    #[test]
    fn validate_rejects_packageless_scoped_rules() {
        assert!(StrategyEngine::validate(&[rule(None, StrategyScope::Exact, true)]).is_err());
        assert!(StrategyEngine::validate(&[rule(None, StrategyScope::Global, true)]).is_ok());
    }
}
