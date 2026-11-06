//! "Folder sizes" mode (toolbar button left of the eye): every folder in
//! the list is sized in the background and each row gets a "% of folder"
//! bar, like a disk-usage report.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use finder_rs::core::fs_list::{self, Cancel};
use finder_rs::core::location::Location;
use rayon::prelude::*;
use slint::ComponentHandle;

use super::{App, ViewMode, with_app};
use crate::Bridge;

/// How often finished folders are pushed to the UI.
const FLUSH: Duration = Duration::from_millis(150);

impl App {
    pub(super) fn toggle_folder_sizes(&mut self) {
        self.folder_sizes = !self.folder_sizes;
        self.settings.show_folder_sizes = self.folder_sizes;
        if let Some(ui) = self.ui() {
            ui.global::<Bridge>().set_show_sizes(self.folder_sizes);
        }
        for t in &self.tabs {
            t.model.set_sizes_mode(self.folder_sizes);
        }
        finder_rs::core::sort::set_size_mixes_folders(self.folder_sizes);
        if self.folder_sizes {
            // The sizes and bars live in the list view.
            if self.view != ViewMode::List {
                self.set_view(ViewMode::List);
            }
            // Biggest first (folders re-sort as their sizes come in).
            self.opts.sort = finder_rs::core::sort::SortKey::Size;
            self.opts.ascending = false;
            self.apply_opts(true);
            self.start_folder_sizes();
        } else if let Some((_, c)) = self.sizes_scan.take() {
            c.cancel();
        }
        self.save_settings();
    }

    /// Sizes the active tab's folders (cached ones at once, the rest on
    /// worker threads). Called after a listing finishes and on tab switch.
    pub(super) fn start_folder_sizes(&mut self) {
        if let Some((_, c)) = self.sizes_scan.take() {
            c.cancel();
        }
        let Some(tab) = self.tabs.get(self.active) else { return };
        tab.model.set_sizes_mode(self.folder_sizes);
        if !self.folder_sizes || tab.loading || matches!(tab.loc, Location::Computer | Location::Trash) {
            return;
        }
        let mut cached = Vec::new();
        let mut todo: Vec<(u32, PathBuf)> = Vec::new();
        for idx in tab.model.unsized_dirs() {
            let Some(path) = tab.model.with_entry(idx, |e| tab.loc.path_of(e)) else { continue };
            match self.size_cache.get(&path) {
                Some(&(bytes, _)) => cached.push((idx, bytes)),
                None => todo.push((idx, path)),
            }
        }
        if !cached.is_empty() {
            tab.model.set_dir_sizes(&cached);
        }
        if todo.is_empty() {
            self.folder_sizes_done();
            return;
        }
        let tab_id = tab.id;
        let cancel = Cancel::new();
        self.sizes_scan = Some((tab_id, cancel.clone()));

        let (tx, rx) = mpsc::channel::<(u32, PathBuf, (u64, u64))>();
        let work_cancel = cancel.clone();
        std::thread::spawn(move || {
            todo.par_iter().for_each_with(tx, |tx, (idx, path)| {
                if work_cancel.is_cancelled() {
                    return;
                }
                if let Some(r) = fs_list::dir_size(path, &work_cancel) {
                    let _ = tx.send((*idx, path.clone(), r));
                }
            });
        });
        // Collector: batches results so the list refreshes a few times a
        // second, not once per folder.
        std::thread::spawn(move || {
            let mut batch = Vec::new();
            let mut last = Instant::now();
            loop {
                let msg = rx.recv_timeout(FLUSH);
                let finished = matches!(msg, Err(mpsc::RecvTimeoutError::Disconnected));
                if let Ok(r) = msg {
                    batch.push(r);
                }
                if (finished || last.elapsed() >= FLUSH) && (!batch.is_empty() || finished) {
                    let done = std::mem::take(&mut batch);
                    let cancel = cancel.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if !cancel.is_cancelled() {
                            with_app(|a| a.on_folder_sizes(tab_id, done, finished));
                        }
                    });
                    last = Instant::now();
                }
                if finished {
                    break;
                }
            }
        });
    }

    fn on_folder_sizes(&mut self, tab_id: u64, done: Vec<(u32, PathBuf, (u64, u64))>, finished: bool) {
        let Some(tab) = self.tabs.iter().find(|t| t.id == tab_id) else { return };
        let sizes: Vec<(u32, u64)> = done.iter().map(|(i, _, (b, _))| (*i, *b)).collect();
        if !sizes.is_empty() {
            tab.model.set_dir_sizes(&sizes);
            // Ranked by size: move folders into place as they're measured.
            if self.opts.sort == finder_rs::core::sort::SortKey::Size && tab.id == self.tabs[self.active].id {
                tab.model.sort(&tab.applied);
            }
        }
        for (_, path, r) in done {
            self.size_cache.insert(path, r);
        }
        if finished {
            self.sizes_scan = None;
            self.folder_sizes_done();
        }
        self.sync_status();
    }

    /// All folders sized: re-sort when sorting by size.
    fn folder_sizes_done(&mut self) {
        if self.opts.sort == finder_rs::core::sort::SortKey::Size
            && let Some(tab) = self.tabs.get(self.active)
        {
            tab.model.sort(&tab.applied);
            self.sync_status();
        }
    }

    /// A tab is being re-listed: its entry indices are about to change.
    pub(super) fn cancel_folder_sizes_for(&mut self, tab_id: u64) {
        if self.sizes_scan.as_ref().is_some_and(|(id, _)| *id == tab_id)
            && let Some((_, c)) = self.sizes_scan.take()
        {
            c.cancel();
        }
    }
}
