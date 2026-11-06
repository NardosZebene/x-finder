//! Human-readable sizes, dates and kinds. These run only for rows that are
//! actually on screen (the model formats lazily).

use std::borrow::Cow;

use chrono::{Local, TimeZone};

use super::fs_list::{Entry, NO_TIME};

/// Finder-style decimal sizes: "Zero bytes", "812 bytes", "4 KB", "1.2 MB".
pub fn size(bytes: u64) -> String {
    match bytes {
        0 => "Zero bytes".into(),
        1 => "1 byte".into(),
        b if b < 1000 => format!("{b} bytes"),
        b => {
            let mut v = b as f64 / 1000.0;
            let mut unit = 0;
            const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
            while v >= 999.95 && unit < UNITS.len() - 1 {
                v /= 1000.0;
                unit += 1;
            }
            let s = if v < 100.0 { format!("{v:.1}") } else { format!("{v:.0}") };
            let s = s.strip_suffix(".0").unwrap_or(&s);
            format!("{s} {}", UNITS[unit])
        }
    }
}

/// "Today at 14:03", "Yesterday at 09:12", "3 Nov 2026 at 14:03".
pub fn date(secs: i64) -> String {
    if secs == NO_TIME {
        return "--".into();
    }
    let Some(dt) = Local.timestamp_opt(secs, 0).single() else {
        return "--".into();
    };
    let today = Local::now().date_naive();
    let d = dt.date_naive();
    if d == today {
        dt.format("Today at %H:%M").to_string()
    } else if Some(d) == today.pred_opt() {
        dt.format("Yesterday at %H:%M").to_string()
    } else {
        dt.format("%-d %b %Y at %H:%M").to_string()
    }
}

pub fn thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Extension of a lower-cased file name ("" if none; ".gitignore" has none).
pub fn extension(key: &str) -> &str {
    match key.rfind('.') {
        Some(p) if p > 0 => &key[p + 1..],
        _ => "",
    }
}

/// (kind label, icon id) for known extensions.
fn describe(ext: &str) -> Option<(&'static str, &'static str)> {
    Some(match ext {
        "txt" => ("Plain Text", "text"),
        "md" | "markdown" => ("Markdown Document", "text"),
        "log" => ("Log File", "text"),
        "rtf" => ("Rich Text Document", "text"),
        "ini" | "cfg" | "conf" => ("Configuration File", "text"),
        "csv" => ("CSV Document", "text"),
        "pdf" => ("PDF Document", "pdf"),
        "doc" | "docx" => ("Word Document", "office"),
        "xls" | "xlsx" => ("Excel Spreadsheet", "office"),
        "ppt" | "pptx" => ("PowerPoint Presentation", "office"),
        "odt" => ("OpenDocument Text", "office"),
        "png" => ("PNG Image", "image"),
        "jpg" | "jpeg" => ("JPEG Image", "image"),
        "gif" => ("GIF Image", "image"),
        "webp" => ("WebP Image", "image"),
        "svg" => ("SVG Image", "image"),
        "bmp" => ("BMP Image", "image"),
        "tif" | "tiff" => ("TIFF Image", "image"),
        "ico" => ("Icon", "image"),
        "heic" => ("HEIC Image", "image"),
        "psd" => ("Photoshop Document", "image"),
        "mp3" => ("MP3 Audio", "audio"),
        "wav" => ("WAV Audio", "audio"),
        "flac" => ("FLAC Audio", "audio"),
        "m4a" | "aac" => ("MPEG-4 Audio", "audio"),
        "ogg" | "opus" => ("Ogg Audio", "audio"),
        "mp4" | "m4v" => ("MPEG-4 Movie", "video"),
        "mov" => ("QuickTime Movie", "video"),
        "mkv" => ("Matroska Video", "video"),
        "avi" => ("AVI Video", "video"),
        "webm" => ("WebM Video", "video"),
        "wmv" => ("Windows Media Video", "video"),
        "mts" | "m2ts" => ("MPEG Transport Stream", "video"),
        "mpg" | "mpeg" => ("MPEG Movie", "video"),
        "3gp" => ("3GP Video", "video"),
        "zip" => ("ZIP Archive", "archive"),
        "7z" => ("7-Zip Archive", "archive"),
        "rar" => ("RAR Archive", "archive"),
        "tar" => ("Tar Archive", "archive"),
        "gz" | "tgz" => ("Gzip Archive", "archive"),
        "xz" => ("XZ Archive", "archive"),
        "iso" => ("Disk Image", "archive"),
        "exe" => ("Application", "app"),
        "msi" => ("Windows Installer", "app"),
        "dll" => ("Application Extension", "app"),
        "lnk" => ("Shortcut", "app"),
        "url" => ("Internet Shortcut", "app"),
        "rs" => ("Rust Source", "code"),
        "py" => ("Python Script", "code"),
        "js" | "mjs" | "cjs" => ("JavaScript", "code"),
        "ts" | "tsx" => ("TypeScript", "code"),
        "jsx" => ("JavaScript (JSX)", "code"),
        "json" => ("JSON", "code"),
        "toml" => ("TOML", "code"),
        "yaml" | "yml" => ("YAML", "code"),
        "xml" => ("XML Document", "code"),
        "html" | "htm" => ("HTML Document", "code"),
        "css" | "scss" => ("Style Sheet", "code"),
        "c" => ("C Source", "code"),
        "h" | "hpp" => ("C Header", "code"),
        "cpp" | "cc" | "cxx" => ("C++ Source", "code"),
        "cs" => ("C# Source", "code"),
        "java" => ("Java Source", "code"),
        "kt" => ("Kotlin Source", "code"),
        "go" => ("Go Source", "code"),
        "rb" => ("Ruby Script", "code"),
        "php" => ("PHP Script", "code"),
        "swift" => ("Swift Source", "code"),
        "sh" | "bash" | "zsh" => ("Shell Script", "code"),
        "ps1" => ("PowerShell Script", "code"),
        "bat" | "cmd" => ("Batch File", "code"),
        "sql" => ("SQL Script", "code"),
        "slint" => ("Slint File", "code"),
        "lock" => ("Lock File", "code"),
        _ => return None,
    })
}

/// ".ts" is both TypeScript and MPEG transport-stream video (TV recordings,
/// downloads). Source files are small; anything over a few MB is video.
fn describe_entry(e: &Entry) -> Option<(&'static str, &'static str)> {
    match extension(&e.key) {
        "ts" if e.size > 4 << 20 => Some(("MPEG Transport Stream", "video")),
        ext => describe(ext),
    }
}

pub fn kind_label(e: &Entry) -> Cow<'static, str> {
    if e.drive_type != 0 {
        return Cow::Borrowed(match e.drive_type {
            2 => "Removable Disk",
            3 => "Local Disk",
            4 => "Network Drive",
            5 => "CD Drive",
            6 => "RAM Disk",
            _ => "Drive",
        });
    }
    if e.is_dir {
        return Cow::Borrowed("Folder");
    }
    let ext = extension(&e.key);
    match describe_entry(e) {
        Some((label, _)) => Cow::Borrowed(label),
        None if ext.is_empty() => Cow::Borrowed("File"),
        None => Cow::Owned(format!("{} File", ext.to_uppercase())),
    }
}

pub fn icon(e: &Entry) -> &'static str {
    if e.drive_type != 0 {
        "drive"
    } else if e.is_dir {
        "folder"
    } else if e.link_dir.is_some() {
        "folderlink"
    } else {
        describe_entry(e).map(|(_, i)| i).unwrap_or("file")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(size(0), "Zero bytes");
        assert_eq!(size(812), "812 bytes");
        assert_eq!(size(4000), "4 KB");
        assert_eq!(size(1_234_567), "1.2 MB");
        assert_eq!(size(999_999), "1 MB");
        assert_eq!(size(250_000_000), "250 MB");
    }

    #[test]
    fn grouping() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(100_000), "100,000");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn extensions() {
        assert_eq!(extension("a.tar.gz"), "gz");
        assert_eq!(extension(".gitignore"), "");
        assert_eq!(extension("readme"), "");
    }
}
