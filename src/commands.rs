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
