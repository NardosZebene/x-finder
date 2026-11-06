//! The app's own context menu (dark/light, Finder-like, without Mac-only
//! items), the native Windows menu, colour tags, Send To / Open With,
//! sidebar favourites and the Settings page.

use std::path::PathBuf;
use std::time::Duration;

use finder_rs::core::fileops::{self, Op};
use finder_rs::core::location::Location;
use finder_rs::core::settings::{Appearance, IconStyle, MenuStyle};
use finder_rs::core::shellx::{self, NativeOutcome, OpenWithApp, SendTarget};
use finder_rs::core::sort::SortKey;
use finder_rs::core::tags;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use super::{App, ViewMode, build_places, with_app};
use crate::commands::Command;
use crate::{Bridge, MenuEntry};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MenuCtx {
    /// Right-click on files/folders (the selection).
    Items,
    /// Right-click on empty space in a folder.
    Background,
    /// Right-click on a sidebar place.
    Place(usize),
    /// Right-click on the list header (choose columns).
    Header,
    /// Right-click on a tab.
    Tab(usize),
}

pub(super) struct MenuState {
    ctx: MenuCtx,
    items: Vec<MenuEntry>,
    /// What the menu acts on.
    paths: Vec<PathBuf>,
    open_with: Vec<OpenWithApp>,
    send_to: Vec<SendTarget>,
}

fn entry(id: &str, label: impl Into<SharedString>, icon: &str, shortcut: &str, enabled: bool) -> MenuEntry {
    MenuEntry {
        id: id.into(),
        label: label.into(),
        icon: icon.into(),
        shortcut: shortcut.into(),
        kind: 0,
        enabled,
        checked: false,
        tags: 0,
    }
}

fn check(id: &str, label: &str, checked: bool) -> MenuEntry {
    MenuEntry { checked, ..entry(id, label, "", "", true) }
}

fn sep() -> MenuEntry {
    MenuEntry { kind: 1, ..entry("", "", "", "", false) }
}

fn submenu(id: &str, label: &str, icon: &str) -> MenuEntry {
    MenuEntry { kind: 2, ..entry(id, label, icon, "", true) }
}

fn tag_row(mask: u8) -> MenuEntry {
    MenuEntry { kind: 3, tags: mask as i32, ..entry("tags", "", "", "", true) }
}

fn quoted_name(paths: &[PathBuf]) -> String {
    match paths {
        [one] => format!("“{}”", one.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
        many => format!("{} Items", many.len()),
    }
}

impl App {
    /// Windows menu if chosen in Settings; Shift flips to the other one.
    fn wants_native_menu(&self) -> bool {
        let shift = fileops::modifiers().1;
        (self.settings.menu_style == MenuStyle::Windows) != shift
    }

    // ---- opening ---------------------------------------------------------------

    pub(super) fn open_items_menu(&mut self, x: f32, y: f32) {
        let paths = self.selected_paths();
        if self.tab().loc == Location::Trash {
            let any = !paths.is_empty();
            let items = vec![
                entry("put-back", "Put Back", "undo", "", any),
                entry("delete-now", "Delete Immediately…", "trash", "", any),
                entry("quicklook", "Quick Look", "quicklook", "Space", any),
                sep(),
                entry("empty-trash", "Empty Recycle Bin", "trash", "", true),
            ];
            return self.show_menu(MenuCtx::Items, items, paths, x, y);
        }
        if paths.is_empty() {
            return self.open_background_menu(x, y);
        }
        // Tags, search results and Recents mix folders: offer the real one.
        let scattered = !matches!(self.tab().loc, Location::Dir(_));
        if self.wants_native_menu() {
            return self.native_menu(paths, None, x, y);
        }
        let single = paths.len() == 1;
        let one_dir = single && paths[0].is_dir();
        let any_file = paths.iter().any(|p| !p.is_dir());
        let model = self.model();
        let masks: Vec<u8> = model.selected().iter().map(|&i| model.tags_at(i)).collect();
        // Tags shared by every selected item show as set.
        let common = masks.iter().fold(0x7f, |acc, m| acc & m);
        let name = quoted_name(&paths);

        let mut items = vec![entry("open", "Open", "open", "Enter", true)];
        if scattered && single {
            items.push(entry("enclosing", "Show in Enclosing Folder", "folder", "Ctrl+R", true));
        }
        if single && any_file {
            items.push(submenu("open-with", "Open With", "openwith"));
        }
        if one_dir {
            items.push(entry("open-tab", "Open in New Tab", "tab", "", true));
        }
        if single && fileops::vscode().is_some() {
            items.push(entry("vscode", "Open in VS Code", "vscode", "", true));
        }
        items.extend([
            sep(),
            entry("recycle", "Move to Recycle Bin", "trash", "Del", true),
            sep(),
            entry("properties", "Properties", "info", "Alt+Enter", true),
            entry("rename", "Rename…", "rename", "F2", single),
            entry("compress", format!("Compress {name}"), "zip", "", true),
            entry("duplicate", "Duplicate", "duplicate", "Ctrl+D", true),
            entry("shortcut", "Create Shortcut", "shortcut", "", true),
            entry("quicklook", format!("Quick Look {name}"), "quicklook", "Space", true),
            submenu("send-to", "Send To", "send"),
        ]);
        if one_dir {
            items.push(entry("add-sidebar", "Add to Sidebar", "pin", "", true));
        }
        items.extend([
            sep(),
            entry("cut", "Cut", "cut", "Ctrl+X", true),
            entry("copy", format!("Copy {name}"), "copy", "Ctrl+C", true),
            entry("copy-path", "Copy Path", "path", "Ctrl+Shift+C", true),
            sep(),
            submenu("view", "View & Sort", "sort"),
            sep(),
            tag_row(common),
        ]);
        if masks.iter().any(|&m| m != 0) {
            items.push(entry("remove-tags", "Remove Tags", "tag", "", true));
        }
        items.extend([
            sep(),
            entry("reveal", "Show in Windows Explorer", "folder", "", true),
            entry("more", "More Options…", "more", "Shift+Right-click", true),
        ]);
        self.show_menu(MenuCtx::Items, items, paths, x, y);
    }

    pub(super) fn open_background_menu(&mut self, x: f32, y: f32) {
        if self.tab().loc == Location::Trash {
            let items = vec![entry("empty-trash", "Empty Recycle Bin", "trash", "", true)];
            return self.show_menu(MenuCtx::Background, items, Vec::new(), x, y);
        }
        let dir = self.current_dir();
        if let Some(d) = &dir
            && self.wants_native_menu()
        {
            return self.native_menu(Vec::new(), Some(d.clone()), x, y);
        }
        let in_folder = dir.is_some();
        let can_paste = in_folder && fileops::clipboard_files().is_some();
        let items = vec![
            entry("new-folder", "New Folder", "newfolder", "Ctrl+Shift+N", in_folder),
            entry("paste", "Paste", "paste", "Ctrl+V", can_paste),
            sep(),
            submenu("view", "View & Sort", "sort"),
            sep(),
            entry("terminal", "Open in Terminal", "terminal", "", in_folder),
            entry("reveal", "Show in Windows Explorer", "folder", "", in_folder),
            entry("copy-path", "Copy Path", "path", "", in_folder),
            entry("add-sidebar", "Add to Sidebar", "pin", "", in_folder),
            entry("properties", "Properties", "info", "", in_folder),
            sep(),
            entry("settings", "Settings…", "settings", "Ctrl+,", true),
            entry("more", "More Options…", "more", "Shift+Right-click", in_folder),
        ];
        let mut items = items;
        if in_folder && fileops::vscode().is_some() {
            let at = items.iter().position(|e| e.id == "terminal").map_or(items.len(), |i| i + 1);
            items.insert(at, entry("vscode", "Open in VS Code", "vscode", "", true));
        }
        self.show_menu(MenuCtx::Background, items, dir.into_iter().collect(), x, y);
    }

    pub(super) fn open_place_menu(&mut self, i: usize, x: f32, y: f32) {
        let Some(place) = self.places.get(i) else { return };
        let Some(loc) = place.loc.clone() else { return };
        let favorite = place.favorite;
        if loc == Location::Trash {
            let items = vec![
                entry("open", "Open", "open", "", true),
                sep(),
                entry("empty-trash", "Empty Recycle Bin", "trash", "", true),
            ];
            return self.show_menu(MenuCtx::Place(i), items, Vec::new(), x, y);
        }
        if loc == Location::Recent {
            let items = vec![
                entry("open", "Open", "open", "", true),
                entry("open-tab", "Open in New Tab", "tab", "", true),
                sep(),
                entry("hide-recents", "Hide Recents", "unpin", "", true),
            ];
            return self.show_menu(MenuCtx::Place(i), items, Vec::new(), x, y);
        }
        if let Location::Tag(t) = loc {
            let items = vec![
                entry("open", "Show Tagged Items", "tag", "", true),
                entry("open-tab", "Open in New Tab", "tab", "", true),
                sep(),
                entry(&format!("rename-tag:{t}"), "Rename Tag…", "rename", "", true),
            ];
            return self.show_menu(MenuCtx::Place(i), items, Vec::new(), x, y);
        }
        let path = match &loc {
            Location::Dir(p) => Some(p.clone()),
            _ => None,
        };
        if let Some(p) = &path
            && self.wants_native_menu()
        {
            return self.native_menu(vec![p.clone()], None, x, y);
        }
        let real = path.is_some();
        let mut items = vec![
            entry("open", "Open", "open", "", true),
            entry("open-tab", "Open in New Tab", "tab", "", true),
            sep(),
            entry("reveal", "Show in Windows Explorer", "folder", "", real),
            entry("terminal", "Open in Terminal", "terminal", "", real),
            entry("properties", "Properties", "info", "", real),
        ];
        if favorite {
            let favs: Vec<usize> = self.places.iter().enumerate().filter(|(_, p)| p.favorite).map(|(j, _)| j).collect();
            let first = favs.first() == Some(&i);
            let last = favs.last() == Some(&i);
            items.extend([
                sep(),
                entry("fav-up", "Move Up", "sort", "", !first),
                entry("fav-down", "Move Down", "sort", "", !last),
                entry("remove-sidebar", "Remove from Sidebar", "unpin", "", true),
            ]);
        }
        if real {
            items.extend([sep(), entry("more", "More Options…", "more", "Shift+Right-click", true)]);
        }
        self.show_menu(MenuCtx::Place(i), items, path.into_iter().collect(), x, y);
    }

    /// Right-click on a This PC card.
    pub(super) fn open_pc_menu(&mut self, section: i32, index: usize, x: f32, y: f32) {
        let Some(path) = self.pc_path(section, index) else { return };
        if self.wants_native_menu() {
            return self.native_menu(vec![path], None, x, y);
        }
        let items = vec![
            entry("pc-open", "Open", "open", "", true),
            entry("pc-tab", "Open in New Tab", "tab", "", true),
            sep(),
            entry("reveal", "Show in Windows Explorer", "folder", "", true),
            entry("terminal", "Open in Terminal", "terminal", "", true),
            entry("properties", "Properties", "info", "", true),
            sep(),
            entry("more", "More Options…", "more", "Shift+Right-click", true),
        ];
        self.show_menu(MenuCtx::Place(usize::MAX), items, vec![path], x, y);
    }

    pub(super) fn open_header_menu(&mut self, x: f32, y: f32) {
        let Some(ui) = self.ui() else { return };
        let b = ui.global::<Bridge>();
        let items = vec![
            check("col:date", "Date Modified", b.get_show_date()),
            check("col:created", "Date Created", b.get_show_created()),
            check("col:size", "Size", b.get_show_size()),
            check("col:kind", "Kind", b.get_show_kind()),
        ];
        self.show_menu(MenuCtx::Header, items, Vec::new(), x, y);
    }

    /// Right-click on a tab: close tabs, save / open tab groups.
    pub(super) fn open_tab_menu(&mut self, i: usize, x: f32, y: f32) {
        let many = self.tabs.len() > 1;
        let groups = !self.settings.tab_groups.is_empty();
        let items = vec![
            entry("new-tab", "New Tab", "tab", "Ctrl+T", true),
            entry("close-tab", "Close Tab", "close", "Ctrl+W", many),
            entry("close-others", "Close Other Tabs", "close", "", many),
            sep(),
            entry("save-group", "Save Tabs as Group…", "plus", "", true),
            MenuEntry { enabled: groups, ..submenu("groups", "Tab Groups", "tab") },
            MenuEntry { enabled: groups, ..submenu("del-groups", "Delete Group", "trash") },
        ];
        self.show_menu(MenuCtx::Tab(i), items, Vec::new(), x, y);
    }

    /// Narrow windows: the toolbar buttons that didn't fit.
    pub(super) fn open_toolbar_more(&mut self, x: f32, y: f32) {
        let items = vec![
            entry("tb-layout", "Split Layout…", "layout", "", true),
            check("tb-sizes", "Folder Sizes", self.folder_sizes),
            check("toggle-hidden", "Show Hidden Files", self.opts.show_hidden),
            sep(),
            entry("tb-refresh", "Refresh", "refresh", "F5", true),
            entry("tb-palette", "Command Palette", "terminal", "Ctrl+Shift+P", true),
        ];
        // Right edge under the button.
        self.show_menu(MenuCtx::Background, items, Vec::new(), (x + 30.0 - 250.0).max(8.0), y);
    }

    fn show_menu(&mut self, ctx: MenuCtx, items: Vec<MenuEntry>, paths: Vec<PathBuf>, x: f32, y: f32) {
        let Some(ui) = self.ui() else { return };
        let b = ui.global::<Bridge>();
        b.set_menu_items(ModelRc::new(VecModel::from(items.clone())));
        b.set_sub_index(-1);
        b.set_menu_x(x);
        b.set_menu_y(y);
        b.set_menu_open(true);
        self.menu = Some(MenuState { ctx, items, paths, open_with: Vec::new(), send_to: Vec::new() });
    }

    pub(super) fn menu_close(&mut self) {
        self.menu = None;
        if let Some(ui) = self.ui() {
            let b = ui.global::<Bridge>();
            b.set_menu_open(false);
            b.set_sub_index(-1);
        }
    }

    /// Hovering a row: open its submenu (or close the open one).
    pub(super) fn menu_hover(&mut self, index: usize) {
        let Some(ui) = self.ui() else { return };
        let b = ui.global::<Bridge>();
        let Some(state) = self.menu.as_mut() else { return };
        let Some(item) = state.items.get(index).cloned() else { return };
        if item.kind != 2 {
            if item.kind != 1 && b.get_sub_index() != -1 {
                b.set_sub_index(-1);
            }
            return;
        }
        if b.get_sub_index() == index as i32 || !item.enabled {
            return;
        }
        let sub: Vec<MenuEntry> = match item.id.as_str() {
            "open-with" => {
                state.open_with = state.paths.first().map(|p| shellx::open_with_apps(p)).unwrap_or_default();
                let mut v: Vec<MenuEntry> =
                    state.open_with.iter().map(|a| entry(&format!("openwith:{}", a.index), a.name.as_str(), "", "", true)).collect();
                if !v.is_empty() {
                    v.push(sep());
                }
                v.push(entry("openwith-other", "Choose Another App…", "openwith", "", true));
                v
            }
            "send-to" => {
                // Keep the useful ones: zip, desktop shortcut, folders, drives
                // and Bluetooth (drop Fax and installed apps).
                state.send_to = shellx::send_to_targets()
                    .into_iter()
                    .filter(|t| !matches!(t, SendTarget::App { .. }) || t.label().to_lowercase().contains("bluetooth"))
                    .collect();
                // Drives use the same names as the sidebar ("Ventoy (E:)").
                for t in &mut state.send_to {
                    if let SendTarget::Folder { label, path } = t
                        && path.parent().is_none()
                    {
                        let name = path.to_string_lossy().trim_end_matches('\\').to_string();
                        *label = super::places::drive_label(&name, 2, &self.volumes);
                    }
                }
                state
                    .send_to
                    .iter()
                    .enumerate()
                    .map(|(i, t)| {
                        let icon = match t {
                            SendTarget::Zip => "zip",
                            SendTarget::DesktopShortcut => "shortcut",
                            SendTarget::Folder { path, .. } if path.parent().is_none() => "drive",
                            SendTarget::Folder { .. } => "folder",
                            SendTarget::App { .. } => "open",
                        };
                        entry(&format!("sendto:{i}"), t.label(), icon, "", true)
                    })
                    .collect()
            }
            "groups" | "del-groups" => {
                let open = item.id.as_str() == "groups";
                self.settings
                    .tab_groups
                    .iter()
                    .enumerate()
                    .map(|(k, g)| {
                        let n = g.tabs.len();
                        let label = format!("{}  ·  {n} tab{}", g.name, if n == 1 { "" } else { "s" });
                        let id = if open { format!("open-group:{k}") } else { format!("delete-group:{k}") };
                        let key = if open { super::groups::shortcut_label(g.shortcut) } else { String::new() };
                        entry(&id, label, if open { "folder" } else { "trash" }, &key, true)
                    })
                    .collect()
            }
            "view" => {
                let view = self.view;
                let sort = self.opts.sort;
                vec![
                    check("view:0", "as List", view == ViewMode::List),
                    check("view:1", "as Icons", view == ViewMode::Icons),
                    check("view:2", "as Columns", view == ViewMode::Columns),
                    check("view:3", "as Gallery", view == ViewMode::Gallery),
                    sep(),
                    check("sort:0", "Sort by Name", sort == SortKey::Name),
                    check("sort:1", "Sort by Date Modified", sort == SortKey::Modified),
                    check("sort:4", "Sort by Date Created", sort == SortKey::Created),
                    check("sort:2", "Sort by Size", sort == SortKey::Size),
                    check("sort:3", "Sort by Kind", sort == SortKey::Kind),
                    check("sort-desc", "Descending", !self.opts.ascending),
                    sep(),
                    check("toggle-hidden", "Show Hidden Files", self.opts.show_hidden),
                    check("toggle-preview", "Show Preview", self.preview_pane),
                    check("folders-first", "Folders on Top", self.settings.folders_first),
                ]
            }
            _ => return,
        };
        b.set_sub_items(ModelRc::new(VecModel::from(sub)));
        b.set_sub_index(index as i32);
    }

    // ---- actions ------------------------------------------------------------------

    pub(super) fn menu_pick(&mut self, id: &str) {
        let Some(state) = self.menu.take() else { return };
        self.menu_close();
        let paths = state.paths;
        let first = paths.first().cloned();
        let place = match state.ctx {
            MenuCtx::Place(i) if i != usize::MAX => Some(i),
            _ => None,
        };
        let (key, arg) = id.split_once(':').map_or((id, None), |(k, a)| (k, a.parse::<usize>().ok()));
        match (key, arg) {
            ("open", _) if place.is_some() => self.sidebar_clicked(place.unwrap()),
            ("open-tab", _) if place.is_some() => {
                if let Some(loc) = self.places.get(place.unwrap()).and_then(|p| p.loc.clone()) {
                    self.open_tab(loc, true);
                }
            }
            ("open", _) => self.run(Command::Open),
            ("open-tab", _) => self.run(Command::OpenInNewTab),
            ("openwith", Some(i)) => {
                if let Some(p) = &first
                    && let Err(e) = shellx::open_with(p, i)
                {
                    self.note(e);
                }
            }
            ("openwith-other", _) => {
                if let Some(p) = first {
                    // Opened after the menu closes; the dialog is modal.
                    slint::Timer::single_shot(Duration::from_millis(10), move || shellx::open_with_dialog(&p));
                }
            }
            ("recycle", _) => self.run(Command::Recycle),
            ("properties", _) => match first {
                Some(p) => self.properties_of(p),
                None => self.run(Command::Properties),
            },
            ("rename", _) => self.run(Command::Rename),
            ("compress", _) => self.compress(paths),
            ("duplicate", _) => self.run(Command::Duplicate),
            ("shortcut", _) => self.create_shortcuts(&paths),
            ("quicklook", _) => {
                if !self.quicklook {
                    self.run(Command::QuickLook);
                }
            }
            ("sendto", Some(i)) => {
                if let Some(target) = state.send_to.get(i).cloned() {
                    self.send_to(target, paths);
                }
            }
            ("add-sidebar", _) => {
                if let Some(p) = first {
                    self.add_favorite(p);
                }
            }
            ("remove-sidebar", _) => {
                if let Some(i) = place {
                    self.remove_favorite(i);
                }
            }
            ("cut", _) => self.run(Command::Cut),
            ("copy", _) => self.run(Command::Copy),
            ("paste", _) => self.run(Command::Paste),
            ("copy-path", _) => match (state.ctx, first) {
                (MenuCtx::Items, _) | (_, None) => self.run(Command::CopyPath),
                (_, Some(p)) => match fileops::set_clipboard_text(&p.display().to_string()) {
                    Ok(()) => self.note("Copied path"),
                    Err(e) => self.note(e),
                },
            },
            ("view", Some(v)) => {
                self.set_view(ViewMode::from_index(v as i32));
                self.remember_folder_view();
            }
            ("sort", Some(k)) => self.sort_by(SortKey::from_index(k as i32)),
            ("sort-desc", _) => {
                self.opts.ascending = !self.opts.ascending;
                self.apply_opts(true);
                self.remember_folder_view();
            }
            ("toggle-hidden", _) => self.run(Command::ToggleHidden),
            ("tb-sizes", _) => self.toggle_folder_sizes(),
            ("tb-refresh", _) => self.run(Command::Refresh),
            ("tb-palette", _) => self.palette_show(""),
            ("tb-layout", _) => {
                if let Some(ui) = self.ui() {
                    let b = ui.global::<Bridge>();
                    b.set_layout_picker_x(b.get_menu_x() + 120.0);
                    b.set_layout_picker_y(b.get_menu_y());
                    b.set_layout_picker_open(true);
                }
            }
            ("toggle-preview", _) => self.run(Command::TogglePreviewPane),
            ("folders-first", _) => {
                self.settings.folders_first = !self.settings.folders_first;
                finder_rs::core::sort::set_folders_first(self.settings.folders_first);
                self.save_settings();
                self.apply_opts(true);
                // Other tabs too.
                for (i, tab) in self.tabs.iter().enumerate() {
                    if i != self.active && !tab.loading {
                        tab.model.sort(&tab.applied);
                    }
                }
                self.sync_columns();
            }
            ("tag", Some(t)) => self.toggle_tag(t),
            ("rename-tag", Some(t)) => self.begin_rename_tag(t),
            ("put-back", _) => self.trash_put_back(),
            ("delete-now", _) => self.trash_delete_now(),
            ("empty-trash", _) => self.trash_empty(),
            ("hide-recents", _) => self.settings_set("recents", 0),
            ("fav-up", _) | ("fav-down", _) => {
                if let Some(i) = place {
                    let favs: Vec<usize> =
                        self.places.iter().enumerate().filter(|(_, p)| p.favorite).map(|(j, _)| j).collect();
                    if let Some(pos) = favs.iter().position(|&j| j == i) {
                        let target = if key == "fav-up" { pos.checked_sub(1) } else { Some(pos + 1) };
                        if let Some(&to) = target.and_then(|t| favs.get(t)) {
                            self.move_favorite(i, to);
                        }
                    }
                }
            }
            ("pc-open", _) => {
                if let Some(p) = first {
                    self.navigate(Location::Dir(p), None);
                }
            }
            ("pc-tab", _) => {
                if let Some(p) = first {
                    self.open_tab(Location::Dir(p), true);
                }
            }
            ("remove-tags", _) => self.clear_tags(),
            ("new-tab", _) => self.run(Command::NewTab),
            ("enclosing", _) => self.run(Command::ShowEnclosing),
            ("close-tab", _) => {
                if let MenuCtx::Tab(i) = state.ctx {
                    self.close_tab(i);
                }
            }
            ("close-others", _) => {
                if let MenuCtx::Tab(i) = state.ctx {
                    self.activate(i);
                    while self.tabs.len() > 1 {
                        self.close_tab(if self.active == 0 { 1 } else { 0 });
                    }
                }
            }
            ("save-group", _) => self.begin_save_tab_group(),
            ("open-group", Some(k)) => self.open_tab_group(k),
            ("delete-group", Some(k)) => self.delete_tab_group(k),
            ("reveal", _) => match (state.ctx, first) {
                (MenuCtx::Background, Some(dir)) => {
                    finder_rs::core::explorer_catch::allow_explorer();
                    let _ = std::process::Command::new("explorer.exe").arg(dir).spawn();
                }
                (_, Some(p)) => fileops::reveal_in_explorer(&p),
                _ => {}
            },
            ("terminal", _) => {
                if let Some(dir) = first.filter(|p| p.is_dir()).or_else(|| self.current_dir()) {
                    fileops::open_terminal(&dir);
                }
            }
            ("vscode", _) => {
                let target = match state.ctx {
                    MenuCtx::Background => first.or_else(|| self.current_dir()),
                    _ => first,
                };
                if let Some(p) = target
                    && !fileops::open_in_vscode(&p)
                {
                    self.toast("VS Code wasn't found. Is it still installed?");
                }
            }
            ("new-folder", _) => self.run(Command::NewFolder),
            ("settings", _) => self.settings_set("open", 1),
            ("more", _) => {
                let (x, y) = self.ui().map(|ui| {
                    let b = ui.global::<Bridge>();
                    (b.get_menu_x(), b.get_menu_y())
                }).unwrap_or_default();
                match state.ctx {
                    MenuCtx::Background => self.native_menu(Vec::new(), first, x, y),
                    _ => self.native_menu(paths, None, x, y),
                }
            }
            ("col", _) => {
                if let Some(ui) = self.ui() {
                    let b = ui.global::<Bridge>();
                    match id {
                        "col:date" => b.set_show_date(!b.get_show_date()),
                        "col:created" => b.set_show_created(!b.get_show_created()),
                        "col:size" => b.set_show_size(!b.get_show_size()),
                        "col:kind" => b.set_show_kind(!b.get_show_kind()),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    /// Shows Explorer's own menu. It runs its own message loop, so it is
    /// started after the current callback returns (app state not borrowed).
    pub(super) fn native_menu(&mut self, paths: Vec<PathBuf>, background: Option<PathBuf>, x: f32, y: f32) {
        let scale = self.ui().map_or(1.0, |ui| ui.window().scale_factor());
        let extended = fileops::modifiers().1;
        slint::Timer::single_shot(Duration::from_millis(1), move || {
            match shellx::show_native_menu(&paths, background.as_deref(), x, y, scale, extended) {
                Ok(NativeOutcome::Rename) => {
                    with_app(|a| a.run(Command::Rename));
                }
                Ok(NativeOutcome::Nothing) => {}
                Err(e) => {
                    with_app(|a| a.note(format!("Couldn't show the Windows menu: {e}")));
                }
            }
        });
    }

    fn properties_of(&mut self, p: PathBuf) {
        std::thread::spawn(move || {
            finder_rs::core::shell::init_thread(true);
            fileops::show_properties(&p);
            std::thread::sleep(Duration::from_secs(600));
        });
    }

    fn compress(&mut self, sources: Vec<PathBuf>) {
        let Some(dir) = sources.first().and_then(|p| p.parent()).map(PathBuf::from) else { return };
        let dest = dir.join(fileops::zip_name(&sources, &dir));
        let name = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if self.current_dir().as_deref() == Some(dir.as_path()) {
            self.select_after = vec![name.clone()];
        }
        self.note("Compressing…");
        std::thread::spawn(move || {
            let result = fileops::zip_items(&sources, &dest);
            let _ = slint::invoke_from_event_loop(move || {
                with_app(|a| match result {
                    Ok(()) => a.note(format!("Created “{name}”")),
                    Err(e) => a.note(e),
                });
            });
        });
    }

    /// Where the "X Finder" shortcuts go: the Desktop and the Start menu.
    fn app_shortcut_paths() -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Some(d) = finder_rs::core::sys::known_folder(finder_rs::core::sys::KnownFolder::Desktop) {
            out.push(d.join("X Finder.lnk"));
        }
        if let Some(a) = std::env::var_os("APPDATA") {
            out.push(PathBuf::from(a).join(r"Microsoft\Windows\Start Menu\Programs\X Finder.lnk"));
        }
        out
    }

    fn has_app_shortcuts() -> bool {
        Self::app_shortcut_paths().iter().any(|p| p.exists())
    }

    /// Settings toggle: adds (or removes) the Desktop and Start menu
    /// shortcuts; pin X Finder to the taskbar from either.
    fn set_app_shortcuts(&mut self, on: bool) {
        let paths = Self::app_shortcut_paths();
        if on {
            let Ok(exe) = std::env::current_exe() else { return };
            let failed = paths.iter().filter_map(|p| shellx::create_shortcut(&exe, p).err()).next();
            match failed {
                Some(e) => self.toast(format!("Couldn't create the shortcut: {e}")),
                None => self.toast("X Finder is on the Desktop and in the Start menu."),
            }
        } else {
            for p in &paths {
                let _ = std::fs::remove_file(p);
            }
            self.toast("Removed X Finder from the Desktop and Start menu.");
        }
    }

    fn create_shortcuts(&mut self, targets: &[PathBuf]) {
        let mut made = Vec::new();
        for t in targets {
            let Some(dir) = t.parent() else { continue };
            let name = fileops::shortcut_name(t, dir);
            match shellx::create_shortcut(t, &dir.join(&name)) {
                Ok(()) => made.push(name),
                Err(e) => return self.note(e),
            }
        }
        let n = made.len();
        self.select_after = made;
        self.note(if n == 1 { "Created shortcut".to_string() } else { format!("Created {n} shortcuts") });
    }

    fn send_to(&mut self, target: SendTarget, paths: Vec<PathBuf>) {
        match target {
            SendTarget::Folder { path, .. } => self.run_op(Op::Copy { sources: paths, dest: path }),
            SendTarget::Zip => self.compress(paths),
            SendTarget::DesktopShortcut => {
                let Some(desktop) = finder_rs::core::sys::known_folder(finder_rs::core::sys::KnownFolder::Desktop) else {
                    return;
                };
                for p in &paths {
                    let name = fileops::shortcut_name(p, &desktop);
                    if let Err(e) = shellx::create_shortcut(p, &desktop.join(name)) {
                        return self.note(e);
                    }
                }
                self.note("Created shortcut on the Desktop");
            }
            SendTarget::App { lnk, .. } => shellx::run_send_to_app(&lnk, &paths),
        }
    }

    // ---- tags -----------------------------------------------------------------------

    /// Toggles colour `t` on the selection: removed if every item has it,
    /// otherwise added to all.
    pub(super) fn toggle_tag_for_test(&mut self, t: usize) {
        self.toggle_tag(t);
    }

    fn toggle_tag(&mut self, t: usize) {
        let bit = 1u8 << t.min(tags::COLORS - 1);
        let model = self.model();
        let selected = model.selected();
        let all_have = !selected.is_empty() && selected.iter().all(|&i| model.tags_at(i) & bit != 0);
        self.update_tags(|m| if all_have { m & !bit } else { m | bit });
    }

    pub(super) fn clear_tags(&mut self) {
        self.update_tags(|_| 0);
    }

    pub(super) fn update_tags(&mut self, f: impl Fn(u8) -> u8) {
        let model = self.model();
        let mut errors = Vec::new();
        for idx in model.selected() {
            let Some(path) = model.with_entry(idx, |e| self.tab().loc.path_of(e)) else { continue };
            let mask = f(model.tags_at(idx));
            match tags::write(&path, mask) {
                Ok(()) => model.set_tags(idx, mask),
                Err(e) => errors.push(e),
            }
        }
        if let Some(e) = errors.first() {
            self.note(e.clone());
        }
    }

    // ---- sidebar favourites -----------------------------------------------------------

    fn add_favorite(&mut self, path: PathBuf) {
        let mut favs = self.favorite_list();
        if !favs.contains(&path) {
            favs.push(path);
        }
        self.settings.favorites = Some(favs);
        self.rebuild_places();
    }

    fn remove_favorite(&mut self, index: usize) {
        let Some(Location::Dir(target)) = self.places.get(index).and_then(|p| p.loc.clone()) else { return };
        let favs: Vec<PathBuf> = self.favorite_list().into_iter().filter(|p| *p != target).collect();
        self.settings.favorites = Some(favs);
        self.rebuild_places();
    }

    pub(super) fn rebuild_places(&mut self) {
        self.places = build_places(&self.settings, &self.volumes);
        self.sync_sidebar();
        // At start-up the sidebar is built before the first tab exists.
        if let (Some(ui), Some(tab)) = (self.ui(), self.tabs.get(self.active)) {
            let loc = &tab.loc;
            let current = self.places.iter().position(|p| p.loc.as_ref() == Some(loc)).map_or(-1, |i| i as i32);
            ui.global::<Bridge>().set_sidebar_current(current);
        }
        if !self.tabs.is_empty() {
            self.save_settings();
        }
    }

    // ---- settings ----------------------------------------------------------------------

    /// Pushes settings that live in the UI.
    pub(super) fn sync_settings_ui(&self) {
        let Some(ui) = self.ui() else { return };
        let b = ui.global::<Bridge>();
        b.set_set_menu_style(match self.settings.menu_style {
            MenuStyle::App => 0,
            MenuStyle::Windows => 1,
        });
        b.set_set_icon_style(match self.settings.icon_style {
            IconStyle::Windows => 0,
            IconStyle::Finder => 1,
        });
        b.set_set_start_view(self.settings.start_view);
        b.set_set_appearance(self.settings.appearance.index());
        b.set_set_folder_style(self.settings.folder_style);
        b.set_set_folder_previews(self.settings.folder_previews);
        b.set_set_striped_rows(self.settings.striped_rows);
        b.set_set_show_recents(self.settings.show_recents);
        b.set_set_show_trash(self.settings.show_trash);
        b.set_set_restore_tabs(self.settings.restore_tabs);
        b.set_set_app_shortcuts(Self::has_app_shortcuts());
        b.set_set_default_explorer(
            std::env::current_exe().is_ok_and(|exe| finder_rs::core::default_explorer::is_default(&exe)),
        );
        ui.global::<crate::Theme>().set_folder_style(self.settings.folder_style);
        ui.global::<crate::Theme>().set_striped(self.settings.striped_rows);
    }

    /// Applies the light/dark choice to the app, the window title bar and
    /// native Windows menus.
    pub(super) fn apply_appearance(&self) {
        let Some(ui) = self.ui() else { return };
        let theme = ui.global::<crate::Theme>();
        theme.set_system_dark(shellx::system_prefers_dark());
        theme.set_appearance(self.settings.appearance.index());
        // Crisp light-on-dark text only while the app is dark.
        i_slint_renderer_software::x_finder_set_dark(theme.get_dark());
        if let Some(ui) = self.ui() {
            ui.window().request_redraw();
        }
        let forced = match self.settings.appearance {
            Appearance::System => None,
            Appearance::Light => Some(false),
            Appearance::Dark => Some(true),
        };
        shellx::set_menu_theme(forced);
        {
            use slint::winit_030::{WinitWindowAccessor, winit::window::Theme as WinitTheme};
            ui.window().with_winit_window(|w| {
                w.set_theme(forced.map(|dark| if dark { WinitTheme::Dark } else { WinitTheme::Light }));
            });
        }
    }

    pub(super) fn settings_set(&mut self, key: &str, value: i32) {
        let Some(ui) = self.ui() else { return };
        let b = ui.global::<Bridge>();
        match key {
            "open" => {
                self.menu_close();
                self.sync_settings_ui();
                b.set_settings_open(true);
                return;
            }
            "close" => {
                b.set_settings_open(false);
                ui.invoke_focus_list();
                return;
            }
            "appearance" => {
                self.settings.appearance = Appearance::from_index(value);
                self.apply_appearance();
            }
            "menu" => self.settings.menu_style = if value == 1 { MenuStyle::Windows } else { MenuStyle::App },
            "folder-style" | "folder-previews" => {
                if key == "folder-style" {
                    self.settings.folder_style = value.clamp(0, 5);
                } else {
                    self.settings.folder_previews = value == 1;
                }
                let mode = super::folder_preview_mode(&self.settings);
                if self.thumbs.folder_previews.replace(mode) != mode {
                    for tab in &self.tabs {
                        tab.model.notify_all();
                    }
                }
            }
            "recents" => {
                self.settings.show_recents = value == 1;
                self.rebuild_places();
            }
            "trash" => {
                self.settings.show_trash = value == 1;
                self.rebuild_places();
            }
            "restore-tabs" => self.settings.restore_tabs = value == 1,
            "striped-rows" => {
                self.settings.striped_rows = value == 1;
                ui.global::<crate::Theme>().set_striped(value == 1);
            }
            "default-explorer" => {
                if let Ok(exe) = std::env::current_exe() {
                    match finder_rs::core::default_explorer::set_default(&exe, value == 1) {
                        Ok(()) if value == 1 => self.toast("Folders opened from other apps now open here."),
                        Ok(()) => self.toast("Windows Explorer is the default again."),
                        Err(e) => self.toast(e),
                    }
                }
                self.update_explorer_catch();
            }
            "app-shortcuts" => self.set_app_shortcuts(value == 1),
            "reset-tags" => {
                self.settings.tag_names = finder_rs::core::settings::default_tag_names();
                self.refresh_tag_names();
            }
            "icons" => {
                self.settings.icon_style = if value == 1 { IconStyle::Finder } else { IconStyle::Windows };
                self.thumbs.windows_icons.set(self.settings.icon_style == IconStyle::Windows);
                for tab in &self.tabs {
                    tab.model.notify_all();
                }
                self.sync_strip();
            }
            "start-view" => {
                self.settings.start_view = value.clamp(0, 3);
                self.set_view(ViewMode::from_index(value));
            }
            "hidden" => {
                if self.opts.show_hidden != (value == 1) {
                    self.run(Command::ToggleHidden);
                }
            }
            "columns-full" => {
                self.columns_absolute = value == 1;
                b.set_columns_absolute(self.columns_absolute);
                self.sync_columns();
            }
            "reset-favorites" => {
                self.settings.favorites = None;
                self.rebuild_places();
            }
            _ => return,
        }
        self.sync_settings_ui();
        self.save_settings();
    }

    /// Collects settings that live in the UI and writes the settings file.
    pub(super) fn save_settings(&mut self) {
        if let Some(ui) = self.ui() {
            let b = ui.global::<Bridge>();
            self.settings.show_hidden = self.opts.show_hidden;
            self.settings.columns_full_path = self.columns_absolute;
            self.settings.icon_size = b.get_icon_size();
            self.settings.preview_pane = self.preview_pane;
            self.settings.preview_width = b.get_preview_pane_width();
            self.settings.column_width = b.get_pane_width();
        }
        // "Remember tabs": keep the open tabs for next time.
        self.settings.last_tabs = self.settings.restore_tabs.then(|| self.current_tabs()).filter(|g| !g.tabs.is_empty());
        if let Err(e) = self.settings.save() {
            self.note(format!("Couldn't save settings: {e}"));
        }
    }
}

/// Re-applies the appearance (startup, and when Windows switches light/dark).
pub fn apply_appearance() {
    with_app(|a| a.apply_appearance());
}

/// Saves settings when the window closes.
pub fn shutdown() {
    with_app(|a| a.save_settings());
}
