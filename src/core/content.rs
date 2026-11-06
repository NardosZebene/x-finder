//! "Does this file contain …?" for `content:` searches.
//!
//! Text and code files are read directly; Word / PowerPoint / Excel are
//! unzipped and their XML text pulled out; PDFs go through a small text
//! extractor (compressed streams, fonts' ToUnicode maps, object streams).
//! Scanned PDFs (pictures of text) have no text to find.
//!
//! Matching ignores case and whitespace, so a phrase broken across lines or
//! oddly spaced in a PDF is still found.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::Path;

use super::format;

/// Files bigger than this are skipped (keeps a search responsive).
pub const MAX_BYTES: u64 = 50 * 1024 * 1024;

/// File types whose text we can read.
pub fn searchable(name: &str) -> bool {
    matches!(kind_of(name), Some(_))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Text,
    Word,
    PowerPoint,
    Excel,
    OpenDoc,
    Pdf,
}

fn kind_of(name: &str) -> Option<Kind> {
    let lower = name.to_lowercase();
    Some(match format::extension(&lower) {
        "pdf" => Kind::Pdf,
        "docx" | "docm" | "dotx" => Kind::Word,
        "pptx" | "pptm" | "potx" => Kind::PowerPoint,
        "xlsx" | "xlsm" | "xltx" => Kind::Excel,
        "odt" | "ods" | "odp" => Kind::OpenDoc,
        "txt" | "text" | "log" | "md" | "markdown" | "csv" | "tsv" | "rtf" | "nfo" | "ini" | "cfg" | "conf" | "json"
        | "jsonc" | "xml" | "yaml" | "yml" | "toml" | "html" | "htm" | "css" | "scss" | "less" | "js" | "mjs" | "cjs"
        | "ts" | "tsx" | "jsx" | "py" | "rs" | "go" | "java" | "kt" | "c" | "h" | "cpp" | "cc" | "hpp" | "cs" | "php"
        | "rb" | "swift" | "dart" | "lua" | "sql" | "sh" | "bat" | "cmd" | "ps1" | "psm1" | "vue" | "svelte" | "env"
        | "properties" | "gradle" | "srt" | "vtt" | "tex" | "svg" | "eml" | "slint" => Kind::Text,
        _ => return None,
    })
}

/// Lower-cased text with all whitespace removed.
fn squash(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).flat_map(char::to_lowercase).collect()
}

/// True when the file's text contains `needle` (case and spacing ignored).
/// Never panics on odd files: unreadable means "no match".
pub fn contains(path: &Path, needle: &str) -> bool {
    let needle = squash(needle);
    if needle.is_empty() {
        return true;
    }
    let Some(kind) = path.file_name().and_then(|n| kind_of(&n.to_string_lossy())) else { return false };
    if fs::metadata(path).map(|m| m.len() > MAX_BYTES).unwrap_or(true) {
        return false;
    }
    std::panic::catch_unwind(|| {
        let text = match kind {
            Kind::Text => read_text(path),
            Kind::Word => zip_text(path, |n| n == "word/document.xml" || n.starts_with("word/header") || n.starts_with("word/footer") || n == "word/footnotes.xml"),
            Kind::PowerPoint => zip_text(path, |n| n.starts_with("ppt/slides/slide") || n.starts_with("ppt/notesSlides/")),
            Kind::Excel => zip_text(path, |n| n == "xl/sharedStrings.xml" || n.starts_with("xl/worksheets/sheet")),
            Kind::OpenDoc => zip_text(path, |n| n == "content.xml"),
            Kind::Pdf => fs::read(path).ok().map(|b| pdf_text(&b)),
        };
        text.is_some_and(|t| squash(&t).contains(&needle))
    })
    .unwrap_or(false)
}

fn read_text(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    // UTF-16 (Notepad "Unicode") or UTF-8 / ANSI.
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        let be = bytes[0] == 0xFE;
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| if be { u16::from_be_bytes([c[0], c[1]]) } else { u16::from_le_bytes([c[0], c[1]]) })
            .collect();
        return Some(String::from_utf16_lossy(&units));
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Text of the XML parts of an Office / OpenDocument zip.
fn zip_text(path: &Path, want: impl Fn(&str) -> bool) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let mut zip = zip::ZipArchive::new(file).ok()?;
    let mut out = String::new();
    for i in 0..zip.len() {
        let Ok(mut entry) = zip.by_index(i) else { continue };
        let wanted = entry.name().map(|n| want(&n)).unwrap_or(false);
        if !wanted || entry.size() > MAX_BYTES {
            continue;
        }
        let mut xml = String::new();
        if entry.read_to_string(&mut xml).is_ok() {
            xml_text(&xml, &mut out);
            out.push(' ');
        }
    }
    Some(out)
}

/// Strips tags; paragraph ends become spaces; basic entities decoded.
fn xml_text(xml: &str, out: &mut String) {
    let mut rest = xml;
    while let Some(lt) = rest.find('<') {
        decode_entities(&rest[..lt], out);
        let Some(gt) = rest[lt..].find('>') else { return };
        let tag = &rest[lt + 1..lt + gt];
        if matches!(tag, "/w:p" | "/a:p" | "/si" | "/c" | "/text:p" | "/text:h" | "w:tab/" | "w:br/" | "/row") || tag.starts_with("w:br") {
            out.push(' ');
        }
        rest = &rest[lt + gt + 1..];
    }
    decode_entities(rest, out);
}

fn decode_entities(s: &str, out: &mut String) {
    if !s.contains('&') {
        out.push_str(s);
        return;
    }
    out.push_str(
        &s.replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&#160;", " ")
            .replace("&amp;", "&"),
    );
}

// ---- PDF ---------------------------------------------------------------------

/// A raw PDF object: its dictionary text and (decoded) stream.
struct Obj {
    dict: String,
    stream: Option<Vec<u8>>,
}

/// Best-effort text of a PDF (enough to search it).
pub fn pdf_text(data: &[u8]) -> String {
    // Encrypted PDFs: strings are scrambled, nothing to find.
    if find(data, b"/Encrypt", 0).is_some() && find(data, b"/Filter/Standard", 0).or(find(data, b"/Filter /Standard", 0)).is_some() {
        return String::new();
    }
    let mut objs = parse_objects(data);
    // PDF 1.5 object streams hold many small objects (often the fonts).
    let packed: Vec<(u32, Vec<u8>, String)> = objs
        .iter()
        .filter(|(_, o)| o.dict.contains("/ObjStm"))
        .filter_map(|(n, o)| Some((*n, o.stream.clone()?, o.dict.clone())))
        .collect();
    for (_, stream, dict) in packed {
        for (num, text) in unpack_objstm(&stream, &dict) {
            objs.entry(num).or_insert(Obj { dict: text, stream: None });
        }
    }

    // Font resource name (/F1) -> font object -> ToUnicode map.
    let mut font_maps: HashMap<u32, CMap> = HashMap::new();
    for (num, o) in &objs {
        if o.dict.contains("/Font") || o.dict.contains("/ToUnicode") {
            if let Some(r) = reference_after(&o.dict, "/ToUnicode")
                && let Some(cm) = objs.get(&r).and_then(|c| c.stream.as_ref())
            {
                font_maps.insert(*num, parse_cmap(cm));
            }
        }
    }
    let mut names: HashMap<String, u32> = HashMap::new();
    for o in objs.values() {
        collect_font_names(&o.dict, &mut names);
    }

    let mut out = String::new();
    let mut nums: Vec<&u32> = objs.keys().collect();
    nums.sort();
    for n in nums {
        let o = &objs[n];
        let Some(stream) = &o.stream else { continue };
        if o.dict.contains("/Image") || o.dict.contains("/ObjStm") || o.dict.contains("/XRef") || o.dict.contains("/FontFile") {
            continue;
        }
        if stream.windows(2).any(|w| w == b"BT") {
            content_text(stream, &names, &font_maps, &mut out);
            out.push('\n');
        }
    }
    out
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from >= hay.len() {
        return None;
    }
    hay[from..].windows(needle.len()).position(|w| w == needle).map(|p| p + from)
}

/// `N 0 obj << … >> stream … endstream endobj` for every object.
fn parse_objects(data: &[u8]) -> HashMap<u32, Obj> {
    let mut objs = HashMap::new();
    let mut pos = 0;
    while let Some(at) = find(data, b" obj", pos) {
        pos = at + 4;
        // Object number: digits before " 0 obj".
        let head = &data[at.saturating_sub(24)..at];
        let head = String::from_utf8_lossy(head);
        let mut parts = head.split_whitespace().rev();
        let (Some(_gen), Some(num)) = (parts.next(), parts.next()) else { continue };
        let Ok(num) = num.trim_start_matches(|c: char| !c.is_ascii_digit()).parse::<u32>() else { continue };
        let end = find(data, b"endobj", pos).unwrap_or(data.len());
        let body = &data[pos..end];
        let (dict, stream) = match find(body, b"stream", 0) {
            Some(s) if !body[..s].ends_with(b"end") => {
                let dict = String::from_utf8_lossy(&body[..s]).into_owned();
                let mut start = s + 6;
                if body.get(start) == Some(&b'\r') {
                    start += 1;
                }
                if body.get(start) == Some(&b'\n') {
                    start += 1;
                }
                let stop = find(body, b"endstream", start).unwrap_or(body.len());
                let raw = &body[start..stop];
                (dict.clone(), Some(decode_stream(&dict, raw)))
            }
            _ => (String::from_utf8_lossy(body).into_owned(), None),
        };
        objs.insert(num, Obj { dict, stream });
        pos = end;
    }
    objs
}

fn decode_stream(dict: &str, raw: &[u8]) -> Vec<u8> {
    if dict.contains("/FlateDecode") || dict.contains("/Fl ") || dict.contains("/Fl/") {
        let mut out = Vec::new();
        let mut z = flate2::read::ZlibDecoder::new(raw);
        if z.read_to_end(&mut out).is_ok() || !out.is_empty() {
            return out;
        }
        return Vec::new();
    }
    if dict.contains("Decode") {
        // DCT/JPX images, LZW, … : not text.
        return Vec::new();
    }
    raw.to_vec()
}

fn unpack_objstm(stream: &[u8], dict: &str) -> Vec<(u32, String)> {
    let int_after = |key: &str| -> Option<usize> {
        let i = dict.find(key)? + key.len();
        dict[i..].trim_start().split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
    };
    let (Some(n), Some(first)) = (int_after("/N"), int_after("/First")) else { return Vec::new() };
    if first > stream.len() {
        return Vec::new();
    }
    let header = String::from_utf8_lossy(&stream[..first]);
    let nums: Vec<usize> = header.split_whitespace().filter_map(|t| t.parse().ok()).collect();
    let mut out = Vec::new();
    for k in 0..n.min(nums.len() / 2) {
        let (num, off) = (nums[2 * k], nums[2 * k + 1]);
        let next = nums.get(2 * k + 3).copied().map_or(stream.len(), |o| first + o);
        let start = first + off;
        if start < next && next <= stream.len() {
            out.push((num as u32, String::from_utf8_lossy(&stream[start..next]).into_owned()));
        }
    }
    out
}

/// `/Key 12 0 R` → 12
fn reference_after(dict: &str, key: &str) -> Option<u32> {
    let i = dict.find(key)? + key.len();
    let mut it = dict[i..].split_whitespace();
    let num = it.next()?.parse().ok()?;
    let _gen = it.next()?;
    (it.next()?.starts_with('R')).then_some(num)
}

/// Finds `/Font << /F1 12 0 R /F2 13 0 R >>` and records name -> object.
fn collect_font_names(dict: &str, names: &mut HashMap<String, u32>) {
    let mut rest = dict;
    while let Some(i) = rest.find("/Font") {
        rest = &rest[i + 5..];
        let r = rest.trim_start();
        let Some(body) = r.strip_prefix("<<") else { continue };
        let Some(end) = body.find(">>") else { continue };
        let inner = &body[..end];
        let toks: Vec<&str> = inner.split(|c: char| c.is_whitespace() || c == '/').filter(|t| !t.is_empty()).collect();
        // name num gen R
        let mut k = 0;
        while k + 3 < toks.len() + 1 && k + 2 < toks.len() {
            if let (Ok(num), true) = (toks[k + 1].parse::<u32>(), toks.get(k + 3).is_some_and(|t| t.starts_with('R'))) {
                names.insert(toks[k].to_string(), num);
                k += 4;
            } else {
                k += 1;
            }
        }
    }
}

/// Character code -> text, from a ToUnicode CMap.
#[derive(Default)]
struct CMap {
    map: HashMap<u32, String>,
    /// Bytes per code (1 for simple fonts, 2 for CID fonts).
    width: usize,
}

fn hex_bytes(h: &str) -> Vec<u8> {
    let digits: Vec<u8> = h.bytes().filter(u8::is_ascii_hexdigit).collect();
    digits
        .chunks(2)
        .map(|c| {
            let s = std::str::from_utf8(c).unwrap_or("0");
            u8::from_str_radix(&format!("{s:0<2}"), 16).unwrap_or(0)
        })
        .collect()
}

fn utf16_text(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes.chunks(2).map(|c| u16::from_be_bytes([c[0], *c.get(1).unwrap_or(&0)])).collect();
    String::from_utf16_lossy(&units)
}

fn parse_cmap(data: &[u8]) -> CMap {
    let text = String::from_utf8_lossy(data);
    let mut cm = CMap { width: 1, ..Default::default() };
    // codespace width
    if let Some(i) = text.find("begincodespacerange")
        && let Some(lt) = text[i..].find('<')
        && let Some(gt) = text[i + lt..].find('>')
    {
        cm.width = (text[i + lt + 1..i + lt + gt].trim().len() / 2).max(1);
    }
    let hexes = |s: &str| -> Vec<String> {
        let mut v = Vec::new();
        let mut rest = s;
        while let Some(a) = rest.find('<') {
            let Some(b) = rest[a..].find('>') else { break };
            v.push(rest[a + 1..a + b].to_string());
            rest = &rest[a + b + 1..];
        }
        v
    };
    let code = |h: &str| hex_bytes(h).iter().fold(0u32, |acc, b| (acc << 8) | *b as u32);
    let mut rest: &str = &text;
    while let Some(i) = rest.find("beginbfchar") {
        let body = &rest[i + 11..];
        let end = body.find("endbfchar").unwrap_or(body.len());
        let h = hexes(&body[..end]);
        for pair in h.chunks(2) {
            if let [src, dst] = pair {
                cm.map.insert(code(src), utf16_text(&hex_bytes(dst)));
            }
        }
        rest = &body[end..];
    }
    let mut rest: &str = &text;
    while let Some(i) = rest.find("beginbfrange") {
        let body = &rest[i + 12..];
        let end = body.find("endbfrange").unwrap_or(body.len());
        for line in body[..end].lines() {
            let h = hexes(line);
            if h.len() >= 3 {
                let (lo, hi) = (code(&h[0]), code(&h[1]));
                if hi < lo || hi - lo > 0xFFFF {
                    continue;
                }
                if line.contains('[') {
                    for (k, dst) in h[2..].iter().enumerate() {
                        cm.map.insert(lo + k as u32, utf16_text(&hex_bytes(dst)));
                    }
                } else {
                    let base = hex_bytes(&h[2]);
                    for (k, c) in (lo..=hi).enumerate() {
                        let mut b = base.clone();
                        if let Some(last) = b.last_mut() {
                            *last = last.wrapping_add(k as u8);
                        }
                        cm.map.insert(c, utf16_text(&b));
                    }
                }
            }
        }
        rest = &body[end..];
    }
    cm
}

/// Text shown by a content stream (Tj, TJ, ', ") using each font's map.
fn content_text(stream: &[u8], names: &HashMap<String, u32>, maps: &HashMap<u32, CMap>, out: &mut String) {
    let mut font: Option<&CMap> = None;
    let mut last_name = String::new();
    let mut i = 0;
    let n = stream.len();
    while i < n {
        let c = stream[i];
        match c {
            b'%' => {
                while i < n && stream[i] != b'\n' && stream[i] != b'\r' {
                    i += 1;
                }
            }
            b'/' => {
                let start = i + 1;
                i += 1;
                while i < n && !b" \t\r\n/[]()<>{}%".contains(&stream[i]) {
                    i += 1;
                }
                last_name = String::from_utf8_lossy(&stream[start..i]).into_owned();
                continue;
            }
            b'(' => {
                let (bytes, next) = literal(stream, i);
                decode_into(&bytes, font, out);
                i = next;
                continue;
            }
            b'<' if stream.get(i + 1) != Some(&b'<') => {
                let end = stream[i..].iter().position(|&b| b == b'>').map_or(n, |p| i + p);
                let bytes = hex_bytes(&String::from_utf8_lossy(&stream[i + 1..end]));
                decode_into(&bytes, font, out);
                i = end + 1;
                continue;
            }
            b'-' | b'0'..=b'9' | b'.' => {
                // Big negative kerning inside TJ arrays is a word gap.
                let start = i;
                i += 1;
                while i < n && (stream[i].is_ascii_digit() || stream[i] == b'.') {
                    i += 1;
                }
                if let Ok(v) = std::str::from_utf8(&stream[start..i]).unwrap_or("0").parse::<f32>()
                    && v < -180.0
                {
                    out.push(' ');
                }
                continue;
            }
            b'T' if i + 1 < n => {
                match stream[i + 1] {
                    // Tf: switch font.
                    b'f' => font = names.get(&last_name).and_then(|o| maps.get(o)),
                    // Td TD T* Tm: moved to a new spot or line.
                    b'd' | b'D' | b'*' | b'm' => out.push(' '),
                    _ => {}
                }
                i += 2;
                continue;
            }
            b'\'' | b'"' => out.push(' '),
            b'E' if stream.get(i + 1) == Some(&b'T') => out.push(' '),
            _ => {}
        }
        i += 1;
    }
}

/// A `( … )` string with escapes and nested parentheses.
fn literal(s: &[u8], open: usize) -> (Vec<u8>, usize) {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut i = open;
    while i < s.len() {
        let c = s[i];
        match c {
            b'(' => {
                if depth > 0 {
                    out.push(c);
                }
                depth += 1;
            }
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return (out, i + 1);
                }
                out.push(c);
            }
            b'\\' if i + 1 < s.len() => {
                i += 1;
                match s[i] {
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'b' | b'f' => {}
                    d @ b'0'..=b'7' => {
                        let mut v = (d - b'0') as u32;
                        for _ in 0..2 {
                            if i + 1 < s.len() && (b'0'..=b'7').contains(&s[i + 1]) {
                                i += 1;
                                v = v * 8 + (s[i] - b'0') as u32;
                            }
                        }
                        out.push(v as u8);
                    }
                    b'\r' | b'\n' => {}
                    other => out.push(other),
                }
            }
            _ => out.push(c),
        }
        i += 1;
    }
    (out, s.len())
}

fn decode_into(bytes: &[u8], font: Option<&CMap>, out: &mut String) {
    match font {
        Some(cm) if !cm.map.is_empty() => {
            for chunk in bytes.chunks(cm.width.max(1)) {
                let code = chunk.iter().fold(0u32, |acc, b| (acc << 8) | *b as u32);
                match cm.map.get(&code) {
                    Some(t) => out.push_str(t),
                    None if cm.width == 1 => out.push(*chunk.first().unwrap_or(&b' ') as char),
                    None => {}
                }
            }
        }
        // Simple fonts: bytes are (close enough to) Latin-1 / WinAnsi.
        _ => out.extend(bytes.iter().map(|&b| b as char)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny hand-written PDF with a compressed page and a CID font.
    fn sample_pdf() -> Vec<u8> {
        use std::io::Write;
        let content = b"BT /F1 12 Tf 72 700 Td (Project) Tj [(Ce) -20 (dar) -400 (budget)] TJ ET BT /F2 10 Tf <00480049> Tj ET";
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(content).unwrap();
        let packed = z.finish().unwrap();
        let cmap = b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n1 begincodespacerange <0000> <FFFF> endcodespacerange\n2 beginbfchar <0048> <0048> <0049> <0069> endbfchar\nendcmap";
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n1 0 obj << /Type /Page /Resources << /Font << /F1 3 0 R /F2 4 0 R >> >> /Contents 2 0 R >> endobj\n");
        pdf.extend_from_slice(format!("2 0 obj << /Length {} /Filter /FlateDecode >> stream\n", packed.len()).as_bytes());
        pdf.extend_from_slice(&packed);
        pdf.extend_from_slice(b"\nendstream endobj\n3 0 obj << /Type /Font /BaseFont /Helvetica >> endobj\n");
        pdf.extend_from_slice(b"4 0 obj << /Type /Font /Subtype /Type0 /ToUnicode 5 0 R >> endobj\n");
        pdf.extend_from_slice(format!("5 0 obj << /Length {} >> stream\n", cmap.len()).as_bytes());
        pdf.extend_from_slice(cmap);
        pdf.extend_from_slice(b"\nendstream endobj\ntrailer << /Root 1 0 R >>\n%%EOF");
        pdf
    }

    #[test]
    fn pdf_text_with_kerning_and_cid_fonts() {
        let text = pdf_text(&sample_pdf());
        let flat = squash(&text);
        assert!(flat.contains("projectcedar"), "{text}");
        assert!(flat.contains("budget"), "{text}");
        assert!(flat.contains("hi"), "{text}");
    }

    #[test]
    fn xml_and_text() {
        let mut out = String::new();
        xml_text("<w:p><w:r><w:t>Project </w:t></w:r><w:r><w:t>Ce</w:t></w:r><w:r><w:t>dar &amp; co</w:t></w:r></w:p>", &mut out);
        assert!(squash(&out).contains("projectcedar&co"));
        let dir = std::env::temp_dir().join("finder-rs-content-test");
        let _ = fs::create_dir_all(&dir);
        let f = dir.join("notes.txt");
        fs::write(&f, "Meeting about Project\nCedar next week").unwrap();
        assert!(contains(&f, "project cedar"));
        assert!(!contains(&f, "project oak"));
        let p = dir.join("doc.pdf");
        fs::write(&p, sample_pdf()).unwrap();
        assert!(contains(&p, "Project Cedar"));
        assert!(!contains(&dir.join("photo.jpg"), "x"));
    }

    #[test]
    fn garbage_never_panics() {
        for junk in [&b"%PDF-1.7 1 0 obj << /Filter /FlateDecode >> stream\nxx"[..], b"0 obj", b"( ((", b"<< /Font << /F1"] {
            let _ = pdf_text(junk);
        }
    }
}

/// Manual check over real files: `FINDER_PDF_DIR=… cargo test pdf_survey -- --ignored --nocapture`
#[cfg(test)]
#[test]
#[ignore]
fn pdf_survey() {
    let Some(dir) = std::env::var_os("FINDER_PDF_DIR") else { return };
    let (mut total, mut with_text) = (0, 0);
    let t = std::time::Instant::now();
    for e in fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("pdf")) {
            total += 1;
            let text = fs::read(&p).map(|b| pdf_text(&b)).unwrap_or_default();
            let words = text.split_whitespace().filter(|w| w.chars().filter(|c| c.is_alphabetic()).count() >= 3).count();
            if words > 20 {
                with_text += 1;
            }
            let sample: String = text.split_whitespace().take(12).collect::<Vec<_>>().join(" ");
            println!("{:>6} words | {} | {}", words, p.file_name().unwrap().to_string_lossy(), sample.chars().take(90).collect::<String>());
        }
    }
    println!("{with_text}/{total} PDFs with text in {:?}", t.elapsed());
}

/// FINDER_CONTENT_FILES="a.pptx;b.docx" FINDER_CONTENT_WORD=invoice cargo test office_survey -- --ignored --nocapture
#[cfg(test)]
#[test]
#[ignore]
fn office_survey() {
    let word = std::env::var("FINDER_CONTENT_WORD").unwrap_or_default();
    for p in std::env::var("FINDER_CONTENT_FILES").unwrap_or_default().split(';').filter(|s| !s.is_empty()) {
        let t = std::time::Instant::now();
        println!("{p}: {} in {:?}", contains(Path::new(p), &word), t.elapsed());
    }
}
