//! Thumbnail service for icon and gallery views.
//!
//! - Requests come from `FileModel::row_data`, i.e. only for rows on screen
//!   (plan §9 rule 4).
//! - Worker threads ask the Windows shell, which also keeps the on-disk
//!   thumbnail cache Explorer uses, so repeat visits are fast.
//! - The queue is last-in-first-out and capped: while you scroll, rows that
//!   scrolled away are dropped instead of delaying the ones now visible.
//! - Decoded images live in an in-memory cache with a byte budget.
//! - File types without a thumbnail handler are detected after a few misses
//!   and skipped from then on.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use finder_rs::core::{format, preview};
use finder_rs::core::shell::{self, Pixels};
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

/// Video frames take a while each (Windows decodes the file), so several
/// run side by side.
fn workers() -> usize {
    std::thread::available_parallelism().map(|n| n.get() / 2).unwrap_or(4).clamp(3, 8)
}
const QUEUE_MAX: usize = 192;
const BUDGET_BYTES: usize = 160 << 20;
/// Misses (without a single hit) after which an extension is skipped.
const EXT_GIVE_UP: u32 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobKind {
    /// Content thumbnail (photo, video frame, PDF page…).
    Thumb,
    /// The file's Windows icon.
    Icon,
    /// A folder as Windows draws it, with pictures from inside.
    FolderWindows,
    /// The first picture or video inside a folder, to show on our folder icon.
    FolderPeek,
}

pub struct Job {
    pub key: String,
    pub path: PathBuf,
    pub size: u32,
    pub kind: JobKind,
    pub model_id: u64,
    pub entry: u32,
}

/// Files whose icon is their own (not shared by their file type).
const OWN_ICON: [&str; 10] = ["exe", "ico", "lnk", "url", "cur", "ani", "msc", "cpl", "scr", "appref-ms"];

struct Queue {
    jobs: Mutex<VecDeque<Job>>,
    ready: Condvar,
}

struct Cached {
    image: Image,
    bytes: usize,
    used: u64,
}

pub struct Thumbs {
    queue: Arc<Queue>,
    cache: RefCell<HashMap<String, Cached>>,
    bytes: Cell<usize>,
    clock: Cell<u64>,
    pending: RefCell<HashSet<String>>,
    failed: RefCell<HashSet<String>>,
    /// extension -> (hits, misses)
    ext_stats: RefCell<HashMap<String, (u32, u32)>>,
    /// Requested edge length in physical pixels (tiles).
    size: Cell<u32>,
    /// Small icon size in physical pixels (list and column rows).
    small: Cell<u32>,
    /// Use Windows icons (vs the app's Finder-style ones).
    pub windows_icons: Cell<bool>,
    /// Folder previews: 0 off, 1 a picture on our folder icon, 2 Windows' own.
    pub folder_previews: Cell<u8>,
}

impl Thumbs {
    /// `on_done` runs on a worker thread for every finished job.
    pub fn new(on_done: impl Fn(Job, Option<Pixels>) + Send + Sync + 'static) -> Rc<Self> {
        let queue = Arc::new(Queue { jobs: Mutex::new(VecDeque::new()), ready: Condvar::new() });
        let on_done = Arc::new(on_done);
        for n in 0..workers() {
            let queue = queue.clone();
            let on_done = on_done.clone();
            let _ = thread::Builder::new().name(format!("thumbs-{n}")).spawn(move || {
                shell::init_thread(true);
                loop {
                    let job = {
                        let mut jobs = queue.jobs.lock().unwrap();
                        loop {
                            if let Some(job) = jobs.pop_back() {
                                break job;
                            }
                            jobs = queue.ready.wait(jobs).unwrap();
                        }
                    };
                    // Delivered at exactly the requested size: the renderer
                    // doesn't smooth when scaling, so pre-scaled stays sharp.
                    let px = match job.kind {
                        // (Videos without a shell thumbnail get a decoded frame.)
                        JobKind::Thumb => shell::thumbnail(&job.path, job.size),
                        JobKind::Icon => shell::icon(&job.path, job.size),
                        JobKind::FolderWindows => folder_windows(&job.path, job.size),
                        JobKind::FolderPeek => folder_peek(&job.path, job.size),
                    }
                    .map(|p| shell::fit(&p, job.size, job.size));
                    on_done(job, px);
                }
            });
        }
        Rc::new(Thumbs {
            queue,
            cache: RefCell::default(),
            bytes: Cell::new(0),
            clock: Cell::new(0),
            pending: RefCell::default(),
            failed: RefCell::default(),
            ext_stats: RefCell::default(),
            size: Cell::new(128),
            small: Cell::new(16),
            windows_icons: Cell::new(true),
            folder_previews: Cell::new(0),
        })
    }

    pub fn size(&self) -> u32 {
        self.size.get()
    }

    pub fn small(&self) -> u32 {
        self.small.get()
    }

    /// Exact pixel sizes for tiles (`logical` icon size) and row icons.
    pub fn set_icon_size(&self, logical: f32, row_icon: f32, scale: f32) {
        let scale = scale.max(1.0);
        self.size.set(((logical * scale).round() as u32).clamp(16, 1024));
        self.small.set(((row_icon * scale).round() as u32).clamp(12, 64));
    }

    pub fn is_failed(&self, key: &str) -> bool {
        self.failed.borrow().contains(key)
    }

    /// Cache key for a file's icon: per file type, except files that carry
    /// their own icon (programs, shortcuts, …).
    pub fn icon_key(path: &std::path::Path, is_dir: bool, px: u32) -> String {
        let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        let ext = format::extension(&name);
        if is_dir {
            format!("icon\u{1}<folder>\u{1}{px}")
        } else if OWN_ICON.contains(&ext) {
            format!("icon\u{1}{}\u{1}{px}", path.display())
        } else {
            format!("icon\u{1}.{ext}\u{1}{px}")
        }
    }

    pub fn key(path: &str, modified: i64, size: u32) -> String {
        format!("{path}\u{1}{modified}\u{1}{size}")
    }

    pub fn get(&self, key: &str) -> Option<Image> {
        let mut cache = self.cache.borrow_mut();
        let c = cache.get_mut(key)?;
        let t = self.clock.get() + 1;
        self.clock.set(t);
        c.used = t;
        Some(c.image.clone())
    }

    /// Skips files whose type has repeatedly produced no thumbnail. Photos
    /// and videos are always tried: a few files Windows can't decode (say,
    /// HEVC) mustn't turn off thumbnails for the rest.
    pub fn worth_trying(&self, name: &str) -> bool {
        if matches!(preview::classify(name), preview::Kind::Image | preview::Kind::Video) {
            return true;
        }
        let lower = name.to_lowercase();
        let ext = format::extension(&lower);
        !matches!(self.ext_stats.borrow().get(ext), Some(&(0, misses)) if misses >= EXT_GIVE_UP)
    }

    pub fn request(&self, job: Job) {
        if self.failed.borrow().contains(&job.key) || !self.pending.borrow_mut().insert(job.key.clone()) {
            return;
        }
        let dropped: Vec<String> = {
            let mut jobs = self.queue.jobs.lock().unwrap();
            jobs.push_back(job);
            let excess = jobs.len().saturating_sub(QUEUE_MAX);
            jobs.drain(..excess).map(|j| j.key).collect()
        };
        let mut pending = self.pending.borrow_mut();
        for k in dropped {
            pending.remove(&k);
        }
        self.queue.ready.notify_one();
    }

    /// Stores a finished job. Returns true if a new image is available.
    pub fn finish(&self, job: &Job, px: Option<Pixels>) -> bool {
        self.pending.borrow_mut().remove(&job.key);
        let name = job.path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        let ext = format::extension(&name).to_string();
        if job.kind == JobKind::Thumb {
            let mut stats = self.ext_stats.borrow_mut();
            let s = stats.entry(ext).or_default();
            if px.is_some() { s.0 += 1 } else { s.1 += 1 }
        }
        let Some(px) = px else {
            self.failed.borrow_mut().insert(job.key.clone());
            return false;
        };
        let image = to_image(&px);
        let bytes = px.rgba.len();
        let t = self.clock.get() + 1;
        self.clock.set(t);
        self.cache.borrow_mut().insert(job.key.clone(), Cached { image, bytes, used: t });
        self.bytes.set(self.bytes.get() + bytes);
        self.evict();
        true
    }

    fn evict(&self) {
        let mut cache = self.cache.borrow_mut();
        while self.bytes.get() > BUDGET_BYTES && cache.len() > 1 {
            let Some(oldest) = cache.iter().min_by_key(|(_, c)| c.used).map(|(k, _)| k.clone()) else { break };
            if let Some(c) = cache.remove(&oldest) {
                self.bytes.set(self.bytes.get() - c.bytes);
            }
        }
    }
}

/// Picture for a Finder-style folder: like Windows, the first picture or
/// video inside (else a document with a preview: PDF, Word…), then the same
/// one level down in the first few subfolders, else the first file's icon.
/// Returned as a square exactly the size it's drawn at (the renderer doesn't
/// smooth when shrinking, so a bigger picture would look jagged).
fn folder_peek(dir: &std::path::Path, size: u32) -> Option<Pixels> {
    // Inner picture on the folder: 19/32 of the tile, less the white margin.
    let edge = ((size as f32 * 19.0 / 32.0 * 0.9).round() as u32).max(8);
    let list = |d: &std::path::Path| -> (Vec<(u8, String)>, Vec<String>) {
        let mut files = Vec::new();
        let mut dirs = Vec::new();
        for e in std::fs::read_dir(d).into_iter().flatten().flatten().take(400) {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || name.starts_with('~') || name.eq_ignore_ascii_case("desktop.ini") {
                continue;
            }
            match e.file_type() {
                Ok(t) if t.is_dir() => dirs.push(name),
                Ok(t) if t.is_file() => {
                    let rank = match preview::classify(&name) {
                        preview::Kind::Image | preview::Kind::Video => 0,
                        _ if preview::has_thumbnail(&name) => 1,
                        _ => 2,
                    };
                    files.push((rank, name));
                }
                _ => {}
            }
        }
        files.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| finder_rs::core::sort::natural_cmp(&a.1, &b.1)));
        dirs.sort_by(|a, b| finder_rs::core::sort::natural_cmp(a, b));
        (files, dirs)
    };
    let picture = |d: &std::path::Path, files: &[(u8, String)]| {
        // A file Windows can't decode shouldn't leave the folder plain.
        files.iter().filter(|f| f.0 < 2).take(3).find_map(|f| shell::thumbnail(&d.join(&f.1), edge * 2))
    };
    let (files, dirs) = list(dir);
    if let Some(p) = picture(dir, &files) {
        return Some(square(&p, edge, true));
    }
    for sub in dirs.iter().take(3) {
        let d = dir.join(sub);
        if let Some(p) = picture(&d, &list(&d).0) {
            return Some(square(&p, edge, true));
        }
    }
    let first = files.first()?;
    shell::icon(&dir.join(&first.1), edge).map(|p| square(&p, edge, false))
}

/// `n`×`n` image: `cover` crops to fill it (photos), otherwise the picture
/// is centred on white (icons).
fn square(p: &Pixels, n: u32, cover: bool) -> Pixels {
    if cover {
        let side = p.width.min(p.height).max(1);
        let (x0, y0) = ((p.width - side) / 2, (p.height - side) / 2);
        let mut rgba = Vec::with_capacity((side * side * 4) as usize);
        for y in y0..y0 + side {
            let row = (y * p.width + x0) as usize * 4;
            rgba.extend_from_slice(&p.rgba[row..row + side as usize * 4]);
        }
        let cropped = Pixels { width: side, height: side, rgba };
        return shell::fit(&cropped, n, n);
    }
    let icon = shell::fit(p, n * 3 / 4, n * 3 / 4);
    let mut rgba = vec![255u8; (n * n * 4) as usize];
    let (ox, oy) = ((n - icon.width) / 2, (n - icon.height) / 2);
    for y in 0..icon.height {
        for x in 0..icon.width {
            let s = ((y * icon.width + x) * 4) as usize;
            let d = (((y + oy) * n + x + ox) * 4) as usize;
            // Premultiplied "over" white.
            let a = icon.rgba[s + 3] as u32;
            for c in 0..3 {
                rgba[d + c] = (icon.rgba[s + c] as u32 + (255 - a)).min(255) as u8;
            }
        }
    }
    Pixels { width: n, height: n, rgba }
}

/// A folder as Windows draws it. Empty folders have no picture: then
/// Windows' plain folder icon, so they match the others. Some come back
/// without transparency (black around the folder): make that clear again.
fn folder_windows(dir: &std::path::Path, size: u32) -> Option<Pixels> {
    let Some(mut p) = shell::thumbnail(dir, size) else {
        return shell::icon(dir, size);
    };
    if p.rgba.chunks_exact(4).all(|c| c[3] == 255) {
        clear_black_border(&mut p);
    }
    Some(p)
}

/// Makes the black background around a picture transparent: every
/// (near-)black pixel reachable from the edges.
fn clear_black_border(p: &mut Pixels) {
    let (w, h) = (p.width as usize, p.height as usize);
    let dark = |px: &[u8]| px[0] < 12 && px[1] < 12 && px[2] < 12;
    let mut seen = vec![false; w * h];
    let mut stack: Vec<usize> = Vec::new();
    for x in 0..w {
        stack.push(x);
        stack.push((h - 1) * w + x);
    }
    for y in 0..h {
        stack.push(y * w);
        stack.push(y * w + w - 1);
    }
    while let Some(i) = stack.pop() {
        if seen[i] || !dark(&p.rgba[i * 4..i * 4 + 4]) {
            continue;
        }
        seen[i] = true;
        p.rgba[i * 4..i * 4 + 4].copy_from_slice(&[0, 0, 0, 0]);
        let (x, y) = (i % w, i / w);
        if x > 0 { stack.push(i - 1) }
        if x + 1 < w { stack.push(i + 1) }
        if y > 0 { stack.push(i - w) }
        if y + 1 < h { stack.push(i + w) }
    }
}

pub fn to_image(px: &Pixels) -> Image {
    let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&px.rgba, px.width, px.height);
    Image::from_rgba8_premultiplied(buf)
}
