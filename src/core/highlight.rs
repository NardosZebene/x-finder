//! Small, dependency-free syntax highlighter for code previews.
//!
//! Not a parser: a per-language lexer that knows comments, strings,
//! numbers, keywords, types, function calls and (for HTML/XML) tags and
//! attributes. Good enough to make a preview readable, and fast: it is one
//! pass over at most the previewed lines.

/// What a piece of a line is; the UI picks a colour per kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Tok {
    Plain = 0,
    Keyword = 1,
    Str = 2,
    Comment = 3,
    Number = 4,
    Type = 5,
    Function = 6,
    Tag = 7,
    Attr = 8,
    Constant = 9,
}

/// One coloured piece of a line.
pub type Span = (Tok, String);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Family {
    /// C, C++, Java, C#, JS/TS, Rust, Go, Kotlin, Swift, PHP, Dart, Scala…
    CLike,
    Python,
    Shell,
    PowerShell,
    Sql,
    Markup,
    Css,
    Json,
    /// YAML, TOML, INI
    Config,
    Lua,
}

pub struct Lang {
    family: Family,
    keywords: &'static [&'static str],
    constants: &'static [&'static str],
    types: &'static [&'static str],
    /// Capitalised identifiers are types (Java, C#, Rust, Swift, TS…).
    caps_are_types: bool,
    /// Backtick strings that may span lines (JS/TS template, Go raw).
    backtick: bool,
    /// `#include` style directives (C/C++/C#).
    preprocessor: bool,
    /// Rust: `'a` is a lifetime, not a char.
    lifetimes: bool,
    /// `$name` is a variable (PHP).
    dollar_vars: bool,
}

const C_KW: &[&str] = &[
    "auto", "break", "case", "const", "continue", "default", "do", "else", "enum", "extern", "for", "goto", "if",
    "inline", "register", "restrict", "return", "sizeof", "static", "struct", "switch", "typedef", "union",
    "volatile", "while", "class", "namespace", "template", "typename", "public", "private", "protected", "virtual",
    "override", "final", "new", "delete", "this", "operator", "friend", "using", "try", "catch", "throw",
    "noexcept", "constexpr", "consteval", "static_cast", "dynamic_cast", "reinterpret_cast", "const_cast",
    "explicit", "mutable", "co_await", "co_return", "co_yield", "concept", "requires", "decltype", "alignas",
];
const C_TYPES: &[&str] = &[
    "void", "char", "short", "int", "long", "float", "double", "signed", "unsigned", "bool", "size_t", "wchar_t",
    "int8_t", "int16_t", "int32_t", "int64_t", "uint8_t", "uint16_t", "uint32_t", "uint64_t", "string", "vector",
    "map", "auto", "std",
];
const C_CONST: &[&str] = &["true", "false", "NULL", "nullptr"];

const JAVA_KW: &[&str] = &[
    "abstract", "assert", "break", "case", "catch", "class", "continue", "default", "do", "else", "enum", "extends",
    "final", "finally", "for", "if", "implements", "import", "instanceof", "interface", "native", "new", "package",
    "private", "protected", "public", "return", "static", "strictfp", "super", "switch", "synchronized", "this",
    "throw", "throws", "transient", "try", "volatile", "while", "var", "record", "sealed", "permits", "yield",
    // C#
    "namespace", "using", "internal", "readonly", "override", "virtual", "async", "await", "get", "set", "out",
    "ref", "in", "is", "as", "base", "foreach", "lock", "params", "partial", "where", "event", "delegate",
    "operator", "struct", "sizeof", "typeof", "unchecked", "checked", "fixed", "unsafe", "init", "required",
    // Kotlin / Scala / Dart / Swift
    "fun", "val", "when", "object", "companion", "data", "open", "lateinit", "suspend", "def", "trait", "match",
    "case", "func", "let", "guard", "extension", "protocol", "self", "Self", "mut", "late", "factory", "mixin",
];
const JAVA_TYPES: &[&str] = &[
    "boolean", "byte", "char", "short", "int", "long", "float", "double", "void", "String", "Object", "Integer",
    "string", "decimal", "object", "dynamic", "uint", "ulong", "ushort", "sbyte", "Int", "Double", "Bool", "Unit",
    "Any", "List", "Map", "Set",
];
const JAVA_CONST: &[&str] = &["true", "false", "null", "nil", "undefined"];

const JS_KW: &[&str] = &[
    "async", "await", "break", "case", "catch", "class", "const", "continue", "debugger", "default", "delete", "do",
    "else", "export", "extends", "finally", "for", "from", "function", "if", "import", "in", "instanceof", "let",
    "new", "of", "return", "static", "super", "switch", "this", "throw", "try", "typeof", "var", "void", "while",
    "with", "yield", "as", "type", "interface", "enum", "implements", "private", "public", "protected", "readonly",
    "declare", "abstract", "namespace", "keyof", "satisfies", "get", "set",
];
const JS_TYPES: &[&str] = &[
    "string", "number", "boolean", "any", "unknown", "never", "object", "symbol", "bigint", "Promise", "Array",
    "Record", "Partial", "Map", "Set",
];
const JS_CONST: &[&str] = &["true", "false", "null", "undefined", "NaN", "Infinity"];

const RUST_KW: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "fn", "for",
    "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self", "Self",
    "static", "struct", "super", "trait", "type", "unsafe", "use", "where", "while", "macro_rules",
];
const RUST_TYPES: &[&str] = &[
    "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize", "f32", "f64", "bool",
    "char", "str",
];
const RUST_CONST: &[&str] = &["true", "false", "None", "Some", "Ok", "Err"];

const GO_KW: &[&str] = &[
    "break", "case", "chan", "const", "continue", "default", "defer", "else", "fallthrough", "for", "func", "go",
    "goto", "if", "import", "interface", "map", "package", "range", "return", "select", "struct", "switch", "type",
    "var",
];
const GO_TYPES: &[&str] = &[
    "bool", "byte", "complex64", "complex128", "error", "float32", "float64", "int", "int8", "int16", "int32",
    "int64", "rune", "string", "uint", "uint8", "uint16", "uint32", "uint64", "uintptr", "any",
];
const GO_CONST: &[&str] = &["true", "false", "nil", "iota"];

const PHP_KW: &[&str] = &[
    "abstract", "and", "as", "break", "case", "catch", "class", "clone", "const", "continue", "declare", "default",
    "do", "echo", "else", "elseif", "empty", "endif", "endforeach", "endwhile", "extends", "final", "finally", "fn",
    "for", "foreach", "function", "global", "if", "implements", "include", "include_once", "instanceof",
    "interface", "isset", "list", "match", "namespace", "new", "or", "print", "private", "protected", "public",
    "require", "require_once", "return", "static", "switch", "throw", "trait", "try", "unset", "use", "var",
    "while", "yield",
];

const PY_KW: &[&str] = &[
    "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif", "else", "except",
    "finally", "for", "from", "global", "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass",
    "raise", "return", "try", "while", "with", "yield", "match", "case",
];
const PY_TYPES: &[&str] = &["int", "float", "str", "bool", "list", "dict", "set", "tuple", "bytes", "object", "type"];
const PY_CONST: &[&str] = &["True", "False", "None", "self", "cls"];

const SH_KW: &[&str] = &[
    "if", "then", "else", "elif", "fi", "for", "while", "until", "do", "done", "case", "esac", "in", "function",
    "return", "exit", "export", "local", "readonly", "source", "echo", "cd", "set", "unset", "shift", "break",
    "continue", "sudo", "alias",
];
const PS_KW: &[&str] = &[
    "begin", "break", "catch", "class", "continue", "data", "do", "dynamicparam", "else", "elseif", "end", "exit",
    "filter", "finally", "for", "foreach", "from", "function", "if", "in", "param", "process", "return", "switch",
    "throw", "trap", "try", "until", "using", "var", "while", "-eq", "-ne", "-gt", "-lt", "-ge", "-le", "-like",
    "-notlike", "-match", "-and", "-or", "-not",
];
const PS_CONST: &[&str] = &["$true", "$false", "$null"];

const SQL_KW: &[&str] = &[
    "select", "from", "where", "insert", "into", "values", "update", "set", "delete", "create", "table", "drop",
    "alter", "add", "index", "view", "join", "inner", "left", "right", "outer", "full", "on", "and", "or", "not",
    "null", "is", "in", "as", "order", "by", "group", "having", "limit", "offset", "distinct", "union", "all",
    "primary", "key", "foreign", "references", "default", "unique", "constraint", "case", "when", "then", "else",
    "end", "exists", "between", "like", "begin", "commit", "rollback", "transaction", "procedure", "function",
    "returns", "return", "declare", "if", "with", "top", "asc", "desc", "count", "sum", "avg", "min", "max",
];
const SQL_TYPES: &[&str] = &[
    "int", "integer", "bigint", "smallint", "varchar", "nvarchar", "char", "text", "date", "datetime", "timestamp",
    "decimal", "numeric", "float", "real", "boolean", "bool", "blob", "uuid", "serial", "money", "bit",
];

const CSS_KW: &[&str] = &["!important", "@media", "@import", "@keyframes", "@font-face", "@supports", "@layer"];
const LUA_KW: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "for", "function", "goto", "if", "in", "local", "not", "or",
    "repeat", "return", "then", "until", "while",
];
const CFG_CONST: &[&str] = &["true", "false", "yes", "no", "on", "off", "null", "~"];

const fn lang(family: Family, keywords: &'static [&'static str], constants: &'static [&'static str], types: &'static [&'static str]) -> Lang {
    Lang {
        family,
        keywords,
        constants,
        types,
        caps_are_types: false,
        backtick: false,
        preprocessor: false,
        lifetimes: false,
        dollar_vars: false,
    }
}

/// The language for a file extension (lower case, without the dot).
pub fn for_extension(ext: &str) -> Option<Lang> {
    use Family::*;
    let l = match ext {
        "c" | "h" | "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" | "ino" | "cu" => {
            Lang { preprocessor: true, ..lang(CLike, C_KW, C_CONST, C_TYPES) }
        }
        "java" | "kt" | "kts" | "scala" | "dart" | "swift" | "groovy" | "gradle" => {
            Lang { caps_are_types: true, ..lang(CLike, JAVA_KW, JAVA_CONST, JAVA_TYPES) }
        }
        "cs" => Lang { caps_are_types: true, preprocessor: true, ..lang(CLike, JAVA_KW, JAVA_CONST, JAVA_TYPES) },
        "js" | "mjs" | "cjs" | "jsx" | "ts" | "tsx" | "mts" | "cts" => {
            Lang { caps_are_types: true, backtick: true, ..lang(CLike, JS_KW, JS_CONST, JS_TYPES) }
        }
        "rs" => Lang { caps_are_types: true, lifetimes: true, ..lang(CLike, RUST_KW, RUST_CONST, RUST_TYPES) },
        "go" => Lang { backtick: true, ..lang(CLike, GO_KW, GO_CONST, GO_TYPES) },
        "php" => Lang { dollar_vars: true, caps_are_types: true, ..lang(CLike, PHP_KW, JAVA_CONST, &[]) },
        "slint" | "qml" => Lang { caps_are_types: true, ..lang(CLike, JS_KW, JS_CONST, JS_TYPES) },
        "py" | "pyw" | "pyi" => lang(Python, PY_KW, PY_CONST, PY_TYPES),
        "sh" | "bash" | "zsh" | "fish" | "bat" | "cmd" => lang(Shell, SH_KW, &[], &[]),
        "ps1" | "psm1" | "psd1" => lang(PowerShell, PS_KW, PS_CONST, &[]),
        "sql" => lang(Sql, SQL_KW, &["null", "true", "false"], SQL_TYPES),
        "html" | "htm" | "xhtml" | "xml" | "svg" | "xaml" | "csproj" | "vcxproj" | "props" | "plist" | "vue"
        | "svelte" | "config" | "manifest" | "resx" => lang(Markup, &[], &[], &[]),
        "css" | "scss" | "sass" | "less" => lang(Css, CSS_KW, &[], &[]),
        "json" | "jsonc" | "json5" | "geojson" | "ipynb" | "webmanifest" => lang(Json, &[], CFG_CONST, &[]),
        "yaml" | "yml" | "toml" | "ini" | "cfg" | "conf" | "properties" | "env" | "editorconfig" | "gitconfig" => {
            lang(Config, &[], CFG_CONST, &[])
        }
        "lua" => lang(Lua, LUA_KW, &["true", "false", "nil"], &[]),
        _ => return None,
    };
    Some(l)
}

/// Whether a line comment starts at `rest`.
fn line_comment(l: &Lang, rest: &str, at_line_start: bool) -> bool {
    match l.family {
        Family::CLike | Family::Css | Family::Json => rest.starts_with("//") && l.family != Family::Css,
        Family::Python | Family::PowerShell | Family::Config => {
            rest.starts_with('#') || (l.family == Family::Config && at_line_start && rest.starts_with(';'))
        }
        Family::Shell => {
            rest.starts_with('#')
                || (at_line_start && (rest.to_ascii_lowercase().starts_with("rem ") || rest.starts_with("::")))
        }
        Family::Sql | Family::Lua => rest.starts_with("--") && !rest.starts_with("--[["),
        Family::Markup => false,
    }
}

/// Block comment opener → closer.
fn block_comment(l: &Lang, rest: &str) -> Option<&'static str> {
    match l.family {
        Family::CLike | Family::Css | Family::Sql | Family::Json if rest.starts_with("/*") => Some("*/"),
        Family::PowerShell if rest.starts_with("<#") => Some("#>"),
        Family::Lua if rest.starts_with("--[[") => Some("]]"),
        Family::Markup if rest.starts_with("<!--") => Some("-->"),
        _ => None,
    }
}

/// Carried from one line to the next.
#[derive(Clone, Debug, Default)]
enum State {
    #[default]
    Normal,
    /// Inside a comment that ends with this text.
    Comment(&'static str),
    /// Inside a string that ends with this text (triple quotes, backticks).
    Str(String),
    /// HTML/XML: inside `<tag … >` spanning lines.
    InTag,
}

struct Out {
    spans: Vec<Span>,
}

impl Out {
    fn push(&mut self, kind: Tok, text: &str) {
        if text.is_empty() {
            return;
        }
        match self.spans.last_mut() {
            Some((k, s)) if *k == kind => s.push_str(text),
            _ => self.spans.push((kind, text.to_string())),
        }
    }
}

/// Longest line we colour; the rest is plain (minified files).
const MAX_COLUMNS: usize = 2000;

/// Highlights `lines` (tabs already expanded or not), carrying multi-line
/// comments and strings across lines.
pub fn highlight(l: &Lang, lines: &[String]) -> Vec<Vec<Span>> {
    let mut state = State::Normal;
    lines
        .iter()
        .map(|line| {
            let line = line.replace('\t', "    ");
            let (head, tail) = match line.char_indices().nth(MAX_COLUMNS) {
                Some((i, _)) => line.split_at(i),
                None => (line.as_str(), ""),
            };
            let mut out = Out { spans: Vec::new() };
            state = if l.family == Family::Markup {
                markup_line(head, state.clone(), &mut out)
            } else {
                code_line(l, head, state.clone(), &mut out)
            };
            out.push(Tok::Plain, tail);
            out.spans
        })
        .collect()
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn code_line(l: &Lang, line: &str, mut state: State, out: &mut Out) -> State {
    let mut i = 0;
    let bytes_len = line.len();
    // Config files: `key = value` / `key: value` / `[section]`.
    let mut cfg_key_done = false;
    while i < bytes_len {
        let rest = &line[i..];
        match &state {
            State::Comment(end) => {
                let end = *end;
                match rest.find(end) {
                    Some(p) => {
                        out.push(Tok::Comment, &rest[..p + end.len()]);
                        i += p + end.len();
                        state = State::Normal;
                    }
                    None => {
                        out.push(Tok::Comment, rest);
                        return state;
                    }
                }
                continue;
            }
            State::Str(end) => {
                let end = end.clone();
                match find_unescaped(rest, &end) {
                    Some(p) => {
                        out.push(Tok::Str, &rest[..p + end.len()]);
                        i += p + end.len();
                        state = State::Normal;
                    }
                    None => {
                        out.push(Tok::Str, rest);
                        return state;
                    }
                }
                continue;
            }
            _ => {}
        }
        let at_start = line[..i].trim().is_empty();
        let c = rest.chars().next().unwrap();

        if line_comment(l, rest, at_start) {
            out.push(Tok::Comment, rest);
            return State::Normal;
        }
        if let Some(end) = block_comment(l, rest) {
            let open = if rest.starts_with("--[[") { 4 } else { 2 };
            out.push(Tok::Comment, &rest[..open]);
            i += open;
            state = State::Comment(end);
            continue;
        }
        if l.preprocessor && at_start && c == '#' {
            // `#include <x>`: directive keyword, rest as string-ish
            let word_end = rest[1..].find(|ch: char| !is_ident(ch)).map_or(rest.len(), |p| p + 1);
            out.push(Tok::Keyword, &rest[..word_end]);
            i += word_end;
            continue;
        }
        // Config: section headers and keys.
        if l.family == Family::Config && at_start && !cfg_key_done {
            cfg_key_done = true;
            if c == '[' {
                let end = rest.find(']').map_or(rest.len(), |p| p + 1);
                out.push(Tok::Tag, &rest[..end]);
                i += end;
                continue;
            }
            if let Some(p) = rest.find(['=', ':']) {
                let key = rest[..p].trim();
                if !key.is_empty() && !key.contains(['"', '\'', ' ']) {
                    out.push(Tok::Attr, &rest[..p]);
                    i += p;
                    continue;
                }
            }
        }
        // Strings
        if l.family == Family::Python {
            let prefix = rest.chars().take_while(|ch| "rRbBuUfF".contains(*ch)).count();
            let after = &rest[prefix..];
            for q in ["\"\"\"", "'''"] {
                if after.starts_with(q) && (prefix == 0 || !line[..i].ends_with(is_ident)) {
                    let open = prefix + 3;
                    out.push(Tok::Str, &rest[..open]);
                    i += open;
                    state = State::Str(q.to_string());
                    break;
                }
            }
            if matches!(state, State::Str(_)) {
                continue;
            }
            if prefix > 0 && (after.starts_with('"') || after.starts_with('\'')) && !line[..i].ends_with(is_ident) {
                out.push(Tok::Str, &rest[..prefix]);
                i += prefix;
                continue;
            }
        }
        if c == '"' || (c == '\'' && !(l.lifetimes && is_lifetime(rest))) || (c == '`' && l.backtick) {
            if c == '\'' && matches!(l.family, Family::Shell) {
                // single-quoted shell strings: no escapes
            }
            let q = c.to_string();
            match find_unescaped(&rest[1..], &q) {
                Some(p) => {
                    let len = 1 + p + 1;
                    // JSON keys look like attributes.
                    let kind = if l.family == Family::Json && rest[len..].trim_start().starts_with(':') {
                        Tok::Attr
                    } else {
                        Tok::Str
                    };
                    out.push(kind, &rest[..len]);
                    i += len;
                }
                None if c == '`' => {
                    out.push(Tok::Str, rest);
                    return State::Str("`".into());
                }
                None => {
                    out.push(Tok::Str, rest);
                    return State::Normal;
                }
            }
            continue;
        }
        if l.lifetimes && c == '\'' {
            let len = 1 + rest[1..].find(|ch: char| !is_ident(ch)).unwrap_or(rest.len() - 1);
            out.push(Tok::Type, &rest[..len]);
            i += len;
            continue;
        }
        // Numbers
        if c.is_ascii_digit() || (c == '.' && rest[1..].starts_with(|ch: char| ch.is_ascii_digit()) && !line[..i].ends_with(is_ident)) {
            if !line[..i].ends_with(is_ident) {
                let len = rest
                    .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '%'))
                    .unwrap_or(rest.len());
                out.push(Tok::Number, &rest[..len]);
                i += len;
                continue;
            }
        }
        // CSS colours: #fff
        if l.family == Family::Css && c == '#' {
            let len = 1 + rest[1..].find(|ch: char| !ch.is_ascii_alphanumeric()).unwrap_or(rest.len() - 1);
            out.push(Tok::Number, &rest[..len]);
            i += len;
            continue;
        }
        // Variables: $x (shell, PowerShell, PHP)
        if c == '$' && matches!(l.family, Family::Shell | Family::PowerShell) || (c == '$' && l.dollar_vars) {
            let body = &rest[1..];
            let len = if body.starts_with('{') {
                body.find('}').map_or(rest.len(), |p| p + 2)
            } else {
                1 + body.find(|ch: char| !(is_ident(ch) || ch == ':')).unwrap_or(body.len())
            };
            let word = &rest[..len];
            let kind = if l.constants.iter().any(|k| k.eq_ignore_ascii_case(word)) { Tok::Constant } else { Tok::Type };
            out.push(kind, word);
            i += len.max(1);
            continue;
        }
        // Batch variables: %NAME%
        if c == '%' && l.family == Family::Shell
            && let Some(p) = rest[1..].find('%')
            && p > 0
            && rest[1..1 + p].chars().all(is_ident)
        {
            out.push(Tok::Type, &rest[..p + 2]);
            i += p + 2;
            continue;
        }
        // Decorators / annotations / attributes
        if c == '@' && matches!(l.family, Family::Python | Family::CLike) {
            let len = 1 + rest[1..].find(|ch: char| !(is_ident(ch) || ch == '.')).unwrap_or(rest.len() - 1);
            if len > 1 {
                out.push(Tok::Function, &rest[..len]);
                i += len;
                continue;
            }
        }
        if c == '@' && l.family == Family::Css {
            let len = 1 + rest[1..].find(|ch: char| !(is_ident(ch) || ch == '-')).unwrap_or(rest.len() - 1);
            out.push(Tok::Keyword, &rest[..len]);
            i += len;
            continue;
        }
        // Words
        if is_ident_start(c) || (c == '-' && l.family == Family::PowerShell && rest[1..].starts_with(char::is_alphabetic)) {
            let extra = |ch: char| is_ident(ch) || (matches!(l.family, Family::Css | Family::PowerShell) && ch == '-');
            let len = c.len_utf8() + rest[c.len_utf8()..].find(|ch: char| !extra(ch)).unwrap_or(rest.len() - c.len_utf8());
            let word = &rest[..len];
            let after = rest[len..].trim_start();
            let kind = classify(l, word, after);
            out.push(kind, word);
            i += len;
            continue;
        }
        out.push(Tok::Plain, &rest[..c.len_utf8()]);
        i += c.len_utf8();
    }
    state
}

fn classify(l: &Lang, word: &str, after: &str) -> Tok {
    let case_insensitive = matches!(l.family, Family::Sql | Family::PowerShell);
    let has = |list: &[&str]| {
        if case_insensitive { list.iter().any(|k| k.eq_ignore_ascii_case(word)) } else { list.contains(&word) }
    };
    if l.family == Family::Css {
        return if after.starts_with(':') && !after.starts_with("::") { Tok::Attr } else { Tok::Plain };
    }
    if has(l.constants) {
        return Tok::Constant;
    }
    if has(l.keywords) {
        return Tok::Keyword;
    }
    if has(l.types) {
        return Tok::Type;
    }
    if after.starts_with('(') || (l.lifetimes && after.starts_with('!')) {
        return Tok::Function;
    }
    if l.caps_are_types && word.starts_with(|c: char| c.is_ascii_uppercase()) {
        // ALL_CAPS reads as a constant, CamelCase as a type.
        if word.len() > 1 && word.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_') {
            return Tok::Constant;
        }
        return Tok::Type;
    }
    Tok::Plain
}

/// Rust lifetime `'a` (no closing quote right after one char).
fn is_lifetime(rest: &str) -> bool {
    let mut it = rest.chars().skip(1);
    match (it.next(), it.next()) {
        (Some(a), Some(b)) => is_ident_start(a) && b != '\'',
        (Some(a), None) => is_ident_start(a),
        _ => false,
    }
}

/// Byte offset of `end` in `s`, skipping backslash-escaped characters.
fn find_unescaped(s: &str, end: &str) -> Option<usize> {
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
            continue;
        }
        if s[i..].starts_with(end) {
            return Some(i);
        }
    }
    None
}

fn markup_line(line: &str, mut state: State, out: &mut Out) -> State {
    let mut i = 0;
    while i < line.len() {
        let rest = &line[i..];
        match state {
            State::Comment(end) => match rest.find(end) {
                Some(p) => {
                    out.push(Tok::Comment, &rest[..p + end.len()]);
                    i += p + end.len();
                    state = State::Normal;
                }
                None => {
                    out.push(Tok::Comment, rest);
                    return state;
                }
            },
            State::InTag => {
                let c = rest.chars().next().unwrap();
                if rest.starts_with("/>") {
                    out.push(Tok::Tag, "/>");
                    i += 2;
                    state = State::Normal;
                } else if c == '>' {
                    out.push(Tok::Tag, ">");
                    i += 1;
                    state = State::Normal;
                } else if c == '"' || c == '\'' {
                    let len = rest[1..].find(c).map_or(rest.len(), |p| p + 2);
                    out.push(Tok::Str, &rest[..len]);
                    i += len;
                } else if is_ident_start(c) || c == ':' || c == '@' {
                    let len = rest
                        .find(|ch: char| !(is_ident(ch) || ch == '-' || ch == ':' || ch == '.' || ch == '@'))
                        .unwrap_or(rest.len());
                    out.push(Tok::Attr, &rest[..len.max(1)]);
                    i += len.max(1);
                } else {
                    out.push(Tok::Plain, &rest[..c.len_utf8()]);
                    i += c.len_utf8();
                }
            }
            _ => {
                if rest.starts_with("<!--") {
                    out.push(Tok::Comment, "<!--");
                    i += 4;
                    state = State::Comment("-->");
                } else if rest.starts_with("<!") || rest.starts_with("<?") {
                    let len = rest.find('>').map_or(rest.len(), |p| p + 1);
                    out.push(Tok::Keyword, &rest[..len]);
                    i += len;
                } else if rest.starts_with('<') && rest[1..].starts_with(|ch: char| is_ident_start(ch) || ch == '/') {
                    let start = if rest.starts_with("</") { 2 } else { 1 };
                    let len = start
                        + rest[start..]
                            .find(|ch: char| !(is_ident(ch) || ch == '-' || ch == ':' || ch == '.'))
                            .unwrap_or(rest.len() - start);
                    out.push(Tok::Tag, &rest[..len]);
                    i += len;
                    state = State::InTag;
                } else if rest.starts_with('&') && let Some(p) = rest.find(';').filter(|p| *p < 10) {
                    out.push(Tok::Constant, &rest[..p + 1]);
                    i += p + 1;
                } else {
                    // Skip the first character whole: it may be multi-byte (Δ, é, emoji).
                    let first = rest.chars().next().map_or(1, char::len_utf8);
                    let len = rest[first..].find(['<', '&']).map_or(rest.len(), |p| p + first);
                    out.push(Tok::Plain, &rest[..len]);
                    i += len;
                }
            }
        }
    }
    state
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(ext: &str, src: &str) -> Vec<Vec<(Tok, String)>> {
        let lines: Vec<String> = src.lines().map(String::from).collect();
        highlight(&for_extension(ext).unwrap(), &lines)
    }

    fn has(spans: &[Span], kind: Tok, text: &str) -> bool {
        spans.iter().any(|(k, s)| *k == kind && s.contains(text))
    }

    #[test]
    fn python_basics() {
        let h = kinds("py", "def f(x):  # hi\n    return \"s\" + 1\ns = '''a\nb'''\nprint(None)");
        assert!(has(&h[0], Tok::Keyword, "def"));
        assert!(has(&h[0], Tok::Function, "f"));
        assert!(has(&h[0], Tok::Comment, "# hi"));
        assert!(has(&h[1], Tok::Str, "\"s\""));
        assert!(has(&h[1], Tok::Number, "1"));
        assert!(has(&h[3], Tok::Str, "b'''"));
        assert!(has(&h[4], Tok::Constant, "None"));
    }

    #[test]
    fn c_like_and_block_comments() {
        let h = kinds("java", "public class A { /* x\n y */ int n = 0x1F; String s = \"a\\\"b\"; }");
        assert!(has(&h[0], Tok::Keyword, "public"));
        assert!(has(&h[0], Tok::Type, "A"));
        assert!(has(&h[0], Tok::Comment, "/* x"));
        assert!(has(&h[1], Tok::Comment, "y */"));
        assert!(has(&h[1], Tok::Type, "int"));
        assert!(has(&h[1], Tok::Number, "0x1F"));
        assert!(has(&h[1], Tok::Str, "\"a\\\"b\""));
        let cpp = kinds("cpp", "#include <vector>\nstd::vector<int> v; // c");
        assert!(has(&cpp[0], Tok::Keyword, "#include"));
        assert!(has(&cpp[1], Tok::Comment, "// c"));
    }

    #[test]
    fn rust_lifetimes_and_chars() {
        let h = kinds("rs", "fn f<'a>(s: &'a str) -> char { 'x' }");
        assert!(has(&h[0], Tok::Type, "'a"));
        assert!(has(&h[0], Tok::Str, "'x'"));
    }

    #[test]
    fn html_tags_and_attributes() {
        let h = kinds("html", "<!-- c -->\n<a href=\"x\" class='y'>Hi &amp; bye</a>\n<div\n  id=\"z\">");
        assert!(has(&h[0], Tok::Comment, "<!-- c -->"));
        assert!(has(&h[1], Tok::Tag, "<a"));
        assert!(has(&h[1], Tok::Attr, "href"));
        assert!(has(&h[1], Tok::Str, "\"x\""));
        assert!(has(&h[1], Tok::Constant, "&amp;"));
        assert!(has(&h[1], Tok::Tag, "</a"));
        assert!(has(&h[3], Tok::Attr, "id"));
    }

    #[test]
    fn text_is_preserved() {
        let src = "let x = `a ${b}\nc`; // done";
        let h = kinds("ts", src);
        let joined: Vec<String> = h.iter().map(|l| l.iter().map(|(_, s)| s.as_str()).collect()).collect();
        assert_eq!(joined.join("\n"), src);
        assert!(has(&h[1], Tok::Str, "c`"));
    }

    /// Multi-byte characters anywhere, in every language: never panics and
    /// never changes the text (a crash on "Δ" in an HTML page).
    #[test]
    fn unicode_everywhere() {
        let exts = [
            "c", "java", "cs", "ts", "rs", "go", "php", "py", "sh", "bat", "ps1", "sql", "html", "css", "json", "toml",
            "lua",
        ];
        let samples = [
            "Δ", "éa", "😀<b>", "Δ<b>é</b>&amp;Δ", "'Δ' \"é\" `😀`", "$Δ %Δ% @Δ #Δ .5Δ -Δ 'aΔ", "<Δ attr=\"é\">",
            "/* Δ */ // é", "# é", "-- é", "x = 'é", "\"\"\"é", "<!-- é", "[é] é = \"Δ\"", "aΔb(é)", "\\Δ",
        ];
        for ext in exts {
            let l = for_extension(ext).unwrap();
            let lines: Vec<String> = samples.iter().map(|s| s.to_string()).collect();
            let h = highlight(&l, &lines);
            for (spans, src) in h.iter().zip(&lines) {
                let joined: String = spans.iter().map(|(_, s)| s.as_str()).collect();
                assert_eq!(&joined, src, "{ext}");
            }
        }
    }

    /// The real file that crashed the preview, when present.
    #[test]
    fn real_html_file() {
        let Some(home) = std::env::var_os("USERPROFILE") else { return };
        let p = std::path::Path::new(&home).join("Documents").join("00 Macro Analysis Dashboard (cloud).html");
        let Ok(text) = std::fs::read_to_string(p) else { return };
        let lines: Vec<String> = text.lines().map(String::from).collect();
        let h = highlight(&for_extension("html").unwrap(), &lines);
        assert_eq!(h.len(), lines.len());
    }

    #[test]
    fn json_keys_and_config() {
        let h = kinds("json", "{\"name\": \"x\", \"n\": 2, \"ok\": true}");
        assert!(has(&h[0], Tok::Attr, "\"name\""));
        assert!(has(&h[0], Tok::Str, "\"x\""));
        assert!(has(&h[0], Tok::Constant, "true"));
        let t = kinds("toml", "[deps]\nname = \"v\" # c");
        assert!(has(&t[0], Tok::Tag, "[deps]"));
        assert!(has(&t[1], Tok::Attr, "name"));
        assert!(has(&t[1], Tok::Comment, "# c"));
    }
}
