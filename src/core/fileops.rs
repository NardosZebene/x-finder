//! File operations, clipboard and shell helpers.
//!
//! Copy, move, rename and delete go through Windows' own copy engine
//! (`IFileOperation`), which gives the native progress dialog, conflict
//! ("replace or skip") prompts, elevation prompts, the Recycle Bin and
//! Explorer-compatible undo. The clipboard uses the standard file-list format,
//! so copy/paste works both ways with Explorer.
//!
//! Operations block: run them on a worker thread after
//! [`super::shell::init_thread`]`(true)`.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    /// Copy `sources` into `dest`.
    Copy { sources: Vec<PathBuf>, dest: PathBuf },
    /// Move `sources` into `dest`.
    Move { sources: Vec<PathBuf>, dest: PathBuf },
    /// Copy next to the original ("name (2).ext").
    Duplicate { sources: Vec<PathBuf> },
    /// Move to the Recycle Bin.
    Recycle { sources: Vec<PathBuf> },
    /// Delete permanently (Windows asks for confirmation).
    Delete { sources: Vec<PathBuf> },
    Rename { source: PathBuf, new_name: String },
}

impl Op {
    /// Short label for the status bar.
    pub fn describe(&self) -> String {
        let n = |v: &Vec<PathBuf>| if v.len() == 1 { "1 item".to_string() } else { format!("{} items", v.len()) };
        match self {
            Op::Copy { sources, .. } => format!("Copied {}", n(sources)),
            Op::Move { sources, .. } => format!("Moved {}", n(sources)),
            Op::Duplicate { sources } => format!("Duplicated {}", n(sources)),
            Op::Recycle { sources } => format!("Moved {} to the Recycle Bin", n(sources)),
            Op::Delete { sources } => format!("Deleted {}", n(sources)),
            Op::Rename { new_name, .. } => format!("Renamed to “{new_name}”"),
        }
    }
}

/// True if moving/copying `source` into `dest` would put a folder inside itself.
pub fn is_inside(dest: &Path, source: &Path) -> bool {
    dest.starts_with(source)
}

/// Both paths on the same drive (moves there are instant; Windows' default
/// drag action is move on the same drive and copy across drives).
pub fn same_volume(a: &Path, b: &Path) -> bool {
    let root = |p: &Path| p.components().next().map(|c| c.as_os_str().to_string_lossy().to_lowercase());
    root(a) == root(b)
}

/// Picks "New folder", "New folder (2)", … that doesn't exist yet in `dir`.
pub fn unique_name(dir: &Path, base: &str) -> String {
    if !dir.join(base).exists() {
        return base.to_string();
    }
    (2..).map(|i| format!("{base} ({i})")).find(|n| !dir.join(n).exists()).unwrap()
}

/// Checks a new file name before handing it to Windows.
pub fn validate_name(name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("A name can't be empty.".into());
    }
    if let Some(c) = name.chars().find(|c| r#"\/:*?"<>|"#.contains(*c) || c.is_control()) {
        return Err(format!("A name can't contain “{c}”."));
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Err("A name can't end with a dot or a space.".into());
    }
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = ["CON", "PRN", "AUX", "NUL"];
    if reserved.contains(&stem.as_str())
        || ((stem.starts_with("COM") || stem.starts_with("LPT")) && stem.len() == 4 && stem.as_bytes()[3].is_ascii_digit())
    {
        return Err(format!("“{name}” is reserved by Windows."));
    }
    Ok(())
}

#[cfg(windows)]
mod imp {
    use super::Op;
    use std::os::windows::ffi::OsStringExt;
    use std::path::{Path, PathBuf};

    use windows::Win32::Foundation::{HANDLE, HGLOBAL};
    use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
    use windows::Win32::System::Ole::{CF_HDROP, CF_UNICODETEXT};
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_SHIFT};
    use windows::Win32::UI::Shell::{
        DROPFILES, DragQueryFileW, FOF_ALLOWUNDO, FOF_NOCONFIRMMKDIR, FOF_RENAMEONCOLLISION, FOFX_ADDUNDORECORD,
        FOFX_RECYCLEONDELETE, FOFX_SHOWELEVATIONPROMPT, FileOperation, HDROP, IFileOperation,
        IFileOperationProgressSink, IShellItem, SHCreateItemFromParsingName, SHOP_FILEPATH, SHObjectProperties,
    };
    use windows::core::{HSTRING, PCWSTR, w};

    fn item(p: &Path) -> Result<IShellItem, String> {
        // SAFETY: COM is initialised on the calling thread.
        unsafe { SHCreateItemFromParsingName(&HSTRING::from(p.as_os_str()), None) }
            .map_err(|_| format!("“{}” can't be found.", p.display()))
    }

    pub fn run(op: &Op) -> Result<(), String> {
        let msg = |e: windows::core::Error| e.message().to_string();
        // SAFETY: COM calls on a thread where COM is initialised; all items
        // and strings outlive PerformOperations.
        unsafe {
            let fo: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL).map_err(msg)?;
            let base = FOF_ALLOWUNDO | FOF_NOCONFIRMMKDIR | FOFX_SHOWELEVATIONPROMPT | FOFX_ADDUNDORECORD;
            let flags = match op {
                Op::Recycle { .. } => base | FOFX_RECYCLEONDELETE,
                Op::Delete { .. } => FOF_NOCONFIRMMKDIR | FOFX_SHOWELEVATIONPROMPT,
                Op::Duplicate { .. } => base | FOF_RENAMEONCOLLISION,
                // Copying into the folder the files are already in makes copies.
                Op::Copy { sources, dest } if sources.iter().any(|s| s.parent() == Some(dest.as_path())) => {
                    base | FOF_RENAMEONCOLLISION
                }
                _ => base,
            };
            fo.SetOperationFlags(flags).map_err(msg)?;
            let no_sink: Option<&IFileOperationProgressSink> = None;
            match op {
                Op::Copy { sources, dest } | Op::Move { sources, dest } => {
                    let to = item(dest)?;
                    for s in sources {
                        let it = item(s)?;
                        if matches!(op, Op::Copy { .. }) {
                            fo.CopyItem(&it, &to, PCWSTR::null(), no_sink).map_err(msg)?;
                        } else {
                            fo.MoveItem(&it, &to, PCWSTR::null(), no_sink).map_err(msg)?;
                        }
                    }
                }
                Op::Duplicate { sources } => {
                    for s in sources {
                        let parent = s.parent().ok_or("Can't duplicate a drive.")?;
                        fo.CopyItem(&item(s)?, &item(parent)?, PCWSTR::null(), no_sink).map_err(msg)?;
                    }
                }
                Op::Recycle { sources } | Op::Delete { sources } => {
                    for s in sources {
                        fo.DeleteItem(&item(s)?, no_sink).map_err(msg)?;
                    }
                }
                Op::Rename { source, new_name } => {
                    fo.RenameItem(&item(source)?, &HSTRING::from(new_name.as_str()), no_sink).map_err(msg)?;
                }
            }
            match fo.PerformOperations() {
                Ok(()) => {}
                // Cancelled by the user in a Windows dialog.
                Err(e) if e.code().0 as u32 == 0x8007_04C7 => return Err("Cancelled.".into()),
                Err(e) => return Err(msg(e)),
            }
            if fo.GetAnyOperationsAborted().map(|b| b.as_bool()).unwrap_or(false) {
                return Err("Some items were skipped or the operation was cancelled.".into());
            }
            Ok(())
        }
    }

    /// Puts files on the clipboard, as Explorer does (CF_HDROP + drop effect).
    pub fn set_clipboard_files(paths: &[PathBuf], cut: bool) -> Result<(), String> {
        // DROPFILES header followed by double-NUL-terminated UTF-16 paths.
        let mut wide: Vec<u16> = Vec::new();
        for p in paths {
            use std::os::windows::ffi::OsStrExt;
            wide.extend(p.as_os_str().encode_wide());
            wide.push(0);
        }
        wide.push(0);
        let header = size_of::<DROPFILES>();
        // SAFETY: the clipboard takes ownership of the global memory blocks.
        unsafe {
            let mem = GlobalAlloc(GMEM_MOVEABLE, header + wide.len() * 2).map_err(|e| e.message().to_string())?;
            let ptr = GlobalLock(mem) as *mut u8;
            let df = DROPFILES { pFiles: header as u32, fWide: true.into(), ..Default::default() };
            std::ptr::copy_nonoverlapping(&df as *const DROPFILES as *const u8, ptr, header);
            std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, ptr.add(header), wide.len() * 2);
            let _ = GlobalUnlock(mem);

            let effect_mem = GlobalAlloc(GMEM_MOVEABLE, 4).map_err(|e| e.message().to_string())?;
            let eptr = GlobalLock(effect_mem) as *mut u32;
            *eptr = if cut { 2 } else { 5 }; // DROPEFFECT_MOVE : COPY|LINK
            let _ = GlobalUnlock(effect_mem);

            OpenClipboard(None).map_err(|_| "The clipboard is busy.".to_string())?;
            let _ = EmptyClipboard();
            let ok = SetClipboardData(CF_HDROP.0 as u32, Some(HANDLE(mem.0))).is_ok();
            let fmt = RegisterClipboardFormatW(w!("Preferred DropEffect"));
            let _ = SetClipboardData(fmt, Some(HANDLE(effect_mem.0)));
            let _ = CloseClipboard();
            if ok { Ok(()) } else { Err("Couldn't use the clipboard.".into()) }
        }
    }

    /// Files on the clipboard and whether they were cut (vs copied).
    pub fn clipboard_files() -> Option<(Vec<PathBuf>, bool)> {
        // SAFETY: clipboard handles are only used while the clipboard is open.
        unsafe {
            OpenClipboard(None).ok()?;
            let result = (|| {
                let h = GetClipboardData(CF_HDROP.0 as u32).ok()?;
                let hdrop = HDROP(h.0);
                let count = DragQueryFileW(hdrop, u32::MAX, None);
                let mut files = Vec::with_capacity(count as usize);
                for i in 0..count {
                    let len = DragQueryFileW(hdrop, i, None) as usize;
                    let mut buf = vec![0u16; len + 1];
                    DragQueryFileW(hdrop, i, Some(&mut buf));
                    buf.truncate(len);
                    files.push(PathBuf::from(std::ffi::OsString::from_wide(&buf)));
                }
                let fmt = RegisterClipboardFormatW(w!("Preferred DropEffect"));
                let cut = GetClipboardData(fmt)
                    .ok()
                    .map(|h| {
                        let p = GlobalLock(HGLOBAL(h.0)) as *const u32;
                        let v = if p.is_null() { 0 } else { *p };
                        let _ = GlobalUnlock(HGLOBAL(h.0));
                        v & 2 != 0 && v & 1 == 0
                    })
                    .unwrap_or(false);
                (!files.is_empty()).then_some((files, cut))
            })();
            let _ = CloseClipboard();
            result
        }
    }

    pub fn set_clipboard_text(text: &str) -> Result<(), String> {
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: the clipboard takes ownership of the global memory block.
        unsafe {
            let mem = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2).map_err(|e| e.message().to_string())?;
            let ptr = GlobalLock(mem) as *mut u16;
            std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
            let _ = GlobalUnlock(mem);
            OpenClipboard(None).map_err(|_| "The clipboard is busy.".to_string())?;
            let _ = EmptyClipboard();
            let ok = SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(mem.0))).is_ok();
            let _ = CloseClipboard();
            if ok { Ok(()) } else { Err("Couldn't use the clipboard.".into()) }
        }
    }

    pub fn show_properties(path: &Path) {
        // SAFETY: valid NUL-terminated path string.
        unsafe {
            let _ = SHObjectProperties(None, SHOP_FILEPATH, &HSTRING::from(path.as_os_str()), PCWSTR::null());
        }
    }

    /// (ctrl, shift) currently held — used to pick copy vs move on drop.
    pub fn modifiers() -> (bool, bool) {
        // SAFETY: plain state query.
        unsafe {
            (GetAsyncKeyState(VK_CONTROL.0 as i32) < 0, GetAsyncKeyState(VK_SHIFT.0 as i32) < 0)
        }
    }
}

#[cfg(windows)]
pub use imp::{clipboard_files, modifiers, run, set_clipboard_files, set_clipboard_text, show_properties};

#[cfg(not(windows))]
mod fallback {
    use super::Op;
    use std::path::{Path, PathBuf};
    pub fn run(op: &Op) -> Result<(), String> {
        match op {
            Op::Rename { source, new_name } => {
                std::fs::rename(source, source.with_file_name(new_name)).map_err(|e| e.to_string())
            }
            _ => Err("Not supported on this platform yet.".into()),
        }
    }
    pub fn set_clipboard_files(_: &[PathBuf], _: bool) -> Result<(), String> {
        Err("Not supported on this platform yet.".into())
    }
    pub fn clipboard_files() -> Option<(Vec<PathBuf>, bool)> {
        None
    }
    pub fn set_clipboard_text(_: &str) -> Result<(), String> {
        Err("Not supported on this platform yet.".into())
    }
    pub fn show_properties(_: &Path) {}
    pub fn modifiers() -> (bool, bool) {
        (false, false)
    }
}
#[cfg(not(windows))]
pub use fallback::*;

/// Zips `sources` into `dest` (a new .zip file). Folders are added recursively.
pub fn zip_items(sources: &[PathBuf], dest: &Path) -> Result<(), String> {
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    let file = std::fs::File::create(dest).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipWriter::new(file);
    let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut buf = Vec::new();
    fn add(
        zip: &mut zip::ZipWriter<std::fs::File>,
        opts: SimpleFileOptions,
        path: &Path,
        name: &str,
        buf: &mut Vec<u8>,
    ) -> Result<(), String> {
        if path.is_dir() {
            zip.add_directory(format!("{name}/"), opts).map_err(|e| e.to_string())?;
            let mut children: Vec<_> = std::fs::read_dir(path).map_err(|e| e.to_string())?.flatten().collect();
            children.sort_by_key(|c| c.file_name());
            for c in children {
                let child = format!("{name}/{}", c.file_name().to_string_lossy());
                add(zip, opts, &c.path(), &child, buf)?;
            }
        } else {
            zip.start_file(name, opts).map_err(|e| e.to_string())?;
            buf.clear();
            std::io::Read::read_to_end(&mut std::fs::File::open(path).map_err(|e| e.to_string())?, buf)
                .map_err(|e| e.to_string())?;
            zip.write_all(buf).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    for s in sources {
        let name = s.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if let Err(e) = add(&mut zip, opts, s, &name, &mut buf) {
            drop(zip);
            let _ = std::fs::remove_file(dest);
            return Err(format!("Couldn't compress: {e}"));
        }
    }
    zip.finish().map_err(|e| e.to_string())?;
    Ok(())
}

/// "Photos.zip" for one item, "Archive.zip" for several, made unique in `dir`.
pub fn zip_name(sources: &[PathBuf], dir: &Path) -> String {
    let base = match sources {
        [one] => one.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Archive".into()),
        _ => "Archive".into(),
    };
    let first = format!("{base}.zip");
    if !dir.join(&first).exists() {
        return first;
    }
    (2..).map(|i| format!("{base} ({i}).zip")).find(|n| !dir.join(n).exists()).unwrap()
}

/// "Report.pdf - Shortcut.lnk", made unique in `dir`.
pub fn shortcut_name(target: &Path, dir: &Path) -> String {
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Drive".into());
    let first = format!("{name} - Shortcut.lnk");
    if !dir.join(&first).exists() {
        return first;
    }
    (2..).map(|i| format!("{name} - Shortcut ({i}).lnk")).find(|n| !dir.join(n).exists()).unwrap()
}

/// Opens Windows Explorer with `path` selected.
pub fn reveal_in_explorer(path: &Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        super::explorer_catch::allow_explorer();
        let _ = std::process::Command::new("explorer.exe").raw_arg(format!("/select,\"{}\"", path.display())).spawn();
    }
    #[cfg(not(windows))]
    let _ = path;
}

/// Opens Windows Terminal (or PowerShell if it isn't installed) in `dir`.
pub fn open_terminal(dir: &Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_CONSOLE: u32 = 0x10;
        if std::process::Command::new("wt.exe").arg("-d").arg(dir).spawn().is_err() {
            let _ = std::process::Command::new("powershell.exe")
                .arg("-NoExit")
                .current_dir(dir)
                .creation_flags(CREATE_NEW_CONSOLE)
                .spawn();
        }
    }
    #[cfg(not(windows))]
    let _ = dir;
}

/// VS Code's Code.exe, when it's installed (per-user, for everyone, or
/// found through `code` on the PATH). Looked up once; `None` if missing.
pub fn vscode() -> Option<PathBuf> {
    static FOUND: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    FOUND
        .get_or_init(|| {
            let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
            let mut tries: Vec<PathBuf> = [
                env("LOCALAPPDATA").map(|d| d.join(r"Programs\Microsoft VS Code\Code.exe")),
                env("ProgramFiles").map(|d| d.join(r"Microsoft VS Code\Code.exe")),
                env("ProgramFiles(x86)").map(|d| d.join(r"Microsoft VS Code\Code.exe")),
            ]
            .into_iter()
            .flatten()
            .collect();
            // `code` on the PATH lives in <install>/bin/code.cmd.
            if let Some(path) = std::env::var_os("PATH") {
                for dir in std::env::split_paths(&path) {
                    if dir.join("code.cmd").is_file()
                        && let Some(install) = dir.parent()
                    {
                        tries.push(install.join("Code.exe"));
                    }
                }
            }
            tries.into_iter().find(|p| p.is_file())
        })
        .clone()
        // Uninstalled since: gone.
        .filter(|p| p.is_file())
}

/// Opens a file or folder in VS Code. False when it isn't installed or
/// didn't start.
pub fn open_in_vscode(path: &Path) -> bool {
    let Some(exe) = vscode() else { return false };
    std::process::Command::new(exe).arg(path).spawn().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(validate_name("report.pdf").is_ok());
        assert!(validate_name("  ").is_err());
        assert!(validate_name("a:b").is_err());
        assert!(validate_name("con.txt").is_err());
        assert!(validate_name("COM1").is_err());
        assert!(validate_name("COMPUTER").is_ok());
        assert!(validate_name("trailing.").is_err());
    }

    #[test]
    fn zips_folders() {
        let root = std::env::temp_dir().join("finder-rs-zip-test");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(root.join("a/b/c.txt"), "hello").unwrap();
        let dest = root.join(zip_name(&[root.join("a")], &root));
        zip_items(&[root.join("a")], &dest).unwrap();
        let mut z = zip::ZipArchive::new(std::fs::File::open(&dest).unwrap()).unwrap();
        let mut s = String::new();
        std::io::Read::read_to_string(&mut z.by_name("a/b/c.txt").unwrap(), &mut s).unwrap();
        assert_eq!(s, "hello");
        assert_eq!(dest.file_name().unwrap(), "a.zip");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    #[test]
    fn volumes_and_nesting() {
        assert!(same_volume(Path::new(r"C:\a"), Path::new(r"c:\b\c")));
        assert!(!same_volume(Path::new(r"C:\a"), Path::new(r"D:\a")));
        assert!(is_inside(Path::new(r"C:\a\b"), Path::new(r"C:\a")));
        assert!(!is_inside(Path::new(r"C:\ab"), Path::new(r"C:\a")));
    }
}
