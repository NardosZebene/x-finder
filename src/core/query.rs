//! Filter / search queries typed in the toolbar box.
//!
//!   report               name contains "report"
//!   pdf:                 PDF files            (any extension followed by ':')
//!   pdf: invoice         PDFs named like "invoice"
//!   ext:jpg,png          those extensions
//!   kind:image           image | video | audio | document | code | archive | app | folder
//!   size:>5mb  size:<1kb
//!   modified:today | yesterday | week | month | year
//!   content:"project cedar"   text inside the files (Enter-search only; needs
//!                             a type such as pdf: or kind:document)
//!   "two words"          quotes keep a phrase together

use chrono::{Duration, Local, TimeZone};

use super::format;
use super::fs_list::Entry;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Query {
    /// Lower-cased words that must all appear in the name.
    pub words: Vec<String>,
    pub exts: Vec<String>,
    pub kind: Option<String>,
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    /// Unix seconds.
    pub modified_after: Option<i64>,
    /// Lower-cased text that must appear inside the file (`content:`).
    pub content: Option<String>,
}

/// Splits on whitespace, keeping "quoted phrases" (also after `key:`)
/// together, without the quotes.
fn tokens(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in input.chars() {
        match c {
            '"' | '\u{201C}' | '\u{201D}' => quoted = !quoted,
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

const KINDS: [&str; 8] = ["image", "video", "audio", "document", "code", "archive", "app", "folder"];

fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim().to_lowercase();
    let (num, mult) = [("tb", 1u64 << 40), ("gb", 1 << 30), ("mb", 1 << 20), ("kb", 1 << 10), ("b", 1)]
        .iter()
        .find_map(|(suf, m)| s.strip_suffix(suf).map(|n| (n.to_string(), *m)))
        .unwrap_or((s.clone(), 1));
    num.trim().parse::<f64>().ok().map(|n| (n * mult as f64) as u64)
}

impl Query {
    pub fn parse(input: &str) -> Query {
        let mut q = Query::default();
        let mut toks = tokens(input).into_iter().peekable();
        while let Some(token) = toks.next() {
            let mut lower = token.to_lowercase();
            // "content: \"trading\"", "kind: image", "size: >5mb": a space
            // after a known key still gives it the next word as its value.
            if let Some(key) = lower.strip_suffix(':')
                && matches!(key, "content" | "text" | "contains" | "ext" | "type" | "kind" | "size" | "modified" | "date")
                && let Some(next) = toks.next_if(|t| !t.contains(':') || t.starts_with('>') || t.starts_with('<'))
            {
                lower.push_str(&next.to_lowercase());
            }
            if let Some((key, value)) = lower.split_once(':') {
                match key {
                    "content" | "text" | "contains" => {
                        let v = value.trim();
                        if !v.is_empty() {
                            q.content = Some(v.to_string());
                        }
                    }
                    "ext" | "type" => q.exts.extend(value.split(',').filter(|v| !v.is_empty()).map(|v| v.trim_start_matches('.').to_string())),
                    "kind" if KINDS.contains(&value) || value == "doc" || value == "docs" => {
                        q.kind = Some(if value.starts_with("doc") { "document".into() } else { value.into() })
                    }
                    "size" => {
                        if let Some(v) = value.strip_prefix('>') {
                            q.min_size = parse_size(v);
                        } else if let Some(v) = value.strip_prefix('<') {
                            q.max_size = parse_size(v);
                        }
                    }
                    "modified" | "date" => {
                        let today = Local::now().date_naive();
                        let start = match value {
                            "today" => Some(today),
                            "yesterday" => today.pred_opt(),
                            "week" => Some(today - Duration::days(7)),
                            "month" => Some(today - Duration::days(31)),
                            "year" => Some(today - Duration::days(365)),
                            _ => None,
                        };
                        q.modified_after = start
                            .and_then(|d| d.and_hms_opt(0, 0, 0))
                            .and_then(|dt| Local.from_local_datetime(&dt).single())
                            .map(|dt| dt.timestamp());
                    }
                    // "pdf:" (or "pdf:invoice") = that file type.
                    ext if value.is_empty() || !ext.is_empty() && ext.chars().all(|c| c.is_ascii_alphanumeric()) => {
                        q.exts.push(ext.to_string());
                        if !value.is_empty() {
                            q.words.push(value.to_string());
                        }
                    }
                    _ => q.words.push(lower),
                }
            } else {
                q.words.push(lower);
            }
        }
        q
    }

    /// Content search without a file type would read every file.
    pub fn content_needs_type(&self) -> bool {
        self.content.is_some() && self.exts.is_empty() && self.kind.is_none()
    }

    pub fn is_empty(&self) -> bool {
        *self == Query::default()
    }

    /// Plain text only (no type/size/date conditions).
    pub fn is_plain(&self) -> bool {
        self.content.is_none()
            && self.exts.is_empty() && self.kind.is_none() && self.min_size.is_none() && self.max_size.is_none() && self.modified_after.is_none()
    }

    pub fn matches(&self, e: &Entry) -> bool {
        if !self.words.iter().all(|w| e.key.contains(w.as_str())) {
            return false;
        }
        if !self.exts.is_empty() && (e.is_dir || !self.exts.iter().any(|x| format::extension(&e.key) == x)) {
            return false;
        }
        if let Some(kind) = &self.kind {
            let icon = format::icon(e);
            let ok = match kind.as_str() {
                "folder" => e.is_dir,
                "document" => matches!(icon, "pdf" | "office" | "text"),
                k => icon == k,
            };
            if !ok {
                return false;
            }
        }
        if self.min_size.is_some_and(|m| e.is_dir || e.size < m) || self.max_size.is_some_and(|m| e.is_dir || e.size > m) {
            return false;
        }
        if self.modified_after.is_some_and(|t| e.modified < t) {
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, size: u64) -> Entry {
        Entry::new(name.into(), false, false, size, 0)
    }

    #[test]
    fn type_shortcuts() {
        let q = Query::parse("pdf:");
        assert!(q.matches(&file("Report.PDF", 10)));
        assert!(!q.matches(&file("notes.txt", 10)));
        let q = Query::parse("pdf: invoice");
        assert!(q.matches(&file("Invoice-7.pdf", 1)));
        assert!(!q.matches(&file("Report.pdf", 1)));
        let q = Query::parse("ext:jpg,png");
        assert!(q.matches(&file("a.png", 1)) && q.matches(&file("b.JPG", 1)) && !q.matches(&file("c.gif", 1)));
    }

    #[test]
    fn kinds_and_sizes() {
        assert!(Query::parse("kind:image").matches(&file("x.jpeg", 1)));
        assert!(Query::parse("kind:doc").matches(&file("x.docx", 1)));
        let big = Query::parse("size:>5mb");
        assert!(big.matches(&file("a.bin", 6 << 20)) && !big.matches(&file("b.bin", 4 << 20)));
        assert!(Query::parse("report").is_plain());
        assert!(!Query::parse("pdf:").is_plain());
    }

    #[test]
    fn content_queries() {
        let q = Query::parse("pdf: content:\"Project Cedar\" budget");
        assert_eq!(q.exts, vec!["pdf"]);
        assert_eq!(q.content.as_deref(), Some("project cedar"));
        assert_eq!(q.words, vec!["budget"]);
        assert!(!q.content_needs_type());
        assert!(Query::parse("content:cedar").content_needs_type());
        assert!(!Query::parse("kind:document content:cedar").content_needs_type());
        assert_eq!(Query::parse("\"annual report\"").words, vec!["annual report"]);
        // A space after the key, as people often type it.
        let q = Query::parse("md: content: \"Your Expectations\"");
        assert_eq!(q.exts, vec!["md"]);
        assert_eq!(q.content.as_deref(), Some("your expectations"));
        assert!(q.words.is_empty());
        assert_eq!(Query::parse("kind: image").kind.as_deref(), Some("image"));
        assert_eq!(Query::parse("size: >5mb").min_size, Some(5 << 20));
    }
}
