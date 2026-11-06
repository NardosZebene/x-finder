//! "Use as the default file explorer": folders opened by other apps (and by
//! double-clicking folder shortcuts, `start C:\…`, Run) open in finder-rs.
//!
//! Per-user only (HKCU), nothing system-wide, and fully undone by turning
//! the setting off: the `open` verb of Directory / Drive is pointed at our
//! exe. Virtual folders (Control Panel, Recycle Bin…) stay with Explorer:
//! the app hands anything that isn't a real path back to explorer.exe.

use std::path::Path;

/// Classes whose "open" we take over.
const CLASSES: [&str; 2] = ["Directory", "Drive"];

/// The "This PC / Quick access" object Win+E opens; its `opennewwindow`
/// verb is what the shortcut runs.
const WIN_E: &str = r"Software\Classes\CLSID\{52205fd8-5dfb-447d-801a-d0b52f2e83e1}";

/// Win+E: a new tab at the start folder (Desktop).
fn win_e_command(exe: &Path) -> String {
    format!("\"{}\" --start", exe.display())
}

fn command_line(exe: &Path) -> String {
    format!("\"{}\" \"%1\"", exe.display())
}

#[cfg(windows)]
mod imp {
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::*;
    use windows::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};
    use windows::core::{HSTRING, PCWSTR};

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    pub fn set_string(key: &str, name: Option<&str>, value: &str) -> Result<(), String> {
        // RegSetKeyValueW creates the key when it doesn't exist yet.
        let data = wide(value);
        let name_w = name.map(HSTRING::from);
        let r = unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                &HSTRING::from(key),
                name_w.as_ref().map_or(PCWSTR::null(), |n| PCWSTR(n.as_ptr())),
                REG_SZ.0,
                Some(data.as_ptr() as *const _),
                (data.len() * 2) as u32,
            )
        };
        if r != ERROR_SUCCESS {
            return Err(format!("Couldn't write the registry ({}).", r.0));
        }
        Ok(())
    }

    pub fn get_string(key: &str, name: Option<&str>) -> Option<String> {
        unsafe {
            let mut buf = vec![0u16; 2048];
            let mut len = (buf.len() * 2) as u32;
            let name_w = name.map(HSTRING::from);
            let r = RegGetValueW(
                HKEY_CURRENT_USER,
                &HSTRING::from(key),
                name_w.as_ref().map_or(PCWSTR::null(), |n| PCWSTR(n.as_ptr())),
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr() as *mut _),
                Some(&mut len),
            );
            if r != ERROR_SUCCESS {
                return None;
            }
            let n = (len as usize / 2).saturating_sub(1);
            Some(String::from_utf16_lossy(&buf[..n.min(buf.len())]))
        }
    }

    pub fn delete_tree(key: &str) {
        unsafe {
            let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(key));
        }
    }

    pub fn delete_value(key: &str, name: Option<&str>) {
        unsafe {
            let mut h = HKEY::default();
            if RegOpenKeyExW(HKEY_CURRENT_USER, &HSTRING::from(key), None, KEY_SET_VALUE, &mut h) == ERROR_SUCCESS {
                let name_w = name.map(HSTRING::from);
                let _ = RegDeleteValueW(h, name_w.as_ref().map_or(PCWSTR::null(), |n| PCWSTR(n.as_ptr())));
                let _ = RegCloseKey(h);
            }
        }
    }

    pub fn notify() {
        unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
    }
}

/// Where X Finder keeps the copy Windows opens folders with: a permanent
/// place, so it keeps working when the app was run from a zip (WinRAR and
/// Windows unpack those into a temporary folder that is deleted later).
pub fn installed_exe() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(Path::new(&base).join("Programs").join("X Finder").join("X Finder.exe"))
}

/// Copies `exe` to the permanent place (unless it's already there or
/// identical). Returns the path to register.
pub fn install_copy(exe: &Path) -> std::path::PathBuf {
    let Some(dst) = installed_exe() else { return exe.to_path_buf() };
    if same_file(exe, &dst) {
        return dst;
    }
    let same_bytes = std::fs::metadata(exe).ok().map(|m| m.len()) == std::fs::metadata(&dst).ok().map(|m| m.len())
        && std::fs::read(exe).ok() == std::fs::read(&dst).ok();
    if !same_bytes {
        let _ = dst.parent().map(std::fs::create_dir_all);
        // A running copy can't be overwritten, but it can be renamed away.
        let old = dst.with_extension("old.exe");
        let _ = std::fs::remove_file(&old);
        if dst.exists() {
            let _ = std::fs::rename(&dst, &old);
        }
        if std::fs::copy(exe, &dst).is_err() {
            let _ = std::fs::rename(&old, &dst);
            return if dst.exists() { dst } else { exe.to_path_buf() };
        }
    }
    dst
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// The exe Windows currently opens folders with, if it's an X Finder.
pub fn registered_x_finder() -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    {
        let v = imp::get_string("Software\\Classes\\Directory\\shell\\open\\command", None)?;
        let exe = v.trim().strip_prefix('"')?.split('"').next()?.to_string();
        let lower = exe.to_lowercase();
        (lower.contains("x finder") || lower.contains("finder-rs")).then(|| std::path::PathBuf::from(exe))
    }
    #[cfg(not(windows))]
    None
}

/// X Finder is the default but registered from a place that's gone or
/// temporary (run from inside a zip): register `exe`'s permanent copy.
pub fn repair(exe: &Path) {
    let Some(reg) = registered_x_finder() else { return };
    let lower = reg.to_string_lossy().to_lowercase();
    if !reg.exists() || lower.contains("\\temp\\") || lower.contains("rar$") {
        let _ = set_default(exe, true);
    }
}

/// Default explorer is on and points at the permanent copy: keep that copy
/// up to date when a newer X Finder is started from somewhere else.
pub fn refresh_installed(exe: &Path) {
    if let Some(dst) = installed_exe()
        && !same_file(exe, &dst)
        && is_default_for(&dst)
    {
        install_copy(exe);
    }
}

/// Starts `exe --hidden` at sign-in (the "open X Finder" key), or with
/// `None` stops doing so.
pub fn set_startup(exe: Option<&Path>) {
    #[cfg(windows)]
    {
        const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
        match exe {
            Some(exe) => {
                let _ = imp::set_string(RUN, Some("X Finder"), &format!("\"{}\" --hidden", exe.display()));
            }
            None => imp::delete_value(RUN, Some("X Finder")),
        }
    }
    #[cfg(not(windows))]
    let _ = exe;
}

/// An earlier version started a background helper with Windows; take it
/// out of start-up (it quits by itself at the next sign-in).
pub fn remove_background() {
    #[cfg(windows)]
    imp::delete_value(r"Software\Microsoft\Windows\CurrentVersion\Run", Some("X Finder helper"));
}

/// True when folders currently open in this exe (or in X Finder's
/// permanent copy).
pub fn is_default(exe: &Path) -> bool {
    is_default_for(exe) || installed_exe().is_some_and(|p| is_default_for(&p))
}

fn is_default_for(exe: &Path) -> bool {
    #[cfg(windows)]
    {
        let want = command_line(exe).to_lowercase();
        let win_e = imp::get_string(&format!("{WIN_E}\\shell\\opennewwindow\\command"), None)
            .is_some_and(|v| v.to_lowercase() == win_e_command(exe).to_lowercase());
        win_e
            && CLASSES.iter().all(|c| {
                imp::get_string(&format!("Software\\Classes\\{c}\\shell\\open\\command"), None)
                    .is_some_and(|v| v.to_lowercase() == want)
            })
    }
    #[cfg(not(windows))]
    {
        let _ = exe;
        false
    }
}

/// Points (or stops pointing) the folder "open" action at X Finder (its
/// permanent copy, see `install_copy`).
pub fn set_default(exe: &Path, on: bool) -> Result<(), String> {
    #[cfg(windows)]
    {
        let installed = if on { install_copy(exe) } else { exe.to_path_buf() };
        let exe = installed.as_path();
        for c in CLASSES {
            let shell = format!("Software\\Classes\\{c}\\shell");
            let open = format!("{shell}\\open");
            if on {
                imp::set_string(&format!("{open}\\command"), None, &command_line(exe))?;
                // Explorer's own handler would win otherwise.
                imp::set_string(&format!("{open}\\command"), Some("DelegateExecute"), "")?;
                imp::set_string(&shell, None, "open")?;
            } else {
                // Only remove what we added (this exe, or an older copy
                // under either name).
                let name = exe.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
                let ours = imp::get_string(&format!("{open}\\command"), None).is_some_and(|v| {
                    let v = v.to_lowercase();
                    v.contains("finder-rs") || v.contains("x finder") || (!name.is_empty() && v.contains(&name))
                });
                if ours {
                    imp::delete_tree(&open);
                    if imp::get_string(&shell, None).as_deref() == Some("open") {
                        imp::delete_value(&shell, None);
                    }
                }
            }
        }
        // Win+E.
        let win_e = format!("{WIN_E}\\shell\\opennewwindow\\command");
        if on {
            imp::set_string(&win_e, None, &win_e_command(exe))?;
            imp::set_string(&win_e, Some("DelegateExecute"), "")?;
        } else {
            let name = exe.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
            let ours = imp::get_string(&win_e, None).is_some_and(|v| {
                let v = v.to_lowercase();
                v.contains("finder-rs") || v.contains("x finder") || (!name.is_empty() && v.contains(&name))
            });
            if ours {
                imp::delete_tree(WIN_E);
            }
        }
        imp::notify();
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = (exe, on);
        Err("Only on Windows.".into())
    }
}
