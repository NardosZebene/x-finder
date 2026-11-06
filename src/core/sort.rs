//! Sorting: folders first, natural ("file2" < "file10"), case-insensitive.

use std::cmp::Ordering;

use super::format;
use super::fs_list::Entry;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Modified,
    Size,
    Kind,
    Created,
}

impl SortKey {
    pub fn from_index(i: i32) -> Self {
        match i {
            1 => SortKey::Modified,
            2 => SortKey::Size,
            3 => SortKey::Kind,
            4 => SortKey::Created,
            _ => SortKey::Name,
        }
    }
    pub fn index(self) -> i32 {
        match self {
            SortKey::Name => 0,
            SortKey::Modified => 1,
            SortKey::Size => 2,
            SortKey::Kind => 3,
            SortKey::Created => 4,
        }
    }
    /// Direction used the first time a column is clicked (newest / largest first).
    pub fn default_ascending(self) -> bool {
        matches!(self, SortKey::Name | SortKey::Kind)
    }
}

/// Natural comparison of two (already lower-cased) names.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        let (ca, cb) = (a[i], b[j]);
        if ca.is_ascii_digit() && cb.is_ascii_digit() {
            let (si, sj) = (i, j);
            while i < a.len() && a[i].is_ascii_digit() {
                i += 1;
            }
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let na = trim_zeros(&a[si..i]);
            let nb = trim_zeros(&b[sj..j]);
            let ord = na.len().cmp(&nb.len()).then_with(|| na.cmp(nb));
            if ord != Ordering::Equal {
                return ord;
            }
            let ord = (i - si).cmp(&(j - sj));
            if ord != Ordering::Equal {
                return ord;
            }
        } else {
            if ca != cb {
                return ca.cmp(&cb);
            }
            i += 1;
            j += 1;
        }
    }
    (a.len() - i).cmp(&(b.len() - j))
}

fn trim_zeros(d: &[u8]) -> &[u8] {
    let n = d.iter().take_while(|&&c| c == b'0').count();
    &d[n.min(d.len().saturating_sub(1))..]
}

/// Compares two entries. `kind` compares the kind labels; it is a parameter so
/// bulk sorts can use precomputed labels instead of rebuilding them per comparison.
/// Folder-sizes mode: sorting by size mixes folders and files, so the
/// biggest thing in the folder is on top whatever it is.
static SIZE_MIXES_FOLDERS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_size_mixes_folders(on: bool) {
    SIZE_MIXES_FOLDERS.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// "Folders on Top" (the default); off, folders sort in among the files.
static FOLDERS_FIRST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn set_folders_first(on: bool) {
    FOLDERS_FIRST.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn folders_first() -> bool {
    FOLDERS_FIRST.load(std::sync::atomic::Ordering::Relaxed)
}

fn compare_by(
    a: &Entry,
    b: &Entry,
    key: SortKey,
    ascending: bool,
    kind: impl FnOnce() -> Ordering,
) -> Ordering {
    // Folders come first, whatever the direction (not when ranking by size
    // in folder-sizes mode).
    let mixed = !folders_first() || key == SortKey::Size && SIZE_MIXES_FOLDERS.load(std::sync::atomic::Ordering::Relaxed);
    match (a.is_dir, b.is_dir) {
        _ if mixed => {}
        (true, false) => return Ordering::Less,
        (false, true) => return Ordering::Greater,
        _ => {}
    }
    let by_name = || natural_cmp(&a.key, &b.key);
    let ord = match key {
        SortKey::Name => by_name(),
        SortKey::Modified => a.modified.cmp(&b.modified).then_with(by_name),
        SortKey::Size => a.size.cmp(&b.size).then_with(by_name),
        SortKey::Created => a.created.cmp(&b.created).then_with(by_name),
        SortKey::Kind => kind().then_with(by_name),
    };
    if ascending { ord } else { ord.reverse() }
}

pub fn compare(a: &Entry, b: &Entry, key: SortKey, ascending: bool) -> Ordering {
    compare_by(a, b, key, ascending, || format::kind_label(a).cmp(&format::kind_label(b)))
}

/// Sorts a list of indices into `entries`, in parallel.
pub fn sort_indices(idx: &mut [u32], entries: &[Entry], key: SortKey, ascending: bool) {
    use rayon::prelude::*;
    if key == SortKey::Kind {
        let labels: Vec<_> = entries.par_iter().map(format::kind_label).collect();
        idx.par_sort_unstable_by(|&x, &y| {
            let (x, y) = (x as usize, y as usize);
            compare_by(&entries[x], &entries[y], key, ascending, || labels[x].cmp(&labels[y]))
        });
    } else {
        idx.par_sort_unstable_by(|&x, &y| compare(&entries[x as usize], &entries[y as usize], key, ascending));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        let mut v = vec!["file10", "file2", "file1", "file02", "a", "file"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["a", "file", "file1", "file2", "file02", "file10"]);
    }

    #[test]
    fn folders_first() {
        let f = Entry::new("b".into(), false, false, 1, 0);
        let d = Entry::new("z".into(), true, false, 0, 0);
        assert_eq!(compare(&d, &f, SortKey::Name, false), Ordering::Less);
        assert_eq!(compare(&d, &f, SortKey::Size, true), Ordering::Less);
    }
}
