//! Where a tab is: the "This PC" drive list, or a folder.

use std::path::{Component, Path, PathBuf};

use super::fs_list::Entry;
use super::sys;

pub const COMPUTER_NAME: &str = "This PC";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Location {
    Computer,
    Dir(PathBuf),
    /// Everything tagged with colour `n` (0 = red … 6 = gray), from anywhere.
    Tag(u8),
    /// Recently opened files.
    Recent,
    /// The Recycle Bin.
    Trash,
    /// Search results below a folder.
    Search { root: PathBuf, query: String },
}

impl Location {
    /// Short name for tab titles and the window title.
    pub fn title(&self) -> String {
        match self {
            Location::Computer => COMPUTER_NAME.into(),
            Location::Tag(t) => super::tags::NAMES[(*t as usize).min(6)].into(),
            Location::Recent => "Recents".into(),
            Location::Trash => "Recycle Bin".into(),
            Location::Search { query, .. } => format!("Search: {query}"),
            Location::Dir(p) => match p.file_name() {
                Some(n) => n.to_string_lossy().into_owned(),
                None => p.to_string_lossy().trim_end_matches(['\\', '/']).to_string(),
            },
        }
    }

    /// Full path for the editable path field.
    pub fn display(&self) -> String {
        match self {
            Location::Computer => COMPUTER_NAME.into(),
            Location::Tag(_) | Location::Recent | Location::Trash => self.title(),
            Location::Search { root, query } => format!("{query} in {}", root.display()),
            Location::Dir(p) => p.to_string_lossy().into_owned(),
        }
    }

    /// Going up from a drive root leads to "This PC".
    pub fn parent(&self) -> Option<Location> {
        match self {
            Location::Computer | Location::Tag(_) | Location::Recent | Location::Trash => None,
            Location::Search { root, .. } => Some(Location::Dir(root.clone())),
            Location::Dir(p) => Some(match p.parent() {
                Some(pp) if !pp.as_os_str().is_empty() => Location::Dir(pp.to_path_buf()),
                _ => Location::Computer,
            }),
        }
    }

    /// The name this location has inside its parent's listing, so that going
    /// up can re-select the folder you came from.
    pub fn name_in_parent(&self) -> Option<String> {
        match self {
            Location::Computer | Location::Tag(_) | Location::Recent | Location::Trash | Location::Search { .. } => None,
            Location::Dir(_) => Some(self.title()),
        }
    }

    /// Path of a child entry.
    pub fn child(&self, name: &str) -> PathBuf {
        match self {
            Location::Computer => drive_root(name),
            Location::Dir(p) => p.join(name),
            Location::Tag(_) | Location::Recent | Location::Trash | Location::Search { .. } => PathBuf::from(name),
        }
    }

    /// Full path of a listed entry (tag views list items from many folders).
    pub fn path_of(&self, e: &Entry) -> PathBuf {
        match &e.path {
            Some(p) => p.clone(),
            None => self.child(&e.name),
        }
    }

    /// Breadcrumb segments, starting with "This PC".
    pub fn crumbs(&self) -> Vec<(String, Location)> {
        if matches!(self, Location::Tag(_) | Location::Recent | Location::Trash | Location::Search { .. }) {
            return vec![(self.title(), self.clone())];
        }
        let mut out = vec![(COMPUTER_NAME.to_string(), Location::Computer)];
        let Location::Dir(p) = self else { return out };
        let mut acc = PathBuf::new();
        for c in p.components() {
            match c {
                Component::Prefix(px) => {
                    let label = px.as_os_str().to_string_lossy().into_owned();
                    acc = drive_root(&label);
                    out.push((label, Location::Dir(acc.clone())));
                }
                Component::RootDir => {
                    if acc.as_os_str().is_empty() {
                        acc = PathBuf::from("/");
                        out.push(("/".into(), Location::Dir(acc.clone())));
                    }
                }
                Component::Normal(n) => {
                    acc.push(n);
                    out.push((n.to_string_lossy().into_owned(), Location::Dir(acc.clone())));
                }
                Component::CurDir | Component::ParentDir => {}
            }
        }
        out
    }
}

fn drive_root(name: &str) -> PathBuf {
    let n = name.trim_end_matches(['\\', '/']);
    PathBuf::from(format!("{n}\\"))
}

/// Interprets what the user typed in the path field.
/// Accepts quotes, `~`, `%VAR%`, bare drive letters ("d:") and "This PC".
pub fn parse_user_input(input: &str) -> Option<Location> {
    let s = input.trim().trim_matches('"').trim();
    if s.is_empty() {
        return None;
    }
    if s.eq_ignore_ascii_case(COMPUTER_NAME) || s.eq_ignore_ascii_case("computer") {
        return Some(Location::Computer);
    }
    let mut s = expand_env(s);
    if let Some(rest) = s.strip_prefix('~') {
        s = format!("{}{}", sys::home().display(), rest);
    }
    if s.len() == 2 && s.ends_with(':') {
        s.push('\\');
    }
    let p = PathBuf::from(&s);
    p.is_absolute().then_some(Location::Dir(normalize(&p)))
}

/// Removes `.` and `..` without touching the disk (canonicalize would turn the
/// path into a `\\?\` verbatim path, which is ugly to show).
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if out.parent().is_some() {
                    out.pop();
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn expand_env(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find('%') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('%') else { break };
        let var = &after[..end];
        match std::env::var(var) {
            Ok(v) if !var.is_empty() => {
                out.push_str(&rest[..start]);
                out.push_str(&v);
            }
            _ => out.push_str(&rest[..start + end + 2]),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn crumbs_and_parent() {
        let l = Location::Dir(PathBuf::from(r"C:\Users\me"));
        let c: Vec<String> = l.crumbs().into_iter().map(|c| c.0).collect();
        assert_eq!(c, ["This PC", "C:", "Users", "me"]);
        let root = Location::Dir(PathBuf::from(r"C:\"));
        assert_eq!(root.parent(), Some(Location::Computer));
        assert_eq!(root.title(), "C:");
        assert_eq!(root.name_in_parent().as_deref(), Some("C:"));
    }

    #[test]
    fn user_input() {
        assert_eq!(parse_user_input("d:"), Some(Location::Dir(PathBuf::from(r"d:\"))));
        assert_eq!(parse_user_input(r#""C:\a\..\b""#), Some(Location::Dir(PathBuf::from(r"C:\b"))));
        assert_eq!(parse_user_input("this pc"), Some(Location::Computer));
        assert_eq!(parse_user_input("relative"), None);
    }
}
