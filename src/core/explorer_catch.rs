//! Default file explorer, the rest of the way: some apps don't open folders
//! the normal way (Directory "open" verb) but start Explorer directly
//! (PotPlayer's "Open folder", `explorer /select,…`, installers…). While X
//! Finder runs as the default, new Explorer windows are noticed, their
//! folder and selected files read, the window closed, and the folder opened
//! here instead, with the same files selected.
//!
//! Explorer windows X Finder opens itself ("Show in Windows Explorer") are
//! left alone, as are windows on virtual folders (Control Panel, This PC…).

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Until this time (ms since 1970) new Explorer windows are X Finder's own.
static ALLOW_UNTIL: AtomicU64 = AtomicU64::new(0);

pub fn log(msg: &str) {
    if let Some(p) = std::env::var_os("FINDER_CATCH_LOG") {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
            let _ = writeln!(f, "{} {msg}", now_ms());
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// Folder to open in Explorer as soon as the watcher is ready (see `relay`).
static RELAY: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// Opens `dir` in Explorer so it is caught: Windows selects the file an
/// app's "Show in folder" meant only in an Explorer window, never in the
/// folder handed to the default explorer. With no watcher running yet, it
/// waits for `start`.
pub fn relay(dir: PathBuf, watcher_running: bool) {
    if watcher_running {
        let _ = std::process::Command::new("explorer.exe").arg(&dir).spawn();
    } else if let Ok(mut r) = RELAY.lock() {
        *r = Some(dir);
    }
}

/// X Finder is about to open Explorer on purpose: don't catch it.
pub fn allow_explorer() {
    ALLOW_UNTIL.store(now_ms() + 6000, Ordering::Relaxed);
}

/// Stops the watcher when dropped.
pub struct Catcher {
    running: Arc<AtomicBool>,
}

impl Drop for Catcher {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

/// Starts watching. `caught(folder, selected file names)` runs on the
/// watcher thread for each Explorer window taken over.
pub fn start(caught: impl Fn(PathBuf, Vec<String>) + Send + 'static) -> Option<Catcher> {
    #[cfg(windows)]
    {
        let running = Arc::new(AtomicBool::new(true));
        let r = running.clone();
        std::thread::Builder::new().name("explorer-catch".into()).spawn(move || imp::run(r, caught)).ok()?;
        Some(Catcher { running })
    }
    #[cfg(not(windows))]
    {
        let _ = caught;
        None
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use windows::Win32::System::Com::*;
    use windows::Win32::System::Variant::VARIANT;
    use windows::Win32::UI::Shell::*;
    use windows::core::Interface;

    /// What is known about one Explorer window while it settles.
    struct Watch {
        first_seen: std::time::Instant,
        /// The real folder it shows, and since when.
        folder: Option<(PathBuf, std::time::Instant)>,
    }

    pub fn run(running: Arc<AtomicBool>, caught: impl Fn(PathBuf, Vec<String>)) {
        use std::collections::HashMap;
        use std::time::Instant;
        // How long a window may take to reach its folder, and then to
        // select the file it was asked to show ("Show in folder").
        const FOLDER_WAIT: Duration = Duration::from_millis(6000);
        const SELECT_WAIT: Duration = Duration::from_millis(1500);
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            log("catcher started");
            let Ok(windows) = CoCreateInstance::<_, IShellWindows>(&ShellWindows, None, CLSCTX_ALL) else { return };
            // Windows already open when X Finder started are the user's.
            let mut done: HashSet<isize> = browsers(&windows).into_iter().map(|(h, _)| h).collect();
            if let Some(dir) = RELAY.lock().ok().and_then(|mut r| r.take()) {
                ALLOW_UNTIL.store(0, Ordering::Relaxed);
                let _ = std::process::Command::new("explorer.exe").arg(&dir).spawn();
            }
            let mut watching: HashMap<isize, Watch> = HashMap::new();
            while running.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(150));
                let now = browsers(&windows);
                let present: HashSet<isize> = now.iter().map(|(h, _)| *h).collect();
                done.retain(|h| present.contains(h));
                watching.retain(|h, w| present.contains(h) || w.first_seen.elapsed() < FOLDER_WAIT);
                for (hwnd, wb) in now {
                    if done.contains(&hwnd) {
                        continue;
                    }
                    if now_ms() < ALLOW_UNTIL.load(Ordering::Relaxed) {
                        done.insert(hwnd);
                        continue;
                    }
                    let w = watching.entry(hwnd).or_insert(Watch { first_seen: Instant::now(), folder: None });
                    let Some(view) = wb.Document().ok().and_then(|d| d.cast::<IShellFolderViewDual>().ok()) else {
                        continue;
                    };
                    let path = view
                        .Folder()
                        .ok()
                        .and_then(|f| f.cast::<Folder2>().ok())
                        .and_then(|f| f.Self_().ok())
                        .and_then(|i| i.Path().ok())
                        .map(|b| b.to_string())
                        .unwrap_or_default();
                    let p = PathBuf::from(&path);
                    if path.is_empty() || path.starts_with("::") || !p.is_dir() {
                        // Home page first, then the folder; a window that
                        // stays on a virtual folder is left to Explorer.
                        if w.first_seen.elapsed() >= FOLDER_WAIT {
                            done.insert(hwnd);
                        }
                        continue;
                    }
                    if w.folder.as_ref().map(|(d, _)| d) != Some(&p) {
                        w.folder = Some((p.clone(), Instant::now()));
                    }
                    let since = w.folder.as_ref().map_or(Duration::ZERO, |(_, t)| t.elapsed());
                    let total = view.Folder().ok().and_then(|f| f.Items().ok()).and_then(|i| i.Count().ok()).unwrap_or(0);
                    let Ok(items) = view.SelectedItems() else { continue };
                    let n = items.Count().unwrap_or(0);
                    if since < SELECT_WAIT && (n == 0 || (n > 1 && n >= total)) {
                        continue;
                    }
                    let n = if n > 1 && n >= total { 0 } else { n };
                    let mut names = Vec::new();
                    for i in 0..n {
                        if let Ok(item) = items.Item(&VARIANT::from(i))
                            && let Ok(ip) = item.Path()
                            && let Some(name) = PathBuf::from(ip.to_string()).file_name()
                        {
                            names.push(name.to_string_lossy().into_owned());
                        }
                    }
                    log(&format!("caught {p:?} {names:?}"));
                    done.insert(hwnd);
                    watching.remove(&hwnd);
                    let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                        Some(windows::Win32::Foundation::HWND(hwnd as _)),
                        windows::Win32::UI::WindowsAndMessaging::WM_CLOSE,
                        windows::Win32::Foundation::WPARAM(0),
                        windows::Win32::Foundation::LPARAM(0),
                    );
                    caught(p, names);
                }
            }
            CoUninitialize();
        }
    }

    /// Open Explorer windows: (window handle, browser).
    unsafe fn browsers(windows: &IShellWindows) -> Vec<(isize, IWebBrowser2)> {
        let mut out = Vec::new();
        unsafe {
            let n = windows.Count().unwrap_or(0);
            for i in 0..n {
                let Ok(d) = windows.Item(&VARIANT::from(i)) else { continue };
                let Ok(wb) = d.cast::<IWebBrowser2>() else { continue };
                let Ok(h) = wb.HWND() else { continue };
                out.push((h.0 as isize, wb));
            }
        }
        out
    }

}
