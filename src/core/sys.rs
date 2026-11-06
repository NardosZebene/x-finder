//! Small OS integration layer: known folders, drives, opening files, memory.
//! Uses direct Win32 declarations instead of the large `windows` crate to keep
//! the build small (plan §8: add crates only when a milestone needs them).

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug)]
pub enum KnownFolder {
    Profile,
    Desktop,
    Documents,
    Downloads,
    Pictures,
    Music,
    Videos,
}

#[derive(Clone, Debug)]
pub struct Drive {
    /// "C:"
    pub name: String,
    /// `GetDriveTypeW` value: 2 removable, 3 fixed, 4 network, 5 CD, 6 RAM disk.
    pub kind: u8,
}

pub fn home() -> PathBuf {
    known_folder(KnownFolder::Profile)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/"))
}

#[cfg(windows)]
mod win {
    use std::ffi::{OsStr, OsString, c_void};
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    #[repr(C)]
    pub struct Guid {
        d1: u32,
        d2: u16,
        d3: u16,
        d4: [u8; 8],
    }

    pub const fn guid(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> Guid {
        Guid { d1, d2, d3, d4 }
    }

    #[repr(C)]
    #[derive(Default)]
    pub struct ProcessMemoryCounters {
        pub cb: u32,
        pub page_fault_count: u32,
        pub peak_working_set_size: usize,
        pub working_set_size: usize,
        pub quota_peak_paged_pool_usage: usize,
        pub quota_paged_pool_usage: usize,
        pub quota_peak_non_paged_pool_usage: usize,
        pub quota_non_paged_pool_usage: usize,
        pub pagefile_usage: usize,
        pub peak_pagefile_usage: usize,
    }

    #[link(name = "shell32")]
    unsafe extern "system" {
        pub fn SHGetKnownFolderPath(rfid: *const Guid, flags: u32, token: *mut c_void, path: *mut *mut u16) -> i32;
        pub fn ShellExecuteW(
            hwnd: *mut c_void,
            op: *const u16,
            file: *const u16,
            params: *const u16,
            dir: *const u16,
            show: i32,
        ) -> isize;
    }

    #[link(name = "ole32")]
    unsafe extern "system" {
        pub fn CoTaskMemFree(pv: *mut c_void);
        pub fn CoInitializeEx(reserved: *mut c_void, coinit: u32) -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn GetVolumeInformationW(
            root: *const u16,
            name: *mut u16,
            name_len: u32,
            serial: *mut u32,
            max_component: *mut u32,
            flags: *mut u32,
            fs_name: *mut u16,
            fs_name_len: u32,
        ) -> i32;
        pub fn GetDiskFreeSpaceExW(dir: *const u16, free_to_caller: *mut u64, total: *mut u64, total_free: *mut u64) -> i32;
        pub fn SetErrorMode(mode: u32) -> u32;
        pub fn GetLogicalDrives() -> u32;
        pub fn GetDriveTypeW(root: *const u16) -> u32;
        pub fn GetCurrentProcess() -> *mut c_void;
        pub fn K32GetProcessMemoryInfo(process: *mut c_void, counters: *mut ProcessMemoryCounters, cb: u32) -> i32;
    }

    pub fn wide(s: &OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    /// Takes ownership of a CoTaskMem-allocated, NUL-terminated wide string.
    pub unsafe fn take_pwstr(p: *mut u16) -> OsString {
        let mut len = 0;
        // SAFETY: caller guarantees `p` is a valid NUL-terminated string.
        unsafe {
            while *p.add(len) != 0 {
                len += 1;
            }
            let s = OsString::from_wide(std::slice::from_raw_parts(p, len));
            CoTaskMemFree(p.cast());
            s
        }
    }
}

#[cfg(windows)]
pub fn known_folder(kf: KnownFolder) -> Option<PathBuf> {
    use win::guid;
    let id = match kf {
        KnownFolder::Profile => guid(0x5E6C858F, 0x0E22, 0x4760, [0x9A, 0xFE, 0xEA, 0x33, 0x17, 0xB6, 0x71, 0x73]),
        KnownFolder::Desktop => guid(0xB4BFCC3A, 0xDB2C, 0x424C, [0xB0, 0x29, 0x7F, 0xE9, 0x9A, 0x87, 0xC6, 0x41]),
        KnownFolder::Documents => guid(0xFDD39AD0, 0x238F, 0x46AF, [0xAD, 0xB4, 0x6C, 0x85, 0x48, 0x03, 0x69, 0xC7]),
        KnownFolder::Downloads => guid(0x374DE290, 0x123F, 0x4565, [0x91, 0x64, 0x39, 0xC4, 0x92, 0x5E, 0x46, 0x7B]),
        KnownFolder::Pictures => guid(0x33E28130, 0x4E1E, 0x4676, [0x83, 0x5A, 0x98, 0x39, 0x5C, 0x3B, 0xC3, 0xBB]),
        KnownFolder::Music => guid(0x4BD8D571, 0x6D19, 0x48D3, [0xBE, 0x97, 0x42, 0x22, 0x20, 0x08, 0x0E, 0x43]),
        KnownFolder::Videos => guid(0x18989B1D, 0x99B5, 0x455B, [0x84, 0x1C, 0xAB, 0x7C, 0x74, 0xE4, 0xDD, 0xFC]),
    };
    let mut out: *mut u16 = std::ptr::null_mut();
    // SAFETY: valid GUID pointer and out-pointer; the result is freed by take_pwstr.
    unsafe {
        let hr = win::SHGetKnownFolderPath(&id, 0, std::ptr::null_mut(), &mut out);
        if out.is_null() {
            return None;
        }
        let s = win::take_pwstr(out);
        (hr >= 0).then(|| PathBuf::from(s))
    }
}

#[cfg(not(windows))]
pub fn known_folder(kf: KnownFolder) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    Some(match kf {
        KnownFolder::Profile => home,
        KnownFolder::Desktop => home.join("Desktop"),
        KnownFolder::Documents => home.join("Documents"),
        KnownFolder::Downloads => home.join("Downloads"),
        KnownFolder::Pictures => home.join("Pictures"),
        KnownFolder::Music => home.join("Music"),
        KnownFolder::Videos => home.join("Videos"),
    })
}

/// Lists drive letters without touching the drives themselves (so a slow
/// network or empty optical drive can't stall the UI).
#[cfg(windows)]
pub fn drives() -> Vec<Drive> {
    // SAFETY: plain Win32 calls with valid NUL-terminated buffers.
    let mask = unsafe { win::GetLogicalDrives() };
    (0..26u8)
        .filter(|i| mask & (1 << i) != 0)
        .map(|i| {
            let name = format!("{}:", (b'A' + i) as char);
            let root = win::wide(std::ffi::OsStr::new(&format!("{name}\\")));
            let kind = unsafe { win::GetDriveTypeW(root.as_ptr()) } as u8;
            Drive { name, kind }
        })
        .collect()
}

#[cfg(not(windows))]
pub fn drives() -> Vec<Drive> {
    vec![Drive { name: "/".into(), kind: 3 }]
}

/// Opens a file with its default application. Blocking: call it from a
/// worker thread.
#[cfg(windows)]
pub fn shell_open(path: &Path) -> Result<(), String> {
    const COINIT_APARTMENTTHREADED: u32 = 0x2;
    const COINIT_DISABLE_OLE1DDE: u32 = 0x4;
    const SW_SHOWNORMAL: i32 = 1;
    let file = win::wide(path.as_os_str());
    let dir = path.parent().map(|d| win::wide(d.as_os_str()));
    // SAFETY: all string pointers are valid NUL-terminated wide strings that
    // outlive the call.
    let r = unsafe {
        win::CoInitializeEx(std::ptr::null_mut(), COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        win::ShellExecuteW(
            std::ptr::null_mut(),
            std::ptr::null(),
            file.as_ptr(),
            std::ptr::null(),
            dir.as_ref().map_or(std::ptr::null(), |d| d.as_ptr()),
            SW_SHOWNORMAL,
        )
    };
    if r > 32 {
        Ok(())
    } else {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        Err(match r {
            2 | 3 => format!("“{name}” could not be found."),
            5 => format!("Access to “{name}” was denied."),
            31 => format!("No app is set up to open “{name}”."),
            _ => format!("Couldn't open “{name}” (error {r})."),
        })
    }
}

#[cfg(not(windows))]
pub fn shell_open(path: &Path) -> Result<(), String> {
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    std::process::Command::new(opener).arg(path).spawn().map(|_| ()).map_err(|e| e.to_string())
}

/// Current working set of this process, in bytes.
#[cfg(windows)]
pub fn memory_usage() -> Option<u64> {
    let mut c = win::ProcessMemoryCounters {
        cb: std::mem::size_of::<win::ProcessMemoryCounters>() as u32,
        ..Default::default()
    };
    // SAFETY: `c` is a correctly sized, writable PROCESS_MEMORY_COUNTERS.
    let ok = unsafe { win::K32GetProcessMemoryInfo(win::GetCurrentProcess(), &mut c, c.cb) };
    (ok != 0).then_some(c.working_set_size as u64)
}

#[cfg(not(windows))]
pub fn memory_usage() -> Option<u64> {
    None
}

/// Name and space of a drive ("Ventoy", 29.2 GB, 5.47 GB free).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VolumeInfo {
    pub label: String,
    pub total: u64,
    pub free: u64,
}

/// Bit mask of present drive letters (bit 0 = A:). Cheap; used to notice USB
/// drives being plugged in or removed.
#[cfg(windows)]
pub fn drive_mask() -> u32 {
    // SAFETY: plain query.
    unsafe { win::GetLogicalDrives() }
}

#[cfg(not(windows))]
pub fn drive_mask() -> u32 {
    1
}

/// Volume label and space. May be slow for network/optical drives: call it
/// off the UI thread.
#[cfg(windows)]
pub fn volume_info(root: &Path) -> Option<VolumeInfo> {
    let root_w = win::wide(root.as_os_str());
    let mut name = [0u16; 261];
    let (mut free, mut total, mut total_free) = (0u64, 0u64, 0u64);
    // SAFETY: valid buffers sized as passed.
    unsafe {
        let has_label = win::GetVolumeInformationW(
            root_w.as_ptr(),
            name.as_mut_ptr(),
            name.len() as u32,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        ) != 0;
        let has_space = win::GetDiskFreeSpaceExW(root_w.as_ptr(), &mut free, &mut total, &mut total_free) != 0;
        if !has_label && !has_space {
            return None;
        }
        let len = name.iter().position(|&c| c == 0).unwrap_or(0);
        Some(VolumeInfo { label: String::from_utf16_lossy(&name[..len]), total, free })
    }
}

#[cfg(not(windows))]
pub fn volume_info(_root: &Path) -> Option<VolumeInfo> {
    None
}

/// Free space available on the volume holding `path`.
pub fn free_space(path: &Path) -> Option<u64> {
    #[cfg(windows)]
    {
        let w = win::wide(path.as_os_str());
        let (mut free, mut total, mut total_free) = (0u64, 0u64, 0u64);
        // SAFETY: valid string and out-pointers.
        let ok = unsafe { win::GetDiskFreeSpaceExW(w.as_ptr(), &mut free, &mut total, &mut total_free) } != 0;
        ok.then_some(free)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        None
    }
}

/// Stops Windows from showing "There is no disk in the drive" dialogs when an
/// empty card reader or DVD drive is queried.
pub fn quiet_drive_errors() {
    #[cfg(windows)]
    // SAFETY: process-wide flag; SEM_FAILCRITICALERRORS | SEM_NOOPENFILEERRORBOX.
    unsafe {
        win::SetErrorMode(0x0001 | 0x8000);
    }
}

/// True when (x, y) in screen pixels is on some monitor (a saved window
/// position from an unplugged screen isn't).
pub fn point_on_screen(x: i32, y: i32) -> bool {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::POINT;
        use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONULL, MonitorFromPoint};
        // SAFETY: plain call with a value argument.
        !unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONULL) }.is_invalid()
    }
    #[cfg(not(windows))]
    {
        let _ = (x, y);
        true
    }
}
