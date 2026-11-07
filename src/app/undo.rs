//! Ctrl+Z: undo the last file operations (copy, move, rename, Recycle Bin,
//! new folder / file), like Explorer. Permanent deletes can't be undone.

use std::cell::RefCell;
use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use finder_rs::core::fileops::{self, Op};
use finder_rs::core::{shell, trash};

use super::{App, with_app};

/// How to reverse one operation.
#[derive(Clone, Debug)]
pub(super) enum Undo {
    /// Copies / new items: send these to the Recycle Bin.
    Remove(Vec<PathBuf>),
    /// Moves / renames: (where it is now, where it was).
    MoveBack(Vec<(PathBuf, PathBuf)>),
    /// Recycled items: put back from the Recycle Bin (deleted at or after `since`).
    Restore { originals: Vec<PathBuf>, since: i64 },
}

/// The most recent operations, newest last.
const KEEP: usize = 50;

thread_local! {
    static STACK: RefCell<Vec<(String, Undo)>> = const { RefCell::new(Vec::new()) };
}

/// File operations still running (command chains wait for them).
pub(super) static RUNNING: AtomicUsize = AtomicUsize::new(0);

pub(super) fn push(desc: impl Into<String>, undo: Undo) {
    let empty = match &undo {
        Undo::Remove(v) => v.is_empty(),
        Undo::MoveBack(v) => v.is_empty(),
        Undo::Restore { originals, .. } => originals.is_empty(),
    };
    if empty {
        return;
    }
    STACK.with(|s| {
        let mut s = s.borrow_mut();
        s.push((desc.into(), undo));
        if s.len() > KEEP {
            s.remove(0);
        }
    });
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn names_in(dir: &Path) -> HashSet<OsString> {
    std::fs::read_dir(dir).map(|it| it.flatten().map(|e| e.file_name()).collect()).unwrap_or_default()
}

/// Taken before an operation runs (on its worker thread); `finish` turns it
/// into the undo step once it has run.
pub(super) enum Before {
    /// Folders that receive copies, with what was in them.
    Copies(Vec<(PathBuf, HashSet<OsString>)>),
    Moves(Vec<(PathBuf, PathBuf)>),
    Recycled(Vec<PathBuf>, i64),
    Nothing,
}

pub(super) fn before(op: &Op) -> Before {
    match op {
        Op::Copy { dest, .. } => Before::Copies(vec![(dest.clone(), names_in(dest))]),
        Op::Duplicate { sources } => {
            let mut dirs: Vec<PathBuf> = sources.iter().filter_map(|s| s.parent().map(Path::to_path_buf)).collect();
            dirs.sort();
            dirs.dedup();
            Before::Copies(dirs.into_iter().map(|d| {
                let n = names_in(&d);
                (d, n)
            }).collect())
        }
        Op::Move { sources, dest } => Before::Moves(
            sources.iter().filter_map(|s| Some((dest.join(s.file_name()?), s.clone()))).filter(|(to, from)| to != from).collect(),
        ),
        Op::Rename { source, new_name } => Before::Moves(vec![(source.with_file_name(new_name), source.clone())]),
        Op::Recycle { sources } => Before::Recycled(sources.clone(), now_secs() - 2),
        Op::Delete { .. } => Before::Nothing,
    }
}

/// What actually happened (also after a partly cancelled operation).
pub(super) fn finish(b: Before) -> Option<Undo> {
    match b {
        Before::Copies(dirs) => {
            let mut made = Vec::new();
            for (dir, old) in dirs {
                for name in names_in(&dir) {
                    if !old.contains(&name) {
                        made.push(dir.join(name));
                    }
                }
            }
            Some(Undo::Remove(made))
        }
        Before::Moves(pairs) => {
            let done: Vec<_> = pairs.into_iter().filter(|(now, was)| now.exists() && !was.exists()).collect();
            Some(Undo::MoveBack(done))
        }
        Before::Recycled(sources, since) => {
            let gone: Vec<_> = sources.into_iter().filter(|s| !s.exists()).collect();
            Some(Undo::Restore { originals: gone, since })
        }
        Before::Nothing => None,
    }
}

/// Runs the reverse operation (on a worker thread).
fn reverse(undo: &Undo) -> Result<(), String> {
    match undo {
        Undo::Remove(paths) => {
            let there: Vec<PathBuf> = paths.iter().filter(|p| p.exists()).cloned().collect();
            if there.is_empty() {
                return Err("Those items are already gone.".into());
            }
            fileops::run(&Op::Recycle { sources: there })
        }
        Undo::MoveBack(pairs) => {
            let mut problems = Vec::new();
            // Renames one by one; moves grouped by the folder they go back to.
            let mut moves: Vec<(PathBuf, Vec<PathBuf>)> = Vec::new();
            for (now, was) in pairs {
                if !now.exists() {
                    problems.push(format!("“{}” is gone", now.display()));
                    continue;
                }
                if now.parent() == was.parent() {
                    let name = was.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    if let Err(e) = fileops::run(&Op::Rename { source: now.clone(), new_name: name }) {
                        problems.push(e);
                    }
                } else if let Some(dir) = was.parent() {
                    match moves.iter_mut().find(|(d, _)| d == dir) {
                        Some((_, v)) => v.push(now.clone()),
                        None => moves.push((dir.to_path_buf(), vec![now.clone()])),
                    }
                }
            }
            for (dest, sources) in moves {
                if let Err(e) = fileops::run(&Op::Move { sources, dest }) {
                    problems.push(e);
                }
            }
            if problems.is_empty() { Ok(()) } else { Err(problems.join("; ")) }
        }
        Undo::Restore { originals, since } => {
            let bin = trash::list();
            let mut problems = Vec::new();
            for orig in originals {
                let newest = bin
                    .iter()
                    .filter(|t| t.deleted >= *since && t.original.as_os_str().eq_ignore_ascii_case(orig.as_os_str()))
                    .max_by_key(|t| t.deleted);
                match newest {
                    Some(t) => {
                        if let Err(e) = trash::put_back(t) {
                            problems.push(e);
                        }
                    }
                    None => problems.push(format!("“{}” isn't in the Recycle Bin", orig.display())),
                }
            }
            if problems.is_empty() { Ok(()) } else { Err(problems.join("; ")) }
        }
    }
}

impl App {
    pub(super) fn undo(&mut self) {
        let Some((desc, undo)) = STACK.with(|s| s.borrow_mut().pop()) else {
            return self.toast("Nothing to undo.");
        };
        self.note(format!("Undoing: {desc}…"));
        RUNNING.fetch_add(1, Ordering::SeqCst);
        std::thread::spawn(move || {
            shell::init_thread(true);
            let result = reverse(&undo);
            RUNNING.fetch_sub(1, Ordering::SeqCst);
            let _ = slint::invoke_from_event_loop(move || {
                with_app(|a| match result {
                    Ok(()) => a.toast(format!("Undone: {desc}")),
                    Err(e) => a.toast(format!("Couldn't fully undo “{desc}”: {e}")),
                });
            });
        });
    }
}
