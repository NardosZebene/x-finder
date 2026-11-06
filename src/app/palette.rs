//! Command palette (Ctrl+Shift+P) and quick jump (Ctrl+G) for keyboard users.
//!
//! One box, three kinds of entries:
//! - typed commands:  `mkdir a, b`  `touch x.txt, y.html`  `cd docs`
//!   `select *.pdf`  `tag work`  `newtab D:\`  `term`
//!   `!ipconfig` (output shows in the palette; `!!cmd` uses a terminal)
//!   `save ip = !ipconfig` then just `ip` (saved commands)
//! - calculations: `2+3*4`, `15% of 80`, `1.5gb in mb`
//! - every app action by name ("copy path", "view columns", …), fuzzy;
//! - folders you visited, favourites and drives (quick jump).

use std::path::{Path, PathBuf};

use finder_rs::core::{calc, fileops};
use finder_rs::core::location::{self, Location};
use finder_rs::core::query::Query;
use finder_rs::core::sys;
use slint::{ComponentHandle, ModelRc, VecModel};

use finder_rs::core::sort::SortKey;

use super::{App, ViewMode};
use crate::commands::Command;
use crate::{Bridge, PaletteItem};

/// Folders remembered for quick jump.
const RECENT_MAX: usize = 60;
/// Rows shown at once.
const MAX_ROWS: usize = 9;

#[derive(Clone)]
pub(super) enum PAction {
    Cmd(Command),
    /// App features that aren't plain commands (settings, layouts…).
    Special(&'static str),
    /// A typed command line, run as is.
    Line(String),
    Goto(PathBuf),
    /// Open the tab group at this index.
    Group(usize),
    /// Puts this text in the box (examples).
    Fill(String),
    /// Copies this text (a calculation's answer).
    Copy(String),
}

/// A `!command` running with its output shown in the palette.
pub(super) struct CmdRun {
    serial: u64,
    pid: u32,
    bytes: Vec<u8>,
}

/// Output kept for one command (the start is dropped beyond this).
const OUTPUT_MAX: usize = 256 << 10;

/// Typed commands: (verb, aliases, example, what it does).
const VERBS: &[(&str, &[&str], &str, &str)] = &[
    ("mkdir", &["md", "folder"], "mkdir folder1, folder2", "Create folders here (comma-separated)"),
    ("touch", &["new", "file"], "touch notes.txt, index.html", "Create empty files here"),
    ("cd", &["go", "goto", "jump"], "cd Downloads", "Go to a folder (path or part of a name)"),
    ("select", &["sel"], "select *.pdf", "Select by pattern or search (select size:>5mb)"),
    ("rename", &["ren"], "rename *.jpg trip-{n}.jpg", "Batch rename, with a preview (rename \"old\" \"new\")"),
    ("copy", &["cp"], "copy to D:\\backup", "Copy the selection to a folder"),
    ("move", &["mv"], "move to D:\\backup", "Move the selection to a folder"),
    ("zip", &[], "zip archive.zip", "Zip the selection into this folder"),
    ("tag", &[], "tag work", "Tag the selection (tag red · tag none)"),
    ("newtab", &["tab"], "newtab D:\\", "Open a new tab here or at a path"),
    ("term", &["terminal", "cmd", "shell"], "term", "Open a terminal in this folder"),
    ("!", &[], "!ipconfig", "Run a command here, output below (!! = in a terminal)"),
    ("save", &["alias"], "save ip = !ipconfig", "Save a command as a short name ({dir} {file} {files} {name})"),
    ("unsave", &["forget", "unalias"], "unsave ip", "Delete a saved command"),
    ("sort", &[], "sort kind all", "Sort by name, date, size, kind or created (all = subfolders too)"),
    ("view", &[], "view icons all", "Show as list, icons, columns or gallery (all = subfolders too)"),
];

/// What "sort …" / "view …" asks for.
#[derive(Debug, PartialEq)]
enum FolderRule {
    Set { sort: Option<(SortKey, bool)>, view: Option<ViewMode>, all: bool },
    Reset { all: bool },
}

fn parse_folder_rule(verb: &str, rest: &str) -> Result<FolderRule, &'static str> {
    let words: Vec<String> = rest.split_whitespace().map(|w| w.to_lowercase()).collect();
    let all = words.iter().any(|w| matches!(w.as_str(), "all" | "subfolders" | "-r" | "/s"));
    if words.iter().any(|w| w == "reset") {
        return Ok(FolderRule::Reset { all });
    }
    if verb == "sort" {
        let key = words.iter().find_map(|w| match w.as_str() {
            "name" => Some(SortKey::Name),
            "date" | "modified" | "changed" => Some(SortKey::Modified),
            "size" => Some(SortKey::Size),
            "kind" | "type" => Some(SortKey::Kind),
            "created" => Some(SortKey::Created),
            _ => None,
        });
        let Some(key) = key else { return Err("Type: sort name | date | size | kind | created  [up | down]  [all]") };
        let ascending = words
            .iter()
            .find_map(|w| match w.as_str() {
                "up" | "asc" | "a-z" | "oldest" | "smallest" => Some(true),
                "down" | "desc" | "z-a" | "newest" | "biggest" | "largest" => Some(false),
                _ => None,
            })
            .unwrap_or(key.default_ascending());
        Ok(FolderRule::Set { sort: Some((key, ascending)), view: None, all })
    } else {
        let view = words.iter().find_map(|w| match w.as_str() {
            "list" => Some(ViewMode::List),
            "icons" | "icon" | "grid" => Some(ViewMode::Icons),
            "columns" | "column" => Some(ViewMode::Columns),
            "gallery" => Some(ViewMode::Gallery),
            _ => None,
        });
        let Some(view) = view else { return Err("Type: view list | icons | columns | gallery  [all]") };
        Ok(FolderRule::Set { sort: None, view: Some(view), all })
    }
}

fn sort_label(key: SortKey, ascending: bool) -> String {
    let (name, up, down) = match key {
        SortKey::Name => ("name", "A→Z", "Z→A"),
        SortKey::Modified => ("date modified", "oldest first", "newest first"),
        SortKey::Size => ("size", "smallest first", "biggest first"),
        SortKey::Kind => ("kind", "A→Z", "Z→A"),
        SortKey::Created => ("date created", "oldest first", "newest first"),
    };
    format!("by {name}, {}", if ascending { up } else { down })
}

fn view_label(v: ViewMode) -> &'static str {
    match v {
        ViewMode::List => "as a list",
        ViewMode::Icons => "as icons",
        ViewMode::Columns => "in columns",
        ViewMode::Gallery => "as a gallery",
    }
}

/// Actions listed by name, with their shortcut.
const ACTIONS: &[(Command, &str)] = &[
    (Command::NewFolder, "Ctrl+Shift+N"),
    (Command::NewTab, "Ctrl+T"),
    (Command::CloseTab, "Ctrl+W"),
    (Command::CopyPath, "Ctrl+Shift+C"),
    (Command::Copy, "Ctrl+C"),
    (Command::Cut, "Ctrl+X"),
    (Command::Paste, "Ctrl+V"),
    (Command::Duplicate, "Ctrl+D"),
    (Command::Rename, "F2"),
    (Command::Recycle, "Del"),
    (Command::DeleteForever, "Shift+Del"),
    (Command::Properties, "Alt+Enter"),
    (Command::SelectAll, "Ctrl+A"),
    (Command::ToggleHidden, "Ctrl+H"),
    (Command::QuickLook, "Space"),
    (Command::TogglePreviewPane, "Ctrl+P"),
    (Command::ViewList, "Ctrl+1"),
    (Command::ViewIcons, "Ctrl+2"),
    (Command::ViewColumns, "Ctrl+3"),
    (Command::ViewGallery, "Ctrl+4"),
    (Command::Refresh, "F5"),
    (Command::Back, "Alt+Left"),
    (Command::Forward, "Alt+Right"),
    (Command::Up, "Alt+Up"),
    (Command::ShowEnclosing, "Ctrl+R"),
    (Command::Help, "F1"),
];

const SPECIALS: &[(&str, &str, &str)] = &[
    ("settings", "Settings", "Ctrl+,"),
    ("sizes", "Toggle Folder Sizes", ""),
    ("layout:0", "Layout: Single Pane", ""),
    ("layout:1", "Layout: Two Side by Side", ""),
    ("layout:2", "Layout: Two Stacked", ""),
    ("layout:3", "Layout: One + Two", ""),
    ("layout:4", "Layout: Grid of Four", ""),
    ("save-group", "Save Tabs as Group…", ""),
    ("explorer", "Show in Windows Explorer", ""),
    ("terminal", "Open in Terminal", ""),
];

/// Fuzzy score: substring (earlier and shorter is better), else letters in
/// order ("cpp" → "Copy Path"). None = no match.
pub(super) fn score(hay: &str, needle: &str) -> Option<i32> {
    let h = hay.to_lowercase();
    let n = needle.to_lowercase();
    if n.is_empty() {
        return Some(0);
    }
    if let Some(p) = h.find(&n) {
        let word_start = p == 0 || !h[..p].ends_with(char::is_alphanumeric);
        return Some(1000 - p as i32 - h.len() as i32 / 4 + if word_start { 150 } else { 0 });
    }
    let mut chars = h.chars();
    let mut gaps = 0;
    for c in n.chars().filter(|c| !c.is_whitespace()) {
        loop {
            match chars.next() {
                Some(x) if x == c => break,
                Some(_) => gaps += 1,
                None => return None,
            }
        }
    }
    Some(500 - gaps * 4 - h.len() as i32 / 4)
}

/// `a, b` → [a, b]; without commas, spaces separate (use quotes for names
/// with spaces: `touch "my notes.txt" todo.md`).
pub(super) fn split_names(rest: &str) -> Vec<String> {
    let rest = rest.trim();
    if rest.contains(',') {
        return rest.split(',').map(|s| s.trim().trim_matches('"').trim().to_string()).filter(|s| !s.is_empty()).collect();
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in rest.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// `*.pdf`, `report-??.docx` (case-insensitive).
fn wildcard(pattern: &str, name: &str) -> bool {
    fn go(p: &[char], n: &[char]) -> bool {
        match (p.first(), n.first()) {
            (None, None) => true,
            (Some('*'), _) => go(&p[1..], n) || (!n.is_empty() && go(p, &n[1..])),
            (Some('?'), Some(_)) => go(&p[1..], &n[1..]),
            (Some(a), Some(b)) if a == b => go(&p[1..], &n[1..]),
            _ => false,
        }
    }
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let n: Vec<char> = name.to_lowercase().chars().collect();
    go(&p, &n)
}

/// Splits "mkdir a, b" into the canonical verb and the rest.
fn split_verb(text: &str) -> Option<(&'static str, &str)> {
    let t = text.trim_start();
    if let Some(rest) = t.strip_prefix('!') {
        return Some(("!", rest));
    }
    let (word, rest) = t.split_once(char::is_whitespace).unwrap_or((t, ""));
    let w = word.to_lowercase();
    let verb = VERBS.iter().find(|(v, aliases, ..)| *v == w || aliases.contains(&w.as_str())).map(|(v, ..)| *v)?;
    // "copy path", "rename" alone… are app actions, not typed commands.
    let r = rest.trim_start().to_lowercase();
    match verb {
        "copy" | "move" if !(r == "to" || r.starts_with("to ")) => None,
        "rename" if r.is_empty() => None,
        // Alone they find the app's "View as…" / sort actions instead.
        "sort" | "view" if r.is_empty() => None,
        _ => Some((verb, rest)),
    }
}

/// Words with "quotes" kept together (rename arguments).
fn args(rest: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut had_quotes = false;
    for c in rest.chars() {
        match c {
            '"' | '\u{201C}' | '\u{201D}' => {
                quoted = !quoted;
                had_quotes = true;
            }
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() || had_quotes {
                    out.push(std::mem::take(&mut cur));
                }
                had_quotes = false;
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() || had_quotes {
        out.push(cur);
    }
    out
}

/// New name from a template: {n} {nn} {nnn} (numbering, zero-padded),
/// {name} (old name without extension), {ext}, {date} (modified,
/// yyyy-mm-dd). Without an extension in the template the old one is kept.
pub(super) fn expand_template(template: &str, old: &str, n: usize, modified: i64) -> String {
    let (stem, ext) = match old.rfind('.') {
        Some(i) if i > 0 => (&old[..i], &old[i + 1..]),
        _ => (old, ""),
    };
    let mut out = String::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('}') else {
            out.push_str(&rest[open..]);
            rest = "";
            break;
        };
        let token = &rest[open + 1..open + close];
        match token {
            t if !t.is_empty() && t.chars().all(|c| c == 'n') => out.push_str(&format!("{:0width$}", n, width = t.len())),
            "name" => out.push_str(stem),
            "ext" => out.push_str(ext),
            "date" => out.push_str(&chrono::DateTime::from_timestamp(modified, 0).map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default()),
            other => {
                out.push('{');
                out.push_str(other);
                out.push('}');
            }
        }
        rest = &rest[open + close + 1..];
    }
    out.push_str(rest);
    if !ext.is_empty() && !template.contains('.') && !template.contains("{ext}") {
        out.push('.');
        out.push_str(ext);
    }
    out
}

/// Case-insensitive replace of every `find` in `name`.
fn replace_ci(name: &str, find: &str, with: &str) -> String {
    if find.is_empty() {
        return name.to_string();
    }
    let lower = name.to_lowercase();
    let f = find.to_lowercase();
    let mut out = String::new();
    let mut pos = 0;
    while let Some(i) = lower[pos..].find(&f) {
        let at = pos + i;
        // Same byte length in lower case for ASCII; otherwise fall back.
        if !name.is_char_boundary(at) || !name.is_char_boundary(at + f.len()) {
            return name.replace(find, with);
        }
        out.push_str(&name[pos..at]);
        out.push_str(with);
        pos = at + f.len();
    }
    out.push_str(&name[pos..]);
    out
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

impl App {
    /// Opens the palette, optionally with text already typed ("cd " for Ctrl+G).
    pub(super) fn palette_show(&mut self, prefill: &str) {
        let Some(ui) = self.ui() else { return };
        self.menu_close();
        let b = ui.global::<Bridge>();
        b.set_palette_text(prefill.into());
        b.set_palette_open(true);
        b.set_palette_fill_serial(b.get_palette_fill_serial() + 1);
        self.palette_changed(prefill);
    }

    pub(super) fn palette_close(&mut self) {
        self.palette_actions.clear();
        self.stop_captured();
        if let Some(ui) = self.ui() {
            let b = ui.global::<Bridge>();
            b.set_palette_output("".into());
            b.set_palette_running(false);
            b.set_palette_output_status("".into());
            b.set_palette_open(false);
            ui.invoke_focus_list();
        }
    }

    /// Remembers folders for quick jump (most recent first).
    pub(super) fn remember_folder(&mut self, p: &Path) {
        let s = p.to_string_lossy().into_owned();
        self.settings.recent_folders.retain(|x| !x.eq_ignore_ascii_case(&s));
        self.settings.recent_folders.insert(0, s);
        self.settings.recent_folders.truncate(RECENT_MAX);
    }

    /// Candidate folders for quick jump: visited, favourites, drives.
    fn jump_folders(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = self.settings.recent_folders.iter().map(PathBuf::from).collect();
        for p in &self.places {
            if let Some(Location::Dir(d)) = &p.loc
                && !out.iter().any(|x| x == d)
            {
                out.push(d.clone());
            }
        }
        out
    }

    fn folder_matches(&self, needle: &str, limit: usize) -> Vec<(i32, PathBuf)> {
        let mut found: Vec<(i32, PathBuf)> = self
            .jump_folders()
            .into_iter()
            .enumerate()
            .filter_map(|(rank, p)| {
                let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.to_string_lossy().into_owned());
                let by_name = score(&name, needle).map(|s| s + 100);
                let by_path = score(&p.to_string_lossy(), needle);
                // Recently used folders rank a little higher.
                by_name.max(by_path).map(|s| (s - rank as i32, p))
            })
            .collect();
        found.sort_by(|a, b| b.0.cmp(&a.0));
        found.truncate(limit);
        found
    }

    pub(super) fn palette_changed(&mut self, text: &str) {
        let here = self.current_dir();
        let here_name = here
            .as_ref()
            .map(|d| self.loc_title(&Location::Dir(d.clone())))
            .unwrap_or_else(|| "this folder".into());
        let mut rows: Vec<(PaletteItem, PAction)> = Vec::new();
        let item = |title: String, detail: String, shortcut: &str, icon: &str| PaletteItem {
            title: title.into(),
            detail: detail.into(),
            shortcut: shortcut.into(),
            icon: icon.into(),
        };
        let t = text.trim();

        if let Some(answer) = calc::eval(t) {
            rows.push((item(format!("= {answer}"), "Enter copies the answer".into(), "Enter", "more"), PAction::Copy(answer)));
        }
        if t.is_empty() {
            for c in self.settings.saved_commands.iter().take(4) {
                rows.push((item(c.name.clone(), c.line.clone(), "", "terminal"), PAction::Line(c.line.clone())));
            }
            for (_, _, example, what) in VERBS {
                let fill = if example.contains(' ') { format!("{} ", example.split(' ').next().unwrap()) } else { example.to_string() };
                rows.push((item(example.to_string(), what.to_string(), "", "terminal"), PAction::Fill(fill)));
            }
            for (_, p) in self.folder_matches("", 4) {
                rows.push((item(self.loc_title(&Location::Dir(p.clone())), p.display().to_string(), "", "folder"), PAction::Goto(p)));
            }
        } else if let Some(line) = self.saved_line(t) {
            // A saved command's name (plus anything typed after it).
            let name = t.split_whitespace().next().unwrap_or_default().to_string();
            rows.push((item(name, format!("Runs: {line}"), "Enter", "terminal"), PAction::Line(line)));
        } else if let Some((verb, rest)) = split_verb(t) {
            let names = split_names(rest);
            let (title, detail) = match verb {
                "mkdir" | "touch" => {
                    let what = if verb == "mkdir" { ("folder", "folders") } else { ("file", "files") };
                    if names.is_empty() {
                        (t.to_string(), format!("Type {} separated by commas", what.1))
                    } else {
                        (t.to_string(), format!("Create {} in {here_name}: {}", plural(names.len(), what.0, what.1), names.join(", ")))
                    }
                }
                "cd" => (t.to_string(), if rest.trim().is_empty() { "Go to your home folder".into() } else { format!("Go to “{}”", rest.trim()) }),
                "select" => (t.to_string(), "Select matching items in this folder".into()),
                "tag" => (t.to_string(), "Tag the selected items".into()),
                "newtab" => (t.to_string(), "Open a new tab".into()),
                "term" => (t.to_string(), format!("Open a terminal in {here_name}")),
                "!" if rest.trim_start().starts_with('!') => (t.to_string(), format!("Run in a terminal window, in {here_name}")),
                "!" => (t.to_string(), format!("Run in {here_name} · output shows here")),
                "save" => match parse_save(rest) {
                    Some((name, line)) => {
                        let verb = if self.settings.saved_commands.iter().any(|c| c.name.eq_ignore_ascii_case(&name)) { "Replace" } else { "Save" };
                        (t.to_string(), format!("{verb} “{name}” → {line}"))
                    }
                    None => (t.to_string(), "Type: save name = command   e.g.  save vs = !code {dir}".into()),
                },
                "sort" | "view" => match parse_folder_rule(verb, rest) {
                    Err(usage) => (t.to_string(), usage.to_string()),
                    Ok(FolderRule::Reset { all }) => {
                        let scope = if all { " and all its subfolders" } else { "" };
                        (t.to_string(), format!("Forget the sort and view chosen for {here_name}{scope}"))
                    }
                    Ok(FolderRule::Set { sort, view, all }) => {
                        let how = match (sort, view) {
                            (Some((k, a)), _) => format!("Sort {}", sort_label(k, a)),
                            (_, Some(v)) => format!("Show {}", view_label(v)),
                            _ => String::new(),
                        };
                        let scope = if all { " and every subfolder (also new ones)" } else { "" };
                        (t.to_string(), format!("{how}: {here_name}{scope}"))
                    }
                },
                "unsave" => {
                    let name = rest.trim();
                    if self.settings.saved_commands.iter().any(|c| c.name.eq_ignore_ascii_case(name)) {
                        (t.to_string(), format!("Delete the saved command “{name}”"))
                    } else {
                        (t.to_string(), "Type the name of a saved command".into())
                    }
                }
                "copy" | "move" => {
                    let n = self.model().selected().len();
                    let to = rest.trim_start()[2..].trim();
                    match (n, self.resolve_dest(to)) {
                        (0, _) => (t.to_string(), "Select the items first".into()),
                        (_, None) => (t.to_string(), "Type a folder, e.g. D:\\backup".into()),
                        (n, Some(d)) => {
                            let new = if d.exists() { "" } else { " (new folder)" };
                            let what = if verb == "copy" { "Copy" } else { "Move" };
                            (t.to_string(), format!("{what} {} to {}{new}", plural(n, "item", "items"), d.display()))
                        }
                    }
                }
                "zip" => {
                    let n = self.model().selected().len();
                    if n == 0 {
                        (t.to_string(), "Select the items first".into())
                    } else {
                        (t.to_string(), format!("Zip {} into {}", plural(n, "item", "items"), zip_file_name(rest)))
                    }
                }
                "rename" => match self.rename_plan(rest) {
                    Ok(plan) => (t.to_string(), format!("Rename {} · Enter to apply", plural(plan.len(), "item", "items"))),
                    Err(e) => (t.to_string(), e),
                },
                _ => (t.to_string(), String::new()),
            };
            rows.push((item(title, detail, "Enter", "terminal"), PAction::Line(t.to_string())));
            // rename: preview old → new before anything changes.
            if verb == "rename"
                && let Ok(plan) = self.rename_plan(rest)
            {
                let shown = MAX_ROWS - 2;
                for (old, new) in plan.iter().take(shown) {
                    let old_name = old.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    rows.push((item(old_name, format!("→  {new}"), "", "rename"), PAction::Line(t.to_string())));
                }
                if plan.len() > shown {
                    rows.push((item(format!("… and {} more", plan.len() - shown), String::new(), "", "more"), PAction::Line(t.to_string())));
                }
            }
            // cd: also suggest matching folders.
            if verb == "cd" && !rest.trim().is_empty() {
                for (_, p) in self.folder_matches(rest.trim(), MAX_ROWS - 1) {
                    rows.push((item(self.loc_title(&Location::Dir(p.clone())), p.display().to_string(), "", "folder"), PAction::Goto(p)));
                }
            }
        } else {
            let mut scored: Vec<(i32, PaletteItem, PAction)> = Vec::new();
            for (cmd, key) in ACTIONS {
                if let Some(s) = score(cmd.title(), t) {
                    scored.push((s + 50, item(cmd.title().into(), String::new(), key, "more"), PAction::Cmd(*cmd)));
                }
            }
            for c in &self.settings.saved_commands {
                if let Some(sc) = score(&c.name, t).or_else(|| score(&c.line, t).map(|x| x - 20)) {
                    scored.push((sc + 80, item(c.name.clone(), c.line.clone(), "", "terminal"), PAction::Line(c.line.clone())));
                }
            }
            for (id, title, key) in SPECIALS {
                if let Some(s) = score(title, t) {
                    scored.push((s + 50, item(title.to_string(), String::new(), key, "more"), PAction::Special(id)));
                }
            }
            for (i, g) in self.settings.tab_groups.iter().enumerate() {
                if let Some(s) = score(&format!("open group {}", g.name), t) {
                    let key = super::groups::shortcut_label(g.shortcut);
                    scored.push((
                        s,
                        item(format!("Open Tab Group: {}", g.name), format!("{} tabs", g.tabs.len()), &key, "tab"),
                        PAction::Group(i),
                    ));
                }
            }
            for (s, p) in self.folder_matches(t, MAX_ROWS) {
                scored.push((s, item(self.loc_title(&Location::Dir(p.clone())), p.display().to_string(), "", "folder"), PAction::Goto(p)));
            }
            // A typed path that exists: go straight there.
            if let Some(Location::Dir(p)) = location::parse_user_input(t)
                && p.is_dir()
            {
                scored.push((5000, item(format!("Go to {}", p.display()), String::new(), "Enter", "folder"), PAction::Goto(p)));
            }
            scored.sort_by(|a, b| b.0.cmp(&a.0));
            rows.extend(scored.into_iter().map(|(_, i, a)| (i, a)));
            if rows.is_empty() {
                rows.push((item(format!("No match for “{t}”"), "Try: mkdir · touch · cd · select · tag · !command · 2+3".into(), "", "info"), PAction::Fill(String::new())));
            }
        }
        // Command output below the list needs the room.
        rows.truncate(if self.cmd_output_shown() { 4 } else { MAX_ROWS });
        let Some(ui) = self.ui() else { return };
        let b = ui.global::<Bridge>();
        let items: Vec<PaletteItem> = rows.iter().map(|(i, _)| i.clone()).collect();
        self.palette_actions = rows.into_iter().map(|(_, a)| a).collect();
        b.set_palette_items(ModelRc::new(VecModel::from(items)));
        b.set_palette_index(0);
    }

    pub(super) fn palette_run(&mut self, index: usize) {
        let Some(action) = self.palette_actions.get(index).cloned() else { return };
        match action {
            PAction::Fill(text) => {
                if let Some(ui) = self.ui() {
                    let b = ui.global::<Bridge>();
                    b.set_palette_text(text.as_str().into());
                    b.set_palette_fill_serial(b.get_palette_fill_serial() + 1);
                }
                self.palette_changed(&text);
            }
            PAction::Cmd(cmd) => {
                self.palette_close();
                self.run(cmd);
            }
            PAction::Goto(p) => {
                self.palette_close();
                self.navigate(Location::Dir(p), None);
            }
            PAction::Special(id) => {
                self.palette_close();
                self.palette_special(id);
            }
            PAction::Group(i) => {
                self.palette_close();
                self.open_tab_group(i);
            }
            PAction::Copy(text) => {
                self.palette_close();
                match fileops::set_clipboard_text(&text.replace(',', "")) {
                    Ok(()) => self.toast(format!("Copied {text}")),
                    Err(e) => self.toast(e),
                }
            }
            PAction::Line(line) => {
                // "!command": runs with its output in the palette, which stays open.
                let line = self.saved_line(&line).unwrap_or(line);
                if let Some(("!", rest)) = split_verb(&line)
                    && !rest.trim().is_empty()
                    && !rest.trim_start().starts_with('!')
                    && let Some(dir) = self.current_dir()
                {
                    let cmd = self.expand_placeholders(rest.trim());
                    return self.run_captured(cmd, dir);
                }
                self.palette_close();
                self.run_line(&line);
            }
        }
    }

    /// A saved command typed by name ("ip", or "ip /all": extra words are
    /// added to the end).
    fn saved_line(&self, text: &str) -> Option<String> {
        let t = text.trim();
        let (word, rest) = t.split_once(char::is_whitespace).unwrap_or((t, ""));
        let c = self.settings.saved_commands.iter().find(|c| c.name.eq_ignore_ascii_case(word))?;
        Some(if rest.trim().is_empty() { c.line.clone() } else { format!("{} {}", c.line, rest.trim()) })
    }

    /// {dir} {file} {files} {name} → the current folder and selection
    /// (paths quoted, so spaces are fine).
    fn expand_placeholders(&self, line: &str) -> String {
        if !line.contains('{') {
            return line.to_string();
        }
        let quote = |p: &Path| format!("\"{}\"", p.display());
        let dir = self.current_dir().map(|d| quote(&d)).unwrap_or_default();
        let mut sel = self.selected_only();
        if sel.is_empty() {
            let model = self.model();
            if let Some(p) = model.cursor().and_then(|c| model.with_entry(c, |e| self.tab().loc.path_of(e))) {
                sel.push(p);
            }
        }
        let files = sel.iter().map(|p| quote(p)).collect::<Vec<_>>().join(" ");
        let file = sel.first().map(|p| quote(p)).unwrap_or_default();
        let name = sel.first().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        line.replace("{dir}", &dir).replace("{files}", &files).replace("{file}", &file).replace("{name}", &name)
    }

    fn cmd_output_shown(&self) -> bool {
        self.ui().is_some_and(|ui| {
            let b = ui.global::<Bridge>();
            b.get_palette_running() || !b.get_palette_output().is_empty()
        })
    }

    /// Runs `cmd` in `dir` without a window; its output streams into the
    /// palette. Closing the palette (or running another) stops it.
    fn run_captured(&mut self, cmd: String, dir: PathBuf) {
        self.stop_captured();
        let Some(ui) = self.ui() else { return };
        let b = ui.global::<Bridge>();
        #[cfg(windows)]
        {
            use std::io::Read;
            use std::os::windows::process::CommandExt;
            use std::process::Stdio;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            // UTF-8 output where the program allows it (chcp 65001).
            let child = std::process::Command::new("cmd")
                .raw_arg(format!("/d /s /c \"chcp 65001>nul & {cmd}\""))
                .current_dir(&dir)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .creation_flags(CREATE_NO_WINDOW)
                .spawn();
            let mut child = match child {
                Ok(c) => c,
                Err(e) => {
                    b.set_palette_output(format!("Couldn't run it: {e}").into());
                    return;
                }
            };
            self.cmd_serial += 1;
            let serial = self.cmd_serial;
            self.cmd_run = Some(CmdRun { serial, pid: child.id(), bytes: Vec::new() });
            b.set_palette_output("".into());
            b.set_palette_running(true);
            b.set_palette_output_status(format!("Running {cmd}").into());
            let pipes: Vec<Box<dyn Read + Send>> = [
                child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>),
                child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>),
            ]
            .into_iter()
            .flatten()
            .collect();
            for mut pipe in pipes {
                std::thread::spawn(move || {
                    let mut buf = [0u8; 8192];
                    while let Ok(n) = pipe.read(&mut buf) {
                        if n == 0 {
                            break;
                        }
                        let chunk = buf[..n].to_vec();
                        let _ = slint::invoke_from_event_loop(move || {
                            super::with_app(|a| a.captured_output(serial, &chunk));
                        });
                    }
                });
            }
            std::thread::spawn(move || {
                let code = child.wait().ok().and_then(|s| s.code());
                // Let the readers deliver the last bytes first.
                std::thread::sleep(std::time::Duration::from_millis(60));
                let _ = slint::invoke_from_event_loop(move || {
                    super::with_app(|a| a.captured_done(serial, code));
                });
            });
        }
        #[cfg(not(windows))]
        let _ = (cmd, dir);
        let text = b.get_palette_text().to_string();
        self.palette_changed(&text);
    }

    fn captured_output(&mut self, serial: u64, chunk: &[u8]) {
        let Some(run) = self.cmd_run.as_mut().filter(|r| r.serial == serial) else { return };
        run.bytes.extend_from_slice(chunk);
        if run.bytes.len() > OUTPUT_MAX {
            let cut = run.bytes.len() - OUTPUT_MAX;
            run.bytes.drain(..cut);
        }
        let text = String::from_utf8_lossy(&run.bytes).replace("\r\n", "\n").replace('\r', "");
        if let Some(ui) = self.ui() {
            ui.global::<Bridge>().set_palette_output(text.trim_end().into());
        }
    }

    fn captured_done(&mut self, serial: u64, code: Option<i32>) {
        if self.cmd_run.as_ref().is_none_or(|r| r.serial != serial) {
            return;
        }
        self.cmd_run = None;
        if let Some(ui) = self.ui() {
            let b = ui.global::<Bridge>();
            b.set_palette_running(false);
            b.set_palette_output_status(match code {
                Some(0) => "Done".into(),
                Some(c) => format!("Finished with exit code {c}").into(),
                None => "Stopped".into(),
            });
            if b.get_palette_output().is_empty() {
                b.set_palette_output("(no output)".into());
            }
        }
    }

    /// Stops a running `!command` (and anything it started).
    fn stop_captured(&mut self) {
        if let Some(run) = self.cmd_run.take() {
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                let _ = std::process::Command::new("taskkill")
                    .args(["/T", "/F", "/PID", &run.pid.to_string()])
                    .creation_flags(0x0800_0000)
                    .spawn();
            }
            #[cfg(not(windows))]
            let _ = run;
        }
    }

    pub(super) fn palette_copy_output(&mut self) {
        let Some(ui) = self.ui() else { return };
        let b = ui.global::<Bridge>();
        let text = b.get_palette_output().to_string();
        if !text.is_empty() && fileops::set_clipboard_text(&text.replace('\n', "\r\n")).is_ok() {
            b.set_palette_output_status("Output copied".into());
        }
    }

    fn palette_special(&mut self, id: &str) {
        let (key, arg) = id.split_once(':').map_or((id, None), |(k, a)| (k, a.parse::<usize>().ok()));
        match (key, arg) {
            ("settings", _) => self.settings_set("open", 1),
            ("sizes", _) => self.toggle_folder_sizes(),
            ("layout", Some(l)) => self.set_layout(l),
            ("save-group", _) => self.begin_save_tab_group(),
            ("explorer", _) => {
                if let Some(d) = self.current_dir() {
                    finder_rs::core::explorer_catch::allow_explorer();
                    let _ = std::process::Command::new("explorer.exe").arg(d).spawn();
                }
            }
            ("terminal", _) => {
                if let Some(d) = self.current_dir() {
                    fileops::open_terminal(&d);
                }
            }
            _ => {}
        }
    }

    /// Runs a typed command line ("mkdir a, b", "touch x.txt", "cd docs"…).
    pub(super) fn run_line(&mut self, line: &str) {
        let saved = self.saved_line(line);
        let line = saved.as_deref().unwrap_or(line);
        let Some((verb, rest)) = split_verb(line) else {
            return self.toast(format!("Unknown command “{}”", line.trim()));
        };
        let here = self.current_dir();
        let needs_folder = matches!(verb, "mkdir" | "touch" | "term" | "!" | "zip" | "rename");
        if needs_folder && here.is_none() {
            return self.toast("Open a folder first.");
        }
        let dir = here.clone().unwrap_or_else(sys::home);
        match verb {
            "mkdir" | "touch" => {
                let names = split_names(rest);
                if names.is_empty() {
                    return self.toast(if verb == "mkdir" { "Usage: mkdir folder1, folder2" } else { "Usage: touch notes.txt, index.html" });
                }
                let (mut made, mut problems) = (Vec::new(), Vec::new());
                for name in &names {
                    // Sub-paths are fine ("src/app"); each part must be a valid name.
                    let parts: Vec<&str> = name.split(['/', '\\']).filter(|p| !p.is_empty()).collect();
                    if let Some(e) = parts.iter().find_map(|p| fileops::validate_name(p).err()) {
                        problems.push(format!("{name}: {e}"));
                        continue;
                    }
                    let target = parts.iter().fold(dir.clone(), |acc, p| acc.join(p));
                    let result = if verb == "mkdir" {
                        if target.exists() { Err("already exists".to_string()) } else { std::fs::create_dir_all(&target).map_err(|e| e.to_string()) }
                    } else {
                        if let Some(parent) = target.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        std::fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(&target)
                            .map(|_| ())
                            .map_err(|e| if e.kind() == std::io::ErrorKind::AlreadyExists { "already exists".into() } else { e.to_string() })
                    };
                    match result {
                        Ok(()) => made.push(parts[0].to_string()),
                        Err(e) => problems.push(format!("{name}: {e}")),
                    }
                }
                made.dedup();
                let what = if verb == "mkdir" { ("folder", "folders") } else { ("file", "files") };
                if !made.is_empty() {
                    // Select them once the folder watcher has listed them.
                    self.select_after = made.clone();
                    self.apply_select_after();
                }
                let mut msg = if made.is_empty() { String::new() } else { format!("Created {}", plural(made.len(), what.0, what.1)) };
                if !problems.is_empty() {
                    if !msg.is_empty() {
                        msg.push_str(" · ");
                    }
                    msg.push_str(&format!("Skipped {}", problems.join("; ")));
                }
                self.toast(msg);
            }
            "cd" => {
                let target = rest.trim().trim_matches('"');
                if target.is_empty() {
                    return self.navigate(Location::Dir(sys::home()), None);
                }
                // Relative to here, a full path ("D:\work", "~/Downloads"), or a
                // remembered folder by name.
                let rel = dir.join(target);
                if here.is_some() && rel.is_dir() {
                    return self.navigate(Location::Dir(rel), None);
                }
                if let Some(loc) = location::parse_user_input(target)
                    && (!matches!(&loc, Location::Dir(p) if !p.is_dir()))
                {
                    return self.navigate(loc, None);
                }
                match self.folder_matches(target, 1).into_iter().next() {
                    Some((_, p)) => self.navigate(Location::Dir(p), None),
                    None => self.toast(format!("No folder matches “{target}”")),
                }
            }
            "select" => {
                let pat = rest.trim();
                let model = self.model();
                let loc = self.tab().loc.clone();
                let n = slint::Model::row_count(&*model);
                let hits: Vec<bool> = (0..n)
                    .map(|row| {
                        model
                            .entry_at(row)
                            .and_then(|i| {
                                model.with_entry(i, |e| {
                                    if pat.contains(['*', '?']) {
                                        wildcard(pat, &e.name)
                                    } else {
                                        let q = Query::parse(pat);
                                        !q.is_empty() && q.matches(e)
                                    }
                                })
                            })
                            .unwrap_or(false)
                    })
                    .collect();
                let _ = loc;
                let count = hits.iter().filter(|h| **h).count();
                model.select_where(|row| hits.get(row).copied().unwrap_or(false));
                self.sync_status();
                self.toast(format!("Selected {}", plural(count, "item", "items")));
            }
            "tag" => {
                let want = rest.trim().to_lowercase();
                if matches!(want.as_str(), "none" | "clear" | "off" | "-") {
                    return self.clear_tags();
                }
                let t = (0..finder_rs::core::tags::COLORS).find(|&i| {
                    self.settings.tag_name(i).to_lowercase() == want
                        || finder_rs::core::tags::NAMES[i].to_lowercase() == want
                });
                match t {
                    Some(i) => {
                        let bit = 1u8 << i;
                        self.update_tags(|m| m | bit);
                    }
                    None => self.toast(format!("No tag named “{}”", rest.trim())),
                }
            }
            "newtab" => {
                let target = rest.trim();
                let loc = if target.is_empty() {
                    self.tab().loc.clone()
                } else {
                    location::parse_user_input(target).unwrap_or_else(|| Location::Dir(dir.join(target)))
                };
                self.open_tab(loc, true);
            }
            "copy" | "move" => {
                let sources = self.selected_only();
                if sources.is_empty() {
                    return self.toast("Select the items first.");
                }
                let to = rest.trim_start()[2..].trim();
                let Some(dest) = self.resolve_dest(to) else {
                    return self.toast("Type a folder, e.g.  copy to D:\\backup");
                };
                if !dest.exists()
                    && let Err(e) = std::fs::create_dir_all(&dest)
                {
                    return self.toast(format!("Couldn't create {}: {e}", dest.display()));
                }
                let op = if verb == "copy" {
                    fileops::Op::Copy { sources, dest }
                } else {
                    fileops::Op::Move { sources, dest }
                };
                self.run_op(op);
            }
            "zip" => {
                let sources = self.selected_only();
                if sources.is_empty() {
                    return self.toast("Select the items first.");
                }
                let name = zip_file_name(rest);
                if let Err(e) = fileops::validate_name(&name) {
                    return self.toast(e);
                }
                let dest = dir.join(&name);
                if dest.exists() {
                    return self.toast(format!("“{name}” already exists."));
                }
                self.select_after = vec![name.clone()];
                self.toast("Zipping…");
                std::thread::spawn(move || {
                    let result = fileops::zip_items(&sources, &dest);
                    let _ = slint::invoke_from_event_loop(move || {
                        super::with_app(|a| match result {
                            Ok(()) => a.toast(format!("Created “{name}”")),
                            Err(e) => a.toast(e),
                        });
                    });
                });
            }
            "rename" => match self.rename_plan(rest) {
                Ok(plan) => self.apply_renames(plan),
                Err(e) => self.toast(e),
            },
            "save" => {
                let Some((name, cmd)) = parse_save(rest) else {
                    return self.toast("Usage: save name = command   e.g.  save ip = !ipconfig");
                };
                if split_verb(&name).is_some() {
                    return self.toast(format!("“{name}” is already a command; pick another name."));
                }
                let list = &mut self.settings.saved_commands;
                list.retain(|c| !c.name.eq_ignore_ascii_case(&name));
                list.insert(0, finder_rs::core::settings::SavedCommand { name: name.clone(), line: cmd });
                self.save_settings();
                self.toast(format!("Saved. Type “{name}” in the palette to run it."));
            }
            "sort" | "view" => match parse_folder_rule(verb, rest) {
                Err(usage) => self.toast(usage),
                Ok(FolderRule::Reset { all }) => {
                    self.reset_folder_rule(all);
                    self.toast(if all { "Back to the default sort and view here and in every subfolder." } else { "Back to the default sort and view here." });
                }
                Ok(FolderRule::Set { sort, view, all }) => {
                    self.set_folder_rule(sort, view, all);
                    if all {
                        self.toast("Done. Every subfolder opens this way too.");
                    }
                }
            },
            "unsave" => {
                let name = rest.trim().to_string();
                let before = self.settings.saved_commands.len();
                self.settings.saved_commands.retain(|c| !c.name.eq_ignore_ascii_case(&name));
                if self.settings.saved_commands.len() < before {
                    self.save_settings();
                    self.toast(format!("Deleted “{name}”"));
                } else {
                    self.toast(format!("No saved command “{name}”"));
                }
            }
            "term" => fileops::open_terminal(&dir),
            "!" => {
                // "!!cmd" (or "!cmd" from outside the palette): in a terminal.
                let expanded = self.expand_placeholders(rest.trim().trim_start_matches('!').trim());
                let cmd = expanded.as_str();
                if cmd.is_empty() {
                    return fileops::open_terminal(&dir);
                }
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    // New console window that stays open to show the output.
                    let r = std::process::Command::new("cmd")
                        .arg("/k")
                        .raw_arg(cmd)
                        .current_dir(&dir)
                        .creation_flags(0x0000_0010)
                        .spawn();
                    if let Err(e) = r {
                        self.toast(format!("Couldn't run it: {e}"));
                    }
                }
            }
            _ => {}
        }
        if self.view == ViewMode::Gallery {
            self.sync_strip();
        }
    }
}

/// "ip = !ipconfig" → ("ip", "!ipconfig").
fn parse_save(rest: &str) -> Option<(String, String)> {
    let (name, line) = rest.split_once('=')?;
    let (name, line) = (name.trim(), line.trim());
    if name.is_empty() || line.is_empty() || name.contains(char::is_whitespace) {
        return None;
    }
    Some((name.to_string(), line.to_string()))
}

/// "archive" → "archive.zip"; empty → "Archive.zip".
fn zip_file_name(rest: &str) -> String {
    let n = rest.trim().trim_matches('"').trim();
    let n = if n.is_empty() { "Archive" } else { n };
    if n.to_lowercase().ends_with(".zip") { n.to_string() } else { format!("{n}.zip") }
}

impl App {
    /// Only explicitly selected items (not the cursor alone).
    fn selected_only(&self) -> Vec<PathBuf> {
        let model = self.model();
        let loc = &self.tab().loc;
        model.selected().into_iter().filter_map(|i| model.with_entry(i, |e| loc.path_of(e))).collect()
    }

    /// "D:\backup", "~/x", "sub" (relative to this folder).
    fn resolve_dest(&self, to: &str) -> Option<PathBuf> {
        let to = to.trim().trim_matches('"');
        if to.is_empty() {
            return None;
        }
        match location::parse_user_input(to) {
            Some(Location::Dir(p)) if p.is_absolute() => Some(p),
            _ => self.current_dir().map(|d| d.join(to)),
        }
    }

    /// Works out every old → new name, or why it can't be done.
    pub(super) fn rename_plan(&self, rest: &str) -> Result<Vec<(PathBuf, String)>, String> {
        const USAGE: &str = "Try: rename *.jpg trip-{n}.jpg  ·  rename \"old\" \"new\"  ·  rename photo-{nn} (selection)";
        let mut a = args(rest);
        // "rename old -> new"
        if a.len() == 3 && a[1] == "->" {
            a.remove(1);
        }
        let model = self.model();
        let loc = self.tab().loc.clone();
        // Rows in the order you see them.
        let rows: Vec<(PathBuf, String, i64, bool)> = (0..slint::Model::row_count(&*model))
            .filter_map(|r| model.entry_at(r))
            .filter_map(|i| model.with_entry(i, |e| (loc.path_of(e), e.name.to_string(), e.modified, e.selected)))
            .collect();
        let selected: Vec<_> = rows.iter().filter(|r| r.3).cloned().collect();
        let plan: Vec<(PathBuf, String)> = match a.as_slice() {
            // rename trip-{n}: the selection
            [template] => {
                if selected.is_empty() {
                    return Err("Select the items to rename first (or give a pattern: rename *.jpg trip-{n}.jpg)".into());
                }
                selected.iter().enumerate().map(|(k, r)| (r.0.clone(), expand_template(template, &r.1, k + 1, r.2))).collect()
            }
            // rename *.jpg trip-{n}.jpg  — pattern
            [pattern, template] if pattern.contains(['*', '?']) => {
                let hits: Vec<_> = rows.iter().filter(|r| wildcard(pattern, &r.1)).collect();
                if hits.is_empty() {
                    return Err(format!("Nothing here matches {pattern}"));
                }
                hits.iter().enumerate().map(|(k, r)| (r.0.clone(), expand_template(template, &r.1, k + 1, r.2))).collect()
            }
            // rename "old" "new" — find & replace (selection, else every match)
            [find, with] => {
                // Selected items that contain it; if none do, every name here.
                let contains = |r: &&(PathBuf, String, i64, bool)| r.1.to_lowercase().contains(&find.to_lowercase());
                let mut hits: Vec<_> = selected.iter().filter(contains).collect();
                if hits.is_empty() {
                    hits = rows.iter().filter(contains).collect();
                }
                if hits.is_empty() {
                    return Err(format!("No names contain “{find}”"));
                }
                hits.iter().map(|r| (r.0.clone(), replace_ci(&r.1, find, with))).collect()
            }
            _ => return Err(USAGE.into()),
        };
        let plan: Vec<(PathBuf, String)> = plan
            .into_iter()
            .filter(|(old, new)| old.file_name().map(|n| n.to_string_lossy() != new.as_str()).unwrap_or(true))
            .collect();
        if plan.is_empty() {
            return Err("Nothing would change".into());
        }
        // Valid, unique, and not clashing with a file that stays.
        let renamed: std::collections::HashSet<String> =
            plan.iter().filter_map(|(o, _)| o.file_name().map(|n| n.to_string_lossy().to_lowercase())).collect();
        let mut seen = std::collections::HashSet::new();
        for (old, new) in &plan {
            fileops::validate_name(new).map_err(|e| format!("“{new}”: {e}"))?;
            let key = new.to_lowercase();
            if !seen.insert(key.clone()) {
                return Err(format!("Two items would both be named “{new}”"));
            }
            if old.with_file_name(new).exists() && !renamed.contains(&key) {
                return Err(format!("“{new}” already exists"));
            }
        }
        Ok(plan)
    }

    /// Renames in two steps (via temporary names) so swaps like
    /// a→b, b→a work; tags follow the files.
    fn apply_renames(&mut self, plan: Vec<(PathBuf, String)>) {
        let mut temps = Vec::new();
        for (k, (old, new)) in plan.iter().enumerate() {
            let tmp = old.with_file_name(format!(".finder-rs-rename-{k}-{}", std::process::id()));
            match std::fs::rename(old, &tmp) {
                Ok(()) => temps.push((old.clone(), tmp, new.clone())),
                Err(e) => {
                    // Put back what already moved.
                    for (o, t, _) in temps {
                        let _ = std::fs::rename(&t, &o);
                    }
                    return self.toast(format!("Couldn't rename {}: {e}", old.display()));
                }
            }
        }
        let mut done = Vec::new();
        let mut failed = 0;
        for (old, tmp, new) in temps {
            let target = old.with_file_name(&new);
            if std::fs::rename(&tmp, &target).is_ok() {
                finder_rs::core::tags::moved(&old, &target);
                done.push(new);
            } else {
                failed += 1;
                let _ = std::fs::rename(&tmp, &old);
            }
        }
        self.select_after = done.clone();
        let msg = if failed == 0 {
            format!("Renamed {}", plural(done.len(), "item", "items"))
        } else {
            format!("Renamed {}, {failed} failed", done.len())
        };
        self.toast(msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_rules() {
        let set = |sort, view, all| Ok(FolderRule::Set { sort, view, all });
        assert_eq!(parse_folder_rule("sort", "kind all"), set(Some((SortKey::Kind, true)), None, true));
        assert_eq!(parse_folder_rule("sort", "date"), set(Some((SortKey::Modified, false)), None, false));
        assert_eq!(parse_folder_rule("sort", "Size up"), set(Some((SortKey::Size, true)), None, false));
        assert_eq!(parse_folder_rule("view", "icons all"), set(None, Some(ViewMode::Icons), true));
        assert_eq!(parse_folder_rule("sort", "reset all"), Ok(FolderRule::Reset { all: true }));
        assert!(parse_folder_rule("sort", "banana").is_err());
        assert_eq!(split_verb("sort"), None);
        assert_eq!(split_verb("view icons"), Some(("view", "icons")));
    }

    #[test]
    fn names_and_verbs() {
        assert_eq!(split_names("folder1, folder2"), vec!["folder1", "folder2"]);
        assert_eq!(split_names("fast.txt fast.html"), vec!["fast.txt", "fast.html"]);
        assert_eq!(split_names("\"my notes.txt\" todo.md"), vec!["my notes.txt", "todo.md"]);
        assert_eq!(split_names("a b, c"), vec!["a b", "c"]);
        assert_eq!(split_verb("mkdir a, b"), Some(("mkdir", "a, b")));
        assert_eq!(split_verb("MD x"), Some(("mkdir", "x")));
        assert_eq!(split_verb("!git status"), Some(("!", "git status")));
        assert_eq!(split_verb("copy path"), None);
    }

    #[test]
    fn rename_templates() {
        assert_eq!(expand_template("trip-{n}.jpg", "IMG_001.JPG", 3, 0), "trip-3.jpg");
        assert_eq!(expand_template("photo-{nnn}", "a.png", 7, 0), "photo-007.png");
        assert_eq!(expand_template("{name}-final.{ext}", "report.docx", 1, 0), "report-final.docx");
        assert_eq!(expand_template("{date} {name}", "x.txt", 1, 1_700_000_000), "2023-11-14 x.txt");
        assert_eq!(replace_ci("Draft Report DRAFT.docx", "draft", "Final"), "Final Report Final.docx");
        assert_eq!(args("\"old name\" \"new\""), vec!["old name", "new"]);
        assert_eq!(args("draft \"\""), vec!["draft", ""]);
        assert_eq!(split_verb("copy path"), None);
        assert_eq!(split_verb("copy to D:\\x"), Some(("copy", "to D:\\x")));
        assert_eq!(split_verb("rename"), None);
        assert_eq!(zip_file_name("photos"), "photos.zip");
    }

    #[test]
    fn fuzzy_and_wildcards() {
        assert!(score("Copy Path", "cpp").is_some());
        assert!(score("Copy Path", "copy").unwrap() > score("Duplicate", "copy").unwrap_or(0));
        assert!(score("View as Columns", "xyz").is_none());
        assert!(wildcard("*.pdf", "Report.PDF"));
        assert!(wildcard("img_??.jpg", "IMG_01.jpg"));
        assert!(!wildcard("*.pdf", "a.pdfx"));
    }
}
