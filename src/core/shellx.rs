//! Windows shell integration that must run on the UI thread:
//! the native Explorer context menu, "Open with" apps, "Send to" targets,
//! shortcuts and dark-mode menus.

use std::path::{Path, PathBuf};

/// What the native menu asked us to do ourselves (Explorer-only verbs).
#[derive(Debug, PartialEq, Eq)]
pub enum NativeOutcome {
    Nothing,
    Rename,
}

/// An app that can open a file type ("Open With" submenu).
#[derive(Clone, Debug)]
pub struct OpenWithApp {
    pub name: String,
    pub index: usize,
}

/// A "Send To" destination.
#[derive(Clone, Debug)]
pub enum SendTarget {
    /// Copy into this folder (Desktop, Documents, a USB drive, …).
    Folder { label: String, path: PathBuf },
    /// Create a .zip next to the items.
    Zip,
    /// Shortcut on the desktop.
    DesktopShortcut,
    /// A shortcut from the user's SendTo folder (e.g. Notepad): run it with
    /// the files as arguments.
    App { label: String, lnk: PathBuf },
}

impl SendTarget {
    pub fn label(&self) -> String {
        match self {
            SendTarget::Folder { label, .. } | SendTarget::App { label, .. } => label.clone(),
            SendTarget::Zip => "Compressed (zipped) folder".into(),
            SendTarget::DesktopShortcut => "Desktop (create shortcut)".into(),
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::{NativeOutcome, OpenWithApp, SendTarget};
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows::Win32::Graphics::Gdi::ClientToScreen;
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IDataObject, IPersistFile};
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    use windows::Win32::UI::Input::KeyboardAndMouse::GetActiveWindow;
    use windows::Win32::UI::Shell::Common::ITEMIDLIST;
    use windows::Win32::UI::Shell::{
        ASSOC_FILTER_RECOMMENDED, BHID_DataObject, CMF_EXTENDEDVERBS, CMF_NORMAL, CMIC_MASK_PTINVOKE,
        CMINVOKECOMMANDINFO, CMINVOKECOMMANDINFOEX, DefSubclassProc, IAssocHandler,
        IContextMenu, IContextMenu2, IContextMenu3, IShellFolder, IShellItem, IShellLinkW, ILFindLastID, ILFree,
        OAIF_EXEC, OAIF_ALLOW_REGISTRATION, OPENASINFO, RemoveWindowSubclass, SHAssocEnumHandlers, SHBindToParent,
        FOLDERID_SendTo, KNOWN_FOLDER_FLAG, SHCreateItemFromParsingName, SHGetDesktopFolder, SHGetKnownFolderPath,
        SHOpenWithDialog, SHParseDisplayName, SetWindowSubclass, ShellExecuteW, ShellLink,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreatePopupMenu, DestroyMenu, SW_SHOWNORMAL, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenuEx, WM_DRAWITEM,
        WM_INITMENUPOPUP, WM_MEASUREITEM, WM_MENUCHAR,
    };
    use windows::core::{HSTRING, Interface, PCSTR, PCWSTR, PWSTR, w};

    /// Not exported by the `windows` crate (same value as SEE_MASK_UNICODE).
    const CMIC_MASK_UNICODE: u32 = 0x4000;

    thread_local! {
        /// The menu being shown, so the window's message hook can let it draw
        /// its owner-drawn submenus ("Send to", "Open with", …).
        static ACTIVE_MENU: RefCell<Option<(Option<IContextMenu3>, Option<IContextMenu2>)>> = const { RefCell::new(None) };
    }

    unsafe extern "system" fn menu_hook(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _id: usize,
        _data: usize,
    ) -> LRESULT {
        if matches!(msg, WM_INITMENUPOPUP | WM_DRAWITEM | WM_MEASUREITEM | WM_MENUCHAR) {
            let handled = ACTIVE_MENU.with(|m| {
                let m = m.borrow();
                let (cm3, cm2) = m.as_ref()?;
                // SAFETY: forwarding the message as documented for IContextMenu2/3.
                unsafe {
                    if let Some(cm3) = cm3 {
                        let mut result = LRESULT(0);
                        if cm3.HandleMenuMsg2(msg, wparam, lparam, Some(&mut result)).is_ok() {
                            return Some(result);
                        }
                    } else if let Some(cm2) = cm2
                        && cm2.HandleMenuMsg(msg, wparam, lparam).is_ok()
                    {
                        return Some(LRESULT(0));
                    }
                }
                None
            });
            if let Some(r) = handled {
                return r;
            }
        }
        // SAFETY: default handling for everything else.
        unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
    }

    fn window() -> HWND {
        // SAFETY: plain query; the right-clicked window is the active one.
        unsafe { GetActiveWindow() }
    }

    fn screen_point(hwnd: HWND, x: f32, y: f32, scale: f32) -> POINT {
        let mut pt = POINT { x: (x * scale) as i32, y: (y * scale) as i32 };
        // SAFETY: valid window and point.
        unsafe {
            let _ = ClientToScreen(hwnd, &mut pt);
        }
        pt
    }

    struct Pidls(Vec<*mut ITEMIDLIST>);
    impl Drop for Pidls {
        fn drop(&mut self) {
            for p in &self.0 {
                // SAFETY: allocated by SHParseDisplayName.
                unsafe { ILFree(Some(*p)) };
            }
        }
    }

    fn parse(path: &Path) -> windows::core::Result<*mut ITEMIDLIST> {
        let mut pidl = std::ptr::null_mut();
        // SAFETY: valid string and out-pointer.
        unsafe { SHParseDisplayName(&HSTRING::from(path.as_os_str()), None, &mut pidl, 0, None)? };
        Ok(pidl)
    }

    fn menu_for_items(hwnd: HWND, paths: &[PathBuf]) -> windows::core::Result<IContextMenu> {
        let pidls = Pidls(paths.iter().map(|p| parse(p)).collect::<windows::core::Result<_>>()?);
        // SAFETY: pidls stay alive (owned by `pidls`) until after GetUIObjectOf.
        unsafe {
            let folder: IShellFolder = SHBindToParent(pidls.0[0], None)?;
            let children: Vec<*const ITEMIDLIST> = pidls.0.iter().map(|&p| ILFindLastID(p) as *const _).collect();
            folder.GetUIObjectOf(hwnd, &children, None)
        }
    }

    fn menu_for_background(hwnd: HWND, dir: &Path) -> windows::core::Result<IContextMenu> {
        let pidl = Pidls(vec![parse(dir)?]);
        // SAFETY: COM calls on the UI thread (STA, initialised by winit/OLE).
        unsafe {
            let desktop = SHGetDesktopFolder()?;
            let folder: IShellFolder = if (*pidl.0[0]).mkid.cb == 0 {
                desktop
            } else {
                desktop.BindToObject(pidl.0[0], None)?
            };
            folder.CreateViewObject(hwnd)
        }
    }

    /// Shows Explorer's own context menu for `paths` (or for the folder
    /// background when `paths` is empty) at window position (x, y).
    pub fn show_native_menu(
        paths: &[PathBuf],
        background: Option<&Path>,
        x: f32,
        y: f32,
        scale: f32,
        extended: bool,
    ) -> Result<NativeOutcome, String> {
        let hwnd = window();
        let cm = match background {
            Some(dir) => menu_for_background(hwnd, dir),
            None => menu_for_items(hwnd, paths),
        }
        .map_err(|e| e.message().to_string())?;
        let pt = screen_point(hwnd, x, y, scale);
        // SAFETY: menu handle and COM object are used only within this call.
        unsafe {
            let menu = CreatePopupMenu().map_err(|e| e.message().to_string())?;
            let flags = CMF_NORMAL | if extended { CMF_EXTENDEDVERBS } else { 0 };
            let _ = cm.QueryContextMenu(menu, 0, 1, 0x7FFF, flags);
            ACTIVE_MENU.with(|m| *m.borrow_mut() = Some((cm.cast().ok(), cm.cast().ok())));
            let _ = SetWindowSubclass(hwnd, Some(menu_hook), 0x5EED, 0);
            let cmd = TrackPopupMenuEx(menu, (TPM_RETURNCMD | TPM_RIGHTBUTTON).0, pt.x, pt.y, hwnd, None).0 as u32;
            let _ = RemoveWindowSubclass(hwnd, Some(menu_hook), 0x5EED);
            ACTIVE_MENU.with(|m| *m.borrow_mut() = None);
            let mut outcome = NativeOutcome::Nothing;
            if cmd > 0 {
                let id = (cmd - 1) as usize;
                // Explorer handles "rename" in its own view; we do it ourselves.
                let mut verb = [0u16; 64];
                let is_rename = cm
                    .GetCommandString(id, 4 /* GCS_VERBW */, None, windows::core::PSTR(verb.as_mut_ptr().cast()), 64)
                    .is_ok()
                    && String::from_utf16_lossy(&verb[..verb.iter().position(|&c| c == 0).unwrap_or(0)])
                        .eq_ignore_ascii_case("rename");
                if is_rename {
                    outcome = NativeOutcome::Rename;
                } else {
                    let info = CMINVOKECOMMANDINFOEX {
                        cbSize: size_of::<CMINVOKECOMMANDINFOEX>() as u32,
                        fMask: CMIC_MASK_UNICODE | CMIC_MASK_PTINVOKE,
                        hwnd,
                        lpVerb: PCSTR(id as *const u8),
                        lpVerbW: PCWSTR(id as *const u16),
                        nShow: SW_SHOWNORMAL.0,
                        ptInvoke: pt,
                        ..Default::default()
                    };
                    let _ = cm.InvokeCommand(&info as *const _ as *const CMINVOKECOMMANDINFO);
                }
            }
            let _ = DestroyMenu(menu);
            Ok(outcome)
        }
    }

    /// Native menu theme: `None` follows Windows, `Some(true)` dark, `Some(false)` light.
    pub fn set_menu_theme(dark: Option<bool>) {
        // uxtheme ordinal 135 = SetPreferredAppMode: 1 AllowDark, 2 ForceDark, 3 ForceLight.
        let mode = match dark {
            None => 1,
            Some(true) => 2,
            Some(false) => 3,
        };
        // SAFETY: see enable_dark_menus; pointers checked for null.
        unsafe {
            let Ok(lib) = LoadLibraryW(w!("uxtheme.dll")) else { return };
            if let Some(set_mode) = GetProcAddress(lib, PCSTR(135 as *const u8)) {
                let f: extern "system" fn(i32) -> i32 = std::mem::transmute(set_mode);
                f(mode);
            }
            if let Some(flush) = GetProcAddress(lib, PCSTR(136 as *const u8)) {
                let f: extern "system" fn() = std::mem::transmute(flush);
                f();
            }
        }
    }

    /// Whether Windows apps are set to dark mode (Settings > Personalization > Colors).
    pub fn system_prefers_dark() -> bool {
        use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
        let mut value: u32 = 1;
        let mut size = 4u32;
        // SAFETY: valid key/value names and a 4-byte output buffer.
        let ok = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
                w!("AppsUseLightTheme"),
                RRF_RT_REG_DWORD,
                None,
                Some(&mut value as *mut u32 as *mut _),
                Some(&mut size),
            )
        }
        .is_ok();
        ok && value == 0
    }

    /// Lets native menus follow the system dark mode (as Explorer's do).
    pub fn enable_dark_menus() {
        // SAFETY: uxtheme ordinals 135 (SetPreferredAppMode) and 136
        // (FlushMenuThemes) exist on Windows 10 1903+; checked for null.
        unsafe {
            let Ok(lib) = LoadLibraryW(w!("uxtheme.dll")) else { return };
            if let Some(set_mode) = GetProcAddress(lib, PCSTR(135 as *const u8)) {
                let f: extern "system" fn(i32) -> i32 = std::mem::transmute(set_mode);
                f(1); // AllowDark: follow the system setting
            }
            if let Some(flush) = GetProcAddress(lib, PCSTR(136 as *const u8)) {
                let f: extern "system" fn() = std::mem::transmute(flush);
                f();
            }
        }
    }

    fn handlers(path: &Path) -> Vec<IAssocHandler> {
        let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
        if ext.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        // SAFETY: COM enumeration on the UI thread.
        unsafe {
            let Ok(list) = SHAssocEnumHandlers(&HSTRING::from(ext), ASSOC_FILTER_RECOMMENDED) else { return out };
            loop {
                let mut one = [None];
                let mut got = 0u32;
                if list.Next(&mut one, Some(&mut got)).is_err() || got == 0 {
                    break;
                }
                if let Some(h) = one[0].take() {
                    out.push(h);
                }
                if out.len() >= 12 {
                    break;
                }
            }
        }
        out
    }

    fn take(p: PWSTR) -> String {
        // SAFETY: shell-allocated string, freed after copying.
        unsafe {
            let s = p.to_string().unwrap_or_default();
            windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const _));
            s
        }
    }

    pub fn open_with_apps(path: &Path) -> Vec<OpenWithApp> {
        handlers(path)
            .iter()
            .enumerate()
            // SAFETY: valid handler objects.
            .filter_map(|(index, h)| unsafe { h.GetUIName().ok() }.map(|n| OpenWithApp { name: take(n), index }))
            .collect()
    }

    pub fn open_with(path: &Path, index: usize) -> Result<(), String> {
        let h = handlers(path).into_iter().nth(index).ok_or("That app is no longer available.")?;
        // SAFETY: COM calls on the UI thread.
        unsafe {
            let item: IShellItem =
                SHCreateItemFromParsingName(&HSTRING::from(path.as_os_str()), None).map_err(|e| e.message().to_string())?;
            let data: IDataObject = item.BindToHandler(None, &BHID_DataObject).map_err(|e| e.message().to_string())?;
            h.Invoke(&data).map_err(|e| e.message().to_string())
        }
    }

    /// Windows' "How do you want to open this file?" dialog.
    pub fn open_with_dialog(path: &Path) {
        let file = HSTRING::from(path.as_os_str());
        let info = OPENASINFO { pcszFile: PCWSTR(file.as_ptr()), pcszClass: PCWSTR::null(), oaifInFlags: OAIF_EXEC | OAIF_ALLOW_REGISTRATION };
        // SAFETY: `file` outlives the call.
        unsafe {
            let _ = SHOpenWithDialog(Some(window()), &info);
        }
    }

    pub fn create_shortcut(target: &Path, link: &Path) -> Result<(), String> {
        let msg = |e: windows::core::Error| e.message().to_string();
        // SAFETY: COM calls; strings outlive them.
        unsafe {
            let sl: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(msg)?;
            sl.SetPath(&HSTRING::from(target.as_os_str())).map_err(msg)?;
            if let Some(dir) = target.parent() {
                let _ = sl.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()));
            }
            let pf: IPersistFile = sl.cast().map_err(msg)?;
            pf.Save(&HSTRING::from(link.as_os_str()), true).map_err(msg)
        }
    }

    /// Runs a shortcut (from the SendTo folder) with `files` as arguments.
    pub fn run_with_files(lnk: &Path, files: &[PathBuf]) {
        let params: String = files.iter().map(|f| format!("\"{}\" ", f.display())).collect();
        let (lnk, params) = (HSTRING::from(lnk.as_os_str()), HSTRING::from(params.trim_end()));
        // SAFETY: strings outlive the call.
        unsafe {
            ShellExecuteW(None, w!("open"), &lnk, &params, PCWSTR::null(), SW_SHOWNORMAL);
        }
    }

    /// Target of a .lnk shortcut.
    pub fn resolve_shortcut(lnk: &Path) -> Option<PathBuf> {
        // SAFETY: COM calls on a thread with COM initialised.
        unsafe {
            let sl: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
            let pf: IPersistFile = sl.cast().ok()?;
            pf.Load(&HSTRING::from(lnk.as_os_str()), windows::Win32::System::Com::STGM_READ).ok()?;
            let mut buf = [0u16; 1024];
            sl.GetPath(&mut buf, std::ptr::null_mut(), 0).ok()?;
            let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
            (len > 0).then(|| PathBuf::from(String::from_utf16_lossy(&buf[..len])))
        }
    }

    pub fn send_to_apps() -> Vec<SendTarget> {
        // SAFETY: shell-allocated path string, freed by `take`.
        let Ok(dir) = (unsafe { SHGetKnownFolderPath(&FOLDERID_SendTo, KNOWN_FOLDER_FLAG(0), None) }) else {
            return Vec::new();
        };
        let dir = PathBuf::from(take(dir));
        let mut apps: Vec<SendTarget> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("lnk")))
            .map(|e| SendTarget::App {
                label: e.path().file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
                lnk: e.path(),
            })
            .collect();
        apps.sort_by_key(|a| a.label().to_lowercase());
        apps
    }
}

#[cfg(windows)]
pub use imp::{
    create_shortcut, enable_dark_menus, open_with, open_with_apps, open_with_dialog, resolve_shortcut, set_menu_theme,
    show_native_menu, system_prefers_dark,
};

#[cfg(not(windows))]
mod fallback {
    use super::*;
    pub fn show_native_menu(_: &[PathBuf], _: Option<&Path>, _: f32, _: f32, _: f32, _: bool) -> Result<NativeOutcome, String> {
        Err("Not available on this platform.".into())
    }
    pub fn enable_dark_menus() {}
    pub fn resolve_shortcut(_: &Path) -> Option<PathBuf> {
        None
    }
    pub fn set_menu_theme(_: Option<bool>) {}
    pub fn system_prefers_dark() -> bool {
        false
    }
    pub fn open_with_apps(_: &Path) -> Vec<OpenWithApp> {
        Vec::new()
    }
    pub fn open_with(_: &Path, _: usize) -> Result<(), String> {
        Err("Not available on this platform.".into())
    }
    pub fn open_with_dialog(_: &Path) {}
    pub fn create_shortcut(_: &Path, _: &Path) -> Result<(), String> {
        Err("Not available on this platform.".into())
    }
}
#[cfg(not(windows))]
pub use fallback::*;

/// All "Send to" destinations: common folders, removable drives, zip,
/// desktop shortcut, and the apps in the user's SendTo folder.
pub fn send_to_targets() -> Vec<SendTarget> {
    use super::sys::{self, KnownFolder};
    let mut out = vec![SendTarget::Zip, SendTarget::DesktopShortcut];
    for (label, kf) in [("Desktop", KnownFolder::Desktop), ("Documents", KnownFolder::Documents), ("Downloads", KnownFolder::Downloads)] {
        if let Some(path) = sys::known_folder(kf) {
            out.push(SendTarget::Folder { label: label.into(), path });
        }
    }
    for d in sys::drives().into_iter().filter(|d| d.kind == 2) {
        out.push(SendTarget::Folder { label: format!("USB Drive ({})", d.name), path: PathBuf::from(format!("{}\\", d.name)) });
    }
    #[cfg(windows)]
    out.extend(imp::send_to_apps());
    out
}

/// Recently opened files (Windows' own Recent list), newest first, with the
/// time they were last opened (Unix seconds). Folders are skipped.
pub fn recent_files(limit: usize) -> Vec<(PathBuf, i64)> {
    let Some(dir) = std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("Microsoft").join("Windows").join("Recent"))
    else {
        return Vec::new();
    };
    let mut links: Vec<(PathBuf, std::time::SystemTime)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("lnk")))
        .filter_map(|e| Some((e.path(), e.metadata().ok()?.modified().ok()?)))
        .collect();
    links.sort_by_key(|l| std::cmp::Reverse(l.1));
    let mut out: Vec<(PathBuf, i64)> = Vec::new();
    for (lnk, when) in links.into_iter().take(limit * 3) {
        #[cfg(windows)]
        let target = imp::resolve_shortcut(&lnk);
        #[cfg(not(windows))]
        let target: Option<PathBuf> = {
            let _ = &lnk;
            None
        };
        let Some(target) = target.filter(|t| t.is_file()) else { continue };
        if out.iter().any(|(p, _)| *p == target) {
            continue;
        }
        let secs = when.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        out.push((target, secs));
        if out.len() >= limit {
            break;
        }
    }
    out
}

/// Runs a SendTo app shortcut with the files as arguments.
pub fn run_send_to_app(lnk: &Path, files: &[PathBuf]) {
    #[cfg(windows)]
    imp::run_with_files(lnk, files);
    #[cfg(not(windows))]
    let _ = (lnk, files);
}
