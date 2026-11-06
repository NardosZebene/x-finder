//! Live change detection for the open folder.
//!
//! Raw notifications are debounced on a worker thread, the changed paths are
//! stat-ed there (never on the UI thread), and one consolidated event is
//! emitted. Bursts larger than `MAX_INCREMENTAL` become a single `Rescan`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use notify::{RecursiveMode, Watcher};

use super::fs_list::Entry;

const DEBOUNCE: Duration = Duration::from_millis(150);
const MAX_INCREMENTAL: usize = 2000;

#[derive(Debug)]
pub enum WatchEvent {
    /// (name, new state) — `None` means the entry is gone.
    Changed(Vec<(String, Option<Entry>)>),
    Rescan,
}

/// Dropping this stops watching.
pub struct DirWatcher {
    _inner: notify::RecommendedWatcher,
}

pub fn watch<F>(dir: &Path, mut emit: F) -> Option<DirWatcher>
where
    F: FnMut(WatchEvent) + Send + 'static,
{
    let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
    let mut inner = notify::recommended_watcher(tx).ok()?;
    inner.watch(dir, RecursiveMode::NonRecursive).ok()?;
    let dir = dir.to_path_buf();

    thread::Builder::new()
        .name("fs-watch".into())
        .spawn(move || {
            // Exits when the watcher (and with it the sender) is dropped.
            while let Ok(first) = rx.recv() {
                let mut paths: HashSet<PathBuf> = HashSet::new();
                let mut rescan = false;
                let mut take = |ev: notify::Result<notify::Event>| match ev {
                    Ok(ev) if ev.need_rescan() => rescan = true,
                    Ok(ev) => paths.extend(ev.paths.into_iter().filter(|p| p.parent() == Some(dir.as_path()))),
                    Err(_) => rescan = true,
                };
                take(first);
                let deadline = Instant::now() + DEBOUNCE;
                while let Some(left) = deadline.checked_duration_since(Instant::now()) {
                    match rx.recv_timeout(left) {
                        Ok(ev) => take(ev),
                        Err(mpsc::RecvTimeoutError::Timeout) => break,
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                }
                if rescan || paths.len() > MAX_INCREMENTAL {
                    emit(WatchEvent::Rescan);
                } else if !paths.is_empty() {
                    let changes = paths
                        .into_iter()
                        .filter_map(|p| {
                            let name = p.file_name()?.to_string_lossy().into_owned();
                            Some((name, Entry::from_path(&p)))
                        })
                        .collect();
                    emit(WatchEvent::Changed(changes));
                }
            }
        })
        .ok()?;

    Some(DirWatcher { _inner: inner })
}
