//! Split layouts: 2 or 4 folders on screen at once (side by side, stacked,
//! one + two, or a grid), for copying between folders. Every pane is a tab;
//! the focused pane is the full view, the others are live lists that take
//! drops. Picked from the toolbar's layout button (like Windows snap layouts).

use std::path::PathBuf;

use finder_rs::core::location::Location;
use finder_rs::core::watcher::{self, DirWatcher, WatchEvent};
use slint::{ComponentHandle, Model, ModelRc};

use super::{App, with_app};
use crate::{Bridge, SlotData};

/// Panes per layout: single, side by side, stacked, one + two, grid of four.
pub(super) fn pane_count(layout: usize) -> usize {
    match layout {
        0 => 1,
        3 => 3,
        4 => 4,
        _ => 2,
    }
}

/// Watches a pane that isn't focused, so it stays current while files are
/// copied into it.
pub(super) struct PaneWatch {
    tab_id: u64,
    path: PathBuf,
    _watcher: DirWatcher,
}

impl App {
    pub(super) fn set_layout(&mut self, layout: usize) {
        let layout = layout.min(4);
        let n = pane_count(layout);
        // Missing panes open on the current folder.
        while self.tabs.len() < n {
            let loc = self.tab().loc.clone();
            let tab = self.make_tab(loc);
            self.tabs.push(tab);
            let idx = self.tabs.len() - 1;
            self.start_load(idx);
        }
        self.layout = layout;
        self.fix_slots(None);
        self.settings.layout = layout as i32;
        self.sync_tabs();
        self.rewatch();
        self.save_settings();
    }

    /// Keeps `pane_ids` valid: existing tabs only, the focused tab always
    /// shown (in the slot of `prev` when it just took over), filled up with
    /// other open tabs.
    pub(super) fn fix_slots(&mut self, prev: Option<u64>) {
        let active_id = self.tabs.get(self.active).map(|t| t.id);
        if self.layout == 0 {
            self.pane_ids = active_id.into_iter().collect();
            return;
        }
        // Fewer tabs than panes (a pane was closed): use a smaller layout.
        let n = pane_count(self.layout);
        if self.tabs.len() < n {
            self.layout = match self.tabs.len() {
                0 | 1 => 0,
                2 => {
                    if self.layout == 2 { 2 } else { 1 }
                }
                _ => 3,
            };
            self.settings.layout = self.layout as i32;
            if self.layout == 0 {
                self.pane_ids = active_id.into_iter().collect();
                return;
            }
        }
        let n = pane_count(self.layout);
        let mut ids: Vec<u64> = Vec::new();
        for id in self.pane_ids.drain(..) {
            if self.tabs.iter().any(|t| t.id == id) && !ids.contains(&id) {
                ids.push(id);
            }
        }
        if let Some(a) = active_id
            && !ids.contains(&a)
        {
            match prev.and_then(|p| ids.iter().position(|&id| id == p)) {
                Some(slot) => ids[slot] = a,
                None if ids.len() < n => ids.push(a),
                None => {
                    let last = ids.len() - 1;
                    ids[last] = a;
                }
            }
        }
        for t in &self.tabs {
            if ids.len() >= n {
                break;
            }
            if !ids.contains(&t.id) {
                ids.push(t.id);
            }
        }
        ids.truncate(n);
        self.pane_ids = ids;
    }

    /// Pushes the panes to the UI (called with every tab-bar sync).
    pub(super) fn sync_slots(&self) {
        let Some(ui) = self.ui() else { return };
        let b = ui.global::<Bridge>();
        b.set_layout(self.layout as i32);
        let mut active_slot = 0;
        let rows: Vec<SlotData> = self
            .pane_ids
            .iter()
            .enumerate()
            .filter_map(|(k, id)| {
                let idx = self.tabs.iter().position(|t| t.id == *id)?;
                let t = &self.tabs[idx];
                if idx == self.active {
                    active_slot = k;
                }
                let n = t.model.row_count();
                Some(SlotData {
                    tab: idx as i32,
                    title: self.loc_title(&t.loc).into(),
                    status: if t.loading {
                        "Loading…".into()
                    } else {
                        format!("{} item{}", finder_rs::core::format::thousands(n), if n == 1 { "" } else { "s" }).into()
                    },
                    files: ModelRc::from(t.model.clone()),
                    active: idx == self.active,
                })
            })
            .collect();
        // Update in place so the other panes keep their scroll position.
        let model = &self.slots_model;
        if model.row_count() == rows.len() {
            for (k, r) in rows.into_iter().enumerate() {
                model.set_row_data(k, r);
            }
        } else {
            model.set_vec(rows);
        }
        b.set_active_slot(active_slot as i32);
    }

    fn slot_tab(&self, slot: usize) -> Option<usize> {
        let id = *self.pane_ids.get(slot)?;
        self.tabs.iter().position(|t| t.id == id)
    }

    pub(super) fn slot_focus(&mut self, slot: usize) {
        if let Some(i) = self.slot_tab(slot)
            && i != self.active
        {
            self.activate(i);
        }
    }

    pub(super) fn slot_pressed(&mut self, slot: usize, row: usize, ctrl: bool, shift: bool) {
        self.slot_focus(slot);
        self.row_pressed(row, ctrl, shift);
    }

    pub(super) fn slot_activated(&mut self, slot: usize, row: usize) {
        self.slot_focus(slot);
        self.row_activated(row);
    }

    pub(super) fn slot_context(&mut self, slot: usize, row: usize, x: f32, y: f32) {
        self.slot_focus(slot);
        self.row_context(row, x, y);
    }

    /// Watches the visible panes other than the focused one.
    pub(super) fn rewatch_panes(&mut self) {
        let want: Vec<(u64, PathBuf)> = if self.layout == 0 {
            Vec::new()
        } else {
            self.pane_ids
                .iter()
                .filter_map(|id| self.tabs.iter().enumerate().find(|(_, t)| t.id == *id))
                .filter(|(i, _)| *i != self.active)
                .filter_map(|(_, t)| match &t.loc {
                    Location::Dir(p) => Some((t.id, p.clone())),
                    _ => None,
                })
                .collect()
        };
        self.pane_watches.retain(|w| want.iter().any(|(id, p)| *id == w.tab_id && *p == w.path));
        for (id, path) in want {
            if self.pane_watches.iter().any(|w| w.tab_id == id && w.path == path) {
                continue;
            }
            let watch_path = path.clone();
            if let Some(w) = watcher::watch(&path, move |ev| {
                let watch_path = watch_path.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    with_app(|a| a.on_pane_watch(id, watch_path.clone(), ev));
                });
            }) {
                self.pane_watches.push(PaneWatch { tab_id: id, path, _watcher: w });
            }
        }
    }

    fn on_pane_watch(&mut self, tab_id: u64, path: PathBuf, ev: WatchEvent) {
        let Some(idx) = self.tabs.iter().position(|t| t.id == tab_id) else { return };
        // The focused pane has its own watcher; ignore stale events.
        if idx == self.active || self.tabs[idx].loading || self.tabs[idx].loc != Location::Dir(path) {
            return;
        }
        match ev {
            WatchEvent::Rescan => self.start_load(idx),
            WatchEvent::Changed(changes) => {
                let tab = &self.tabs[idx];
                for (name, entry) in changes {
                    tab.model.apply_change(&name, entry, &tab.applied);
                }
            }
        }
        self.sync_slots();
    }
}
