//! User preferences, stored as JSON in the platform config directory.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

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
    /// Colors given to branch folders, by repository path: the folder (`feat`,
    /// `feat/menu`) and its hue.
    pub folder_colors: BTreeMap<String, BTreeMap<String, f32>>,
    /// One set of folder colors for every repository, in place of each one's own.
    pub share_folder_colors: bool,
    /// That one set. Each repository's own colors are kept beside it, for when
    /// sharing is turned off again.
    pub shared_folder_colors: BTreeMap<String, f32>,
    pub layout: Layout,
    pub tools: Tools,
}

/// Programs Spoon hands work to.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tools {
    /// The diff program: empty for none, the name of one found on this machine, or
    /// [`Tools::CUSTOM`] for `diff_command`.
    pub diff_tool: String,
    /// A command line of the user's own; the old and the new file are added at its end.
    pub diff_command: String,
}

impl Tools {
    pub const CUSTOM: &str = "custom";
}

/// How the panes are arranged. One layout serves every tab.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Layout {
    /// Width of the sidebar.
    pub sidebar: f32,
    /// Width of the file list beside a diff.
    pub files: f32,
    /// Share of the height the list gets above the details.
    pub split: f32,
    /// Open sidebar sections: branches, remotes, tags, submodules, worktrees.
    pub sections: [bool; 5],
}

impl Default for Layout {
    fn default() -> Self {
        Self { sidebar: 280.0, files: 360.0, split: 0.52, sections: [true, true, false, true, true] }
    }
}

impl Default for Prefs {
    fn default() -> Self {
        Self { theme: ThemePrefs::default(), repos: Vec::new(), open: Vec::new(), history_limit: 5000, zoom: 0.9, open_folders: BTreeMap::new(), folder_colors: BTreeMap::new(), share_folder_colors: false, shared_folder_colors: BTreeMap::new(), layout: Layout::default(), tools: Tools::default() }
    }
}

impl Prefs {
    /// The folder colors a repository shows: the shared set, or its own.
    pub fn colors_of(&self, repo: &Path) -> &BTreeMap<String, f32> {
        static NONE: BTreeMap<String, f32> = BTreeMap::new();
        match self.share_folder_colors {
            true => &self.shared_folder_colors,
            false => self.folder_colors.get(&repo.display().to_string()).unwrap_or(&NONE),
        }
    }

    /// Stores the colors a repository shows now, in the shared set when colors are
    /// shared. Says whether that changed anything.
    pub fn set_colors(&mut self, repo: &Path, colors: &BTreeMap<String, f32>) -> bool {
        if self.colors_of(repo) == colors {
            return false;
        }
        let key = repo.display().to_string();
        match (self.share_folder_colors, colors.is_empty()) {
            (true, _) => self.shared_folder_colors.clone_from(colors),
            (false, true) => drop(self.folder_colors.remove(&key)),
            (false, false) => drop(self.folder_colors.insert(key, colors.clone())),
        }
        true
    }

    /// Turns sharing of folder colors on or off. A shared set that is still empty
    /// starts as every repository's colors together; where two repositories color
    /// a folder differently, `lead` (the repository in front) has its way.
    pub fn share_colors(&mut self, on: bool, lead: Option<&Path>) {
        self.share_folder_colors = on;
        if on && self.shared_folder_colors.is_empty() {
            let own = |colors: &BTreeMap<String, f32>| colors.iter().map(|(folder, hue)| (folder.clone(), *hue)).collect::<Vec<_>>();
            let lead = lead.and_then(|repo| self.folder_colors.get(&repo.display().to_string()));
            self.shared_folder_colors = self.folder_colors.values().chain(lead).flat_map(own).collect();
        }
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
