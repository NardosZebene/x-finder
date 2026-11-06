//! Settings → Keyboard shortcuts: every action can get the user's own key,
//! plus one key that opens X Finder from anywhere.
//!
//! Action keys live in `settings.shortcuts` (see `commands::lookup`). The
//! "open X Finder" key is a system-wide hotkey X Finder holds itself
//! (instant); while one is set X Finder starts hidden at sign-in and closing
//! the window only hides it, so the key always works.

use std::path::PathBuf;

use finder_rs::core::shellx;
use slint::{ComponentHandle, ModelRc, VecModel};

use super::App;
use crate::commands::{self, BINDABLE};
use crate::{Bridge, ShortcutRow};

/// The id the "open X Finder" row uses in the UI.
const OPEN_ID: &str = "open";

impl App {
    /// Applies the saved keys and shows them in Settings.
    pub(super) fn sync_shortcuts(&self) {
        commands::set_user_keys(&self.settings.shortcuts);
        let Some(ui) = self.ui() else { return };
        let rows: Vec<ShortcutRow> = BINDABLE
            .iter()
            .map(|(id, cmd, _)| ShortcutRow {
                id: (*id).into(),
                title: cmd.title().into(),
                key: commands::key_for_id(id).into(),
                custom: self.settings.shortcuts.contains_key(*id),
            })
            .collect();
        let b = ui.global::<Bridge>();
        b.set_shortcuts(ModelRc::new(VecModel::from(rows)));
        b.set_open_key(self.settings.open_key.clone().into());
    }

    /// A key was pressed while the shortcut box of `id` was waiting.
    pub(super) fn shortcut_record(&mut self, id: &str, text: &str, ctrl: bool, alt: bool, shift: bool) {
        // A lone Ctrl/Alt/Shift: keep waiting for the rest.
        let Some(key) = commands::combo(text, ctrl, alt, shift) else { return };
        self.stop_recording();
        if id == OPEN_ID {
            self.set_open_key(&key);
            return;
        }
        // Plain letters and digits type names to jump to them.
        let plain = !ctrl && !alt && key.trim_start_matches("Shift+").chars().count() == 1 && key != "Space";
        if plain {
            self.toast("Add Ctrl or Alt: plain letters jump to names as you type.");
            return;
        }
        let Some(&(_, cmd, default)) = BINDABLE.iter().find(|b| b.0 == id) else { return };
        // The key moves here from the action that had it.
        let mut note = None;
        for &(other, other_cmd, _) in BINDABLE {
            if other != id && commands::key_for_id(other).eq_ignore_ascii_case(&key) {
                self.settings.shortcuts.insert(other.to_string(), String::new());
                note = Some(format!("{key} now does “{}” (no longer “{}”).", cmd.title(), other_cmd.title()));
            }
        }
        if key == default {
            self.settings.shortcuts.remove(id);
        } else {
            self.settings.shortcuts.insert(id.to_string(), key.clone());
        }
        self.save_settings();
        self.sync_shortcuts();
        self.toast(note.unwrap_or_else(|| format!("{key}: {}", cmd.title())));
    }

    /// The × next to a key.
    pub(super) fn shortcut_clear(&mut self, id: &str) {
        self.stop_recording();
        if id == OPEN_ID {
            self.set_open_key("");
            return;
        }
        self.settings.shortcuts.insert(id.to_string(), String::new());
        self.save_settings();
        self.sync_shortcuts();
    }

    pub(super) fn shortcuts_reset(&mut self) {
        self.stop_recording();
        self.settings.shortcuts.clear();
        self.save_settings();
        self.sync_shortcuts();
        self.toast("All shortcuts are back to their built-in keys.");
    }

    pub(super) fn stop_recording(&self) {
        if let Some(ui) = self.ui() {
            ui.global::<Bridge>().set_shortcut_recording("".into());
        }
    }

    /// Sets (or with "" removes) the key that opens X Finder from anywhere.
    fn set_open_key(&mut self, key: &str) {
        if !key.is_empty() && register_parts(key).is_none() {
            self.toast("Add Ctrl or Alt, e.g. Ctrl+Alt+E or Ctrl+Alt+Space.");
            return;
        }
        let _ = super::instance::set_open_hotkey(None);
        self.settings.open_key = key.to_string();
        self.save_settings();
        self.sync_shortcuts();
        self.claim_open_key();
        self.toast(if key.is_empty() {
            "X Finder no longer has a key of its own.".to_string()
        } else {
            // One modifier and a letter (Ctrl+Y) is taken from every app.
            let common = key.matches('+').count() == 1 && key.chars().last().is_some_and(|c| c.is_ascii_alphanumeric());
            if common {
                format!("{key} now opens X Finder from anywhere, so other apps no longer get {key}.")
            } else {
                format!("{key} now opens X Finder from anywhere.")
            }
        });
    }

    /// X Finder answers its "open" key itself: instant, unlike a Windows
    /// shortcut key. So it starts hidden at sign-in while a key is set
    /// (`--hidden`), and closing the window only hides it.
    pub(super) fn claim_open_key(&self) {
        let key = self.settings.open_key.clone();
        finder_rs::core::default_explorer::set_startup(
            (!key.is_empty()).then(|| std::env::current_exe().ok()).flatten().as_deref(),
        );
        // An earlier version put the key on the Start menu shortcut, where
        // Explorer would keep it.
        if let Some(link) = start_menu_link() {
            shellx::clear_shortcut_hotkey(&link);
        }
        let Some(parts) = register_parts(&key) else { return };
        // Retried while something else (Explorer) still holds the key.
        // (Later tries run from a timer, where the settings can be read.)
        fn attempt(key: String, parts: (u32, u32), n: u32) {
            if n > 0 && super::with_app(|a| a.settings.open_key.clone()).as_deref() != Some(key.as_str()) {
                return;
            }
            if super::instance::set_open_hotkey(Some(parts)) {
                return;
            }
            let wait = if n < 20 { 250 } else { 3000 };
            slint::Timer::single_shot(std::time::Duration::from_millis(wait), move || attempt(key, parts, n + 1));
        }
        attempt(key, parts, 0);
    }

    /// X Finder is quitting.
    pub(super) fn release_open_key(&self) {
        let _ = super::instance::set_open_hotkey(None);
    }
}

pub(super) fn start_menu_link() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join(r"Microsoft\Windows\Start Menu\Programs\X Finder.lnk"))
}

/// "Ctrl+Alt+E" → RegisterHotKey's (MOD_ALT 1 / MOD_CONTROL 2 / MOD_SHIFT 4,
/// virtual key). Needs Ctrl or Alt, so plain typing is never taken over.
fn register_parts(key: &str) -> Option<(u32, u32)> {
    let parts: Vec<&str> = if key.ends_with("++") { vec![] } else { key.split('+').collect() };
    let (name, mods) = parts.split_last()?;
    let mut flags = 0u32;
    for m in mods {
        flags |= match *m {
            "Alt" => 1,
            "Ctrl" => 2,
            "Shift" => 4,
            _ => return None,
        };
    }
    if flags & 3 == 0 {
        return None;
    }
    let named: &[(&str, u32)] = &[
        ("Space", 0x20),
        ("Enter", 0x0D),
        ("Tab", 0x09),
        ("Backspace", 0x08),
        ("Insert", 0x2D),
        ("Delete", 0x2E),
        ("Home", 0x24),
        ("End", 0x23),
        ("PageUp", 0x21),
        ("PageDown", 0x22),
        ("Left", 0x25),
        ("Up", 0x26),
        ("Right", 0x27),
        ("Down", 0x28),
        ("=", 0xBB),
        ("-", 0xBD),
        (",", 0xBC),
        (".", 0xBE),
        ("/", 0xBF),
        (";", 0xBA),
        ("`", 0xC0),
        ("[", 0xDB),
        ("\\", 0xDC),
        ("]", 0xDD),
        ("'", 0xDE),
    ];
    let vk = match *name {
        n if n.len() == 1 && n.chars().all(|c| c.is_ascii_alphanumeric()) => n.chars().next()?.to_ascii_uppercase() as u32,
        n if n.starts_with('F') && n.len() > 1 && n[1..].chars().all(|c| c.is_ascii_digit()) => {
            let f: u32 = n[1..].parse().ok()?;
            if !(1..=24).contains(&f) {
                return None;
            }
            0x6F + f
        }
        n => named.iter().find(|(k, _)| *k == n)?.1,
    };
    Some((flags, vk))
}

#[cfg(test)]
mod tests {
    use super::register_parts;

    #[test]
    fn hotkeys() {
        assert_eq!(register_parts("Ctrl+Alt+E"), Some((3, 0x45)));
        assert_eq!(register_parts("Ctrl+Y"), Some((2, 0x59)));
        assert_eq!(register_parts("Ctrl+Alt+Space"), Some((3, 0x20)));
        assert_eq!(register_parts("Alt+F9"), Some((1, 0x78)));
        assert_eq!(register_parts("Shift+E"), None);
        assert_eq!(register_parts("E"), None);
    }
}
