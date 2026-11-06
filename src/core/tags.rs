//! Colour tags (Finder-style labels), stored with the file itself in an NTFS
//! alternate data stream, so they follow the file when it is moved or renamed
//! on the same volume. The file's "Date Modified" is preserved.

use std::fs;
use std::path::{Path, PathBuf};

pub const COLORS: usize = 7;
pub const NAMES: [&str; COLORS] = ["Red", "Orange", "Yellow", "Green", "Blue", "Purple", "Gray"];
const STREAM: &str = ":finder-rs.tags";

fn stream_path(path: &Path) -> std::ffi::OsString {
    let mut s = path.as_os_str().to_os_string();
    s.push(STREAM);
    s
}

/// Bit mask of the file's tags (bit 0 = red … bit 6 = gray). 0 if none.
pub fn read(path: &Path) -> u8 {
    fs::read(stream_path(path)).ok().and_then(|b| b.first().copied()).unwrap_or(0) & 0x7f
}

/// Replaces the file's tags with `mask`.
pub fn write(path: &Path, mask: u8) -> Result<(), String> {
    // Writing a stream bumps the file's modified time; put it back afterwards.
    let modified = fs::metadata(path).and_then(|m| m.modified()).ok();
    let result = if mask == 0 {
        match fs::remove_file(stream_path(path)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    } else {
        fs::write(stream_path(path), [mask & 0x7f])
    };
    if let Some(t) = modified
        && let Ok(f) = fs::OpenOptions::new().write(true).open(path)
    {
        let _ = f.set_modified(t);
    }
    result.map_err(|e| format!("Couldn't tag “{}”: {e}", path.display()))?;
    update_index(path, mask);
    Ok(())
}

// ---- index -------------------------------------------------------------------
// Tags live inside each file, so to show "everything tagged Work" without
// scanning the disk, the app also keeps a small list of what it has tagged:
// %APPDATA%\finder-rs\tags.txt, one "mask<TAB>path" line per item.

fn index_path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("finder-rs").join("tags.txt"))
}

fn load_index() -> Vec<(PathBuf, u8)> {
    let Some(text) = index_path().and_then(|p| fs::read_to_string(p).ok()) else { return Vec::new() };
    text.lines()
        .filter_map(|l| {
            let (m, p) = l.split_once('\t')?;
            Some((PathBuf::from(p), m.parse().ok()?))
        })
        .collect()
}

fn save_index(items: &[(PathBuf, u8)]) {
    let Some(p) = index_path() else { return };
    if let Some(dir) = p.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let text: String = items.iter().map(|(path, m)| format!("{m}\t{}\n", path.display())).collect();
    let _ = fs::write(p, text);
}

fn update_index(path: &Path, mask: u8) {
    let mut items: Vec<(PathBuf, u8)> = load_index().into_iter().filter(|(p, _)| p != path).collect();
    if mask != 0 {
        items.push((path.to_path_buf(), mask));
    }
    save_index(&items);
}

/// Keeps the index right when a tagged item is renamed or moved by the app.
pub fn moved(from: &Path, to: &Path) {
    let mut items = load_index();
    let mut changed = false;
    for (p, _) in items.iter_mut() {
        if p == from {
            *p = to.to_path_buf();
            changed = true;
        } else if let Ok(rest) = p.strip_prefix(from) {
            *p = to.join(rest);
            changed = true;
        }
    }
    if changed {
        save_index(&items);
    }
}

/// Everything known to carry tag `bit` (0 = red … 6 = gray). Items that no
/// longer exist or lost the tag are dropped from the index.
pub fn tagged(bit: u8) -> Vec<PathBuf> {
    let items = load_index();
    let mut keep = Vec::with_capacity(items.len());
    let mut out = Vec::new();
    for (p, _) in items {
        if !p.exists() {
            continue;
        }
        let mask = read(&p);
        if mask == 0 {
            continue;
        }
        if mask & (1 << bit) != 0 {
            out.push(p.clone());
        }
        keep.push((p, mask));
    }
    save_index(&keep);
    out
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_keeps_mtime() {
        let dir = std::env::temp_dir().join("finder-rs-tag-test");
        let _ = fs::create_dir_all(&dir);
        let f = dir.join("t.txt");
        fs::write(&f, "x").unwrap();
        let before = fs::metadata(&f).unwrap().modified().unwrap();
        write(&f, 0b101).unwrap();
        assert_eq!(read(&f), 0b101);
        assert_eq!(fs::metadata(&f).unwrap().modified().unwrap(), before);
        write(&f, 0).unwrap();
        assert_eq!(read(&f), 0);
        let _ = fs::remove_dir_all(&dir);
    }
}
