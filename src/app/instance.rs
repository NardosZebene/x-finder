//! One window: when finder-rs is already open, folders opened from other
//! apps (double-clicking folders/shortcuts, "Show in folder"…) arrive as a
//! new tab in that window instead of starting a second one.
//!
//! The running window is marked with a window property; a new process that
//! was given a folder sends it there (WM_COPYDATA) and quits.

use std::path::Path;

use finder_rs::core::location::{self, Location};
use finder_rs::core::sys;
use slint::ComponentHandle;

use super::with_app;

/// Folder (or file: its folder, with the file selected) from a command-line
/// argument. Virtual folders are handled by Explorer (see main).
pub fn start_from_arg(arg: Option<&str>) -> (Location, Option<String>) {
    let mut select = None;
    let start = arg
        .map(|a| a.trim().trim_matches('"'))
        // Win+E: the start folder.
        .filter(|a| *a != "--start")
        .map(|a| a.strip_prefix("/select,").unwrap_or(a))
        .and_then(|a| {
            let p = Path::new(a);
            if p.is_file() {
                select = p.file_name().map(|n| n.to_string_lossy().into_owned());
                return p.parent().map(|d| Location::Dir(d.to_path_buf()));
            }
            // "Show in folder" (Telegram, browsers…) only tells us the
            // folder; the file it means is almost always the one that just
            // arrived, so select that.
            if p.is_dir() {
                select = in_window_title(p).or_else(|| just_changed(p));
            }
            location::parse_user_input(a)
        })
        // Default: the Desktop (follows OneDrive / moved Desktop folders).
        .unwrap_or_else(|| Location::Dir(sys::known_folder(sys::KnownFolder::Desktop).unwrap_or_else(sys::home)));
    (start, select)
}

/// A folder another app asked Windows to show ("Show in folder", "Open file
/// location", "Reveal in File Explorer"…), as opposed to a folder
/// double-clicked in Explorer or on the desktop. Windows only passes the
/// folder on; the file it means is selected only in an Explorer window, so
/// such folders go through Explorer (see `explorer_catch::relay`).
pub fn shown_by_app(arg: &str) -> Option<std::path::PathBuf> {
    let p = Path::new(arg.trim().trim_matches('"'));
    if !p.is_dir()
        || std::env::var_os("FINDER_SNAPSHOT").is_some()
        || finder_rs::core::default_explorer::registered_x_finder().is_none()
    {
        return None;
    }
    let parent = parent_exe()?.to_lowercase();
    let from_explorer = parent == "explorer.exe" || parent.contains("finder");
    (!from_explorer).then(|| p.to_path_buf())
}

/// File name of the process that started this one.
fn parent_exe() -> Option<String> {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Diagnostics::ToolHelp::*;
        let me = std::process::id();
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut procs: Vec<(u32, u32, String)> = Vec::new();
        if Process32FirstW(snap, &mut e).is_ok() {
            loop {
                let n = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
                procs.push((e.th32ProcessID, e.th32ParentProcessID, String::from_utf16_lossy(&e.szExeFile[..n])));
                if Process32NextW(snap, &mut e).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
        let ppid = procs.iter().find(|p| p.0 == me)?.1;
        procs.into_iter().find(|p| p.0 == ppid).map(|p| p.2)
    }
    #[cfg(not(windows))]
    None
}

/// The file in `dir` an open window is showing ("video.mp4 - PotPlayer",
/// "report.docx - Word"): apps that only send the folder for "Open folder"
/// almost always mean the file they have open.
fn in_window_title(dir: &Path) -> Option<String> {
    let titles = window_titles();
    if titles.is_empty() {
        return None;
    }
    let mut best: Option<String> = None;
    for item in std::fs::read_dir(dir).ok()?.flatten().take(20_000) {
        let name = item.file_name().to_string_lossy().into_owned();
        let lower = name.to_lowercase();
        // Very short names ("a.txt") would match too much.
        if lower.len() < 5 || !titles.iter().any(|t| t.contains(&lower)) {
            continue;
        }
        if best.as_ref().is_none_or(|b| name.len() > b.len()) {
            best = Some(name);
        }
    }
    best
}

/// Titles of the visible top-level windows, lower-cased.
fn window_titles() -> Vec<String> {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::{HWND, LPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowTextW, IsWindowVisible};
        use windows::core::BOOL;
        unsafe extern "system" fn each(h: HWND, out: LPARAM) -> BOOL {
            unsafe {
                if IsWindowVisible(h).as_bool() {
                    let mut buf = [0u16; 512];
                    let n = GetWindowTextW(h, &mut buf);
                    if n > 0 {
                        (*(out.0 as *mut Vec<String>)).push(String::from_utf16_lossy(&buf[..n as usize]).to_lowercase());
                    }
                }
            }
            true.into()
        }
        let mut titles: Vec<String> = Vec::new();
        unsafe {
            let _ = EnumWindows(Some(each), LPARAM(&mut titles as *mut Vec<String> as isize));
        }
        titles
    }
    #[cfg(not(windows))]
    Vec::new()
}

/// The file in `dir` saved in the last few minutes (a download that just
/// finished, a file Telegram just saved). Only when one clearly stands
/// out: several written together (a build, an unzip) select nothing, which
/// beats selecting the wrong one.
fn just_changed(dir: &Path) -> Option<String> {
    // Downloads (browsers' "Show in folder") and Telegram's folder: the
    // newest file is the one meant, even if it isn't brand new.
    let name = dir.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    let downloads = sys::known_folder(sys::KnownFolder::Downloads).is_some_and(|d| d == dir)
        || name.contains("download")
        || name.contains("telegram");
    let now = std::time::SystemTime::now();
    let recent = std::time::Duration::from_secs(if downloads { 24 * 3600 } else { 10 * 60 });
    let mut times: Vec<(std::time::SystemTime, String)> = Vec::new();
    for item in std::fs::read_dir(dir).ok()?.flatten().take(20_000) {
        let Ok(meta) = item.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let Some(t) = [meta.created().ok(), meta.modified().ok()].into_iter().flatten().max() else { continue };
        if now.duration_since(t).is_ok_and(|age| age <= recent) {
            let name = item.file_name().to_string_lossy().into_owned();
            // Half-finished downloads don't count.
            if [".crdownload", ".part", ".tmp", ".partial"].iter().any(|x| name.to_lowercase().ends_with(x)) {
                continue;
            }
            times.push((t, name));
        }
    }
    times.sort_by(|a, b| b.0.cmp(&a.0));
    if downloads {
        return times.first().map(|(_, n)| n.clone());
    }
    match times.as_slice() {
        [] => None,
        [(_, only)] => Some(only.clone()),
        [(t1, newest), (t2, _), ..] => {
            let gap = t1.duration_since(*t2).unwrap_or_default();
            (gap >= std::time::Duration::from_secs(3)).then(|| newest.clone())
        }
    }
}

/// An Explorer window another app opened, taken over: its folder in a new
/// tab, with the same files selected.
pub(crate) fn open_caught(dir: std::path::PathBuf, names: Vec<String>) {
    finder_rs::core::explorer_catch::log(&format!("open_caught {dir:?} {names:?}"));
    let _ = slint::invoke_from_event_loop(move || {
        with_app(|a| {
            let loc = Location::Dir(dir);
            if super::take_fresh() {
                a.start_over(loc, names.first().cloned());
            } else if a.tab().loc == loc {
                // Already showing it (X Finder was started with it).
                a.navigate(loc, names.first().cloned());
            } else {
                a.open_tab(loc, true);
                a.tabs[a.active].pending_select = names.first().cloned();
            }
            if names.len() > 1 {
                a.select_after = names;
            }
            a.bring_to_front();
        });
    });
}

/// A folder sent by another finder-rs process: open it in a new tab.
fn open_from_outside(arg: String) {
    let _ = slint::invoke_from_event_loop(move || {
        with_app(|a| {
            if let Some(dir) = arg.strip_prefix("--relay,") {
                if a.catcher.is_some() {
                    finder_rs::core::explorer_catch::relay(dir.into(), true);
                    return;
                }
            }
            let arg = arg.strip_prefix("--relay,").unwrap_or(&arg).to_string();
            // The "open X Finder" key or Win+E while the window is hidden:
            // just show it as it was.
            let (loc, select) = start_from_arg(Some(&arg));
            finder_rs::core::explorer_catch::log(&format!("from outside {arg:?} -> select {select:?}"));
            if super::take_fresh() {
                // Closed (hidden) without "remember open tabs": start over
                // with just this folder.
                a.start_over(loc, select);
            } else if arg == "--start" && super::window_hidden() {
                // Remembered tabs: just show them again.
            } else {
                a.open_tab(loc, true);
                if let Some(name) = select {
                    a.tabs[a.active].pending_select = Some(name);
                }
            }
            a.bring_to_front();
        });
    });
}

#[cfg(windows)]
mod imp {
    use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::DataExchange::COPYDATASTRUCT;
    use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::core::{BOOL, w};

    /// Marks our main window so other processes can find it.
    const PROP: windows::core::PCWSTR = w!("finder-rs.main");
    /// Tag on the WM_COPYDATA payload ("folder to open").
    const OPEN_FOLDER: usize = 0xF1D0;

    unsafe extern "system" fn hook(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, _id: usize, _data: usize) -> LRESULT {
        if msg == WM_LBUTTONDOWN {
            super::PRESS_AT.store(lparam.0 as u32 as u64, std::sync::atomic::Ordering::Relaxed);
            super::DRAG_MOVED.store(false, std::sync::atomic::Ordering::Relaxed);
        }
        // Alt+Tab (or any switch to another window) while dragging files:
        // carry on as a Windows drag, so they can be dropped over there.
        let deactivated = (msg == WM_ACTIVATE && wparam.0 & 0xFFFF == 0) || msg == WM_CAPTURECHANGED;
        if deactivated
            && super::DRAG_ARMED.load(std::sync::atomic::Ordering::Relaxed)
            && super::DRAG_MOVED.load(std::sync::atomic::Ordering::Relaxed)
        {
            let h = hwnd.0 as isize;
            slint::Timer::single_shot(std::time::Duration::from_millis(0), move || {
                use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
                let down = unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) } as u16 & 0x8000 != 0;
                if down && super::DRAG_ARMED.swap(false, std::sync::atomic::Ordering::Relaxed) {
                    hand_off(HWND(h as _));
                }
            });
        }
        if msg == WM_LBUTTONUP {
            super::DRAG_ARMED.store(false, std::sync::atomic::Ordering::Relaxed);
            let at = super::PRESS_AT.load(std::sync::atomic::Ordering::Relaxed);
            let xy = |v: u64| ((v & 0xFFFF) as i16 as i32, ((v >> 16) & 0xFFFF) as i16 as i32);
            let ((x0, y0), (x1, y1)) = (xy(at), xy(lparam.0 as u32 as u64));
            let moved = (x1 - x0).abs() > 4 || (y1 - y0).abs() > 4;
            // The app handles the release (a drop, say) first.
            let r = unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) };
            super::with_app(|a| a.press_released(moved));
            return r;
        }
        // Files dragged past the window's edge: hand them to Windows' own
        // drag and drop, so Telegram, browsers, the desktop, Explorer… take
        // them (the app's own drag only works inside its window).
        if msg == WM_MOUSEMOVE
            && wparam.0 & 0x0001 != 0 // MK_LBUTTON
            && super::DRAG_ARMED.load(std::sync::atomic::Ordering::Relaxed)
        {
            let (x, y) = ((lparam.0 & 0xFFFF) as i16 as i32, ((lparam.0 >> 16) & 0xFFFF) as i16 as i32);
            let at = super::PRESS_AT.load(std::sync::atomic::Ordering::Relaxed);
            let (x0, y0) = ((at & 0xFFFF) as i16 as i32, ((at >> 16) & 0xFFFF) as i16 as i32);
            if (x - x0).abs() > 4 || (y - y0).abs() > 4 {
                super::DRAG_MOVED.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            let mut rc = windows::Win32::Foundation::RECT::default();
            let _ = unsafe { GetClientRect(hwnd, &mut rc) };
            if x < rc.left || y < rc.top || x >= rc.right || y >= rc.bottom {
                super::DRAG_ARMED.store(false, std::sync::atomic::Ordering::Relaxed);
                hand_off(hwnd);
                return LRESULT(0);
            }
        }
        // Windows itself wants part of the window painted again (back from
        // Show Desktop, another app or minimised; Quick Look's video window
        // closed). The app's own redraws never leave an update region, so
        // this only fires then: draw everything, not just what changed,
        // or stale desktop pixels stay in the window.
        if msg == WM_PAINT && unsafe { windows::Win32::Graphics::Gdi::GetUpdateRect(hwnd, None, false) }.as_bool() {
            schedule_full_repaint();
        }
        // The "open X Finder" key while the app is open: like Win+E.
        if msg == WM_HOTKEY && wparam.0 as i32 == OPEN_HOTKEY {
            super::open_from_outside("--start".into());
            return LRESULT(0);
        }
        if msg == WM_COPYDATA {
            // SAFETY: WM_COPYDATA's lParam is a COPYDATASTRUCT valid for this call.
            let cds = unsafe { &*(lparam.0 as *const COPYDATASTRUCT) };
            if cds.dwData == OPEN_FOLDER && !cds.lpData.is_null() {
                let units = unsafe { std::slice::from_raw_parts(cds.lpData as *const u16, cds.cbData as usize / 2) };
                let arg = String::from_utf16_lossy(units).trim_end_matches('\0').to_string();
                super::open_from_outside(arg);
                return LRESULT(1);
            }
        }
        unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
    }

    /// The app's own drag becomes a Windows drag of the selected files.
    fn hand_off(hwnd: HWND) {
        let paths = super::with_app(|a| a.drag_out_paths()).unwrap_or_default();
        // Far outside the window: the app's own drag isn't over any of its
        // folders, so it can't drop anything there when it ends.
        let away = LPARAM(((-30000i32 as u16 as u32) | ((-30000i32 as u16 as u32) << 16)) as isize);
        unsafe {
            DefSubclassProc(hwnd, WM_MOUSEMOVE, WPARAM(0x0001), away);
            let _ = windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture();
        }
        if !paths.is_empty() {
            drag_out(hwnd, &paths);
        }
        unsafe { DefSubclassProc(hwnd, WM_LBUTTONUP, WPARAM(0), away) };
        super::with_app(|a| a.after_drag_out());
    }

    /// Windows drag and drop of `paths`, with the shell's own data (the same
    /// as dragging them from Explorer: file list, names, drag picture).
    fn drag_out(hwnd: HWND, paths: &[std::path::PathBuf]) {
        use windows::Win32::System::Com::{CoTaskMemFree, IDataObject};
        use windows::Win32::System::Ole::{DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE};
        use windows::Win32::UI::Shell::Common::ITEMIDLIST;
        use windows::Win32::UI::Shell::{BHID_DataObject, IShellItemArray, SHCreateShellItemArrayFromIDLists, SHDoDragDrop, SHParseDisplayName};
        unsafe {
            let mut pidls: Vec<*mut ITEMIDLIST> = Vec::new();
            for p in paths {
                let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
                if SHParseDisplayName(&windows::core::HSTRING::from(p.as_os_str()), None, &mut pidl, 0, None).is_ok() && !pidl.is_null() {
                    pidls.push(pidl);
                }
            }
            if !pidls.is_empty() {
                let list: Vec<*const ITEMIDLIST> = pidls.iter().map(|p| *p as *const _).collect();
                let data: windows::core::Result<IDataObject> = SHCreateShellItemArrayFromIDLists(&list)
                    .and_then(|arr: IShellItemArray| arr.BindToHandler(None, &BHID_DataObject));
                if let Ok(data) = data {
                    let _ = SHDoDragDrop(Some(hwnd), &data, None, DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK);
                }
            }
            for p in pidls {
                CoTaskMemFree(Some(p as *const _));
            }
        }
    }

    /// At most one pending full repaint (WM_PAINT can come in bursts).
    fn schedule_full_repaint() {
        use std::sync::atomic::{AtomicBool, Ordering};
        static PENDING: AtomicBool = AtomicBool::new(false);
        if PENDING.swap(true, Ordering::Relaxed) {
            return;
        }
        slint::Timer::single_shot(std::time::Duration::from_millis(0), || {
            PENDING.store(false, Ordering::Relaxed);
            crate::app::repaint_all();
        });
    }

    /// Starts accepting folders from other processes.
    pub fn listen(hwnd: HWND) {
        MAIN.store(hwnd.0 as isize, std::sync::atomic::Ordering::Relaxed);
        unsafe {
            let _ = SetPropW(hwnd, PROP, Some(HANDLE(1 as _)));
            let _ = SetWindowSubclass(hwnd, Some(hook), 0xF1D0, 0);
        }
        super::with_app(|a| a.claim_open_key());
    }

    /// The main window, once it exists.
    static MAIN: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
    // Applications may only use ids up to 0xBFFF.
    const OPEN_HOTKEY: i32 = 0x0F1D;

    /// Takes the "open X Finder" key for this window (RegisterHotKey
    /// modifiers and virtual key), or gives it up with `None`. False when
    /// the window doesn't exist yet or Windows still has the key elsewhere.
    pub fn set_hotkey(key: Option<(u32, u32)>) -> bool {
        use windows::Win32::UI::Input::KeyboardAndMouse::{HOT_KEY_MODIFIERS, RegisterHotKey, UnregisterHotKey};
        let h = MAIN.load(std::sync::atomic::Ordering::Relaxed);
        if h == 0 {
            return false;
        }
        let hwnd = HWND(h as _);
        unsafe {
            let _ = UnregisterHotKey(Some(hwnd), OPEN_HOTKEY);
            match key {
                Some((mods, vk)) => RegisterHotKey(Some(hwnd), OPEN_HOTKEY, HOT_KEY_MODIFIERS(mods | 0x4000), vk).is_ok(),
                None => true,
            }
        }
    }

    /// An X Finder window exists (shown or hidden).
    pub fn running() -> bool {
        let mut target = HWND::default();
        unsafe extern "system" fn find(h: HWND, out: LPARAM) -> BOOL {
            if unsafe { !GetPropW(h, PROP).is_invalid() } {
                unsafe { *(out.0 as *mut HWND) = h };
                return false.into();
            }
            true.into()
        }
        unsafe {
            let _ = EnumWindows(Some(find), LPARAM(&mut target as *mut HWND as isize));
        }
        !target.is_invalid()
    }

    /// Sends `arg` to a running finder-rs window; true when it took it.
    pub fn forward(arg: &str) -> bool {
        let mut target = HWND::default();
        unsafe extern "system" fn find(h: HWND, out: LPARAM) -> BOOL {
            // Top-most first, so the window used last gets the tab.
            // Hidden too: X Finder waiting in the background for its key.
            if unsafe { !GetPropW(h, PROP).is_invalid() } {
                unsafe { *(out.0 as *mut HWND) = h };
                return false.into();
            }
            true.into()
        }
        unsafe {
            let _ = EnumWindows(Some(find), LPARAM(&mut target as *mut HWND as isize));
        }
        if target.is_invalid() {
            return false;
        }
        let data: Vec<u16> = arg.encode_utf16().chain(Some(0)).collect();
        let cds = COPYDATASTRUCT { dwData: OPEN_FOLDER, cbData: (data.len() * 2) as u32, lpData: data.as_ptr() as *mut _ };
        let mut result = 0usize;
        let ok = unsafe {
            SendMessageTimeoutW(
                target,
                WM_COPYDATA,
                WPARAM(0),
                LPARAM(&cds as *const COPYDATASTRUCT as isize),
                SMTO_ABORTIFHUNG,
                3000,
                Some(&mut result),
            )
        };
        if ok.0 == 0 || result != 1 {
            return false;
        }
        // We were just started by the user, so we may hand over the foreground.
        unsafe {
            if IsIconic(target).as_bool() {
                let _ = ShowWindow(target, SW_RESTORE);
            }
            let _ = SetForegroundWindow(target);
        }
        true
    }
}

/// A press on a file or folder: if the pointer then leaves the window with
/// the button down, the selection is dragged out to other apps.
pub(crate) static DRAG_ARMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// The pointer moved since the press (a drag, not a click).
pub(crate) static DRAG_MOVED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Where the left button went down (client coordinates, packed like lParam).
pub(crate) static PRESS_AT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn arm_drag_out() {
    DRAG_ARMED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Second process with a folder: give it to the running window instead.
pub fn forward_to_running(arg: &str) -> bool {
    #[cfg(windows)]
    {
        imp::forward(arg)
    }
    #[cfg(not(windows))]
    {
        let _ = arg;
        false
    }
}

/// An X Finder window exists (shown or hidden).
pub fn is_running() -> bool {
    #[cfg(windows)]
    {
        imp::running()
    }
    #[cfg(not(windows))]
    false
}

/// Takes (or with `None` gives up) the "open X Finder" key for the main
/// window: (RegisterHotKey modifiers, virtual key). False if it failed.
pub(super) fn set_open_hotkey(key: Option<(u32, u32)>) -> bool {
    #[cfg(windows)]
    {
        imp::set_hotkey(key)
    }
    #[cfg(not(windows))]
    {
        let _ = key;
        false
    }
}

/// Lets later processes send folders to this window (once it exists).
pub fn listen(ui: &crate::MainWindow) {
    #[cfg(windows)]
    {
        use slint::winit_030::WinitWindowAccessor;
        use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let hwnd = ui.window().with_winit_window(|w| match w.window_handle().ok()?.as_raw() {
            RawWindowHandle::Win32(h) => Some(windows::Win32::Foundation::HWND(h.hwnd.get() as _)),
            _ => None,
        });
        match hwnd {
            Some(Some(h)) => imp::listen(h),
            // The native window isn't there yet: try again shortly.
            _ => {
                let weak = ui.as_weak();
                slint::Timer::single_shot(std::time::Duration::from_millis(100), move || {
                    if let Some(ui) = weak.upgrade() {
                        listen(&ui);
                    }
                });
            }
        }
    }
    #[cfg(not(windows))]
    let _ = ui;
}
