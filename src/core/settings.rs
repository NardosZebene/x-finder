//! User settings, saved as TOML in %APPDATA%\finder-rs\settings.toml.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MenuStyle {
    /// The app's own dark menu ("More Options…" opens the Windows one).
    #[default]
    App,
    /// The classic Windows Explorer menu, with every installed extension.
    Windows,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IconStyle {
    /// Real Windows icons (app icons for .exe, registered icons per file type).
    #[default]
    Windows,
    /// The app's own Finder-style icons.
    Finder,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    /// Follow the Windows light/dark setting (live).
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    pub fn index(self) -> i32 {
        match self {
            Appearance::System => 0,
            Appearance::Light => 1,
            Appearance::Dark => 2,
        }
    }
    pub fn from_index(i: i32) -> Self {
        match i {
            1 => Appearance::Light,
            2 => Appearance::Dark,
            _ => Appearance::System,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub appearance: Appearance,
    pub menu_style: MenuStyle,
    pub icon_style: IconStyle,
    /// 0 list, 1 icons, 2 columns, 3 gallery
    pub start_view: i32,
    pub show_hidden: bool,
    pub columns_full_path: bool,
    pub icon_size: f32,
    pub preview_pane: bool,
    pub preview_width: f32,
    pub column_width: f32,
    /// Sidebar favourites; `None` means the default set (Home, Desktop, …).
    pub favorites: Option<Vec<PathBuf>>,
    /// Folder icon look: 0 Finder, 1 Windows, 2 GNOME, 3 Ubuntu, 4 KDE, 5 Mint.
    pub folder_style: i32,
    /// Folders show what's inside them in Icons and Gallery view.
    pub folder_previews: bool,
    /// Names of the seven colour tags (e.g. "Work" instead of "Red").
    pub tag_names: Vec<String>,
    /// Sidebar extras.
    pub show_recents: bool,
    pub show_trash: bool,
    /// Toolbar "folder sizes" mode (size of every folder + share bars).
    pub show_folder_sizes: bool,
    /// Folders listed before files (off: mixed in, by the sort).
    pub folders_first: bool,
    /// Every other row in lists is shaded.
    pub striped_rows: bool,
    /// Key that opens X Finder from anywhere (set on its Start menu
    /// shortcut, so it works while the app is closed); "" = none.
    pub open_key: String,
    /// Times the app was started (the default-explorer banner waits a bit).
    pub launches: u32,
    /// "Don't ask again" on the default-explorer banner.
    pub explorer_ask_never: bool,
    /// "Not now" presses; after three it stops asking.
    pub explorer_not_now: u32,
    /// Unix seconds: don't ask before this.
    pub explorer_ask_after: i64,
    /// Split layout: 0 single, 1 side by side, 2 stacked, 3 one + two, 4 grid.
    pub layout: i32,
    /// Last window size (logical px; 0 = not saved yet), position (physical
    /// px) and whether it was maximized.
    pub window_w: f32,
    pub window_h: f32,
    pub window_x: i32,
    pub window_y: i32,
    pub window_maximized: bool,
    /// Window size in screen pixels (doesn't depend on when Windows reports
    /// the display scale, unlike `window_w`/`window_h`).
    pub window_pw: u32,
    pub window_ph: u32,
    /// Folders visited, most recent first (quick jump in the palette).
    pub recent_folders: Vec<String>,
    /// Commands typed in the palette, most recent first (Up arrow).
    pub command_history: Vec<String>,
    /// Reopen the tabs that were open when the window closed.
    pub restore_tabs: bool,
    /// Those tabs (written while `restore_tabs` is on).
    pub last_tabs: Option<TabGroup>,
    /// Sort and view chosen in each folder (lower-cased path → choice),
    /// like Explorer remembers them.
    pub folder_views: std::collections::BTreeMap<String, FolderView>,
    /// Keys chosen in Settings → Keyboard shortcuts (action id → key,
    /// "" = no key); actions not listed keep their built-in key.
    pub shortcuts: std::collections::BTreeMap<String, String>,
    /// Commands saved in the palette (`save ip = !ipconfig`).
    /// Arrays of tables stay last: TOML writes them after plain values.
    pub saved_commands: Vec<SavedCommand>,
    /// Saved sets of tabs ("Work", "Taxes", …) reopened from the tab menu.
    pub tab_groups: Vec<TabGroup>,
}

/// How one folder was last shown.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FolderView {
    /// Sort column (0 name, 1 date modified, 2 size, 3 kind, 4 date created).
    pub sort: i32,
    pub ascending: bool,
    /// 0 list, 1 icons, 2 columns, 3 gallery.
    pub view: i32,
    /// Unix seconds; the oldest are forgotten past a limit.
    pub used: i64,
    /// Rule for every subfolder without its own choice ("sort kind all"):
    /// sort column and direction…
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sub_sort: Option<i32>,
    pub sub_ascending: bool,
    /// …and view.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sub_view: Option<i32>,
}

/// A palette command saved under a short name: typing `ip` runs `!ipconfig`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedCommand {
    pub name: String,
    /// Any palette line, with {dir} {file} {files} {name} filled in when run.
    pub line: String,
}

/// A named set of tabs. Each tab is a folder path or a special place
/// ("::computer", "::recent", "::trash").
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TabGroup {
    pub name: String,
    pub tabs: Vec<String>,
    /// Which tab was in front when the group was saved.
    pub active: usize,
    /// Ctrl+Alt+1…9 opens the group; 0 = no shortcut.
    pub shortcut: i32,
}

pub fn default_tag_names() -> Vec<String> {
    super::tags::NAMES.iter().map(|s| s.to_string()).collect()
}

impl Settings {
    /// Tag name, falling back to the colour name.
    pub fn tag_name(&self, i: usize) -> String {
        self.tag_names
            .get(i)
            .filter(|s| !s.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| super::tags::NAMES[i.min(6)].to_string())
    }
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            appearance: Appearance::System,
            menu_style: MenuStyle::App,
            icon_style: IconStyle::Windows,
            start_view: 0,
            show_hidden: false,
            columns_full_path: false,
            icon_size: 64.0,
            preview_pane: false,
            preview_width: 300.0,
            column_width: 230.0,
            favorites: None,
            folder_style: 0,
            folder_previews: false,
            tag_names: default_tag_names(),
            show_recents: true,
            show_trash: true,
            show_folder_sizes: false,
            folders_first: true,
            striped_rows: true,
            open_key: String::new(),
            launches: 0,
            explorer_ask_never: false,
            explorer_not_now: 0,
            explorer_ask_after: 0,
            layout: 0,
            window_w: 0.0,
            window_h: 0.0,
            window_x: i32::MIN,
            window_y: i32::MIN,
            window_maximized: false,
            window_pw: 0,
            window_ph: 0,
            recent_folders: Vec::new(),
            command_history: Vec::new(),
            restore_tabs: false,
            last_tabs: None,
            folder_views: Default::default(),
            shortcuts: Default::default(),
            saved_commands: Vec::new(),
            tab_groups: Vec::new(),
        }
    }
}

pub fn path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("finder-rs").join("settings.toml"))
}

impl Settings {
    pub fn load() -> Settings {
        path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|s| toml::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let p = path().ok_or("No settings folder.")?;
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let text = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        // Write then rename, so a crash can't leave a half-written file.
        let tmp = p.with_extension("toml.tmp");
        std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &p).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_roundtrip_and_defaults() {
        let s = Settings {
            menu_style: MenuStyle::Windows,
            favorites: Some(vec![PathBuf::from(r"C:\Work")]),
            tab_groups: vec![TabGroup { name: "Work".into(), tabs: vec![r"D:\".into(), "::recent".into()], active: 1, shortcut: 2 }],
            last_tabs: Some(TabGroup { tabs: vec![r"C:\".into()], ..Default::default() }),
            ..Default::default()
        };
        let text = toml::to_string_pretty(&s).unwrap();
        assert_eq!(toml::from_str::<Settings>(&text).unwrap(), s);
        // Missing keys fall back to defaults (older settings files keep working).
        let partial: Settings = toml::from_str("show_hidden = true").unwrap();
        assert!(partial.show_hidden);
        assert_eq!(partial.menu_style, MenuStyle::App);
    }
}
