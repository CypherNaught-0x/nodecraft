//! Dependency layering rules (docs/architecture.md).
//!
//! The rule engine works on a small, metadata-independent model so it can be
//! unit-tested; `from_metadata` builds that model from `cargo metadata`.

use serde_json::Value;

/// Where a workspace crate sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Regular layered crate: may depend on crates in strictly lower layers.
    Layer(u8),
    /// Layer 0, and additionally may depend on no workspace crate at all.
    Standalone,
    /// Test tooling: may depend on anything below L5 (it sits at L5 for rule
    /// purposes); other crates may use it only as a dev-dependency.
    Testkit,
    /// Binaries and build tooling: exempt from the rules.
    Exempt,
}

impl Class {
    fn layer(self) -> Option<u8> {
        match self {
            Class::Layer(l) => Some(l),
            Class::Standalone => Some(0),
            Class::Testkit => Some(5),
            Class::Exempt => None,
        }
    }
}

/// The layering table (plan §4.1). Names are package names without the
/// `nodecraft-` prefix.
pub const TABLE: &[(&str, Class)] = &[
    ("types", Class::Layer(0)),
    ("media", Class::Standalone),
    ("plugin", Class::Standalone),
    ("mesh-preview", Class::Layer(1)),
    ("comfy", Class::Layer(1)),
    ("graph", Class::Layer(1)),
    ("plugin-store", Class::Layer(1)),
    ("nodes", Class::Layer(2)),
    ("catalog", Class::Layer(2)),
    ("actions", Class::Layer(2)),
    ("sync", Class::Layer(2)),
    ("exec", Class::Layer(3)),
    ("providers", Class::Layer(3)),
    ("gpu", Class::Layer(3)),
    ("format", Class::Layer(3)),
    ("engine", Class::Layer(4)),
    ("agent", Class::Layer(5)),
    ("automation", Class::Layer(5)),
    ("ui-egui", Class::Layer(5)),
    ("testkit", Class::Testkit),
    ("provider-testkit", Class::Testkit),
    // apps and tooling
    ("nodecraft", Class::Exempt),
    ("cli", Class::Exempt),
    ("web", Class::Exempt),
    ("xtask", Class::Exempt),
];

/// External crates that constitute a UI toolkit / windowing dependency.
/// Entries ending in `*` are prefixes.
pub const UI_CRATES: &[&str] = &["egui", "eframe", "winit", "egui_kittest", "egui_extras", "craft-ui", "craft-quit", "rfd"];

/// The only layered crates that may use UI crates: the egui shell. Apps are exempt anyway.
pub const UI_ALLOWED: &[&str] = &["ui-egui"];

/// The LLM client crate (docs/decisions/2026-11-06-llm-layer-rig.md) and its companions: only
/// nodecraft-providers may depend on them, and no other crate's source may name them.
pub const RIG_ALLOWED: &str = "providers";

fn is_rig_crate(name: &str) -> bool {
    name == "rig" || name.starts_with("rig-") || name.starts_with("rig_")
}

pub fn short_name(pkg: &str) -> &str {
    pkg.strip_prefix("nodecraft-").unwrap_or(pkg)
}

pub fn classify(pkg: &str) -> Option<Class> {
    let s = short_name(pkg);
    TABLE.iter().find(|(n, _)| *n == s).map(|(_, c)| *c)
}

fn is_ui_crate(name: &str) -> bool {
    UI_CRATES.iter().any(|p| match p.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => name == *p,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepKind {
    Normal,
    Dev,
    Build,
}

#[derive(Debug, Clone)]
pub struct Dep {
    pub name: String,
    pub kind: DepKind,
    /// `true` if the dependency is a workspace member.
    pub workspace: bool,
}

#[derive(Debug, Clone)]
pub struct Crate {
    pub name: String,
    pub deps: Vec<Dep>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Violation {
    Unregistered { krate: String },
    Upward { krate: String, dep: String, from: u8, to: u8, kind: DepKind },
    StandaloneHasWorkspaceDep { krate: String, dep: String },
    TestkitAsNormalDep { krate: String },
    UiOutsideShell { krate: String, dep: String },
    ProviderFacadeWorkspaceDep { dep: String, kind: DepKind },
    RigOutsideProviders { krate: String, dep: String },
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Violation::Unregistered { krate } => {
                write!(f, "{krate}: unknown workspace crate; register it in xtask/src/layers.rs TABLE (see docs/architecture.md)")
            }
            Violation::Upward { krate, dep, from, to, kind } => {
                write!(f, "{krate} (L{from}) -> {dep} (L{to}) [{kind:?}]: may only depend on strictly lower layers")
            }
            Violation::StandaloneHasWorkspaceDep { krate, dep } => {
                write!(f, "{krate}: standalone crate must not depend on workspace crate {dep}")
            }
            Violation::TestkitAsNormalDep { krate } => {
                write!(f, "{krate}: test tooling may only be a dev-dependency")
            }
            Violation::UiOutsideShell { krate, dep } => {
                write!(f, "{krate} depends on UI crate `{dep}`; UI toolkits are only allowed in ui-egui and the apps")
            }
            Violation::RigOutsideProviders { krate, dep } => {
                write!(f, "{krate} depends on `{dep}`; the LLM client crate stays inside nodecraft-providers")
            }
            Violation::ProviderFacadeWorkspaceDep { dep, kind } => {
                write!(f, "nodecraft-provider-testkit -> {dep} [{kind:?}]: workspace dependencies may only be nodecraft-providers or nodecraft-types")
            }
        }
    }
}

/// Check all rules. Returns violations sorted for stable output.
pub fn check(crates: &[Crate]) -> Vec<Violation> {
    let mut out = Vec::new();
    for c in crates {
        // Every crate, apps and tools included: rig-core types never leave the providers crate.
        if short_name(&c.name) != RIG_ALLOWED {
            for d in c.deps.iter().filter(|d| !d.workspace && is_rig_crate(&d.name)) {
                out.push(Violation::RigOutsideProviders { krate: c.name.clone(), dep: d.name.clone() });
            }
        }
        let Some(class) = classify(&c.name) else {
            out.push(Violation::Unregistered { krate: c.name.clone() });
            continue;
        };
        let Some(layer) = class.layer() else {
            continue; // exempt
        };
        for d in &c.deps {
            // This facade must never depend back on its consumers, including through a
            // self edge. Check every workspace dependency kind before the generic bypass.
            if short_name(&c.name) == "provider-testkit" && d.workspace && !matches!(d.name.as_str(), "nodecraft-providers" | "nodecraft-types") {
                out.push(Violation::ProviderFacadeWorkspaceDep { dep: d.name.clone(), kind: d.kind });
                continue;
            }
            // Preserve the existing generic same-package exemption, commonly used for
            // self dev-dependencies enabling test features. Facade workspace edges are above.
            if d.name == c.name {
                continue;
            }
            if !d.workspace {
                if is_ui_crate(&d.name) && !UI_ALLOWED.contains(&short_name(&c.name)) {
                    out.push(Violation::UiOutsideShell { krate: c.name.clone(), dep: d.name.clone() });
                }
                continue;
            }
            if class == Class::Standalone {
                out.push(Violation::StandaloneHasWorkspaceDep { krate: c.name.clone(), dep: d.name.clone() });
                continue;
            }
            match classify(&d.name) {
                // Unregistered deps are reported on their own entry.
                None => {}
                Some(Class::Testkit) => {
                    if d.kind != DepKind::Dev {
                        out.push(Violation::TestkitAsNormalDep { krate: c.name.clone() });
                    }
                }
                Some(dc) => {
                    let to = dc.layer().unwrap_or(u8::MAX);
                    if to >= layer {
                        out.push(Violation::Upward {
                            krate: c.name.clone(),
                            dep: d.name.clone(),
                            from: layer,
                            to: if to == u8::MAX { 6 } else { to },
                            kind: d.kind,
                        });
                    }
                }
            }
        }
    }
    out.sort_by_key(|v| v.to_string());
    out.dedup();
    out
}

/// Build the model from `cargo metadata --format-version 1 --no-deps`.
pub fn from_metadata(meta: &Value) -> Result<Vec<Crate>, String> {
    let pkgs = meta["packages"].as_array().ok_or("metadata: no packages array")?;
    let members: Vec<&str> = pkgs.iter().filter_map(|p| p["name"].as_str()).collect();
    let mut out = Vec::new();
    for p in pkgs {
        let name = p["name"].as_str().ok_or("package without name")?.to_owned();
        let mut deps = Vec::new();
        for d in p["dependencies"].as_array().into_iter().flatten() {
            let dname = d["name"].as_str().unwrap_or_default().to_owned();
            let kind = match d["kind"].as_str() {
                Some("dev") => DepKind::Dev,
                Some("build") => DepKind::Build,
                _ => DepKind::Normal,
            };
            let workspace = members.contains(&dname.as_str()) || d["path"].is_string();
            deps.push(Dep { name: dname, kind, workspace });
        }
        out.push(Crate { name, deps });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Every place outside nodecraft-providers whose source names the LLM client crate (`rig_core`,
/// `rig::`), and every public item of nodecraft-providers that does, as `path:line`, so none of
/// its types is used or exposed outside it.
pub fn rig_mentions(root: &std::path::Path) -> Result<Vec<String>, String> {
    // xtask itself is left out: it names the crate to look for it (its dependencies are checked).
    let mut dirs = Vec::new();
    for group in ["crates", "apps"] {
        for entry in std::fs::read_dir(root.join(group)).map_err(|e| format!("{group}: {e}"))? {
            let path = entry.map_err(|e| format!("{group}: {e}"))?.path();
            if path.join("Cargo.toml").is_file() && path.file_name().and_then(|n| n.to_str()) != Some(RIG_ALLOWED) {
                dirs.extend(["src", "tests", "benches", "examples"].map(|d| path.join(d)));
            }
        }
    }
    let mut found = Vec::new();
    scan(root, dirs, names_rig, &mut found)?;
    // Inside nodecraft-providers, no public item may name it: its types stay private there.
    let providers = root.join("crates").join(RIG_ALLOWED);
    scan(root, vec![providers.join("src")], |line| is_public(line) && names_rig(line), &mut found)?;
    found.sort();
    Ok(found)
}

/// Each line under `dirs` (Rust sources) that `hit` matches, as `path:line`.
fn scan(root: &std::path::Path, mut dirs: Vec<std::path::PathBuf>, hit: impl Fn(&str) -> bool, found: &mut Vec<String>) -> Result<(), String> {
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries {
            let path = entry.map_err(|e| format!("{}: {e}", dir.display()))?.path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                for (i, line) in text.lines().enumerate() {
                    if hit(line) {
                        let shown = path.strip_prefix(root).unwrap_or(&path).display().to_string().replace('\\', "/");
                        found.push(format!("{shown}:{}", i + 1));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Whether `line` declares a public item (`pub use`, `pub fn`, a `pub` field, …): plain `pub`,
/// not `pub(crate)`, `pub(super)` or `pub(in …)`.
fn is_public(line: &str) -> bool {
    let line = line.trim_start();
    line.strip_prefix("pub").is_some_and(|rest| rest.starts_with(char::is_whitespace))
}

/// Whether `line` names the crate as a path (`rig_core::`, `rig::`, `use rig_core`), not as part
/// of a longer identifier (`trig::`, `config_rig_core`).
fn names_rig(line: &str) -> bool {
    ["rig_core", "rig::"].iter().any(|needle| {
        line.match_indices(needle).any(|(at, _)| {
            let before = line.get(..at).and_then(|b| b.chars().next_back());
            !before.is_some_and(|c| c.is_alphanumeric() || c == '_')
        })
    })
}

pub fn describe(class: Option<Class>) -> String {
    match class {
        Some(Class::Layer(l)) => format!("L{l}"),
        Some(Class::Standalone) => "L0 standalone".into(),
        Some(Class::Testkit) => "testkit".into(),
        Some(Class::Exempt) => "exempt".into(),
        None => "UNREGISTERED".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(name: &str, deps: &[(&str, DepKind, bool)]) -> Crate {
        Crate { name: name.into(), deps: deps.iter().map(|(n, k, w)| Dep { name: (*n).into(), kind: *k, workspace: *w }).collect() }
    }
    use DepKind::*;

    #[test]
    fn clean_downward_graph_passes() {
        let g = [
            c("nodecraft-types", &[("serde", Normal, false)]),
            c("nodecraft-graph", &[("nodecraft-types", Normal, true), ("nodecraft-media", Normal, true)]),
            c("nodecraft-engine", &[("nodecraft-graph", Normal, true), ("nodecraft-testkit", Dev, true)]),
            c("nodecraft-ui-egui", &[("nodecraft-engine", Normal, true), ("egui", Normal, false), ("craft-ui", Normal, false)]),
            c("nodecraft-cli", &[("nodecraft-ui-egui", Normal, true)]),
        ];
        assert!(check(&g).is_empty(), "{:?}", check(&g));
    }

    #[test]
    fn upward_dependency_flagged() {
        let v = check(&[c("nodecraft-graph", &[("nodecraft-engine", Normal, true)])]);
        assert!(matches!(v[..], [Violation::Upward { from: 1, to: 4, .. }]));
    }

    #[test]
    fn sideways_dependency_flagged() {
        let v = check(&[c("nodecraft-exec", &[("nodecraft-providers", Normal, true)])]);
        assert!(matches!(v[..], [Violation::Upward { from: 3, to: 3, .. }]));
    }

    #[test]
    fn upward_dev_dependency_flagged() {
        let v = check(&[c("nodecraft-types", &[("nodecraft-graph", Dev, true)])]);
        assert!(matches!(v[..], [Violation::Upward { kind: Dev, .. }]));
    }

    #[test]
    fn self_dev_dependency_ignored() {
        assert!(check(&[c("nodecraft-media", &[("nodecraft-media", Dev, true)])]).is_empty());
    }

    #[test]
    fn standalone_media_has_no_workspace_deps() {
        let v = check(&[c("nodecraft-media", &[("nodecraft-types", Normal, true)])]);
        assert!(matches!(v[..], [Violation::StandaloneHasWorkspaceDep { .. }]));
        assert!(check(&[c("nodecraft-media", &[("image", Normal, false)])]).is_empty());
        assert!(check(&[c("nodecraft-graph", &[("nodecraft-media", Normal, true)])]).is_empty());
    }

    #[test]
    fn ui_crates_only_in_the_shell() {
        for dep in ["egui", "eframe", "winit", "egui_kittest", "craft-ui", "craft-quit", "rfd"] {
            for krate in ["nodecraft-engine", "nodecraft-agent", "nodecraft-automation", "nodecraft-types"] {
                let v = check(&[c(krate, &[(dep, Normal, false)])]);
                assert!(matches!(v[..], [Violation::UiOutsideShell { .. }]), "{krate} -> {dep}");
            }
        }
        assert!(check(&[c("nodecraft-ui-egui", &[("eframe", Normal, false)])]).is_empty());
        assert!(check(&[c("nodecraft-engine", &[("egui_extras_not", Normal, false), ("craft-settings", Normal, false)])]).is_empty());
    }

    #[test]
    fn unregistered_crate_is_error() {
        let v = check(&[c("nodecraft-mystery", &[])]);
        assert!(matches!(&v[..], [Violation::Unregistered { krate }] if krate == "nodecraft-mystery"));
        assert!(v[0].to_string().contains("register"));
    }

    #[test]
    fn testkit_only_as_dev_dependency() {
        let v = check(&[c("nodecraft-graph", &[("nodecraft-testkit", Normal, true)])]);
        assert!(matches!(v[..], [Violation::TestkitAsNormalDep { .. }]));
        assert!(check(&[c("nodecraft-graph", &[("nodecraft-testkit", Dev, true)])]).is_empty());
        // testkit itself may use the engine but not L5 crates.
        assert!(check(&[c("nodecraft-testkit", &[("nodecraft-engine", Normal, true)])]).is_empty());
        assert!(!check(&[c("nodecraft-testkit", &[("nodecraft-ui-egui", Normal, true)])]).is_empty());
    }

    #[test]
    fn provider_facade_has_only_lower_dependencies_and_is_dev_only_for_every_consumer() {
        assert!(check(&[c("nodecraft-provider-testkit", &[("nodecraft-providers", Normal, true), ("nodecraft-types", Normal, true)])]).is_empty());
        for consumer in ["nodecraft-nodes", "nodecraft-exec", "nodecraft-engine", "nodecraft-testkit"] {
            assert!(check(&[c(consumer, &[("nodecraft-provider-testkit", Dev, true)])]).is_empty());
            assert!(matches!(check(&[c(consumer, &[("nodecraft-provider-testkit", Normal, true)])])[..], [Violation::TestkitAsNormalDep { .. }]));
        }
        assert!(matches!(check(&[c("nodecraft-exec", &[("nodecraft-providers", Dev, true)])])[..], [Violation::Upward { .. }]));
        assert!(matches!(check(&[c("nodecraft-media", &[("nodecraft-provider-testkit", Dev, true)])])[..], [Violation::StandaloneHasWorkspaceDep { .. }]));
    }

    #[test]
    fn apps_and_xtask_exempt() {
        for app in ["nodecraft", "nodecraft-cli", "nodecraft-web", "xtask"] {
            assert!(check(&[c(app, &[("egui", Normal, false), ("nodecraft-ui-egui", Normal, true)])]).is_empty());
        }
    }

    #[test]
    fn metadata_parsing() {
        let meta: Value = serde_json::from_str(
            r#"{"packages":[
                {"name":"nodecraft-graph","dependencies":[
                    {"name":"nodecraft-types","kind":null,"path":"/x/crates/types"},
                    {"name":"serde","kind":null},
                    {"name":"proptest","kind":"dev"}]},
                {"name":"nodecraft-types","dependencies":[]}
            ]}"#,
        )
        .unwrap();
        let g = from_metadata(&meta).unwrap();
        assert_eq!(g.len(), 2);
        let graph = g.iter().find(|c| c.name == "nodecraft-graph").unwrap();
        assert!(graph.deps[0].workspace && !graph.deps[1].workspace);
        assert_eq!(graph.deps[2].kind, Dev);
        assert!(check(&g).is_empty());
    }
    #[test]
    fn provider_facade_rejects_every_other_workspace_target_in_every_kind() {
        for kind in [Normal, Dev, Build] {
            for target in [
                "nodecraft-nodes",
                "nodecraft-exec",
                "nodecraft-engine",
                "nodecraft-testkit",
                "nodecraft-ui-egui",
                "nodecraft-cli",
                "xtask",
                "nodecraft-unregistered",
            ] {
                let v = check(&[c("nodecraft-provider-testkit", &[(target, kind, true)])]);
                assert!(
                    matches!(&v[..], [Violation::ProviderFacadeWorkspaceDep { dep, kind: actual }] if dep == target && *actual == kind),
                    "facade accepted {target} [{kind:?}]: {v:?}"
                );
            }
        }
    }
    #[test]
    fn provider_facade_self_workspace_edges_are_not_generic_self_exemptions() {
        for kind in [Normal, Dev, Build] {
            let v = check(&[c("nodecraft-provider-testkit", &[("nodecraft-provider-testkit", kind, true)])]);
            assert!(
                matches!(&v[..], [Violation::ProviderFacadeWorkspaceDep { dep, kind: actual }] if dep == "nodecraft-provider-testkit" && *actual == kind),
                "facade self-edge accepted [{kind:?}]: {v:?}"
            );
        }
    }
    #[test]
    fn provider_facade_allowed_targets_external_rules_and_consumer_boundaries() {
        for kind in [Normal, Dev, Build] {
            for target in ["nodecraft-providers", "nodecraft-types"] {
                assert!(check(&[c("nodecraft-provider-testkit", &[(target, kind, true)])]).is_empty());
            }
            assert!(check(&[c("nodecraft-provider-testkit", &[("serde", kind, false)])]).is_empty());
            assert!(matches!(check(&[c("nodecraft-provider-testkit", &[("egui", kind, false)])])[..], [Violation::UiOutsideShell { .. }]));
            for consumer in ["nodecraft-nodes", "nodecraft-exec", "nodecraft-engine", "nodecraft-testkit"] {
                let v = check(&[c(consumer, &[("nodecraft-provider-testkit", kind, true)])]);
                if kind == Dev {
                    assert!(v.is_empty());
                } else {
                    assert!(matches!(v[..], [Violation::TestkitAsNormalDep { .. }]));
                }
            }
            assert!(matches!(check(&[c("nodecraft-media", &[("nodecraft-provider-testkit", kind, true)])])[..], [Violation::StandaloneHasWorkspaceDep { .. }]));
            // Existing generic self-edge and exempt-app policy is intentionally unchanged.
            for workspace in [false, true] {
                assert!(check(&[c("nodecraft-exec", &[("nodecraft-exec", kind, workspace)])]).is_empty());
            }
            assert!(check(&[c("nodecraft-cli", &[("nodecraft-provider-testkit", kind, true)])]).is_empty());
        }
    }
    #[test]
    fn facade_metadata_uses_canonical_names_and_rejects_unregistered_path_targets() {
        let g = from_metadata(&serde_json::json!({"packages":[
            {"name":"nodecraft-provider-testkit","dependencies":[
                {"name":"nodecraft-engine","rename":"providers","kind":"dev","path":"/x/engine"},
                {"name":"nodecraft-unregistered","kind":"build","path":"/x/unknown"}]},
            {"name":"nodecraft-engine","dependencies":[]}
        ]}))
        .unwrap();
        let v = check(&g);
        assert_eq!(v.len(), 2, "{v:?}");
    }

    #[test]
    fn rig_stays_inside_providers() {
        assert!(check(&[c("nodecraft-providers", &[("rig-core", Normal, false)])]).is_empty());
        for krate in ["nodecraft-engine", "nodecraft-agent", "nodecraft-ui-egui", "nodecraft", "nodecraft-cli", "xtask"] {
            for dep in ["rig-core", "rig-http"] {
                for kind in [Normal, Dev] {
                    assert!(matches!(check(&[c(krate, &[(dep, kind, false)])])[..], [Violation::RigOutsideProviders { .. }]), "{krate} -> {dep} {kind:?}");
                }
            }
        }
        assert!(check(&[c("nodecraft-engine", &[("trigger", Normal, false), ("rigid", Normal, false)])]).is_empty());
        assert!(names_rig("use rig_core::Model;"));
        assert!(names_rig("let m: rig::Model = x;"));
        assert!(names_rig("(rig_core::ProviderError)"));
        assert!(!names_rig("let trig::x = config_rig_core;"));
        assert!(!names_rig("a rigorous rig"));
        for public in ["pub use rig_core::Model;", "    pub fn model(&self) -> rig_core::Model {", "    pub field: rig::Thing,", "pub type T = rig_core::X;"] {
            assert!(is_public(public) && names_rig(public), "{public}");
        }
        for private in
            ["pub(crate) use rig_core::Model;", "    pub(super) fn f() -> rig_core::X {", "use rig_core::Model;", "pub(in crate::llm) type T = rig::X;"]
        {
            assert!(!is_public(private), "{private}");
        }
    }
}
