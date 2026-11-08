//! The interface size (#1107), `[appearance] interface_size`: how large the shell draws its panels, menus,
//! dialogs, top bar and minimap, as a percentage. It is a setting of this installation, never of a
//! project. The shell applies it as egui's zoom factor, so everything drawn in points grows
//! together, canvas cards included; canvas zoom stays a separate multiplier on top of it.
//!
//! The file's value is hostile: anything but a number from [`InterfaceSize::MIN`] to
//! [`InterfaceSize::MAX`] reads as 100 %, marked [damaged](InterfaceSize::damaged) so the shell can
//! say so, rather than failing the whole `[appearance]` section.
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The interface size, in percent: always within [`Self::MIN`]..=[`Self::MAX`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InterfaceSize {
    percent: u16,
    /// The settings file held something else, which reads as [`Self::DEFAULT`].
    damaged: bool,
}

impl Default for InterfaceSize {
    fn default() -> Self {
        Self { percent: Self::DEFAULT, damaged: false }
    }
}

impl InterfaceSize {
    /// The smallest size, in percent.
    pub const MIN: u16 = 80;
    /// The largest size, in percent.
    pub const MAX: u16 = 200;
    /// The usual size.
    pub const DEFAULT: u16 = 100;
    /// How far one larger or smaller step moves, in percent.
    pub const STEP: u16 = 10;
    /// What a person is told when the file's value can't be used.
    pub const DAMAGED: &'static str = "The interface size in your settings wasn't a size from 80 % to 200 %, so NodeCraft uses 100 %.";

    /// The size `percent` names, rounded to a whole percent; `None` unless it is a finite number
    /// from [`Self::MIN`] to [`Self::MAX`].
    pub fn from_percent(percent: f64) -> Option<Self> {
        if !percent.is_finite() {
            return None;
        }
        let rounded = percent.round();
        if rounded < f64::from(Self::MIN) || rounded > f64::from(Self::MAX) {
            return None;
        }
        // In range, so the cast is exact.
        let percent = u16::try_from(rounded as i64).ok()?;
        Some(Self { percent, damaged: false })
    }

    /// The size a stored text names (the browser keeps it as text), or a damaged 100 %.
    pub fn parse(text: &str) -> Self {
        text.trim().parse::<f64>().ok().and_then(Self::from_percent).unwrap_or(Self::DAMAGED_DEFAULT)
    }

    const DAMAGED_DEFAULT: Self = Self { percent: Self::DEFAULT, damaged: true };

    /// The size, in percent.
    pub fn percent(self) -> u16 {
        self.percent
    }

    /// The zoom factor the shell applies: 1.0 at 100 %.
    pub fn zoom_factor(self) -> f32 {
        f32::from(self.percent) / 100.0
    }

    /// Whether the settings file held a value that couldn't be used (now 100 %).
    pub fn damaged(self) -> bool {
        self.damaged
    }

    /// One step larger: the next multiple of [`Self::STEP`], at most [`Self::MAX`].
    pub fn larger(self) -> Self {
        let next = (self.percent / Self::STEP).saturating_add(1).saturating_mul(Self::STEP);
        Self { percent: next.clamp(Self::MIN, Self::MAX), damaged: false }
    }

    /// One step smaller: the multiple of [`Self::STEP`] below, at least [`Self::MIN`].
    pub fn smaller(self) -> Self {
        let next = self.percent.saturating_add(Self::STEP - 1) / Self::STEP;
        let next = next.saturating_sub(1).saturating_mul(Self::STEP);
        Self { percent: next.clamp(Self::MIN, Self::MAX), damaged: false }
    }
}

impl Serialize for InterfaceSize {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u16(self.percent)
    }
}

impl<'de> Deserialize<'de> for InterfaceSize {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Number(f64),
            Unreadable(serde::de::IgnoredAny),
        }
        Ok(match Raw::deserialize(deserializer)? {
            Raw::Number(percent) => Self::from_percent(percent).unwrap_or(Self::DAMAGED_DEFAULT),
            Raw::Unreadable(_) => Self::DAMAGED_DEFAULT,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_finite_sizes_in_range_are_sizes() {
        assert_eq!(InterfaceSize::from_percent(150.0).unwrap().percent(), 150);
        assert_eq!(InterfaceSize::from_percent(124.6).unwrap().percent(), 125);
        assert_eq!(InterfaceSize::from_percent(79.6).unwrap().percent(), 80);
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.0, -100.0, 79.4, 200.6, 1e9, f64::MAX] {
            assert!(InterfaceSize::from_percent(bad).is_none(), "{bad}");
        }
        assert_eq!(InterfaceSize::default().zoom_factor(), 1.0);
        assert_eq!(InterfaceSize::from_percent(175.0).unwrap().zoom_factor(), 1.75);
    }

    #[test]
    fn steps_land_on_tens_and_stop_at_the_ends() {
        let size = |p: f64| InterfaceSize::from_percent(p).unwrap();
        assert_eq!(size(100.0).larger().percent(), 110);
        assert_eq!(size(115.0).larger().percent(), 120);
        assert_eq!(size(195.0).larger().percent(), 200);
        assert_eq!(size(200.0).larger().percent(), 200);
        assert_eq!(size(100.0).smaller().percent(), 90);
        assert_eq!(size(115.0).smaller().percent(), 110);
        assert_eq!(size(85.0).smaller().percent(), 80);
        assert_eq!(size(80.0).smaller().percent(), 80);
    }

    /// The real settings file: a damaged value reads as 100 %, the rest of `[appearance]` keeps
    /// its values, and a change saves as a whole percent that reads back after a restart.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn a_damaged_value_reads_as_100_and_a_change_survives_a_restart() {
        use crate::settings::{Location, Theme, open};
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        let read = |text: &str| {
            std::fs::write(&config, text).unwrap();
            let (store, report) = open(Location::Dir(dir.path().to_path_buf()));
            assert!(report.section_errors.is_empty(), "{text}: {:?}", report.section_errors);
            store.get().appearance.clone()
        };
        assert_eq!(read("[appearance]\ninterface_size = 150\n").interface_size.percent(), 150);
        assert_eq!(read("[appearance]\ninterface_size = 150.0\n").interface_size.percent(), 150);
        assert!(!read("").interface_size.damaged());
        for bad in ["\"big\"", "nan", "inf", "-inf", "0", "1000000000", "-5", "[1]", "{a = 1}", "true", "1979-05-27"] {
            let appearance = read(&format!("[appearance]\ninterface_size = {bad}\ntheme = \"light\"\n"));
            assert_eq!(appearance.interface_size.percent(), 100, "{bad}");
            assert!(appearance.interface_size.damaged(), "{bad}");
            assert_eq!(appearance.theme, Theme::Light, "{bad}: the rest of the section survives");
        }
        std::fs::write(&config, "# my settings\n[appearance]\ntheme = \"light\"\n").unwrap();
        let (mut store, _) = open(Location::Dir(dir.path().to_path_buf()));
        store.update(|settings| settings.appearance.interface_size = InterfaceSize::from_percent(175.0).unwrap());
        store.flush().unwrap();
        drop(store);
        let text = std::fs::read_to_string(&config).unwrap();
        assert!(text.contains("interface_size = 175") && text.contains("# my settings"), "{text}");
        let (store, _) = open(Location::Dir(dir.path().to_path_buf()));
        assert_eq!(store.get().appearance.interface_size.percent(), 175);
        assert_eq!(store.get().appearance.theme, Theme::Light);
    }

    #[test]
    fn stored_text_parses_like_the_file() {
        assert_eq!(InterfaceSize::parse(" 130 ").percent(), 130);
        for bad in ["", "big", "NaN", "1e9", "0", "-0"] {
            assert!(InterfaceSize::parse(bad).damaged(), "{bad}");
            assert_eq!(InterfaceSize::parse(bad).percent(), 100);
        }
    }
}
