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
    ("comfy", Class::Layer(1)),
    ("graph", Class::Layer(1)),
    ("nodes", Class::Layer(2)),
    ("catalog", Class::Layer(2)),
    ("actions", Class::Layer(2)),
    ("exec", Class::Layer(3)),
    ("providers", Class::Layer(3)),
    ("gpu", Class::Layer(3)),
    ("format", Class::Layer(3)),
    ("engine", Class::Layer(4)),
    ("agent", Class::Layer(5)),
    ("automation", Class::Layer(5)),
    ("ui-egui", Class::Layer(5)),
    ("testkit", Class::Testkit),
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
                write!(f, "{krate}: nodecraft-testkit may only be a dev-dependency")
            }
            Violation::UiOutsideShell { krate, dep } => {
                write!(f, "{krate} depends on UI crate `{dep}`; UI toolkits are only allowed in ui-egui and the apps")
            }
        }
    }
}

/// Check all rules. Returns violations sorted for stable output.
pub fn check(crates: &[Crate]) -> Vec<Violation> {
    let mut out = Vec::new();
    for c in crates {
        let Some(class) = classify(&c.name) else {
            out.push(Violation::Unregistered { krate: c.name.clone() });
            continue;
        };
        let Some(layer) = class.layer() else {
            continue; // exempt
        };
        for d in &c.deps {
            // Self dev-dependencies (e.g. to enable features in tests) are fine.
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
}
