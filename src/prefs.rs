//! User preferences, stored as JSON in the platform config directory.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::theme::ThemePrefs;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub theme: ThemePrefs,
    /// Known repositories, most recently opened first.
    pub repos: Vec<PathBuf>,
    /// Tabs to restore on start.
    pub open: Vec<PathBuf>,
    pub history_limit: usize,
    /// UI scale. Ctrl with plus, minus or 0 changes it.
    pub zoom: f32,
    /// Branch folders left open in the sidebar, by repository path.
    pub open_folders: BTreeMap<String, BTreeSet<String>>,
}

impl Default for Prefs {
    fn default() -> Self {
        Self { theme: ThemePrefs::default(), repos: Vec::new(), open: Vec::new(), history_limit: 5000, zoom: 0.9, open_folders: BTreeMap::new() }
    }
}

/// `%APPDATA%\Spoon` on Windows, `$XDG_CONFIG_HOME/spoon` (or `~/.config/spoon`) elsewhere.
pub fn config_dir() -> Option<PathBuf> {
    let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    if cfg!(windows) {
        return Some(var("APPDATA")?.join("Spoon"));
    }
    Some(var("XDG_CONFIG_HOME").or_else(|| Some(var("HOME")?.join(".config")))?.join("spoon"))
}

impl Prefs {
    pub fn load() -> Self {
        let Some(path) = config_dir().map(|d| d.join("preference.json")) else { return Self::default() };
        let Ok(text) = std::fs::read_to_string(&path) else { return Self::default() };
        serde_json::from_str(&text).unwrap_or_else(|e| {
            eprintln!("spoon: ignoring unreadable preferences {}: {e}", path.display());
            Self::default()
        })
    }

    pub fn save(&self) -> Result<(), String> {
        let dir = config_dir().ok_or("no config directory: APPDATA / HOME is not set")?;
        let write = || -> std::io::Result<()> {
            std::fs::create_dir_all(&dir)?;
            let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
            std::fs::write(dir.join("preference.json"), text)
        };
        write().map_err(|e| format!("cannot save preferences to {}: {e}", dir.display()))
    }

    /// Moves `path` to the front of the known repositories.
    pub fn remember(&mut self, path: &std::path::Path) {
        self.repos.retain(|p| p != path);
        self.repos.insert(0, path.to_path_buf());
    }
}
