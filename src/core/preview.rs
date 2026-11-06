//! Quick Look content: turns a file into something the preview UI can show.
//!
//! - images, video frames: shell thumbnails (Explorer's providers + cache)
//! - PDF: Windows' built-in PDF renderer, page by page
//! - Word / PowerPoint: text and headings read straight from the .docx/.pptx
//!   (no Office needed); Excel and CSV: the first sheet as a table
//! - Markdown: rendered into headings, paragraphs, lists and code blocks
//! - text / code / logs (and unknown files that look like text): plain text
//!
//! Everything here blocks; run it on a worker thread.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use super::format;
use super::shell::{self, Pixels};

const TEXT_LIMIT: usize = 1 << 20; // 1 MB of text
const LINE_LIMIT: usize = 20_000;
const BLOCK_LIMIT: usize = 5_000;
const TABLE_ROWS: usize = 500;
const TABLE_COLS: usize = 40;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockKind {
    Para,
    H1,
    H2,
    H3,
    Bullet,
    Code,
    Quote,
    Rule,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub kind: BlockKind,
    pub text: String,
}

#[derive(Debug)]
pub enum Content {
    /// Image, video frame or a document thumbnail.
    Image(Pixels),
    Pdf { page: Pixels, index: u32, count: u32 },
    /// Lines of text; `mono` for code and logs.
    Text { lines: Vec<String>, mono: bool, truncated: bool },
    /// Rendered Markdown, Word or PowerPoint.
    Doc { blocks: Vec<Block>, truncated: bool },
    Table { title: String, rows: Vec<Vec<String>>, truncated: bool },
    /// Nothing to render; the UI shows a large icon and this note.
    Nothing(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Image,
    Video,
    /// Album art via the shell, when the file has any.
    Audio,
    /// Archives, programs, disk images: never sniffed as text.
    Binary,
    Pdf,
    Markdown,
    Text { mono: bool },
    Word,
    PowerPoint,
    Excel,
    Csv,
    Other,
}

pub fn classify(name: &str) -> Kind {
    let lower = name.to_lowercase();
    match format::extension(&lower) {
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "tif" | "tiff" | "ico" | "heic" | "heif" | "avif" | "jfif"
        | "svg" | "psd" | "raw" | "cr2" | "nef" | "arw" | "dng" => Kind::Image,
        "mp4" | "m4v" | "mov" | "mkv" | "avi" | "webm" | "wmv" | "mpg" | "mpeg" | "3gp" | "mts" | "m2ts" => Kind::Video,
        "mp3" | "wav" | "flac" | "m4a" | "aac" | "ogg" | "opus" | "wma" | "aiff" => Kind::Audio,
        "zip" | "7z" | "rar" | "tar" | "gz" | "tgz" | "xz" | "bz2" | "zst" | "iso" | "img" | "vhd" | "vhdx" | "exe"
        | "msi" | "dll" | "sys" | "bin" | "dat" | "db" | "sqlite" | "pak" | "class" | "jar" | "pyc" | "o" | "obj"
        | "lib" | "a" | "so" | "dylib" | "woff" | "woff2" | "ttf" | "otf" | "doc" | "xls" | "ppt" => Kind::Binary,
        "pdf" => Kind::Pdf,
        "md" | "markdown" | "mdown" => Kind::Markdown,
        "docx" | "docm" | "dotx" => Kind::Word,
        "pptx" | "pptm" | "potx" => Kind::PowerPoint,
        "xlsx" | "xlsm" | "xltx" => Kind::Excel,
        "csv" | "tsv" => Kind::Csv,
        "txt" | "log" | "text" | "rtf" | "nfo" => Kind::Text { mono: false },
        "rs" | "py" | "js" | "mjs" | "cjs" | "ts" | "tsx" | "jsx" | "json" | "toml" | "yaml" | "yml" | "xml" | "html"
        | "htm" | "css" | "scss" | "c" | "h" | "hpp" | "cpp" | "cc" | "cxx" | "cs" | "java" | "kt" | "go" | "rb"
        | "php" | "swift" | "sh" | "bash" | "zsh" | "ps1" | "psm1" | "bat" | "cmd" | "sql" | "slint" | "ini" | "cfg"
        | "conf" | "lock" | "gitignore" | "env" | "lua" | "dart" | "vue" | "svelte" | "gradle" | "properties" | "hh"
        | "hxx" | "ino" | "cu" | "kts" | "scala" | "groovy" | "cts" | "pyw" | "pyi" | "fish" | "psd1" | "xhtml"
        | "xaml" | "csproj" | "vcxproj" | "props" | "sass" | "less" | "jsonc" | "json5" | "geojson"
        | "webmanifest" | "editorconfig" | "gitconfig" | "qml" | "resx" | "config" => {
            Kind::Text { mono: true }
        }
        _ => Kind::Other,
    }
}

/// Like [`classify`], but looks inside files whose extension is ambiguous:
/// a ".ts" that starts with MPEG transport-stream packets is video, not
/// TypeScript.
pub fn classify_path(path: &Path) -> Kind {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let kind = classify(&name);
    if format::extension(&name.to_lowercase()) == "ts" && is_transport_stream(path) {
        return Kind::Video;
    }
    kind
}

/// MPEG-TS: 188-byte packets each starting with 0x47 (192 with a 4-byte
/// timestamp in front, as in .m2ts).
fn is_transport_stream(path: &Path) -> bool {
    let Ok(mut f) = File::open(path) else { return false };
    let mut b = [0u8; 400];
    let Ok(n) = f.read(&mut b) else { return false };
    let b = &b[..n];
    let at = |i: usize| b.get(i) == Some(&0x47);
    (at(0) && at(188) && at(376)) || (at(4) && at(196) && at(388))
}

/// True for files worth asking the shell for a thumbnail.
pub fn has_thumbnail(name: &str) -> bool {
    let lower = name.to_lowercase();
    matches!(classify(name), Kind::Image | Kind::Video | Kind::Pdf)
        || matches!(
            format::extension(&lower),
            "docx" | "doc" | "pptx" | "ppt" | "xlsx" | "xls" | "psd" | "ai" | "eps" | "epub" | "3mf" | "stl" | "obj"
        )
}

/// Builds the preview for `path`. `px` is the target size in pixels.
pub fn load(path: &Path, px: u32) -> Content {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    match classify_path(path) {
        Kind::Image => match shell::thumbnail(path, px) {
            Some(img) => Content::Image(img),
            None => Content::Nothing("No preview available for this file.".into()),
        },
        // A video frame: Windows caches thumbnails up to 1024 px, and a
        // bigger request makes it decode the video again (slow).
        Kind::Video => match shell::thumbnail(path, px.min(1024)) {
            Some(img) => Content::Image(img),
            None => Content::Nothing("No preview picture for this video.".into()),
        },
        Kind::Audio => thumbnail_or(path, px, String::new()),
        Kind::Binary => thumbnail_or(path, px, String::new()),
        Kind::Pdf => pdf(path, 0, px),
        Kind::Markdown => match read_text(path) {
            Ok((text, truncated)) => Content::Doc { blocks: markdown(&text), truncated },
            Err(e) => Content::Nothing(e),
        },
        // A "text" extension on a binary file shows no garbage.
        Kind::Text { .. } if has_nul(path) => thumbnail_or(path, px, "No preview available for this file.".into()),
        Kind::Text { mono } => text(path, mono),
        Kind::Word => office(path, px, docx),
        Kind::PowerPoint => office(path, px, pptx),
        Kind::Excel => match xlsx(path) {
            Ok((title, rows, truncated)) => Content::Table { title, rows, truncated },
            Err(e) => thumbnail_or(path, px, e),
        },
        Kind::Csv => match read_text(path) {
            Ok((text, truncated)) => {
                let sep = if name.to_lowercase().ends_with(".tsv") { '\t' } else { ',' };
                let (rows, more) = csv(&text, sep);
                Content::Table { title: String::new(), rows, truncated: truncated || more }
            }
            Err(e) => Content::Nothing(e),
        },
        Kind::Other => {
            if looks_like_text(path) {
                text(path, true)
            } else {
                thumbnail_or(path, px, "No preview available for this file.".into())
            }
        }
    }
}

/// Renders one PDF page (0-based).
pub fn pdf(path: &Path, index: u32, px: u32) -> Content {
    match shell::pdf_page(path, index, px) {
        Ok((page, count)) => Content::Pdf { page, index: index.min(count.saturating_sub(1)), count },
        Err(e) => thumbnail_or(path, px, format!("Couldn't render this PDF: {e}")),
    }
}

fn thumbnail_or(path: &Path, px: u32, note: String) -> Content {
    match shell::thumbnail(path, px) {
        Some(img) => Content::Image(img),
        None => Content::Nothing(note),
    }
}

fn office(path: &Path, px: u32, extract: fn(&Path) -> Result<Vec<Block>, String>) -> Content {
    match extract(path) {
        Ok(blocks) if !blocks.is_empty() => {
            let truncated = blocks.len() >= BLOCK_LIMIT;
            Content::Doc { blocks, truncated }
        }
        Ok(_) => thumbnail_or(path, px, "This document has no text.".into()),
        Err(e) => thumbnail_or(path, px, e),
    }
}

// ---- text ------------------------------------------------------------------

fn read_text(path: &Path) -> Result<(String, bool), String> {
    let mut f = File::open(path).map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    f.by_ref().take(TEXT_LIMIT as u64 + 1).read_to_end(&mut buf).map_err(|e| e.to_string())?;
    let truncated = buf.len() > TEXT_LIMIT;
    buf.truncate(TEXT_LIMIT);
    Ok((decode(&buf), truncated))
}

/// UTF-8 (with or without BOM) or UTF-16 LE/BE with BOM.
fn decode(b: &[u8]) -> String {
    let utf16 = |be: bool| {
        let units: Vec<u16> = b[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| if be { u16::from_be_bytes(c) } else { u16::from_le_bytes(c) })
            .collect();
        String::from_utf16_lossy(&units)
    };
    match b {
        [0xFF, 0xFE, ..] => utf16(false),
        [0xFE, 0xFF, ..] => utf16(true),
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        _ => String::from_utf8_lossy(b).into_owned(),
    }
}

fn text(path: &Path, mono: bool) -> Content {
    match read_text(path) {
        Ok((t, truncated)) => {
            let mut lines: Vec<String> = t.lines().take(LINE_LIMIT + 1).map(|l| l.replace('\t', "    ")).collect();
            let more = lines.len() > LINE_LIMIT;
            lines.truncate(LINE_LIMIT);
            if lines.is_empty() {
                return Content::Nothing("This file is empty.".into());
            }
            Content::Text { lines, mono, truncated: truncated || more }
        }
        Err(e) => Content::Nothing(e),
    }
}

/// Binary data in the first 4 KB (text never has NUL bytes, except UTF-16).
fn has_nul(path: &Path) -> bool {
    let Ok(mut f) = File::open(path) else { return false };
    let mut buf = [0u8; 4096];
    let Ok(n) = f.read(&mut buf) else { return false };
    let b = &buf[..n];
    !matches!(b, [0xFF, 0xFE, ..] | [0xFE, 0xFF, ..]) && b.contains(&0)
}

/// Unknown extension: text if the first 4 KB has no NUL bytes and is valid UTF-8.
fn looks_like_text(path: &Path) -> bool {
    let Ok(mut f) = File::open(path) else { return false };
    let mut buf = [0u8; 4096];
    let Ok(n) = f.read(&mut buf) else { return false };
    let b = &buf[..n];
    if n == 0 || b.contains(&0) {
        return false;
    }
    match std::str::from_utf8(b) {
        Ok(_) => true,
        // A multi-byte character cut off at the 4 KB boundary is fine.
        Err(e) => e.error_len().is_none(),
    }
}

// ---- markdown ----------------------------------------------------------------

pub fn markdown(src: &str) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    let mut para = String::new();
    let mut code: Option<String> = None;
    let flush = |para: &mut String, out: &mut Vec<Block>| {
        if !para.trim().is_empty() {
            out.push(Block { kind: BlockKind::Para, text: inline_md(para.trim()) });
        }
        para.clear();
    };

    for line in src.lines() {
        if out.len() >= BLOCK_LIMIT {
            break;
        }
        let s = line.trim();
        if s.starts_with("```") || s.starts_with("~~~") {
            match code.take() {
                Some(c) => out.push(Block { kind: BlockKind::Code, text: c.trim_end_matches('\n').to_string() }),
                None => {
                    flush(&mut para, &mut out);
                    code = Some(String::new());
                }
            }
            continue;
        }
        if let Some(c) = code.as_mut() {
            c.push_str(&line.replace('\t', "    "));
            c.push('\n');
            continue;
        }
        if s.is_empty() {
            flush(&mut para, &mut out);
            continue;
        }
        let heading = s.find(|c| c != '#').filter(|&n| n > 0 && s[n..].starts_with(' ')).map(|n| (n, &s[n + 1..]));
        if let Some((level, text)) = heading {
            flush(&mut para, &mut out);
            let kind = match level {
                1 => BlockKind::H1,
                2 => BlockKind::H2,
                _ => BlockKind::H3,
            };
            out.push(Block { kind, text: inline_md(text.trim_end_matches('#').trim()) });
        } else if matches!(s, "---" | "***" | "___") || (s.len() >= 3 && s.chars().all(|c| c == '-')) {
            flush(&mut para, &mut out);
            out.push(Block { kind: BlockKind::Rule, text: String::new() });
        } else if let Some(item) = bullet(s) {
            flush(&mut para, &mut out);
            let depth = (line.len() - line.trim_start().len()) / 2;
            out.push(Block { kind: BlockKind::Bullet, text: format!("{}{}", "    ".repeat(depth.min(4)), inline_md(&item)) });
        } else if let Some(q) = s.strip_prefix('>') {
            flush(&mut para, &mut out);
            out.push(Block { kind: BlockKind::Quote, text: inline_md(q.trim()) });
        } else if s.starts_with('|') {
            // Tables are shown as aligned monospace text.
            flush(&mut para, &mut out);
            if s.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ')) {
                continue;
            }
            match out.last_mut() {
                Some(b) if b.kind == BlockKind::Code && b.text.starts_with('|') => {
                    b.text.push('\n');
                    b.text.push_str(s);
                }
                _ => out.push(Block { kind: BlockKind::Code, text: s.to_string() }),
            }
        } else {
            if !para.is_empty() {
                para.push(' ');
            }
            para.push_str(s);
        }
    }
    if let Some(c) = code {
        out.push(Block { kind: BlockKind::Code, text: c });
    }
    flush(&mut para, &mut out);
    out
}

/// "- item", "* item", "+ item", "1. item", "- [x] task".
fn bullet(s: &str) -> Option<String> {
    for p in ["- ", "* ", "+ "] {
        if let Some(rest) = s.strip_prefix(p) {
            let rest = rest
                .strip_prefix("[ ] ")
                .map(|r| format!("○ {r}"))
                .or_else(|| rest.strip_prefix("[x] ").or(rest.strip_prefix("[X] ")).map(|r| format!("☑ {r}")))
                .unwrap_or_else(|| format!("•  {rest}"));
            return Some(rest);
        }
    }
    let digits = s.find(|c: char| !c.is_ascii_digit())?;
    if digits > 0 && (s[digits..].starts_with(". ") || s[digits..].starts_with(") ")) {
        return Some(format!("{}.  {}", &s[..digits], &s[digits + 2..]));
    }
    None
}

/// Strips inline markup: **bold**, *em*, `code`, [text](url), ![alt](src), <tags>.
fn inline_md(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' | '_' if chars.peek() == Some(&c) => {
                chars.next();
            }
            '*' | '`' => {}
            '!' if chars.peek() == Some(&'[') => {}
            '[' => {
                let mut label = String::new();
                for d in chars.by_ref() {
                    if d == ']' {
                        break;
                    }
                    label.push(d);
                }
                if chars.peek() == Some(&'(') {
                    for d in chars.by_ref() {
                        if d == ')' {
                            break;
                        }
                    }
                }
                out.push_str(&label);
            }
            '\\' => {
                if let Some(d) = chars.next() {
                    out.push(d);
                }
            }
            _ => out.push(c),
        }
    }
    out
}

// ---- Office Open XML ---------------------------------------------------------

fn open_zip(path: &Path) -> Result<zip::ZipArchive<File>, String> {
    let f = File::open(path).map_err(|e| e.to_string())?;
    zip::ZipArchive::new(f).map_err(|_| "This file isn't a valid Office document.".to_string())
}

fn zip_text(z: &mut zip::ZipArchive<File>, name: &str) -> Option<String> {
    let mut entry = z.by_name(name).ok()?;
    let mut s = String::new();
    entry.read_to_string(&mut s).ok()?;
    Some(s)
}

enum Xml<'a> {
    /// (tag name, raw tag text, is end tag). Self-closing tags produce both.
    Tag(&'a str, &'a str, bool),
    /// Unescaped text between tags.
    Text(&'a str),
}

/// Walks an XML document, calling `f` for every tag and text run.
fn walk_xml(xml: &str, mut f: impl FnMut(Xml)) {
    let mut i = 0;
    while let Some(lt) = xml[i..].find('<') {
        let text = &xml[i..i + lt];
        if !text.is_empty() {
            f(Xml::Text(&unescape(text)));
        }
        let start = i + lt + 1;
        let Some(gt) = xml[start..].find('>') else { break };
        let raw = &xml[start..start + gt];
        i = start + gt + 1;
        if raw.starts_with('?') || raw.starts_with('!') {
            continue;
        }
        let is_end = raw.starts_with('/');
        let body = raw.trim_start_matches('/');
        let name = body.split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or("");
        f(Xml::Tag(name, raw, is_end));
        if !is_end && raw.ends_with('/') {
            f(Xml::Tag(name, raw, true));
        }
    }
}

fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#10;", "\n")
        .replace("&amp;", "&")
}

fn attr<'a>(raw: &'a str, key: &str) -> Option<&'a str> {
    let pat = format!(" {key}=\"");
    let start = raw.find(&pat)? + pat.len();
    let len = raw[start..].find('"')?;
    Some(&raw[start..start + len])
}

/// Word: paragraphs with heading styles and list items.
fn docx(path: &Path) -> Result<Vec<Block>, String> {
    let mut z = open_zip(path)?;
    let xml = zip_text(&mut z, "word/document.xml").ok_or("This Word file has no document body.")?;
    let mut out = Vec::new();
    let (mut para, mut in_t, mut style, mut list) = (String::new(), false, String::new(), false);
    walk_xml(&xml, |ev| match ev {
        Xml::Text(t) => {
            if in_t {
                para.push_str(t);
            }
        }
        Xml::Tag(name, raw, end) => match (name, end) {
            ("w:p", false) => {
                para.clear();
                style.clear();
                list = false;
            }
            ("w:pStyle", false) => style = attr(raw, "w:val").unwrap_or("").to_lowercase(),
            ("w:numPr", false) => list = true,
            ("w:t", e) => in_t = !e,
            ("w:tab", false) => para.push('\t'),
            ("w:br", false) => para.push('\n'),
            ("w:p", true) if out.len() < BLOCK_LIMIT => {
                let text = para.trim().to_string();
                if text.is_empty() {
                    return;
                }
                let kind = if style == "title" || style.ends_with("heading1") || style == "heading1" {
                    BlockKind::H1
                } else if style.contains("heading2") || style == "subtitle" {
                    BlockKind::H2
                } else if style.contains("heading") {
                    BlockKind::H3
                } else if list || style.contains("list") {
                    BlockKind::Bullet
                } else if style.contains("quote") {
                    BlockKind::Quote
                } else {
                    BlockKind::Para
                };
                let text = if kind == BlockKind::Bullet { format!("•  {text}") } else { text };
                out.push(Block { kind, text });
            }
            _ => {}
        },
    });
    Ok(out)
}

/// PowerPoint: one heading per slide (its first line), then the rest as bullets.
fn pptx(path: &Path) -> Result<Vec<Block>, String> {
    let mut z = open_zip(path)?;
    let mut slides: Vec<(u32, String)> = z
        .file_names()
        .filter_map(Result::ok)
        .filter_map(|n| {
            let num = n.strip_prefix("ppt/slides/slide")?.strip_suffix(".xml")?.parse().ok()?;
            Some((num, n.to_string()))
        })
        .collect();
    slides.sort();
    if slides.is_empty() {
        return Err("This presentation has no slides.".into());
    }
    let mut out = Vec::new();
    for (num, name) in slides {
        let Some(xml) = zip_text(&mut z, &name) else { continue };
        let mut paras: Vec<String> = Vec::new();
        let (mut cur, mut in_t) = (String::new(), false);
        walk_xml(&xml, |ev| match ev {
            Xml::Text(t) => {
                if in_t {
                    cur.push_str(t);
                }
            }
            Xml::Tag(tag, _, end) => match (tag, end) {
                ("a:p", false) => cur.clear(),
                ("a:t", e) => in_t = !e,
                ("a:br", false) => cur.push(' '),
                ("a:p", true) if !cur.trim().is_empty() => paras.push(cur.trim().to_string()),
                _ => {}
            },
        });
        let mut it = paras.into_iter();
        let title = it.next().unwrap_or_default();
        out.push(Block { kind: BlockKind::H2, text: format!("{num}.  {title}") });
        out.extend(it.map(|p| Block { kind: BlockKind::Bullet, text: format!("•  {p}") }));
        if out.len() >= BLOCK_LIMIT {
            break;
        }
    }
    Ok(out)
}

/// Column letters of a cell reference ("BC12" -> 54).
fn col_index(cell_ref: &str) -> usize {
    cell_ref
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .fold(0, |acc, c| acc * 26 + (c.to_ascii_uppercase() as usize - 'A' as usize + 1))
        .saturating_sub(1)
}

/// Excel: the first worksheet as a grid of display strings.
fn xlsx(path: &Path) -> Result<(String, Vec<Vec<String>>, bool), String> {
    let mut z = open_zip(path)?;

    let mut shared: Vec<String> = Vec::new();
    if let Some(xml) = zip_text(&mut z, "xl/sharedStrings.xml") {
        let (mut cur, mut in_t, mut in_si) = (String::new(), false, false);
        walk_xml(&xml, |ev| match ev {
            Xml::Text(t) => {
                if in_t {
                    cur.push_str(t);
                }
            }
            Xml::Tag(tag, _, end) => match (tag, end) {
                ("si", false) => {
                    in_si = true;
                    cur.clear();
                }
                ("si", true) => {
                    in_si = false;
                    shared.push(std::mem::take(&mut cur));
                }
                ("t", e) => in_t = !e && in_si,
                _ => {}
            },
        });
    }

    let mut title = String::new();
    if let Some(xml) = zip_text(&mut z, "xl/workbook.xml") {
        walk_xml(&xml, |ev| {
            if let Xml::Tag("sheet", raw, false) = ev
                && title.is_empty()
            {
                title = unescape(attr(raw, "name").unwrap_or(""));
            }
        });
    }
    let sheet_name = if z.by_name("xl/worksheets/sheet1.xml").is_ok() {
        "xl/worksheets/sheet1.xml".to_string()
    } else {
        let mut names: Vec<String> =
            z.file_names()
                .filter_map(Result::ok)
                .filter(|n| n.starts_with("xl/worksheets/sheet") && n.ends_with(".xml"))
                .map(|n| n.to_string())
                .collect();
        names.sort();
        names.into_iter().next().ok_or("This workbook has no sheets.")?
    };
    let xml = zip_text(&mut z, &sheet_name).ok_or("Couldn't read the first sheet.")?;

    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut truncated = false;
    let (mut row_idx, mut col, mut ty, mut in_v, mut val) = (0usize, 0usize, String::new(), false, String::new());
    walk_xml(&xml, |ev| match ev {
        Xml::Text(t) => {
            if in_v {
                val.push_str(t);
            }
        }
        Xml::Tag(tag, raw, end) => match (tag, end) {
            ("row", false) => {
                row_idx = attr(raw, "r").and_then(|r| r.parse::<usize>().ok()).map_or(rows.len(), |r| r - 1);
            }
            ("c", false) => {
                col = attr(raw, "r").map_or(0, col_index);
                ty = attr(raw, "t").unwrap_or("").to_string();
                val.clear();
            }
            ("v", e) | ("t", e) => in_v = !e,
            ("c", true) => {
                if row_idx >= TABLE_ROWS || col >= TABLE_COLS {
                    truncated = true;
                    return;
                }
                let shown = match ty.as_str() {
                    "s" => val.trim().parse::<usize>().ok().and_then(|i| shared.get(i)).cloned().unwrap_or_default(),
                    "b" => if val.trim() == "1" { "TRUE".into() } else { "FALSE".into() },
                    _ => tidy_number(&val),
                };
                if shown.is_empty() {
                    return;
                }
                while rows.len() <= row_idx {
                    rows.push(Vec::new());
                }
                let r = &mut rows[row_idx];
                while r.len() <= col {
                    r.push(String::new());
                }
                r[col] = shown;
            }
            _ => {}
        },
    });
    if rows.is_empty() {
        return Err("The first sheet is empty.".into());
    }
    Ok((title, rows, truncated))
}

/// "3.0000000000000004" -> "3", "0.1" stays.
fn tidy_number(v: &str) -> String {
    let v = v.trim();
    match v.parse::<f64>() {
        Ok(n) if v.len() > 12 && n.is_finite() => {
            let s = format!("{:.6}", n);
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        }
        _ => v.to_string(),
    }
}

fn csv(text: &str, sep: char) -> (Vec<Vec<String>>, bool) {
    let mut rows = Vec::new();
    for line in text.lines() {
        if rows.len() >= TABLE_ROWS {
            return (rows, true);
        }
        let mut cells = Vec::new();
        let (mut cur, mut quoted) = (String::new(), false);
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '"' if quoted && chars.peek() == Some(&'"') => {
                    cur.push('"');
                    chars.next();
                }
                '"' => quoted = !quoted,
                c if c == sep && !quoted => cells.push(std::mem::take(&mut cur)),
                c => cur.push(c),
            }
        }
        cells.push(cur);
        cells.truncate(TABLE_COLS);
        rows.push(cells);
    }
    (rows, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_blocks() {
        let b = markdown("# Title\n\nSome **bold** [link](x).\nmore\n\n- one\n- [x] done\n\n```\ncode\n```\n---\n> quote");
        let kinds: Vec<BlockKind> = b.iter().map(|b| b.kind).collect();
        assert_eq!(
            kinds,
            [BlockKind::H1, BlockKind::Para, BlockKind::Bullet, BlockKind::Bullet, BlockKind::Code, BlockKind::Rule, BlockKind::Quote]
        );
        assert_eq!(b[1].text, "Some bold link. more");
        assert_eq!(b[3].text, "☑ done");
        assert_eq!(b[4].text, "code");
    }

    #[test]
    fn csv_quotes() {
        let (rows, more) = csv("a,\"b,c\",\"say \"\"hi\"\"\"\n1,2,3", ',');
        assert!(!more);
        assert_eq!(rows[0], ["a", "b,c", "say \"hi\""]);
        assert_eq!(rows[1], ["1", "2", "3"]);
    }

    #[test]
    fn cell_columns() {
        assert_eq!(col_index("A1"), 0);
        assert_eq!(col_index("Z9"), 25);
        assert_eq!(col_index("AA3"), 26);
    }

    #[test]
    fn classify_types() {
        assert_eq!(classify("Photo.JPG"), Kind::Image);
        assert_eq!(classify("report.docx"), Kind::Word);
        assert_eq!(classify("main.rs"), Kind::Text { mono: true });
        assert_eq!(classify("notes.md"), Kind::Markdown);
        assert_eq!(classify("song.mp3"), Kind::Audio);
        assert_eq!(classify("setup.exe"), Kind::Binary);
    }
}
