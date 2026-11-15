use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Default, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    Light,
    Dark,
}

/// The display scales offered in Settings, as zoom factors.
pub const DISPLAY_SCALES: [f32; 4] = [0.9, 1.0, 1.2, 1.4];

/// Missing fields take their defaults, so prefs from another version still load.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Prefs {
    pub theme: Theme,
    pub sidebar_collapsed: bool,
    /// One of [`DISPLAY_SCALES`]; anything else reads as 100%.
    pub display_scale: f32,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            theme: Theme::default(),
            sidebar_collapsed: false,
            display_scale: 1.0,
        }
    }
}

impl Prefs {
    /// The display scale, or 100% when the saved value isn't one offered.
    pub fn scale(&self) -> f32 {
        DISPLAY_SCALES
            .into_iter()
            .find(|s| (s - self.display_scale).abs() < 0.001)
            .unwrap_or(1.0)
    }
}

/// `%APPDATA%\CompetencyTracker\prefs.json`.
pub fn prefs_path() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(
        PathBuf::from(appdata)
            .join("CompetencyTracker")
            .join("prefs.json"),
    )
}

/// Defaults on any error (missing file, bad JSON).
pub fn load_prefs_from(p: &Path) -> Prefs {
    std::fs::read_to_string(p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_prefs_to(p: &Path, prefs: &Prefs) -> io::Result<()> {
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(prefs).map_err(io::Error::other)?;
    std::fs::write(p, json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefs_default_light_and_roundtrip() {
        let p = Prefs::default();
        assert_eq!(p.theme, Theme::Light);
        assert!(!p.sidebar_collapsed);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("prefs.json");
        let saved = Prefs {
            theme: Theme::Dark,
            sidebar_collapsed: true,
            display_scale: 1.2,
        };
        save_prefs_to(&path, &saved).unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("\"dark\""));
        let back = load_prefs_from(&path);
        assert_eq!(back.theme, Theme::Dark);
        assert!(back.sidebar_collapsed);
        assert_eq!(back.scale(), 1.2);
    }

    #[test]
    fn display_scale_defaults_to_100_and_ignores_odd_values() {
        assert_eq!(Prefs::default().scale(), 1.0);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.json");
        std::fs::write(&path, r#"{"theme":"light"}"#).unwrap();
        assert_eq!(load_prefs_from(&path).scale(), 1.0);
        std::fs::write(&path, r#"{"display_scale":3.0}"#).unwrap();
        assert_eq!(load_prefs_from(&path).scale(), 1.0);
        std::fs::write(&path, r#"{"display_scale":1.4}"#).unwrap();
        assert_eq!(load_prefs_from(&path).scale(), 1.4);
    }

    #[test]
    fn load_prefs_defaults_on_missing_or_bad_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            load_prefs_from(&dir.path().join("none.json")).theme,
            Theme::Light
        );
        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, "{not json").unwrap();
        let p = load_prefs_from(&bad);
        assert_eq!(p.theme, Theme::Light);
        assert!(!p.sidebar_collapsed);
    }

    #[test]
    fn load_prefs_keeps_known_fields_when_others_are_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.json");
        // Written by a version that had fewer settings.
        std::fs::write(&path, r#"{"theme":"dark"}"#).unwrap();
        let p = load_prefs_from(&path);
        assert_eq!(p.theme, Theme::Dark);
        assert!(!p.sidebar_collapsed);
    }

    #[test]
    fn prefs_path_ends_with_app_folder() {
        if let Some(p) = prefs_path() {
            assert!(p.ends_with("CompetencyTracker/prefs.json"));
        }
    }
}
