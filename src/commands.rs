//! Every user action as a command object (plan §6 M4 groundwork).
//!
//! Keys map to commands here; the command palette and rebindable keys can
//! later reuse the same `Command` list without touching the UI.

use slint::platform::Key;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Back,
    Forward,
    Up,
    Refresh,
    Open,
    OpenInNewTab,
    NewTab,
    CloseTab,
    NextTab,
    PrevTab,
    MoveTabLeft,
    MoveTabRight,
    ToggleHidden,
    SelectAll,
    Escape,
    ToggleSelectAtCursor,
    DensityTighter,
    DensityLooser,
    ViewList,
    ViewIcons,
    ViewColumns,
    ViewGallery,
    QuickLook,
    TogglePreviewPane,
    Copy,
    Cut,
    Paste,
    Duplicate,
    Undo,
    Rename,
    NewFolder,
    Recycle,
    DeleteForever,
    CopyPath,
    Properties,
    /// Moves the cursor; `extend` grows the selection (Shift).
    CursorBy { delta: i32, extend: bool },
    /// Left/Right: previous/next tile in icon view; up/into a folder in column view.
    CursorSide { forward: bool, extend: bool },
    /// Tags / search / Recents: open the selected item's folder, selected.
    ShowEnclosing,
    /// Command palette (Ctrl+Shift+P) and quick jump to a folder (Ctrl+G).
    Palette,
    GoTo,
    /// The how-to guide (F1).
    Help,
    CursorPage { down: bool, extend: bool },
    CursorHome { extend: bool },
    CursorEnd { extend: bool },
    /// Settings page (Ctrl+,).
    Settings,
    /// Toolbar "folder sizes" mode.
    FolderSizes,
    /// The search box (Ctrl+F) and typing a path (Ctrl+L).
    Search,
    EditPath,
    /// Quits for real (closing the window only hides it while an "open X
    /// Finder" key is set).
    Quit,
}

impl Command {
    /// Human-readable name, for the future command palette.
    #[allow(dead_code)]
    pub fn title(self) -> &'static str {
        match self {
            Command::Back => "Go Back",
            Command::Forward => "Go Forward",
            Command::Up => "Enclosing Folder",
            Command::Refresh => "Refresh",
            Command::Open => "Open",
            Command::OpenInNewTab => "Open in New Tab",
            Command::NewTab => "New Tab",
            Command::CloseTab => "Close Tab",
            Command::NextTab => "Next Tab",
            Command::PrevTab => "Previous Tab",
            Command::MoveTabLeft => "Move Tab Left",
            Command::MoveTabRight => "Move Tab Right",
            Command::ToggleHidden => "Show/Hide Hidden Files",
            Command::SelectAll => "Select All",
            Command::Escape => "Clear Filter / Selection",
            Command::ToggleSelectAtCursor => "Toggle Selection",
            Command::DensityTighter => "Denser Rows",
            Command::DensityLooser => "Roomier Rows",
            Command::ViewList => "View as List",
            Command::ViewIcons => "View as Icons",
            Command::ViewColumns => "View as Columns",
            Command::ViewGallery => "View as Gallery",
            Command::QuickLook => "Quick Look",
            Command::TogglePreviewPane => "Show/Hide Preview",
            Command::Copy => "Copy",
            Command::Cut => "Cut",
            Command::Paste => "Paste",
            Command::Duplicate => "Duplicate",
            Command::Undo => "Undo",
            Command::Rename => "Rename",
            Command::NewFolder => "New Folder",
            Command::Recycle => "Move to Recycle Bin",
            Command::DeleteForever => "Delete Permanently",
            Command::CopyPath => "Copy Path",
            Command::Properties => "Properties",
            Command::ShowEnclosing => "Show in Enclosing Folder",
            Command::Palette => "Command Palette",
            Command::GoTo => "Go to Folder…",
            Command::Help => "Help: How to Use",
            Command::Settings => "Settings",
            Command::FolderSizes => "Show/Hide Folder Sizes",
            Command::Search => "Search",
            Command::EditPath => "Type a Path",
            Command::Quit => "Quit X Finder",
            Command::CursorBy { .. }
            | Command::CursorSide { .. }
            | Command::CursorPage { .. }
            | Command::CursorHome { .. }
            | Command::CursorEnd { .. } => "Move Selection",
        }
    }
}

fn is(c: char, key: Key) -> bool {
    c == char::from(key)
}

/// Actions people can give their own key (Settings → Keyboard shortcuts):
/// (id saved in settings, command, default key).
pub const BINDABLE: &[(&str, Command, &str)] = {
    use Command::*;
    &[
        ("palette", Palette, "Ctrl+Shift+P"),
        ("search", Search, "Ctrl+F"),
        ("goto", GoTo, "Ctrl+G"),
        ("edit-path", EditPath, "Ctrl+L"),
        ("quick-look", QuickLook, "Space"),
        ("preview", TogglePreviewPane, "Ctrl+P"),
        ("view-list", ViewList, "Ctrl+1"),
        ("view-icons", ViewIcons, "Ctrl+2"),
        ("view-columns", ViewColumns, "Ctrl+3"),
        ("view-gallery", ViewGallery, "Ctrl+4"),
        ("folder-sizes", FolderSizes, ""),
        ("hidden", ToggleHidden, "Ctrl+H"),
        ("back", Back, "Alt+Left"),
        ("forward", Forward, "Alt+Right"),
        ("up", Up, "Alt+Up"),
        ("refresh", Refresh, "F5"),
        ("show-enclosing", ShowEnclosing, ""),
        ("new-tab", NewTab, "Ctrl+T"),
        ("close-tab", CloseTab, "Ctrl+W"),
        ("next-tab", NextTab, "Ctrl+Tab"),
        ("prev-tab", PrevTab, "Ctrl+Shift+Tab"),
        ("open-new-tab", OpenInNewTab, "Ctrl+Enter"),
        ("new-folder", NewFolder, "Ctrl+Shift+N"),
        ("rename", Rename, "F2"),
        ("copy", Copy, "Ctrl+C"),
        ("cut", Cut, "Ctrl+X"),
        ("paste", Paste, "Ctrl+V"),
        ("duplicate", Duplicate, "Ctrl+D"),
        ("undo", Undo, "Ctrl+Z"),
        ("copy-path", CopyPath, "Ctrl+Shift+C"),
        ("recycle", Recycle, "Delete"),
        ("delete-forever", DeleteForever, "Shift+Delete"),
        ("properties", Properties, "Alt+Enter"),
        ("select-all", SelectAll, "Ctrl+A"),
        ("bigger", DensityLooser, "Ctrl+="),
        ("smaller", DensityTighter, "Ctrl+-"),
        ("settings", Settings, "Ctrl+,"),
        ("help", Help, "F1"),
        ("quit", Quit, ""),
    ]
};

thread_local! {
    /// The user's own keys (id → key; "" = no key), from settings.
    static USER_KEYS: std::cell::RefCell<std::collections::BTreeMap<String, String>> = Default::default();
}

pub fn set_user_keys(keys: &std::collections::BTreeMap<String, String>) {
    USER_KEYS.with(|k| *k.borrow_mut() = keys.clone());
}

/// The key an action has now ("" = none).
pub fn key_for_id(id: &str) -> String {
    let default = BINDABLE.iter().find(|b| b.0 == id).map_or("", |b| b.2);
    USER_KEYS.with(|k| k.borrow().get(id).cloned()).unwrap_or_else(|| default.to_string())
}

/// The key a command has now, for menus and the palette.
pub fn key_for(cmd: Command) -> Option<String> {
    BINDABLE.iter().find(|b| b.1 == cmd).map(|b| key_for_id(b.0))
}

/// "Ctrl+Shift+P" for a key event; None for a lone modifier key.
pub fn combo(text: &str, ctrl: bool, alt: bool, shift: bool) -> Option<String> {
    let mut chars = text.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    let named = [
        (Key::Return, "Enter"),
        (Key::Tab, "Tab"),
        (Key::Backtab, "Tab"),
        (Key::Backspace, "Backspace"),
        (Key::Delete, "Delete"),
        (Key::Insert, "Insert"),
        (Key::Escape, "Esc"),
        (Key::LeftArrow, "Left"),
        (Key::RightArrow, "Right"),
        (Key::UpArrow, "Up"),
        (Key::DownArrow, "Down"),
        (Key::Home, "Home"),
        (Key::End, "End"),
        (Key::PageUp, "PageUp"),
        (Key::PageDown, "PageDown"),
        (Key::F1, "F1"),
        (Key::F2, "F2"),
        (Key::F3, "F3"),
        (Key::F4, "F4"),
        (Key::F5, "F5"),
        (Key::F6, "F6"),
        (Key::F7, "F7"),
        (Key::F8, "F8"),
        (Key::F9, "F9"),
        (Key::F10, "F10"),
        (Key::F11, "F11"),
        (Key::F12, "F12"),
    ];
    let modifiers = [Key::Shift, Key::ShiftR, Key::Control, Key::ControlR, Key::Alt, Key::AltGr, Key::Meta, Key::MetaR];
    if modifiers.iter().any(|m| is(c, *m)) {
        return None;
    }
    let shift = shift || is(c, Key::Backtab);
    let name = match named.iter().find(|(k, _)| is(c, *k)) {
        Some((_, n)) => n.to_string(),
        None if c == ' ' => "Space".into(),
        // Other keys Slint names with private-use characters: not bindable.
        None if c.is_control() || ('\u{F700}'..='\u{F8FF}').contains(&c) => return None,
        None => match c {
            '+' => "=".into(),
            '_' => "-".into(),
            c => c.to_uppercase().to_string(),
        },
    };
    let mut s = String::new();
    if ctrl {
        s.push_str("Ctrl+");
    }
    if alt {
        s.push_str("Alt+");
    }
    if shift && !(name.chars().count() == 1 && !name.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())) {
        s.push_str("Shift+");
    }
    s.push_str(&name);
    Some(s)
}

/// The command for a key, with the user's own keys applied: an action
/// given another key no longer answers to its old one.
pub fn lookup(text: &str, ctrl: bool, alt: bool, shift: bool) -> Option<Command> {
    if let Some(k) = combo(text, ctrl, alt, shift) {
        if let Some(b) = BINDABLE.iter().find(|b| key_for_id(b.0).eq_ignore_ascii_case(&k)) {
            return Some(b.1);
        }
    }
    let cmd = from_key(text, ctrl, alt, shift)?;
    // Built-in key of an action the user moved elsewhere (or another
    // spelling of a key that's now taken).
    let customised = BINDABLE.iter().any(|b| b.1 == cmd && USER_KEYS.with(|k| k.borrow().contains_key(b.0)));
    (!customised).then_some(cmd)
}

/// Keyboard map (plan §7). Returns `None` for keys that aren't shortcuts,
/// which then fall through to type-to-select.
pub fn from_key(text: &str, ctrl: bool, alt: bool, shift: bool) -> Option<Command> {
    let mut chars = text.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    let lower = c.to_ascii_lowercase();
    use Command::*;

    if alt && !ctrl {
        return match c {
            _ if is(c, Key::Return) => Some(Properties),
            _ if is(c, Key::LeftArrow) => Some(Back),
            _ if is(c, Key::RightArrow) => Some(Forward),
            _ if is(c, Key::UpArrow) => Some(Up),
            _ => None,
        };
    }

    if ctrl {
        return match lower {
            'c' if shift => Some(CopyPath),
            'c' => Some(Copy),
            'x' => Some(Cut),
            'v' => Some(Paste),
            'd' => Some(Duplicate),
            'z' => Some(Undo),
            'n' if shift => Some(NewFolder),
            't' => Some(NewTab),
            'w' => Some(CloseTab),
            'a' => Some(SelectAll),
            'h' => Some(ToggleHidden),
            'r' => Some(Refresh),
            '1' => Some(ViewList),
            '2' => Some(ViewIcons),
            '3' => Some(ViewColumns),
            '4' => Some(ViewGallery),
            'p' if shift => Some(Palette),
            'p' => Some(TogglePreviewPane),
            'g' => Some(GoTo),
            '=' | '+' => Some(DensityLooser),
            '-' | '_' => Some(DensityTighter),
            ' ' => Some(ToggleSelectAtCursor),
            '\t' => Some(if shift { PrevTab } else { NextTab }),
            _ if is(c, Key::Backtab) => Some(PrevTab),
            _ if is(c, Key::PageUp) => Some(if shift { MoveTabLeft } else { PrevTab }),
            _ if is(c, Key::PageDown) => Some(if shift { MoveTabRight } else { NextTab }),
            _ if is(c, Key::Return) => Some(OpenInNewTab),
            _ if is(c, Key::UpArrow) => Some(Up),
            _ if is(c, Key::DownArrow) => Some(Open),
            _ => None,
        };
    }

    match c {
        _ if is(c, Key::UpArrow) => Some(CursorBy { delta: -1, extend: shift }),
        _ if is(c, Key::DownArrow) => Some(CursorBy { delta: 1, extend: shift }),
        _ if is(c, Key::LeftArrow) => Some(CursorSide { forward: false, extend: shift }),
        _ if is(c, Key::RightArrow) => Some(CursorSide { forward: true, extend: shift }),
        _ if is(c, Key::PageUp) => Some(CursorPage { down: false, extend: shift }),
        _ if is(c, Key::PageDown) => Some(CursorPage { down: true, extend: shift }),
        _ if is(c, Key::Home) => Some(CursorHome { extend: shift }),
        _ if is(c, Key::End) => Some(CursorEnd { extend: shift }),
        _ if is(c, Key::Return) => Some(Open),
        _ if is(c, Key::Backspace) => Some(Up),
        _ if is(c, Key::Escape) => Some(Escape),
        _ if is(c, Key::F5) => Some(Refresh),
        _ if is(c, Key::F2) => Some(Rename),
        _ if is(c, Key::F1) => Some(Help),
        _ if is(c, Key::Delete) => Some(if shift { DeleteForever } else { Recycle }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(key: Key) -> String {
        char::from(key).to_string()
    }

    #[test]
    fn plan_keymap() {
        assert_eq!(from_key("t", true, false, false), Some(Command::NewTab));
        assert_eq!(from_key("W", true, false, true), Some(Command::CloseTab));
        assert_eq!(from_key(&k(Key::LeftArrow), false, true, false), Some(Command::Back));
        assert_eq!(from_key(&k(Key::UpArrow), false, true, false), Some(Command::Up));
        assert_eq!(from_key(&k(Key::Return), false, false, false), Some(Command::Open));
        assert_eq!(from_key(&k(Key::Backspace), false, false, false), Some(Command::Up));
        assert_eq!(
            from_key(&k(Key::DownArrow), false, false, true),
            Some(Command::CursorBy { delta: 1, extend: true })
        );
        assert_eq!(from_key("x", false, false, false), None);
        assert_eq!(from_key("2", true, false, false), Some(Command::ViewIcons));
        assert_eq!(from_key("4", true, false, false), Some(Command::ViewGallery));
        assert_eq!(from_key("c", true, false, false), Some(Command::Copy));
        assert_eq!(from_key("N", true, false, true), Some(Command::NewFolder));
        assert_eq!(from_key(&k(Key::Delete), false, false, true), Some(Command::DeleteForever));
        assert_eq!(from_key(&k(Key::F2), false, false, false), Some(Command::Rename));
        assert_eq!(
            from_key(&k(Key::RightArrow), false, false, false),
            Some(Command::CursorSide { forward: true, extend: false })
        );
    }
}
