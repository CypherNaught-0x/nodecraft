//! Local project locations only; read hostile settings through the bounded accessor.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectSettings {
    pub recent: Vec<String>,
}
impl ProjectSettings {
    pub const MAX_RECENT: usize = 16;
    pub const MAX_PATH_BYTES: usize = 4096;
    const MAX_INSPECTED: usize = 256;

    pub fn valid_path(path: &str) -> bool {
        !path.is_empty() && path.len() <= Self::MAX_PATH_BYTES && !path.contains('\0') && std::path::Path::new(path).is_absolute()
    }
    pub fn recent(&self) -> Vec<String> {
        let mut out = Vec::new();
        for path in self.recent.iter().take(Self::MAX_INSPECTED) {
            if out.len() == Self::MAX_RECENT {
                break;
            }
            if Self::valid_path(path) && !out.contains(path) {
                out.push(path.clone());
            }
        }
        out
    }
    pub fn remember(&mut self, path: &str) {
        if !Self::valid_path(path) {
            return;
        }
        self.recent = std::iter::once(path.to_owned()).chain(self.recent().into_iter().filter(|old| old != path)).take(Self::MAX_RECENT).collect();
    }
    pub fn forget(&mut self, path: &str) {
        self.recent = self.recent().into_iter().filter(|old| old != path).collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn path(n: usize) -> String {
        std::env::temp_dir().join(format!("public-{n}.ncraft")).to_str().unwrap().to_owned()
    }
    #[test]
    fn hostile_recents_are_bounded_deduplicated_and_local() {
        let mut p = ProjectSettings { recent: vec!["relative.ncraft".into(), "https://example.test/project".into(), "x".repeat(4097), path(1), path(1)] };
        p.recent.extend((2..100).map(path));
        assert_eq!(p.recent().len(), 16);
        p.remember(&path(12));
        assert_eq!(p.recent().first(), Some(&path(12)));
        assert_eq!(p.recent().len(), 16);
        p.forget(&path(12));
        assert_eq!(p.recent().len(), 15);
        let p = ProjectSettings { recent: vec!["invalid".into(); 257].into_iter().chain([path(1)]).collect() };
        assert!(p.recent().is_empty(), "scan has its own bound");
    }
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn recents_persist_without_removing_other_settings_or_unknown_fields() {
        use super::super::{Location, open};
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        std::fs::write(&config, "schema_version=1\n# public comment\n[appearance]\ntheme=\"light\"\n[projects]\nfuture_flag=true\n[plugins]\nfuture=7\n")
            .unwrap();
        let (mut store, _) = open(Location::Dir(dir.path().to_path_buf()));
        let value = path(9);
        store.update(|s| s.projects.remember(&value));
        store.flush().unwrap();
        let text = std::fs::read_to_string(config).unwrap();
        for fragment in ["# public comment", "future_flag", "[plugins]", "future=7"] {
            assert!(text.contains(fragment), "{text}");
        }
        drop(store);
        let (store, _) = open(Location::Dir(dir.path().to_path_buf()));
        assert_eq!(store.get().projects.recent(), vec![value]);
        assert_eq!(store.get().appearance.theme, super::super::Theme::Light);
    }
}
