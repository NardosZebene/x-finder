//! The Recycle Bin: list, put back, delete immediately, empty.
//!
//! Windows keeps each deleted item as a pair in `<drive>\$Recycle.Bin\<SID>\`:
//! `$R…` is the item itself and `$I…` records where it came from and when it
//! was deleted. Putting back moves `$R…` to the original path.

use std::fs;
use std::path::{Path, PathBuf};

use super::sys;

#[derive(Clone, Debug)]
pub struct TrashItem {
    /// Where it was deleted from.
    pub original: PathBuf,
    /// The `$R…` item.
    pub data: PathBuf,
    /// The `$I…` record.
    pub info: PathBuf,
    /// Unix seconds.
    pub deleted: i64,
}

/// Parses a `$I` record: version 1 (fixed 520-byte path) or 2 (length-prefixed).
fn parse_info(b: &[u8]) -> Option<(i64, PathBuf)> {
    let version = u64::from_le_bytes(b.get(0..8)?.try_into().ok()?);
    let filetime = u64::from_le_bytes(b.get(16..24)?.try_into().ok()?);
    let units: Vec<u16> = match version {
        1 => b.get(24..24 + 520)?.as_chunks::<2>().0.iter().map(|&c| u16::from_le_bytes(c)).collect(),
        2 => {
            let n = u32::from_le_bytes(b.get(24..28)?.try_into().ok()?) as usize;
            b.get(28..28 + n * 2)?.as_chunks::<2>().0.iter().map(|&c| u16::from_le_bytes(c)).collect()
        }
        _ => return None,
    };
    let end = units.iter().position(|&c| c == 0).unwrap_or(units.len());
    let path = PathBuf::from(String::from_utf16_lossy(&units[..end]));
    // FILETIME (100 ns since 1601) -> Unix seconds.
    let secs = (filetime / 10_000_000) as i64 - 11_644_473_600;
    Some((secs, path))
}

/// Everything in the current user's Recycle Bins on all local/USB drives.
pub fn list() -> Vec<TrashItem> {
    let mut out = Vec::new();
    for d in sys::drives().into_iter().filter(|d| d.kind == 2 || d.kind == 3) {
        let bin = PathBuf::from(format!("{}\\$Recycle.Bin", d.name));
        let Ok(users) = fs::read_dir(&bin) else { continue };
        for user in users.flatten() {
            // Other users' bins aren't readable; that's fine.
            let Ok(items) = fs::read_dir(user.path()) else { continue };
            for it in items.flatten() {
                let name = it.file_name().to_string_lossy().into_owned();
                let Some(rest) = name.strip_prefix("$I") else { continue };
                let data = user.path().join(format!("$R{rest}"));
                if !data.exists() {
                    continue;
                }
                let Some((deleted, original)) = fs::read(it.path()).ok().and_then(|b| parse_info(&b)) else { continue };
                out.push(TrashItem { original, data, info: it.path(), deleted });
            }
        }
    }
    out
}

/// Moves the item back to where it was deleted from.
pub fn put_back(item: &TrashItem) -> Result<(), String> {
    if item.original.exists() {
        return Err(format!("“{}” already exists; rename or move it first.", item.original.display()));
    }
    if let Some(parent) = item.original.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::rename(&item.data, &item.original).map_err(|e| format!("Couldn't put back: {e}"))?;
    let _ = fs::remove_file(&item.info);
    Ok(())
}

/// Deletes the item for good.
pub fn delete_now(item: &TrashItem) -> Result<(), String> {
    let r = if item.data.is_dir() { fs::remove_dir_all(&item.data) } else { fs::remove_file(&item.data) };
    r.map_err(|e| format!("Couldn't delete: {e}"))?;
    let _ = fs::remove_file(&item.info);
    Ok(())
}

/// Empties the Recycle Bin (Windows asks for confirmation).
pub fn empty() -> Result<(), String> {
    #[cfg(windows)]
    // SAFETY: plain shell call; flags 0 = show confirmation and progress.
    unsafe {
        windows::Win32::UI::Shell::SHEmptyRecycleBinW(None, windows::core::PCWSTR::null(), 0)
            .map_err(|e| e.message().to_string())
    }
    #[cfg(not(windows))]
    Err("Not available on this platform.".into())
}

pub fn is_trash_path(p: &Path) -> bool {
    p.components().any(|c| c.as_os_str().eq_ignore_ascii_case("$Recycle.Bin"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_v2_records() {
        let path: Vec<u16> = "C:\\Users\\me\\a.txt\0".encode_utf16().collect();
        let mut b = Vec::new();
        b.extend_from_slice(&2u64.to_le_bytes());
        b.extend_from_slice(&123u64.to_le_bytes());
        // 2026-01-01T00:00:00Z as FILETIME
        let ft = (1_767_225_600u64 + 11_644_473_600) * 10_000_000;
        b.extend_from_slice(&ft.to_le_bytes());
        b.extend_from_slice(&(path.len() as u32).to_le_bytes());
        for u in &path {
            b.extend_from_slice(&u.to_le_bytes());
        }
        let (secs, p) = parse_info(&b).unwrap();
        assert_eq!(secs, 1_767_225_600);
        assert_eq!(p, PathBuf::from(r"C:\Users\me\a.txt"));
    }
}
