//! The file list model handed to Slint's virtualized `ListView`.
//!
//! Rows are formatted lazily in `row_data`, so only the ~40 rows on screen
//! ever get date/size/kind strings, no matter how big the folder is.
//!
//! Three layers:
//! - `entries`: everything listed, in arrival order (indices are stable);
//! - `order`:   live entries, sorted once loading finishes;
//! - `view`:    `order` minus hidden / filtered entries = the visible rows.
//!
//! Selection, cursor and anchor are stored by entry index, so they survive
//! sorting, filtering and live updates.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::cmp::Ordering;
use std::path::PathBuf;
use std::rc::Rc;

use finder_rs::core::format;
use finder_rs::core::preview;
use finder_rs::core::fs_list::Entry;
use finder_rs::core::sort::{self, SortKey};
use slint::{Model, ModelNotify, ModelTracker};

use crate::FileRow;
use crate::thumbs::{Job, JobKind, Thumbs};
use finder_rs::core::fs_list::TAGS_UNKNOWN;
use finder_rs::core::tags;

/// Lets a model fetch thumbnails, icons and tags for the rows it renders.
pub struct ThumbCtx {
    pub service: Rc<Thumbs>,
    pub model_id: u64,
    pub dir: PathBuf,
    /// Read colour tags (off for network drives, where it would be slow).
    pub read_tags: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ViewOpts {
    pub sort: SortKey,
    pub ascending: bool,
    pub show_hidden: bool,
    /// Filter text as typed (lower-cased).
    pub filter: String,
    /// Parsed form of `filter` ("pdf:", "kind:image", "size:>5mb", words…).
    pub query: finder_rs::core::query::Query,
}

impl Default for ViewOpts {
    fn default() -> Self {
        ViewOpts {
            sort: SortKey::Name,
            ascending: true,
            show_hidden: false,
            filter: String::new(),
            query: Default::default(),
        }
    }
}

impl ViewOpts {
    fn passes(&self, e: &Entry) -> bool {
        !e.removed && (self.show_hidden || !e.hidden) && (self.query.is_empty() || self.query.matches(e))
    }
}

#[derive(Default)]
pub struct FileModel {
    entries: RefCell<Vec<Entry>>,
    order: RefCell<Vec<u32>>,
    view: RefCell<Vec<u32>>,
    cursor: Cell<Option<u32>>,
    anchor: Cell<Option<u32>>,
    notify: ModelNotify,
    thumbs: RefCell<Option<ThumbCtx>>,
    want_thumbs: Cell<bool>,
    /// "Folder sizes" mode: folders show their total size and every row a
    /// share-of-folder bar.
    sizes_on: Cell<bool>,
    /// Folders whose size has been worked out (their `size` is then valid).
    sized: RefCell<HashSet<u32>>,
    /// Sum of the visible rows' sizes, recomputed lazily.
    total_bytes: Cell<Option<u64>>,
}

impl Model for FileModel {
    type Data = FileRow;

    fn row_count(&self) -> usize {
        self.view.borrow().len()
    }

    fn row_data(&self, row: usize) -> Option<FileRow> {
        let idx = *self.view.borrow().get(row)?;
        let entries = self.entries.borrow();
        let e = entries.get(idx as usize)?;
        let sized = e.is_dir && self.sized.borrow().contains(&idx);
        let share = if self.sizes_on.get() && (!e.is_dir || sized) {
            match self.total_bytes() {
                0 => 0.0,
                t => e.size as f32 / t as f32,
            }
        } else {
            -1.0
        };
        let thumb = self.thumbnail(idx, e);
        let peek = self.folder_preview(idx, e, 1);
        let sys_icon = self.windows_icon(idx, e, false);
        Some(FileRow {
            has_peek: peek.is_some(),
            peek: peek.unwrap_or_default(),
            has_thumb: thumb.is_some(),
            thumb: thumb.unwrap_or_default(),
            has_sys_icon: sys_icon.is_some(),
            sys_icon: sys_icon.unwrap_or_default(),
            tags: self.tags_of(e) as i32,
            // Folders take the colour of their first tag.
            tint: match self.tags_of(e) {
                0 => -1,
                m => m.trailing_zeros() as i32,
            },
            name: e.name.as_ref().into(),
            kind: format::kind_label(e).as_ref().into(),
            size: if !e.is_dir || sized {
                format::size(e.size).into()
            } else if self.sizes_on.get() && e.drive_type == 0 {
                "…".into()
            } else {
                "--".into()
            },
            share,
            share_text: if share < 0.0 { "".into() } else { format!("{:.1}%", share * 100.0).into() },
            date: format::date(e.modified).into(),
            created: format::date(e.created).into(),
            icon: format::icon(e).into(),
            is_dir: e.is_dir,
            selected: e.selected,
            cursor: self.cursor.get() == Some(idx),
            hidden: e.hidden,
        })
    }

    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }
}

impl FileModel {
    /// Where thumbnails come from; `dir` is the folder being listed.
    pub fn set_thumbs(&self, ctx: Option<ThumbCtx>) {
        *self.thumbs.borrow_mut() = ctx;
    }

    /// Thumbnails are only fetched in views that show them.
    pub fn set_want_thumbs(&self, want: bool) {
        if self.want_thumbs.replace(want) != want {
            self.notify.reset();
        }
    }

    pub fn notify_row(&self, row: usize) {
        self.notify.row_changed(row);
    }

    pub fn notify_all(&self) {
        self.notify.reset();
    }

    /// Tile image for icon/gallery views: the content thumbnail when Windows
    /// has one, otherwise the file's large Windows icon. `None` (with a
    /// background request) until ready; the UI then shows its own icon.
    fn thumbnail(&self, idx: u32, e: &Entry) -> Option<slint::Image> {
        if !self.want_thumbs.get() {
            return None;
        }
        let ctx = self.thumbs.borrow();
        let ctx = ctx.as_ref()?;
        if e.link_dir.is_some() {
            return None;
        }
        if e.is_dir {
            return self.folder_preview(idx, e, 2);
        }
        if !e.is_dir && (preview::has_thumbnail(&e.name) || format::icon(e) == "video") && ctx.service.worth_trying(&e.name) {
            let path = e.path.clone().unwrap_or_else(|| ctx.dir.join(&*e.name));
            let size = ctx.service.size();
            let key = Thumbs::key(&path.to_string_lossy(), e.modified, size);
            if let Some(img) = ctx.service.get(&key) {
                return Some(img);
            }
            if !ctx.service.is_failed(&key) {
                ctx.service.request(Job { key, path, size, kind: JobKind::Thumb, model_id: ctx.model_id, entry: idx });
                return None;
            }
        }
        self.windows_icon(idx, e, true)
    }

    /// What's inside a folder, when folder previews are on in `mode`
    /// (1: a picture for our folder icon, 2: Windows' own folder image).
    /// Never on network drives (reading every folder there is slow).
    fn folder_preview(&self, idx: u32, e: &Entry, mode: u8) -> Option<slint::Image> {
        if !e.is_dir || e.link_dir.is_some() || e.drive_type != 0 || !self.want_thumbs.get() {
            return None;
        }
        let ctx = self.thumbs.borrow();
        let ctx = ctx.as_ref()?;
        if ctx.service.folder_previews.get() != mode || !ctx.read_tags {
            return None;
        }
        let path = e.path.clone().unwrap_or_else(|| ctx.dir.join(&*e.name));
        let size = ctx.service.size();
        // The folder's date changes when files are added or removed.
        let key = format!("folder{mode}\u{1}{}", Thumbs::key(&path.to_string_lossy(), e.modified, size));
        if let Some(img) = ctx.service.get(&key) {
            return Some(img);
        }
        if !ctx.service.is_failed(&key) {
            let kind = if mode == 2 { JobKind::FolderWindows } else { JobKind::FolderPeek };
            ctx.service.request(Job { key, path, size, kind, model_id: ctx.model_id, entry: idx });
        }
        None
    }

    /// The file's Windows icon (small for rows, large for tiles), when the
    /// Windows icon style is on. Folders keep the app's folder icon.
    fn windows_icon(&self, idx: u32, e: &Entry, large: bool) -> Option<slint::Image> {
        let ctx = self.thumbs.borrow();
        let ctx = ctx.as_ref()?;
        if !ctx.service.windows_icons.get() || e.is_dir || e.link_dir.is_some() {
            return None;
        }
        let path = e.path.clone().unwrap_or_else(|| ctx.dir.join(&*e.name));
        let size = if large { ctx.service.size() } else { ctx.service.small() };
        let key = Thumbs::icon_key(&path, e.is_dir, size);
        if let Some(img) = ctx.service.get(&key) {
            return Some(img);
        }
        if !ctx.service.is_failed(&key) {
            ctx.service.request(Job { key, path, size, kind: JobKind::Icon, model_id: ctx.model_id, entry: idx });
        }
        None
    }

    /// Colour tags, read from the file the first time its row is shown.
    fn tags_of(&self, e: &Entry) -> u8 {
        let cached = e.tags.get();
        if cached != TAGS_UNKNOWN {
            return cached;
        }
        let ctx = self.thumbs.borrow();
        let mask = match ctx.as_ref() {
            Some(c) if c.read_tags => tags::read(&e.path.clone().unwrap_or_else(|| c.dir.join(&*e.name))),
            _ => 0,
        };
        e.tags.set(mask);
        mask
    }

    /// Forget cached tags (after tagging), so rows re-read them.
    pub fn set_tags(&self, idx: u32, mask: u8) {
        if let Some(e) = self.entries.borrow().get(idx as usize) {
            e.tags.set(mask);
        }
        if let Some(r) = self.row_of(idx) {
            self.notify.row_changed(r);
        }
    }

    pub fn tags_at(&self, idx: u32) -> u8 {
        self.entries.borrow().get(idx as usize).map(|e| self.tags_of(e)).unwrap_or(0)
    }

    pub fn clear(&self) {
        self.sized.borrow_mut().clear();
        self.total_bytes.set(None);
        self.entries.borrow_mut().clear();
        self.order.borrow_mut().clear();
        self.view.borrow_mut().clear();
        self.cursor.set(None);
        self.anchor.set(None);
        self.notify.reset();
    }

    /// Appends a streamed batch. Rows are added unsorted; `sort` runs once
    /// the listing completes.
    pub fn append(&self, batch: Vec<Entry>, opts: &ViewOpts) {
        let (first, added) = {
            let mut entries = self.entries.borrow_mut();
            let mut order = self.order.borrow_mut();
            let mut view = self.view.borrow_mut();
            let first = view.len();
            for e in batch {
                let idx = entries.len() as u32;
                if opts.passes(&e) {
                    view.push(idx);
                }
                order.push(idx);
                entries.push(e);
            }
            (first, view.len() - first)
        };
        self.total_bytes.set(None);
        if added > 0 {
            self.notify.row_added(first, added);
        }
    }

    pub fn sort(&self, opts: &ViewOpts) {
        {
            let entries = self.entries.borrow();
            let mut order = self.order.borrow_mut();
            sort::sort_indices(&mut order, &entries, opts.sort, opts.ascending);
        }
        self.refilter(opts);
    }

    /// Re-applies hidden/filter settings without re-sorting.
    /// Search results, Recents, tags: hide items whose file is gone
    /// (deleted or moved by the app). Those lists aren't watched.
    pub fn drop_missing(&self, opts: &ViewOpts) {
        let mut any = false;
        {
            let mut entries = self.entries.borrow_mut();
            for e in entries.iter_mut() {
                if !e.removed && e.path.as_ref().is_some_and(|p| !p.exists()) {
                    e.removed = true;
                    e.selected = false;
                    any = true;
                }
            }
        }
        if any {
            self.total_bytes.set(None);
            self.refilter(opts);
        }
    }

    pub fn refilter(&self, opts: &ViewOpts) {
        {
            let entries = self.entries.borrow();
            let order = self.order.borrow();
            let mut view = self.view.borrow_mut();
            view.clear();
            view.extend(order.iter().copied().filter(|&i| opts.passes(&entries[i as usize])));
        }
        self.total_bytes.set(None);
        self.notify.reset();
    }

    // ---- folder sizes mode ------------------------------------------------

    pub fn set_sizes_mode(&self, on: bool) {
        if self.sizes_on.replace(on) != on {
            self.notify_all();
        }
    }

    /// Folders (entry index) still waiting for their size.
    pub fn unsized_dirs(&self) -> Vec<u32> {
        let entries = self.entries.borrow();
        let sized = self.sized.borrow();
        self.order
            .borrow()
            .iter()
            .copied()
            .filter(|&i| {
                let e = &entries[i as usize];
                e.is_dir && !e.removed && e.drive_type == 0 && !sized.contains(&i)
            })
            .collect()
    }

    /// Stores computed folder sizes; every row's share changes, so all rows
    /// are refreshed.
    pub fn set_dir_sizes(&self, sizes: &[(u32, u64)]) {
        {
            let mut entries = self.entries.borrow_mut();
            let mut sized = self.sized.borrow_mut();
            for &(idx, bytes) in sizes {
                if let Some(e) = entries.get_mut(idx as usize) {
                    e.size = bytes;
                    sized.insert(idx);
                }
            }
        }
        self.total_bytes.set(None);
        // Row-by-row (not a reset) so the list keeps its scroll position.
        for r in 0..self.view.borrow().len() {
            self.notify.row_changed(r);
        }
    }

    fn total_bytes(&self) -> u64 {
        if let Some(t) = self.total_bytes.get() {
            return t;
        }
        let entries = self.entries.borrow();
        let t = self.view.borrow().iter().map(|&i| entries[i as usize].size).sum();
        self.total_bytes.set(Some(t));
        t
    }

    pub fn total(&self) -> usize {
        self.order.borrow().len()
    }

    pub fn entry_at(&self, row: usize) -> Option<u32> {
        self.view.borrow().get(row).copied()
    }

    pub fn row_of(&self, idx: u32) -> Option<usize> {
        self.view.borrow().iter().position(|&i| i == idx)
    }

    pub fn with_entry<R>(&self, idx: u32, f: impl FnOnce(&Entry) -> R) -> Option<R> {
        self.entries.borrow().get(idx as usize).map(f)
    }

    pub fn find_name(&self, name: &str) -> Option<u32> {
        let key = name.to_lowercase();
        self.entries.borrow().iter().position(|e| !e.removed && *e.key == *key).map(|i| i as u32)
    }

    /// First visible row at or after `from` (wrapping) whose name starts with `prefix`.
    pub fn find_prefix(&self, prefix: &str, from: usize) -> Option<usize> {
        let view = self.view.borrow();
        let entries = self.entries.borrow();
        let n = view.len();
        (0..n).map(|k| (from + k) % n).find(|&r| entries[view[r] as usize].key.starts_with(prefix))
    }

    // ---- cursor & selection -------------------------------------------------

    pub fn cursor(&self) -> Option<u32> {
        self.cursor.get()
    }

    pub fn cursor_row(&self) -> Option<usize> {
        self.cursor.get().and_then(|c| self.row_of(c))
    }

    pub fn anchor(&self) -> Option<u32> {
        self.anchor.get()
    }

    pub fn set_anchor(&self, idx: Option<u32>) {
        self.anchor.set(idx);
    }

    pub fn set_cursor(&self, idx: Option<u32>) {
        let old = self.cursor.replace(idx);
        if old == idx {
            return;
        }
        for i in [old, idx].into_iter().flatten() {
            if let Some(r) = self.row_of(i) {
                self.notify.row_changed(r);
            }
        }
    }

    /// Sets the selection of every visible row from `want(row)`; hidden
    /// entries are deselected. Only rows that change are re-rendered.
    pub fn select_where(&self, want: impl Fn(usize) -> bool) {
        let mut changed = Vec::new();
        {
            let view = self.view.borrow();
            let mut entries = self.entries.borrow_mut();
            let mut visible = vec![false; entries.len()];
            for (row, &i) in view.iter().enumerate() {
                visible[i as usize] = true;
                let w = want(row);
                let e = &mut entries[i as usize];
                if e.selected != w {
                    e.selected = w;
                    changed.push(row);
                }
            }
            for (i, e) in entries.iter_mut().enumerate() {
                if !visible[i] {
                    e.selected = false;
                }
            }
        }
        for r in changed {
            self.notify.row_changed(r);
        }
    }

    pub fn select_only(&self, row: usize) {
        self.select_where(|r| r == row);
    }

    pub fn select_range(&self, a: usize, b: usize, keep_others: bool) {
        let (lo, hi) = (a.min(b), a.max(b));
        if keep_others {
            let current: Vec<bool> = self.selected_flags();
            self.select_where(|r| (lo..=hi).contains(&r) || current[r]);
        } else {
            self.select_where(|r| (lo..=hi).contains(&r));
        }
    }

    pub fn toggle(&self, row: usize) {
        let Some(i) = self.entry_at(row) else { return };
        {
            let mut entries = self.entries.borrow_mut();
            let e = &mut entries[i as usize];
            e.selected = !e.selected;
        }
        self.notify.row_changed(row);
    }

    fn selected_flags(&self) -> Vec<bool> {
        let entries = self.entries.borrow();
        self.view.borrow().iter().map(|&i| entries[i as usize].selected).collect()
    }

    /// Selected entries in display order.
    pub fn selected(&self) -> Vec<u32> {
        let entries = self.entries.borrow();
        self.view.borrow().iter().copied().filter(|&i| entries[i as usize].selected).collect()
    }

    pub fn selected_count(&self) -> usize {
        let entries = self.entries.borrow();
        self.view.borrow().iter().filter(|&&i| entries[i as usize].selected).count()
    }

    // ---- live updates ------------------------------------------------------

    /// Applies one change reported by the watcher: `Some` = created/modified,
    /// `None` = deleted.
    pub fn apply_change(&self, name: &str, new: Option<Entry>, opts: &ViewOpts) {
        let existing = self.find_name(name);
        self.total_bytes.set(None);

        // Take the old entry out of the sorted lists.
        if let Some(idx) = existing {
            let removed_row = {
                let mut order = self.order.borrow_mut();
                if let Some(p) = order.iter().position(|&i| i == idx) {
                    order.remove(p);
                }
                let mut view = self.view.borrow_mut();
                let row = view.iter().position(|&i| i == idx);
                if let Some(r) = row {
                    view.remove(r);
                }
                row
            };
            if let Some(r) = removed_row {
                self.notify.row_removed(r, 1);
            }
        }

        let idx = {
            let mut entries = self.entries.borrow_mut();
            match (existing, new) {
                (Some(idx), Some(mut e)) => {
                    e.selected = entries[idx as usize].selected;
                    // A folder that changed keeps its computed size.
                    if e.is_dir && self.sized.borrow().contains(&idx) {
                        e.size = entries[idx as usize].size;
                    }
                    entries[idx as usize] = e;
                    Some(idx)
                }
                (Some(idx), None) => {
                    let e = &mut entries[idx as usize];
                    e.removed = true;
                    e.selected = false;
                    None
                }
                (None, Some(e)) => {
                    entries.push(e);
                    Some(entries.len() as u32 - 1)
                }
                (None, None) => None,
            }
        };

        let Some(idx) = idx else {
            if existing.is_some() && self.cursor.get() == existing {
                self.cursor.set(None);
            }
            return;
        };

        // Insert at its sorted position.
        let inserted_row = {
            let entries = self.entries.borrow();
            let e = &entries[idx as usize];
            let before = |&j: &u32| sort::compare(&entries[j as usize], e, opts.sort, opts.ascending) == Ordering::Less;
            let mut order = self.order.borrow_mut();
            let p = order.partition_point(before);
            order.insert(p, idx);
            if opts.passes(e) {
                let mut view = self.view.borrow_mut();
                let r = view.partition_point(before);
                view.insert(r, idx);
                Some(r)
            } else {
                None
            }
        };
        if let Some(r) = inserted_row {
            self.notify.row_added(r, 1);
        }
    }
}
