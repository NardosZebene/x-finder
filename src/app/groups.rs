//! Tab groups: save the open tabs under a name ("Work", "Taxes", …), with an
//! optional Ctrl+Alt+1…9 shortcut, and bring them all back later from the
//! tab right-click menu. Also "remember tabs after closing" (Settings).

use std::path::PathBuf;

use finder_rs::core::location::Location;
use finder_rs::core::settings::TabGroup;
use slint::{ComponentHandle, SharedString};

use super::App;
use super::actions::RenameTarget;
use crate::Bridge;

/// How a tab is stored in settings.toml. Searches and tag views are not
/// saved (they are quick to redo and may be stale).
pub(super) fn loc_key(loc: &Location) -> Option<String> {
    match loc {
        Location::Dir(p) => Some(p.to_string_lossy().into_owned()),
        Location::Computer => Some("::computer".into()),
        Location::Recent => Some("::recent".into()),
        Location::Trash => Some("::trash".into()),
        _ => None,
    }
}

pub(super) fn loc_from_key(key: &str) -> Location {
    match key {
        "::computer" => Location::Computer,
        "::recent" => Location::Recent,
        "::trash" => Location::Trash,
        p => Location::Dir(PathBuf::from(p)),
    }
}

/// "Ctrl+Alt+3", or "" without a shortcut.
pub(super) fn shortcut_label(n: i32) -> String {
    if (1..=9).contains(&n) { format!("Ctrl+Alt+{n}") } else { String::new() }
}

impl App {
    /// The open tabs as a group (name left empty).
    pub(super) fn current_tabs(&self) -> TabGroup {
        let mut active = 0;
        let mut tabs = Vec::new();
        for (i, t) in self.tabs.iter().enumerate() {
            if let Some(k) = loc_key(&t.loc) {
                if i == self.active {
                    active = tabs.len();
                }
                tabs.push(k);
            }
        }
        TabGroup { name: String::new(), tabs, active, shortcut: 0 }
    }

    /// "Save Tabs as Group…": asks for a name and an optional shortcut,
    /// saved by `save_tab_group`.
    pub(super) fn begin_save_tab_group(&mut self) {
        let Some(ui) = self.ui() else { return };
        let name = format!("Group {}", self.settings.tab_groups.len() + 1);
        // Offer the first free Ctrl+Alt+number.
        let free = (1..=9).find(|n| !self.settings.tab_groups.iter().any(|g| g.shortcut == *n)).unwrap_or(0);
        let b = ui.global::<Bridge>();
        b.set_rename_title(format!("Save {} tabs as group", self.tabs.len()).into());
        b.set_rename_text(SharedString::from(name.as_str()));
        b.set_rename_stem(name.len() as i32);
        b.set_rename_error(SharedString::new());
        b.set_rename_group(true);
        b.set_rename_shortcut(free);
        b.set_renaming(true);
        self.rename_target = Some(RenameTarget { path: PathBuf::new(), tag: None, group: true });
    }

    /// Saves the open tabs; a group with the same name is replaced (that is
    /// how a group gets updated). A shortcut moves from any other group.
    pub(super) fn save_tab_group(&mut self, name: String, shortcut: i32) {
        let mut group = self.current_tabs();
        if group.tabs.is_empty() {
            self.note("Nothing to save: searches and tag views aren't kept in groups.");
            return;
        }
        let shortcut = if (1..=9).contains(&shortcut) { shortcut } else { 0 };
        if shortcut > 0 {
            for g in &mut self.settings.tab_groups {
                if g.shortcut == shortcut {
                    g.shortcut = 0;
                }
            }
        }
        let n = group.tabs.len();
        group.name = name.clone();
        group.shortcut = shortcut;
        match self.settings.tab_groups.iter_mut().find(|g| g.name.eq_ignore_ascii_case(&name)) {
            Some(g) => *g = group,
            None => self.settings.tab_groups.push(group),
        }
        self.save_settings();
        let key = match shortcut_label(shortcut) {
            k if k.is_empty() => k,
            k => format!(" · {k}"),
        };
        self.note(format!("Saved {n} tab{} as “{name}”{key}", if n == 1 { "" } else { "s" }));
    }

    /// Replaces the open tabs with the group's tabs.
    pub(super) fn open_tab_group(&mut self, index: usize) {
        let Some(group) = self.settings.tab_groups.get(index).cloned() else { return };
        if self.open_tabs(&group) {
            self.note(format!("Opened “{}”", group.name));
        }
    }

    /// Ctrl+Alt+1…9.
    pub(super) fn open_group_by_shortcut(&mut self, n: i32) -> bool {
        match self.settings.tab_groups.iter().position(|g| g.shortcut == n) {
            Some(i) => {
                self.open_tab_group(i);
                true
            }
            None => false,
        }
    }

    /// Replaces the open tabs with `group`'s; false when it has none.
    pub(super) fn open_tabs(&mut self, group: &TabGroup) -> bool {
        if group.tabs.is_empty() {
            return false;
        }
        self.save_scroll();
        for t in self.tabs.drain(..) {
            t.cancel.cancel();
        }
        for key in &group.tabs {
            let tab = self.make_tab(loc_from_key(key));
            self.tabs.push(tab);
        }
        for i in 0..self.tabs.len() {
            self.start_load(i);
        }
        self.active = usize::MAX; // force a full switch
        self.activate(group.active.min(self.tabs.len() - 1));
        true
    }

    pub(super) fn delete_tab_group(&mut self, index: usize) {
        if index < self.settings.tab_groups.len() {
            let g = self.settings.tab_groups.remove(index);
            self.save_settings();
            self.note(format!("Deleted group “{}”", g.name));
        }
    }
}
