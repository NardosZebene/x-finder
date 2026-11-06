//! File menu, clipboard, file operations, rename and drag & drop.

use std::path::{Path, PathBuf};
use std::time::Duration;

use finder_rs::core::fileops::{self, Op};
use finder_rs::core::location::Location;
use finder_rs::core::shell;
use slint::{ComponentHandle, DataTransfer, SharedString};

use super::{App, with_app};
use crate::Bridge;
use crate::commands::Command;

/// What the rename dialog is renaming.
pub(super) struct RenameTarget {
    pub path: PathBuf,
    /// Renaming a sidebar tag instead of a file.
    pub tag: Option<usize>,
    /// Naming a new tab group instead.
    pub group: bool,
}

/// Most files offered as one drag payload (keeps select-all on huge folders cheap).
const DRAG_LIMIT: usize = 20_000;

impl App {
    /// Folder shown in the active tab, if it is a real folder (not "This PC").
    pub(super) fn current_dir(&self) -> Option<PathBuf> {
        match &self.tab().loc {
            Location::Dir(p) => Some(p.clone()),
            _ => None,
        }
    }

    /// Selected items, or the cursor item if nothing is selected.
    pub(super) fn selected_paths(&self) -> Vec<PathBuf> {
        let model = self.model();
        let mut idx = model.selected();
        if idx.is_empty() {
            idx.extend(model.cursor());
        }
        idx.into_iter().filter_map(|i| model.with_entry(i, |e| self.tab().loc.path_of(e))).collect()
    }

    /// What dragging out of the window carries: the selected files and
    /// folders (nothing from This PC).
    pub(super) fn drag_out_paths(&self) -> Vec<PathBuf> {
        if matches!(self.tab().loc, Location::Computer) {
            return Vec::new();
        }
        let mut paths = self.selected_paths();
        paths.truncate(DRAG_LIMIT);
        paths.retain(|p| p.exists());
        paths
    }

    /// A drag to another app finished: a move there may have taken files
    /// from here.
    pub(super) fn after_drag_out(&mut self) {
        self.pending_click = None;
        super::repaint_all();
    }

    /// Menu enable states and the drag payload; called whenever selection changes.
    pub(super) fn sync_selection(&self) {
        let Some(ui) = self.ui() else { return };
        let b = ui.global::<Bridge>();
        let model = self.model();
        let selected = model.selected();
        let one_dir = selected.len() == 1 && model.with_entry(selected[0], |e| e.is_dir).unwrap_or(false);
        b.set_sel_any(!selected.is_empty());
        b.set_sel_one(selected.len() == 1);
        b.set_sel_dir(one_dir);
        b.set_in_folder(self.current_dir().is_some());
        let mut data = DataTransfer::default();
        if !selected.is_empty() && selected.len() <= DRAG_LIMIT && !matches!(self.tab().loc, Location::Computer) {
            let paths: Vec<PathBuf> =
                selected.iter().filter_map(|&i| model.with_entry(i, |e| self.tab().loc.path_of(e))).collect();
            data.set_file_paths(paths);
        }
        b.set_drag_data(data);
    }

    pub(super) fn note(&mut self, msg: impl Into<String>) {
        self.note = Some(msg.into());
        self.sync_status();
    }

    /// A message that must be seen: a pill over the list for a few seconds.
    pub(super) fn toast(&mut self, msg: impl Into<String>) {
        self.toast_with(msg, None);
    }

    /// A toast with a button ("Undo"): `action` is what the button does.
    pub(super) fn toast_with(&mut self, msg: impl Into<String>, action: Option<(&str, &'static str)>) {
        let Some(ui) = self.ui() else { return };
        let b = ui.global::<Bridge>();
        let serial = b.get_toast_serial() + 1;
        b.set_toast_serial(serial);
        b.set_toast(msg.into().into());
        b.set_toast_action(action.map_or(SharedString::new(), |(label, _)| label.into()));
        self.toast_action = action.map(|(_, id)| id);
        slint::Timer::single_shot(std::time::Duration::from_secs(5), move || {
            with_app(|a| {
                if let Some(ui) = a.ui() {
                    let b = ui.global::<Bridge>();
                    if b.get_toast_serial() == serial {
                        b.set_toast(SharedString::new());
                    }
                }
            });
        });
    }

    // ---- context menu --------------------------------------------------------

    /// Right-click on an item: select it (unless it is part of the selection)
    /// and open the menu there.
    pub(super) fn row_context(&mut self, row: usize, x: f32, y: f32) {
        let model = self.model();
        if let Some(idx) = model.entry_at(row)
            && !model.with_entry(idx, |e| e.selected).unwrap_or(false)
        {
            self.row_pressed(row, false, false);
        }
        self.open_items_menu(x, y);
    }

    /// Right-click in a parent or preview column: that item becomes the
    /// selection (its folder opens), then the menu shows for it.
    pub(super) fn pane_context(&mut self, pane: i32, row: usize, x: f32, y: f32) {
        match pane {
            -1 => return self.row_context(row, x, y),
            -2 => self.preview_clicked(row, false),
            p if p >= 0 => self.pane_clicked(p as usize, row, false),
            _ => return,
        }
        // The folder is listing; open the menu once the item is selected.
        let ticks = std::rc::Rc::new(std::cell::Cell::new(0));
        let timer = std::rc::Rc::new(slint::Timer::default());
        let t2 = timer.clone();
        timer.start(slint::TimerMode::Repeated, Duration::from_millis(30), move || {
            ticks.set(ticks.get() + 1);
            let ready = with_app(|a| !a.tab().loading && a.model().cursor().is_some()).unwrap_or(true);
            if ready || ticks.get() > 60 {
                t2.stop();
                with_app(|a| a.open_items_menu(x, y));
            }
        });
    }

    pub(super) fn menu_action(&mut self, id: &str) {
        match id {
            "open" => self.run(Command::Open),
            "open-tab" => self.run(Command::OpenInNewTab),
            "quicklook" if !self.quicklook => self.run(Command::QuickLook),
            "cut" => self.run(Command::Cut),
            "copy" => self.run(Command::Copy),
            "paste" => self.run(Command::Paste),
            "duplicate" => self.run(Command::Duplicate),
            "rename" => self.run(Command::Rename),
            "new-folder" => self.run(Command::NewFolder),
            "recycle" => self.run(Command::Recycle),
            "copy-path" => self.run(Command::CopyPath),
            "properties" => self.run(Command::Properties),
            "reveal" => match self.selected_paths().first() {
                Some(p) => fileops::reveal_in_explorer(p),
                None => {
                    if let Some(dir) = self.current_dir() {
                        finder_rs::core::explorer_catch::allow_explorer();
                    let _ = std::process::Command::new("explorer.exe").arg(dir).spawn();
                    }
                }
            },
            "terminal" => {
                let target = self.selected_paths().into_iter().find(|p| p.is_dir()).or_else(|| self.current_dir());
                if let Some(dir) = target {
                    fileops::open_terminal(&dir);
                }
            }
            _ => {}
        }
    }

    // ---- clipboard -----------------------------------------------------------

    pub(super) fn clipboard_put(&mut self, cut: bool) {
        let paths = self.selected_paths();
        if paths.is_empty() || self.current_dir().is_none() {
            return;
        }
        let n = paths.len();
        match fileops::set_clipboard_files(&paths, cut) {
            Ok(()) => {
                let what = if n == 1 { "1 item".to_string() } else { format!("{n} items") };
                self.note(if cut { format!("Cut {what} — paste to move") } else { format!("Copied {what}") });
            }
            Err(e) => self.note(e),
        }
    }

    pub(super) fn paste(&mut self) {
        let Some(dest) = self.current_dir() else { return };
        let Some((sources, cut)) = fileops::clipboard_files() else {
            self.note("Nothing to paste.");
            return;
        };
        self.expect_new_items();
        let op = if cut { Op::Move { sources, dest } } else { Op::Copy { sources, dest } };
        self.run_op(op);
    }

    pub(super) fn copy_path(&mut self) {
        let paths = self.selected_paths();
        let text = if paths.is_empty() {
            self.tab().loc.display()
        } else {
            paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\r\n")
        };
        match fileops::set_clipboard_text(&text) {
            Ok(()) => self.note(if paths.len() > 1 { "Copied paths" } else { "Copied path" }),
            Err(e) => self.note(e),
        }
    }

    // ---- operations ------------------------------------------------------------

    /// Runs a file operation on a worker thread with Windows' copy engine.
    /// The folder watcher updates the listing as files change.
    pub(super) fn run_op(&mut self, op: Op) {
        self.note("Working…");
        // Where each item ends up, so tagged items stay findable.
        let moves: Vec<(PathBuf, PathBuf)> = match &op {
            Op::Rename { source, new_name } => vec![(source.clone(), source.with_file_name(new_name))],
            Op::Move { sources, dest } => sources
                .iter()
                .filter_map(|s| Some((s.clone(), dest.join(s.file_name()?))))
                .collect(),
            _ => Vec::new(),
        };
        std::thread::spawn(move || {
            shell::init_thread(true);
            let result = fileops::run(&op);
            if result.is_ok() {
                for (from, to) in &moves {
                    finder_rs::core::tags::moved(from, to);
                }
            }
            let desc = op.describe();
            let _ = slint::invoke_from_event_loop(move || {
                with_app(|a| {
                    // Lists that aren't a watched folder: drop what's gone.
                    if !matches!(a.tab().loc, Location::Dir(_)) {
                        let opts = a.tab().applied.clone();
                        a.tab().model.drop_missing(&opts);
                        a.sync_status();
                    }
                    match result {
                        Ok(()) => a.note(desc),
                        Err(e) => {
                            a.select_after.clear();
                            a.note(e);
                        }
                    }
                    // The watcher reports changes ~150 ms after they happen.
                    slint::Timer::single_shot(Duration::from_millis(600), || {
                        with_app(|a| {
                            a.select_new = false;
                            a.new_items.clear();
                        });
                    });
                });
            });
        });
    }

    /// Select the items a running paste/duplicate/drop creates in this folder.
    fn expect_new_items(&mut self) {
        self.select_after.clear();
        self.new_items.clear();
        self.select_new = true;
    }

    pub(super) fn delete_selection(&mut self, permanently: bool) {
        if self.tab().loc == Location::Trash {
            self.note("Right-click → Delete Immediately to remove items for good.");
            return;
        }
        // Folders, and search results / Recents / tags (full paths there).
        if matches!(self.tab().loc, Location::Computer) {
            return;
        }
        let sources = self.selected_paths();
        if sources.is_empty() {
            return;
        }
        self.run_op(if permanently { Op::Delete { sources } } else { Op::Recycle { sources } });
    }

    pub(super) fn duplicate(&mut self) {
        if self.current_dir().is_none() {
            return;
        }
        let sources = self.selected_paths();
        if !sources.is_empty() {
            self.expect_new_items();
            self.run_op(Op::Duplicate { sources });
        }
    }

    pub(super) fn show_properties(&mut self) {
        let target = self.selected_paths().into_iter().next().or_else(|| self.current_dir());
        if let Some(p) = target {
            std::thread::spawn(move || {
                shell::init_thread(true);
                fileops::show_properties(&p);
                // Keep the thread (and with it the dialog) alive while it is open.
                std::thread::sleep(Duration::from_secs(600));
            });
        }
    }

    /// After a paste, drop or rename, select the new items once the watcher reports them.
    pub(super) fn apply_select_after(&mut self) {
        if self.select_after.is_empty() {
            return;
        }
        let model = self.model();
        let found: Vec<u32> = self.select_after.iter().filter_map(|n| model.find_name(n)).collect();
        if found.is_empty() {
            return;
        }
        self.select_after.clear();
        let rows: Vec<usize> = found.iter().filter_map(|&e| model.row_of(e)).collect();
        model.select_where(|r| rows.contains(&r));
        model.set_cursor(found.first().copied());
        model.set_anchor(found.first().copied());
        if let (Some(ui), Some(&row)) = (self.ui(), rows.iter().min()) {
            ui.invoke_scroll_to_row(row as i32);
        }
        self.sync_status();
        self.schedule_preview();
    }

    // ---- rename / new folder -----------------------------------------------------

    pub(super) fn begin_rename(&mut self) {
        if self.current_dir().is_none() {
            return;
        }
        let paths = self.selected_paths();
        let [path] = paths.as_slice() else {
            if paths.len() > 1 {
                self.note("Select one item to rename.");
            }
            return;
        };
        self.open_rename_dialog(path.clone(), "Rename");
    }

    fn open_rename_dialog(&mut self, path: PathBuf, title: &str) {
        let Some(ui) = self.ui() else { return };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        // Pre-select the name without its extension (whole name for folders).
        let stem = match name.rfind('.') {
            Some(dot) if dot > 0 && !path.is_dir() => dot,
            _ => name.len(),
        };
        let b = ui.global::<Bridge>();
        b.set_rename_title(title.into());
        b.set_rename_text(SharedString::from(name.as_str()));
        b.set_rename_stem(stem as i32);
        b.set_rename_error(SharedString::new());
        b.set_rename_group(false);
        b.set_renaming(true);
        self.rename_target = Some(RenameTarget { path, tag: None, group: false });
    }

    /// "Rename Tag…": e.g. Red → Work.
    pub(super) fn begin_rename_tag(&mut self, tag: usize) {
        let Some(ui) = self.ui() else { return };
        let name = self.settings.tag_name(tag);
        let b = ui.global::<Bridge>();
        b.set_rename_title("Rename tag".into());
        b.set_rename_text(SharedString::from(name.as_str()));
        b.set_rename_stem(name.len() as i32);
        b.set_rename_error(SharedString::new());
        b.set_rename_group(false);
        b.set_renaming(true);
        self.rename_target = Some(RenameTarget { path: PathBuf::new(), tag: Some(tag), group: false });
    }

    pub(super) fn rename_commit(&mut self, text: &str) {
        let Some(target) = self.rename_target.as_ref() else { return };
        let Some(ui) = self.ui() else { return };
        let b = ui.global::<Bridge>();
        let new_name = text.trim().to_string();
        if target.group {
            if new_name.is_empty() {
                b.set_rename_error("A group needs a name.".into());
                return;
            }
            self.rename_target = None;
            b.set_renaming(false);
            self.save_tab_group(new_name, b.get_rename_shortcut());
            return;
        }
        if let Some(tag) = target.tag {
            if new_name.is_empty() {
                b.set_rename_error("A tag needs a name.".into());
                return;
            }
            self.rename_target = None;
            b.set_renaming(false);
            while self.settings.tag_names.len() < finder_rs::core::tags::COLORS {
                self.settings.tag_names.push(String::new());
            }
            self.settings.tag_names[tag] = new_name;
            self.refresh_tag_names();
            return;
        }
        if let Err(e) = fileops::validate_name(&new_name) {
            b.set_rename_error(e.into());
            return;
        }
        let old_name = target.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let source = target.path.clone();
        self.rename_target = None;
        b.set_renaming(false);
        if new_name == old_name {
            return;
        }
        if let Some(parent) = source.parent()
            && !old_name.eq_ignore_ascii_case(&new_name)
            && parent.join(&new_name).exists()
        {
            self.note(format!("“{new_name}” already exists here."));
            return;
        }
        self.select_after = vec![new_name.clone()];
        self.run_op(Op::Rename { source, new_name });
    }

    pub(super) fn rename_cancel(&mut self) {
        self.rename_target = None;
        if let Some(ui) = self.ui() {
            ui.global::<Bridge>().set_renaming(false);
        }
    }

    pub(super) fn new_folder(&mut self) {
        let Some(dir) = self.current_dir() else { return };
        let name = fileops::unique_name(&dir, "New folder");
        let path = dir.join(&name);
        match std::fs::create_dir(&path) {
            Ok(()) => {
                self.select_after = vec![name];
                self.open_rename_dialog(path, "Name the new folder");
            }
            Err(e) => self.note(format!("Couldn't create a folder here: {e}")),
        }
    }

    // ---- drag and drop -----------------------------------------------------------

    fn drop_sources(data: &DataTransfer) -> Vec<PathBuf> {
        data.file_paths().map(|it| it.map(Path::to_path_buf).collect()).unwrap_or_default()
    }

    /// Copy or move? Ctrl copies, Shift moves; otherwise move on the same
    /// drive and copy across drives, like Explorer.
    fn drop_op(sources: Vec<PathBuf>, dest: PathBuf) -> Op {
        let (ctrl, shift) = fileops::modifiers();
        let copy = ctrl || (!shift && !sources.first().is_some_and(|s| fileops::same_volume(s, &dest)));
        if copy { Op::Copy { sources, dest } } else { Op::Move { sources, dest } }
    }

    fn valid_drop(sources: &[PathBuf], dest: &Path) -> bool {
        if sources.is_empty() || !dest.is_dir() {
            return false;
        }
        if sources.iter().any(|s| s == dest || fileops::is_inside(dest, s)) {
            return false;
        }
        // Dropping items onto the folder they're already in only makes sense as a copy.
        let all_here = sources.iter().all(|s| s.parent() == Some(dest));
        !all_here || fileops::modifiers().0
    }

    fn row_dir(&self, row: usize) -> Option<PathBuf> {
        let model = self.model();
        let idx = model.entry_at(row)?;
        model.with_entry(idx, |e| e.is_dir.then(|| self.tab().loc.path_of(e))).flatten()
    }

    fn pane_dir(&self, pane: i32, row: usize) -> Option<PathBuf> {
        let (model, base) = match pane {
            -1 => return self.row_dir(row),
            -2 => {
                let p = self.tab().preview.as_ref()?;
                (p.model.clone(), Location::Dir(p.path.clone()))
            }
            p => {
                let p = self.tab().panes.get(usize::try_from(p).ok()?)?;
                (p.model.clone(), p.loc.clone())
            }
        };
        let idx = model.entry_at(row)?;
        model.with_entry(idx, |e| e.is_dir.then(|| base.child(&e.name))).flatten()
    }

    fn place_dir(&self, i: usize) -> Option<PathBuf> {
        match self.places.get(i)?.loc.as_ref()? {
            Location::Dir(p) => Some(p.clone()),
            _ => None,
        }
    }

    fn tab_dir(&self, i: usize) -> Option<PathBuf> {
        match &self.tabs.get(i)?.loc {
            Location::Dir(p) => Some(p.clone()),
            _ => None,
        }
    }

    /// Sidebar tag under the drop, if any.
    pub(super) fn drop_tag(&self, kind: DropKind) -> Option<u8> {
        match kind {
            DropKind::Place(i) => match self.places.get(i)?.loc {
                Some(Location::Tag(t)) => Some(t),
                _ => None,
            },
            _ => None,
        }
    }

    /// Dropping files on a sidebar tag tags them.
    pub(super) fn tag_dropped(&mut self, tag: u8, data: &DataTransfer) {
        let paths = Self::drop_sources(data);
        let bit = 1u8 << tag;
        let mut errors = Vec::new();
        for p in &paths {
            let mask = finder_rs::core::tags::read(p) | bit;
            if let Err(e) = finder_rs::core::tags::write(p, mask) {
                errors.push(e);
            }
        }
        // Refresh dots on rows that are on screen.
        let model = self.model();
        for p in &paths {
            if let Some(name) = p.file_name().map(|n| n.to_string_lossy().into_owned())
                && let Some(idx) = model.find_name(&name)
                && model.with_entry(idx, |e| self.tab().loc.path_of(e) == *p).unwrap_or(false)
            {
                model.set_tags(idx, finder_rs::core::tags::read(p));
            }
        }
        let name = self.settings.tag_name(tag as usize);
        match errors.first() {
            Some(e) => self.note(e.clone()),
            None => self.note(format!("Tagged {} with “{name}”", if paths.len() == 1 { "1 item".into() } else { format!("{} items", paths.len()) })),
        }
    }

    pub(super) fn can_drop_to(&self, dest: Option<PathBuf>, data: &DataTransfer) -> bool {
        dest.is_some_and(|d| Self::valid_drop(&Self::drop_sources(data), &d))
    }

    pub(super) fn drop_to(&mut self, dest: Option<PathBuf>, data: &DataTransfer) {
        self.pending_click = None;
        let Some(dest) = dest else { return };
        let sources = Self::drop_sources(data);
        if !Self::valid_drop(&sources, &dest) {
            return;
        }
        if self.current_dir().as_deref() == Some(dest.as_path()) {
            self.expect_new_items();
        }
        self.run_op(Self::drop_op(sources, dest));
    }

    pub(super) fn drop_target(&self, kind: DropKind) -> Option<PathBuf> {
        match kind {
            DropKind::Row(r) => self.row_dir(r),
            DropKind::Pane(p, r) => self.pane_dir(p, r),
            DropKind::Place(i) => self.place_dir(i),
            DropKind::Tab(i) => self.tab_dir(i),
            DropKind::Here => self.current_dir(),
        }
    }

    /// Files dropped from Explorer or another app arrive one by one; they are
    /// collected briefly and then copied/moved into the current folder.
    pub(super) fn external_drop(&mut self, path: PathBuf) {
        let first = self.pending_drops.is_empty();
        self.pending_drops.push(path);
        if first {
            slint::Timer::single_shot(Duration::from_millis(80), || {
                with_app(|a| {
                    let sources = std::mem::take(&mut a.pending_drops);
                    let Some(dest) = a.current_dir() else {
                        a.note("Open a folder to drop files into it.");
                        return;
                    };
                    if !Self::valid_drop(&sources, &dest) {
                        return;
                    }
                    a.expect_new_items();
                    a.run_op(Self::drop_op(sources, dest));
                });
            });
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) enum DropKind {
    Row(usize),
    Pane(i32, usize),
    Place(usize),
    Tab(usize),
    Here,
}

/// Entry point for the winit file-drop hook in `main.rs`.
pub fn external_drop(path: PathBuf) {
    with_app(|a| a.external_drop(path));
}
