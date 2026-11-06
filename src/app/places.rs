//! Sidebar places, live drive detection, the "This PC" page, Recents,
//! the Recycle Bin and search.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use finder_rs::core::format;
use finder_rs::core::fs_list::{Cancel, Entry, ListEvent};
use finder_rs::core::location::{self, Location};
use finder_rs::core::query::Query;
use finder_rs::core::settings::Settings;
use finder_rs::core::sys::{self, KnownFolder, VolumeInfo};
use finder_rs::core::trash::{self, TrashItem};
use finder_rs::core::{fs_list, shell, shellx, tags};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use super::{App, Place, with_app};
use crate::{Bridge, PcCard};

const KNOWN: [(&str, &str, KnownFolder); 7] = [
    ("Home", "home", KnownFolder::Profile),
    ("Desktop", "desktop", KnownFolder::Desktop),
    ("Documents", "documents", KnownFolder::Documents),
    ("Downloads", "downloads", KnownFolder::Downloads),
    ("Pictures", "pictures", KnownFolder::Pictures),
    ("Music", "music", KnownFolder::Music),
    ("Videos", "videos", KnownFolder::Videos),
];

/// "Ventoy (E:)", or "Local Disk (C:)" / "USB Drive (E:)" without a label.
pub(super) fn drive_label(name: &str, kind: u8, volumes: &HashMap<String, VolumeInfo>) -> String {
    match volumes.get(name).map(|v| v.label.trim()).filter(|l| !l.is_empty()) {
        Some(label) => format!("{label} ({name})"),
        None => {
            let kind = match kind {
                2 => "USB Drive",
                4 => "Network",
                5 => "CD Drive",
                _ => "Local Disk",
            };
            format!("{kind} ({name})")
        }
    }
}

fn drive_icon(kind: u8, name: &str) -> &'static str {
    match kind {
        2 => "usbdrive",
        4 => "netdrive",
        5 => "cddrive",
        _ if name.eq_ignore_ascii_case("C:") => "sysdrive",
        _ => "drive",
    }
}

/// Sidebar: Favorites (Recents + saved folders), Locations (This PC, drives,
/// Recycle Bin), Tags.
pub(super) fn build_places(settings: &Settings, volumes: &HashMap<String, VolumeInfo>) -> Vec<Place> {
    let known: Vec<(&str, &'static str, PathBuf)> =
        KNOWN.iter().filter_map(|(l, i, kf)| sys::known_folder(*kf).map(|p| (*l, *i, p))).collect();
    let header = |label: &str| Place { label: label.into(), icon: "", loc: None, favorite: false, tag: -1 };
    let mut places = vec![header("Favorites")];
    if settings.show_recents {
        places.push(Place { label: "Recents".into(), icon: "recent", loc: Some(Location::Recent), favorite: false, tag: -1 });
    }
    let favs: Vec<PathBuf> = match &settings.favorites {
        Some(list) => list.clone(),
        None => known.iter().map(|k| k.2.clone()).collect(),
    };
    for p in favs.into_iter().filter(|p| p.is_dir()) {
        let (label, icon) = match known.iter().find(|k| k.2 == p) {
            Some((l, i, _)) => (l.to_string(), *i),
            None => (Location::Dir(p.clone()).title(), "folder"),
        };
        places.push(Place { label, icon, loc: Some(Location::Dir(p)), favorite: true, tag: -1 });
    }
    places.push(header("Locations"));
    places.push(Place {
        label: location::COMPUTER_NAME.into(),
        icon: "computer",
        loc: Some(Location::Computer),
        favorite: false,
        tag: -1,
    });
    for d in sys::drives() {
        places.push(Place {
            label: drive_label(&d.name, d.kind, volumes),
            icon: if d.kind == 2 { "eject" } else { "drive" },
            loc: Some(Location::Dir(PathBuf::from(format!("{}\\", d.name)))),
            favorite: false,
            tag: -1,
        });
    }
    if settings.show_trash {
        places.push(Place { label: "Recycle Bin".into(), icon: "trash", loc: Some(Location::Trash), favorite: false, tag: -1 });
    }
    places.push(header("Tags"));
    for i in 0..tags::COLORS {
        places.push(Place {
            label: settings.tag_name(i),
            icon: "",
            loc: Some(Location::Tag(i as u8)),
            favorite: false,
            tag: i as i32,
        });
    }
    places
}

fn size_text(info: &VolumeInfo) -> String {
    if info.total == 0 {
        return String::new();
    }
    format!("{} free of {}", format::size(info.free), format::size(info.total))
}

impl App {
    // ---- drives ---------------------------------------------------------------------

    /// Checks every 2 s whether drives were plugged in or removed.
    pub(super) fn start_drive_watch(&mut self) {
        self.refresh_volumes();
        let timer = slint::Timer::default();
        timer.start(slint::TimerMode::Repeated, Duration::from_secs(2), || {
            with_app(|a| {
                let mask = sys::drive_mask();
                if mask != a.drive_mask {
                    let added = mask & !a.drive_mask;
                    a.drive_mask = mask;
                    if added != 0 {
                        let letter = (b'A' + added.trailing_zeros() as u8) as char;
                        a.note(format!("Drive {letter}: connected"));
                    }
                    a.refresh_volumes();
                }
            });
        });
        self.drive_timer = Some(timer);
    }

    /// Reads drive names and sizes in the background, then updates the
    /// sidebar and the This PC page.
    pub(super) fn refresh_volumes(&mut self) {
        self.drive_mask = sys::drive_mask();
        self.rebuild_places();
        std::thread::spawn(|| {
            let vols: HashMap<String, VolumeInfo> = sys::drives()
                .into_iter()
                .filter(|d| d.kind != 5) // empty DVD drives can be slow
                .filter_map(|d| Some((d.name.clone(), sys::volume_info(&PathBuf::from(format!("{}\\", d.name)))?)))
                .collect();
            let _ = slint::invoke_from_event_loop(move || {
                with_app(|a| {
                    a.volumes = vols;
                    a.rebuild_places();
                    a.sync_computer_page();
                });
            });
        });
    }

    // ---- This PC page ----------------------------------------------------------------

    pub(super) fn sync_computer_page(&self) {
        let Some(ui) = self.ui() else { return };
        let b = ui.global::<Bridge>();
        let Some(tab) = self.tabs.get(self.active) else { return };
        let at_pc = tab.loc == Location::Computer;
        b.set_at_computer(at_pc);
        if !at_pc {
            return;
        }
        let folders: Vec<PcCard> = KNOWN[1..]
            .iter()
            .enumerate()
            .filter_map(|(i, (label, icon, kf))| {
                sys::known_folder(*kf)?;
                Some(PcCard {
                    label: (*label).into(),
                    detail: SharedString::new(),
                    icon: (*icon).into(),
                    used: 0.0,
                    has_bar: false,
                    index: i as i32,
                })
            })
            .collect();
        let drives: Vec<PcCard> = sys::drives()
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let info = self.volumes.get(&d.name);
                PcCard {
                    label: drive_label(&d.name, d.kind, &self.volumes).into(),
                    detail: info.map(size_text).unwrap_or_default().into(),
                    icon: drive_icon(d.kind, &d.name).into(),
                    used: info.filter(|v| v.total > 0).map_or(0.0, |v| 1.0 - v.free as f32 / v.total as f32),
                    has_bar: info.is_some_and(|v| v.total > 0),
                    index: i as i32,
                }
            })
            .collect();
        b.set_pc_folders(ModelRc::new(VecModel::from(folders)));
        b.set_pc_drives(ModelRc::new(VecModel::from(drives)));
    }

    /// Double-click on a This PC card (section 0 folders, 1 drives).
    pub(super) fn pc_open(&mut self, section: i32, index: usize, new_tab: bool) {
        let target = if section == 0 {
            KNOWN[1..].get(index).and_then(|(_, _, kf)| sys::known_folder(*kf)).map(Location::Dir)
        } else {
            sys::drives().get(index).map(|d| Location::Dir(PathBuf::from(format!("{}\\", d.name))))
        };
        if let Some(loc) = target {
            if new_tab { self.open_tab(loc, true) } else { self.navigate(loc, None) }
        }
    }

    pub(super) fn pc_path(&self, section: i32, index: usize) -> Option<PathBuf> {
        if section == 0 {
            KNOWN[1..].get(index).and_then(|(_, _, kf)| sys::known_folder(*kf))
        } else {
            sys::drives().get(index).map(|d| PathBuf::from(format!("{}\\", d.name)))
        }
    }

    // ---- special listings ------------------------------------------------------------------

    /// Lists Recents, the Recycle Bin or search results for tab `idx`.
    pub(super) fn start_special_load(&mut self, idx: usize, loc: Location) {
        let tab_id = self.tabs[idx].id;
        let cancel = self.tabs[idx].cancel.clone();
        match loc {
            Location::Search { root, query } => {
                let token = cancel.clone();
                fs_list::search(root, Query::parse(&query), cancel, move |ev| {
                    let token = token.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if !token.is_cancelled() {
                            with_app(|a| a.on_list_event(tab_id, ev));
                        }
                    });
                });
            }
            Location::Recent => {
                std::thread::spawn(move || {
                    shell::init_thread(true);
                    let entries: Vec<Entry> = shellx::recent_files(300)
                        .into_iter()
                        .filter_map(|(p, opened)| {
                            let mut e = Entry::from_path(&p)?;
                            // Recents are about when you last opened them.
                            e.modified = opened;
                            e.path = Some(p);
                            Some(e)
                        })
                        .collect();
                    deliver(tab_id, cancel, entries, None);
                });
            }
            Location::Trash => {
                std::thread::spawn(move || {
                    let items = trash::list();
                    let entries: Vec<Entry> = items
                        .iter()
                        .filter_map(|it| {
                            let name = it.original.file_name()?.to_string_lossy().into_owned();
                            let mut e = Entry::from_path(&it.data)?;
                            e.key = name.to_lowercase().into_boxed_str();
                            e.name = name.into_boxed_str();
                            e.modified = it.deleted;
                            e.path = Some(it.data.clone());
                            Some(e)
                        })
                        .collect();
                    deliver(tab_id, cancel, entries, Some(items));
                });
            }
            _ => {}
        }
    }

    // ---- Recycle Bin actions ----------------------------------------------------------

    fn trash_selection(&self) -> Vec<TrashItem> {
        self.selected_paths().iter().filter_map(|p| self.trash_items.get(p).cloned()).collect()
    }

    pub(super) fn trash_put_back(&mut self) {
        let items = self.trash_selection();
        let mut errors = Vec::new();
        for it in &items {
            if let Err(e) = trash::put_back(it) {
                errors.push(e);
            }
        }
        match errors.first() {
            Some(e) => self.note(e.clone()),
            None => self.note(format!("Put back {}", plural(items.len()))),
        }
        self.reload();
    }

    pub(super) fn trash_delete_now(&mut self) {
        let items = self.trash_selection();
        let mut errors = Vec::new();
        for it in &items {
            if let Err(e) = trash::delete_now(it) {
                errors.push(e);
            }
        }
        match errors.first() {
            Some(e) => self.note(e.clone()),
            None => self.note(format!("Deleted {} for good", plural(items.len()))),
        }
        self.reload();
    }

    pub(super) fn trash_empty(&mut self) {
        // Windows shows its own confirmation (modal); run it after this callback.
        slint::Timer::single_shot(Duration::from_millis(1), || {
            let result = trash::empty();
            with_app(|a| {
                if let Err(e) = result {
                    a.note(e);
                }
                if a.tab().loc == Location::Trash {
                    a.reload();
                }
            });
        });
    }

    // ---- search ----------------------------------------------------------------------------

    /// Enter in the filter box: search this folder and all subfolders.
    pub(super) fn search_submit(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        // This PC, Recents, tags, Recycle Bin: search the home folder.
        let root = match &self.tab().loc {
            Location::Dir(p) => p.clone(),
            Location::Search { root, .. } => root.clone(),
            _ => sys::home(),
        };
        // Reading every file would be very slow: ask for a type first.
        if finder_rs::core::query::Query::parse(text).content_needs_type() {
            return self.toast("Add a file type to search inside files, e.g.  pdf: content:\"project cedar\"");
        }
        // Enter: search now (don't wait for the typing pause), and keep
        // the text in the box so it can be refined.
        self.live_text = text.to_string();
        if matches!(self.tab().loc, Location::Dir(_) | Location::Search { .. }) {
            return self.live_search();
        }
        self.keep_filter = true;
        self.navigate(Location::Search { root, query: text.to_string() }, None);
    }

    // ---- favourites order --------------------------------------------------------------------

    /// Drag payload for reordering a favourite.
    pub(super) fn fav_drag(&self, i: usize) -> slint::DataTransfer {
        let mut d = slint::DataTransfer::default();
        d.set_plain_text(format!("finder-rs:fav:{i}").into());
        d
    }

    pub(super) fn fav_drag_source(data: &slint::DataTransfer) -> Option<usize> {
        data.plain_text().ok()?.strip_prefix("finder-rs:fav:")?.parse().ok()
    }

    /// Moves favourite at sidebar index `from` to where `to` is.
    pub(super) fn move_favorite(&mut self, from: usize, to: usize) {
        let favs: Vec<usize> = self.places.iter().enumerate().filter(|(_, p)| p.favorite).map(|(i, _)| i).collect();
        let (Some(a), Some(b)) = (favs.iter().position(|&i| i == from), favs.iter().position(|&i| i == to)) else {
            return;
        };
        let mut paths = self.favorite_list();
        if a == b || a >= paths.len() || b >= paths.len() {
            return;
        }
        let item = paths.remove(a);
        paths.insert(b, item);
        self.settings.favorites = Some(paths);
        self.rebuild_places();
    }

    /// Tag names changed: sidebar, titles and settings file.
    pub(super) fn refresh_tag_names(&mut self) {
        self.rebuild_places();
        self.sync_all();
    }

    pub(super) fn favorite_list(&self) -> Vec<PathBuf> {
        self.places
            .iter()
            .filter(|p| p.favorite)
            .filter_map(|p| match &p.loc {
                Some(Location::Dir(d)) => Some(d.clone()),
                _ => None,
            })
            .collect()
    }

    // ---- folder sizes --------------------------------------------------------------------

    /// One folder selected: work out its size in the background (cached).
    pub(super) fn size_selected_folder(&mut self) {
        let model = self.model();
        let sel = model.selected();
        let target = match sel.as_slice() {
            [one] => model.with_entry(*one, |e| e.is_dir.then(|| self.tab().loc.path_of(e))).flatten(),
            _ => None,
        };
        let Some(path) = target.filter(|p| !matches!(self.tab().loc, Location::Computer) && !self.size_cache.contains_key(p))
        else {
            if let Some((_, c)) = self.size_job.take() {
                c.cancel();
            }
            return;
        };
        if self.size_job.as_ref().is_some_and(|(p, _)| *p == path) {
            return;
        }
        if let Some((_, c)) = self.size_job.take() {
            c.cancel();
        }
        let cancel = Cancel::new();
        self.size_job = Some((path.clone(), cancel.clone()));
        std::thread::spawn(move || {
            let result = fs_list::dir_size(&path, &cancel);
            let _ = slint::invoke_from_event_loop(move || {
                with_app(|a| {
                    if let Some(r) = result {
                        a.size_cache.insert(path.clone(), r);
                    }
                    if a.size_job.as_ref().is_some_and(|(p, _)| *p == path) {
                        a.size_job = None;
                    }
                    a.sync_status();
                });
            });
        });
    }

    /// Quick Look: Left/Right turn PDF pages, or scroll documents by a screen.
    pub(super) fn preview_page(&mut self, forward: bool) {
        if self.pv_pages > 1 {
            self.pv_page_step(if forward { 1 } else { -1 });
        } else if let Some(ui) = self.ui() {
            let b = ui.global::<Bridge>();
            b.set_pv_scroll_dir(if forward { 1 } else { -1 });
            b.set_pv_scroll_serial(b.get_pv_scroll_serial() + 1);
        }
    }

    // ---- status: selection size and free space ---------------------------------------

    /// "1 of 532 selected, 4.4 MB" / "532 items, 50.1 GB available".
    pub(super) fn selection_status(&self) -> String {
        let tab = self.tab();
        let model = &tab.model;
        let shown = model.row_count();
        let sel = model.selected();
        let mut s = if !sel.is_empty() {
            let bytes: u64 = sel.iter().filter_map(|&i| model.with_entry(i, |e| if e.is_dir { 0 } else { e.size })).sum();
            let mut t = format!("{} of {} selected", format_count(sel.len()), format_count(shown));
            let one_dir = match sel.as_slice() {
                [one] => model.with_entry(*one, |e| e.is_dir.then(|| tab.loc.path_of(e))).flatten(),
                _ => None,
            };
            if let Some(dir) = one_dir {
                match self.size_cache.get(&dir) {
                    Some((b, n)) => t.push_str(&format!(
                        ", {} ({} item{})",
                        format::size(*b),
                        format_count(*n as usize),
                        if *n == 1 { "" } else { "s" }
                    )),
                    None if self.size_job.as_ref().is_some_and(|(p, _)| *p == dir) => t.push_str(", calculating size…"),
                    None => {}
                }
            } else if bytes > 0 {
                t.push_str(&format!(", {}", format::size(bytes)));
            }
            t
        } else if !self.opts.filter.is_empty() {
            format!("{} of {} items", format_count(shown), format_count(model.total()))
        } else {
            format!("{} item{}", format_count(shown), if shown == 1 { "" } else { "s" })
        };
        if let Some(free) = self.free_space {
            s.push_str(&format!(" · {} available", format::size(free)));
        }
        s
    }
}

fn format_count(n: usize) -> String {
    format::thousands(n)
}

fn plural(n: usize) -> String {
    if n == 1 { "1 item".into() } else { format!("{n} items") }
}

/// Hands a finished background listing to the UI thread.
fn deliver(tab_id: u64, cancel: Cancel, entries: Vec<Entry>, trash_items: Option<Vec<TrashItem>>) {
    let total = entries.len();
    let _ = slint::invoke_from_event_loop(move || {
        if cancel.is_cancelled() {
            return;
        }
        with_app(|a| {
            if let Some(items) = trash_items {
                a.trash_items = items.into_iter().map(|it| (it.data.clone(), it)).collect();
            }
            a.on_list_event(tab_id, ListEvent::Batch(entries));
            a.on_list_event(tab_id, ListEvent::Done { total, elapsed: Duration::ZERO, error: None });
        });
    });
}
