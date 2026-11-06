//! Streamed directory listing.
//!
//! A folder is read on a background thread and handed to the caller in
//! batches, so the first rows appear almost immediately even for folders with
//! 100k+ entries. Each listing carries a [`Cancel`] token; navigating away
//! cancels stale work (plan §9, rules 1-3).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};

/// Sentinel for "modification time unknown".
pub const NO_TIME: i64 = i64::MIN;

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: Box<str>,
    /// Lower-cased name, used for sorting, type-to-select and filtering.
    pub key: Box<str>,
    pub is_dir: bool,
    pub hidden: bool,
    pub size: u64,
    /// Seconds since the Unix epoch, or [`NO_TIME`].
    pub modified: i64,
    /// Creation time, same encoding as `modified`.
    pub created: i64,
    /// Non-zero for entries that are drives (Windows `GetDriveTypeW` value).
    pub drive_type: u8,
    pub selected: bool,
    /// Tombstone for entries deleted while the folder is open.
    pub removed: bool,
    /// Colour tags, read lazily when the row is first shown
    /// ([`TAGS_UNKNOWN`] until then).
    pub tags: TagCache,
    /// Full path for items that don't live in the listed folder (tag views).
    pub path: Option<PathBuf>,
    /// For .lnk shortcuts that point to a folder: that folder.
    pub link_dir: Option<PathBuf>,
}

/// `Entry::tags` value before the file's tags have been read.
pub const TAGS_UNKNOWN: u8 = 0xff;

/// Lazily filled tag mask. Atomic so entries stay shareable with the
/// parallel sort.
#[derive(Debug)]
pub struct TagCache(std::sync::atomic::AtomicU8);

impl TagCache {
    pub fn get(&self) -> u8 {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }
    pub fn set(&self, v: u8) {
        self.0.store(v, std::sync::atomic::Ordering::Relaxed)
    }
}

impl Default for TagCache {
    fn default() -> Self {
        TagCache(std::sync::atomic::AtomicU8::new(TAGS_UNKNOWN))
    }
}

impl Clone for TagCache {
    fn clone(&self) -> Self {
        TagCache(std::sync::atomic::AtomicU8::new(self.get()))
    }
}

impl Entry {
    pub fn new(name: String, is_dir: bool, hidden: bool, size: u64, modified: i64) -> Self {
        let key = name.to_lowercase().into_boxed_str();
        Entry {
            name: name.into_boxed_str(),
            key,
            is_dir,
            hidden,
            size,
            modified,
            created: NO_TIME,
            drive_type: 0,
            selected: false,
            removed: false,
            tags: TagCache::default(),
            path: None,
            link_dir: None,
        }
    }

    pub fn drive(root: &str, drive_type: u8) -> Self {
        let mut e = Entry::new(root.to_string(), true, false, 0, NO_TIME);
        e.drive_type = drive_type;
        e
    }

    /// Builds an entry from a `read_dir` item. On Windows the metadata comes
    /// from the directory enumeration itself, so this costs no extra syscall
    /// (except for symlinks, which are followed).
    pub fn from_dir_entry(de: &fs::DirEntry) -> Option<Self> {
        let name = de.file_name().to_string_lossy().into_owned();
        let own = de.metadata().ok()?;
        let hidden = is_hidden(&name, &own);
        let meta = if own.file_type().is_symlink() {
            fs::metadata(de.path()).unwrap_or(own)
        } else {
            own
        };
        let mut e = Self::from_meta(name, &meta, hidden);
        e.resolve_folder_shortcut(&de.path());
        Some(e)
    }

    /// Shortcuts (.lnk) to folders behave like folders: the app shows them
    /// with its folder icon and opens the target in place.
    fn resolve_folder_shortcut(&mut self, path: &Path) {
        if !self.is_dir && super::format::extension(&self.key) == "lnk" {
            super::shell::init_thread(true);
            self.link_dir = super::shellx::resolve_shortcut(path).filter(|t| t.is_dir());
        }
    }

    /// Builds an entry by stat-ing a path (used by the watcher).
    pub fn from_path(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_string_lossy().into_owned();
        let own = fs::symlink_metadata(path).ok()?;
        let hidden = is_hidden(&name, &own);
        let meta = if own.file_type().is_symlink() {
            fs::metadata(path).unwrap_or(own)
        } else {
            own
        };
        let mut e = Self::from_meta(name, &meta, hidden);
        e.resolve_folder_shortcut(path);
        Some(e)
    }

    fn from_meta(name: String, meta: &fs::Metadata, hidden: bool) -> Self {
        let is_dir = meta.is_dir();
        let secs = |t: std::io::Result<std::time::SystemTime>| {
            t.ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(NO_TIME)
        };
        let size = if is_dir { 0 } else { meta.len() };
        let mut e = Entry::new(name, is_dir, hidden, size, secs(meta.modified()));
        e.created = secs(meta.created());
        e
    }
}

#[cfg(windows)]
fn is_hidden(_name: &str, meta: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    meta.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0
}

#[cfg(not(windows))]
fn is_hidden(name: &str, _meta: &fs::Metadata) -> bool {
    name.starts_with('.')
}

/// Cooperative cancellation flag shared between the UI and a worker.
#[derive(Clone, Default, Debug)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

#[derive(Debug)]
pub enum ListEvent {
    Batch(Vec<Entry>),
    Done { total: usize, elapsed: Duration, error: Option<String> },
}

/// First batch is small so the first rows paint quickly; later batches are
/// larger to keep per-batch overhead on the UI thread low.
const FIRST_BATCH: usize = 256;
const BATCH: usize = 4096;
const MAX_BATCH_DELAY: Duration = Duration::from_millis(60);

/// Lists `path` on a background thread, calling `emit` with batches and a
/// final `Done`. Nothing is emitted after `cancel` is set.
pub fn list_dir<F>(path: PathBuf, cancel: Cancel, emit: F)
where
    F: FnMut(ListEvent) + Send + 'static,
{
    thread::Builder::new()
        .name("fs-list".into())
        .spawn(move || list_dir_blocking(&path, &cancel, emit))
        .expect("failed to spawn listing thread");
}

pub fn list_dir_blocking<F>(path: &Path, cancel: &Cancel, mut emit: F)
where
    F: FnMut(ListEvent),
{
    let start = Instant::now();
    let read = match fs::read_dir(path) {
        Ok(r) => r,
        Err(e) => {
            if !cancel.is_cancelled() {
                emit(ListEvent::Done { total: 0, elapsed: start.elapsed(), error: Some(describe(&e)) });
            }
            return;
        }
    };

    let mut limit = FIRST_BATCH;
    let mut batch = Vec::with_capacity(limit);
    let mut last_flush = Instant::now();
    let mut total = 0usize;

    for item in read {
        if cancel.is_cancelled() {
            return;
        }
        let Ok(de) = item else { continue };
        if let Some(e) = Entry::from_dir_entry(&de) {
            batch.push(e);
        }
        if batch.len() >= limit || (!batch.is_empty() && last_flush.elapsed() >= MAX_BATCH_DELAY) {
            total += batch.len();
            emit(ListEvent::Batch(std::mem::replace(&mut batch, Vec::with_capacity(BATCH))));
            limit = BATCH;
            last_flush = Instant::now();
        }
    }
    if cancel.is_cancelled() {
        return;
    }
    if !batch.is_empty() {
        total += batch.len();
        emit(ListEvent::Batch(batch));
    }
    emit(ListEvent::Done { total, elapsed: start.elapsed(), error: None });
}

pub fn describe(e: &io::Error) -> String {
    match e.kind() {
        io::ErrorKind::PermissionDenied => "You don't have permission to view this folder.".into(),
        io::ErrorKind::NotFound => "This folder doesn't exist any more.".into(),
        _ => format!("Couldn't open this folder: {e}"),
    }
}

/// Recursive search below `root`: streams matching items (with full paths)
/// in batches every ~100 ms, until done or cancelled.
pub fn search<F>(root: PathBuf, query: super::query::Query, cancel: Cancel, mut emit: F)
where
    F: FnMut(ListEvent) + Send + 'static,
{
    thread::Builder::new()
        .name("fs-search".into())
        .spawn(move || {
            use rayon::prelude::*;
            let start = Instant::now();
            let mut stack = vec![root];
            let mut batch = Vec::new();
            let mut last = Instant::now();
            let mut total = 0usize;
            // content:"…": files passing the name/type filters wait here,
            // then are read in parallel a few dozen at a time.
            let needle = query.content.clone();
            let mut pending: Vec<Entry> = Vec::new();
            let mut last_scan = Instant::now();
            let scan = |pending: &mut Vec<Entry>, batch: &mut Vec<Entry>, needle: &str| {
                let hits: Vec<Entry> = std::mem::take(pending)
                    .into_par_iter()
                    .filter(|e| {
                        !cancel.is_cancelled() && e.path.as_deref().is_some_and(|p| super::content::contains(p, needle))
                    })
                    .collect();
                batch.extend(hits);
            };
            while let Some(dir) = stack.pop() {
                if cancel.is_cancelled() {
                    return;
                }
                let Ok(read) = fs::read_dir(&dir) else { continue };
                for item in read.flatten() {
                    let Some(mut e) = Entry::from_dir_entry(&item) else { continue };
                    let is_link = item.file_type().map(|t| t.is_symlink()).unwrap_or(false);
                    if e.is_dir && !is_link && !matches!(&*e.key, "$recycle.bin" | "system volume information") {
                        stack.push(item.path());
                    }
                    if query.matches(&e) {
                        e.path = Some(item.path());
                        match &needle {
                            Some(_) if e.is_dir || !super::content::searchable(&e.name) => {}
                            Some(_) => pending.push(e),
                            None => batch.push(e),
                        }
                    }
                }
                if let Some(n) = &needle
                    && (pending.len() >= 48 || (!pending.is_empty() && last_scan.elapsed() >= Duration::from_millis(250)))
                {
                    scan(&mut pending, &mut batch, n);
                    last_scan = Instant::now();
                }
                if !batch.is_empty() && last.elapsed() >= Duration::from_millis(100) {
                    total += batch.len();
                    emit(ListEvent::Batch(std::mem::take(&mut batch)));
                    last = Instant::now();
                }
            }
            if let Some(n) = &needle
                && !pending.is_empty()
            {
                scan(&mut pending, &mut batch, n);
            }
            if cancel.is_cancelled() {
                return;
            }
            total += batch.len();
            if !batch.is_empty() {
                emit(ListEvent::Batch(batch));
            }
            emit(ListEvent::Done { total, elapsed: start.elapsed(), error: None });
        })
        .expect("failed to spawn search thread");
}

/// Total size and file count below `dir` (for "1 selected, 2.3 GB").
/// Returns `None` if cancelled.
pub fn dir_size(dir: &Path, cancel: &Cancel) -> Option<(u64, u64)> {
    let (mut bytes, mut files) = (0u64, 0u64);
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        if cancel.is_cancelled() {
            return None;
        }
        let Ok(read) = fs::read_dir(&d) else { continue };
        for item in read.flatten() {
            let Ok(ft) = item.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                stack.push(item.path());
            } else if let Ok(m) = item.metadata() {
                bytes += m.len();
                files += 1;
            }
        }
    }
    Some((bytes, files))
}
