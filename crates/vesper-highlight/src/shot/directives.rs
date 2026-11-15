//! `@shot(...)` header and `//^?` / `//^!` callout directives (§43.2).
//!
//! These are read from the source text by Chroma. They are not Vesper
//! attribute syntax and are never handed to the compiler as such: the header
//! line and the callout comments are removed from the displayed text (and from
//! the text `--verify` checks), and every other byte is kept.

use super::{ShotError, ShotLimits};

/// A card theme. The palettes themselves are lane 1's ([`crate::ChromaPalette`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShotTheme {
    Dusk,
    Dawn,
}

impl ShotTheme {
    /// Accepts `dusk`, `dawn`, `vesper-dusk` and `vesper-dawn`.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "dusk" | "vesper-dusk" => Some(Self::Dusk),
            "dawn" | "vesper-dawn" => Some(Self::Dawn),
            _ => None,
        }
    }

    /// The stable theme id that enters the cache key.
    pub fn id(self) -> &'static str {
        match self {
            Self::Dusk => "vesper-dusk",
            Self::Dawn => "vesper-dawn",
        }
    }

    pub fn palette(self) -> crate::ChromaPalette {
        match self {
            Self::Dusk => crate::ChromaPalette::vesper_dusk(),
            Self::Dawn => crate::ChromaPalette::vesper_dawn(),
        }
    }
}

/// Settings from the `@shot(...)` header. Command-line flags override them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShotConfig {
    pub title: Option<String>,
    pub theme: Option<ShotTheme>,
    pub dpi: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CalloutKind {
    /// `//^?`: show the inferred type and mode of the token above the caret.
    /// Text after `^?` is the author's fallback, used only when the caller
    /// cannot resolve the query.
    Query { author_text: Option<String> },
    /// `//^!`: attach a note.
    Note { text: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Callout {
    pub kind: CalloutKind,
    /// 1-based line in the original source (for diagnostics).
    pub source_line: usize,
    /// 0-based line of the displayed text the callout attaches to.
    pub display_line: usize,
    /// Byte offset within that display line the caret points at; `None` for a
    /// trailing `code //^! note`, which pins to the end of the line.
    pub column_byte: Option<usize>,
}

/// A shot source with its directives removed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShotDocument {
    pub config: ShotConfig,
    /// The displayed text: LF line endings, tabs expanded to 4-column stops,
    /// trailing whitespace and leading/trailing blank lines removed, no
    /// trailing newline.
    pub display: String,
    pub callouts: Vec<Callout>,
    /// 1-based original source line of each display line.
    pub source_lines: Vec<usize>,
}

impl ShotDocument {
    /// Byte offset of the start of each display line.
    pub fn line_starts(&self) -> Vec<usize> {
        line_starts(&self.display)
    }
}

pub(crate) fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
    starts
}

fn directive(line: usize, reason: impl Into<String>) -> ShotError {
    ShotError::Directive {
        line,
        reason: reason.into(),
    }
}

fn callout(line: usize, reason: impl Into<String>) -> ShotError {
    ShotError::Callout {
        line,
        reason: reason.into(),
    }
}

/// Splits a shot source into its header configuration, its callouts and the
/// text to display.
pub fn parse_shot_source(source: &str, limits: ShotLimits) -> Result<ShotDocument, ShotError> {
    if source.len() > limits.max_source_bytes {
        return Err(ShotError::Limit("source bytes"));
    }
    let mut config = None;
    let mut lines: Vec<String> = Vec::new();
    let mut source_lines = Vec::new();
    let mut callouts = Vec::new();
    let body = source.strip_suffix('\n').unwrap_or(source);
    // Lexical state carried across lines: directives are recognised only in
    // code and in real line comments, never inside strings or block comments.
    let mut lex = LexState::default();
    for (index, raw) in body.split('\n').enumerate() {
        let number = index + 1;
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some(c) = raw.chars().find(|c| c.is_control() && *c != '\t') {
            return Err(directive(
                number,
                format!("control character U+{:04X}", c as u32),
            ));
        }
        let line = expand_tabs(raw);
        if line.chars().count() > limits.max_line_chars {
            return Err(ShotError::Limit("line length"));
        }
        let at_code = lex.in_code();
        let comment = lex.scan_line(&line);
        let trimmed = line.trim_start();
        if at_code && trimmed.starts_with("@shot") {
            if config.is_some() {
                return Err(directive(number, "duplicate @shot directive"));
            }
            if !lines.is_empty() {
                return Err(directive(number, "@shot must be the first non-blank line"));
            }
            config = Some(parse_header(trimmed, number, limits)?);
            continue;
        }
        if let Some((caret_byte, kind, rest)) = comment.and_then(|at| callout_line(&line, at)) {
            let Some(target) = lines.len().checked_sub(1) else {
                return Err(callout(number, "caret line has no code line above it"));
            };
            let above = &lines[target];
            if above.trim().is_empty() {
                return Err(callout(number, "caret line points at a blank line"));
            }
            let column = line[..caret_byte].chars().count();
            let Some((column_byte, _)) = above.char_indices().nth(column) else {
                return Err(callout(number, "caret is beyond the end of the line above"));
            };
            callouts.push(Callout {
                kind: callout_kind(kind, rest, number, limits)?,
                source_line: number,
                display_line: target,
                column_byte: Some(column_byte),
            });
        } else if let Some((at, kind, rest)) = comment.and_then(|at| trailing_callout(&line, at)) {
            if kind == '?' {
                return Err(callout(
                    number,
                    "a //^? query goes on its own line under the token it points at",
                ));
            }
            let code = line[..at].trim_end().to_string();
            callouts.push(Callout {
                kind: callout_kind(kind, rest, number, limits)?,
                source_line: number,
                display_line: lines.len(),
                column_byte: None,
            });
            lines.push(code);
            source_lines.push(number);
        } else {
            let kept = line.trim_end();
            if lines.is_empty() && kept.is_empty() {
                continue;
            }
            lines.push(kept.to_string());
            source_lines.push(number);
        }
        if lines.len() > limits.max_lines {
            return Err(ShotError::Limit("line count"));
        }
        if callouts.len() > limits.max_callouts {
            return Err(ShotError::Limit("callout count"));
        }
    }
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
        source_lines.pop();
    }
    if lines.is_empty() {
        return Err(directive(1, "no code to render"));
    }
    Ok(ShotDocument {
        config: config.unwrap_or_default(),
        display: lines.join("\n"),
        callouts,
        source_lines,
    })
}

fn expand_tabs(line: &str) -> String {
    if !line.contains('\t') {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len() + 8);
    let mut column = 0usize;
    for c in line.chars() {
        if c == '\t' {
            let pad = 4 - column % 4;
            out.extend(std::iter::repeat_n(' ', pad));
            column += pad;
        } else {
            out.push(c);
            column += 1;
        }
    }
    out
}

/// Strings and (nested) block comments open at the end of a line.
#[derive(Clone, Copy, Debug, Default)]
struct LexState {
    in_string: bool,
    block_depth: usize,
}

impl LexState {
    fn in_code(self) -> bool {
        !self.in_string && self.block_depth == 0
    }

    /// Advances over one line and returns the byte where a line comment
    /// starts in code, if any (the rest of the line is that comment).
    fn scan_line(&mut self, line: &str) -> Option<usize> {
        let bytes = line.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            let pair = bytes.get(i..i + 2);
            if self.block_depth > 0 {
                match pair {
                    Some(b"/*") => {
                        self.block_depth += 1;
                        i += 2;
                    }
                    Some(b"*/") => {
                        self.block_depth -= 1;
                        i += 2;
                    }
                    _ => i += 1,
                }
            } else if self.in_string {
                match bytes[i] {
                    b'\\' => i += 2,
                    b'"' => {
                        self.in_string = false;
                        i += 1;
                    }
                    _ => i += 1,
                }
            } else {
                match (bytes[i], pair) {
                    (_, Some(b"//")) => return Some(i),
                    (_, Some(b"/*")) => {
                        self.block_depth = 1;
                        i += 2;
                    }
                    (b'"', _) => {
                        self.in_string = true;
                        i += 1;
                    }
                    _ => i += 1,
                }
            }
        }
        None
    }
}

/// A line that is only a `//  ^?` or `//^!` comment (the comment starting at
/// byte `at`): (byte of `^`, kind, rest).
fn callout_line(line: &str, at: usize) -> Option<(usize, char, &str)> {
    if !line[..at].trim().is_empty() {
        return None;
    }
    let after = &line[at + 2..];
    let spaces = after.len() - after.trim_start_matches(' ').len();
    let caret = at + 2 + spaces;
    let rest = &line[caret..];
    let kind = match rest.as_bytes() {
        [b'^', b'?', ..] => '?',
        [b'^', b'!', ..] => '!',
        _ => return None,
    };
    Some((caret, kind, &rest[2..]))
}

/// A `code //^! note` comment after code (the comment starting at byte `at`):
/// (byte of `//`, kind, rest).
fn trailing_callout(line: &str, at: usize) -> Option<(usize, char, &str)> {
    match line.as_bytes().get(at + 2..at + 4) {
        Some(b"^?") => Some((at, '?', &line[at + 4..])),
        Some(b"^!") => Some((at, '!', &line[at + 4..])),
        _ => None,
    }
}

fn callout_kind(
    kind: char,
    rest: &str,
    line: usize,
    limits: ShotLimits,
) -> Result<CalloutKind, ShotError> {
    let text = rest.trim();
    if text.chars().count() > limits.max_callout_chars {
        return Err(ShotError::Limit("callout text length"));
    }
    if kind == '?' {
        Ok(CalloutKind::Query {
            author_text: (!text.is_empty()).then(|| text.to_string()),
        })
    } else if text.is_empty() {
        Err(callout(line, "a //^! note needs text"))
    } else {
        Ok(CalloutKind::Note {
            text: text.to_string(),
        })
    }
}

enum Value {
    Text(String),
    Int(u64),
}

fn parse_header(text: &str, line: usize, limits: ShotLimits) -> Result<ShotConfig, ShotError> {
    let mut p = text
        .strip_prefix("@shot")
        .and_then(|t| t.trim_start().strip_prefix('('))
        .ok_or_else(|| directive(line, "expected `@shot(`"))?;
    let mut config = ShotConfig::default();
    loop {
        p = p.trim_start();
        if let Some(rest) = p.strip_prefix(')') {
            if !rest.trim().is_empty() {
                return Err(directive(line, "unexpected text after `)`"));
            }
            return Ok(config);
        }
        let key_len = p
            .bytes()
            .take_while(|b| b.is_ascii_lowercase() || *b == b'_')
            .count();
        if key_len == 0 {
            return Err(directive(line, "expected a key or `)`"));
        }
        let key = &p[..key_len];
        p = p[key_len..].trim_start();
        p = p
            .strip_prefix('=')
            .ok_or_else(|| directive(line, format!("expected `=` after `{key}`")))?
            .trim_start();
        let (value, rest) = parse_value(p, line)?;
        p = rest.trim_start();
        match (key, value) {
            ("title", Value::Text(t)) => {
                if config.title.is_some() {
                    return Err(directive(line, "duplicate key `title`"));
                }
                if t.chars().count() > limits.max_title_chars {
                    return Err(ShotError::Limit("title length"));
                }
                config.title = Some(t);
            }
            ("theme", Value::Text(t)) => {
                if config.theme.is_some() {
                    return Err(directive(line, "duplicate key `theme`"));
                }
                config.theme = Some(
                    ShotTheme::parse(&t)
                        .ok_or_else(|| directive(line, format!("unknown theme `{t}`")))?,
                );
            }
            ("dpi", Value::Int(n)) => {
                if config.dpi.is_some() {
                    return Err(directive(line, "duplicate key `dpi`"));
                }
                if !(1..=4).contains(&n) {
                    return Err(directive(line, "dpi must be 1, 2, 3 or 4"));
                }
                config.dpi = Some(n as u32);
            }
            ("title" | "theme", Value::Int(_)) => {
                return Err(directive(line, format!("`{key}` takes a quoted string")));
            }
            ("dpi", Value::Text(_)) => return Err(directive(line, "`dpi` takes an integer")),
            _ => return Err(directive(line, format!("unknown key `{key}`"))),
        }
        if let Some(rest) = p.strip_prefix(',') {
            p = rest;
        } else if !p.starts_with(')') {
            return Err(directive(line, "expected `,` or `)`"));
        }
    }
}

fn parse_value(p: &str, line: usize) -> Result<(Value, &str), ShotError> {
    let mut chars = p.char_indices();
    match chars.next() {
        Some((_, quote @ ('\'' | '"'))) => {
            let mut out = String::new();
            while let Some((i, c)) = chars.next() {
                match c {
                    '\\' => match chars.next() {
                        Some((_, e @ ('\\' | '\'' | '"'))) => out.push(e),
                        _ => return Err(directive(line, "unsupported escape in string")),
                    },
                    c if c == quote => return Ok((Value::Text(out), &p[i + 1..])),
                    c => out.push(c),
                }
            }
            Err(directive(line, "unterminated string"))
        }
        Some((_, c)) if c.is_ascii_digit() => {
            let len = p.bytes().take_while(u8::is_ascii_digit).count();
            let n = p[..len]
                .parse::<u64>()
                .map_err(|_| directive(line, "integer out of range"))?;
            Ok((Value::Int(n), &p[len..]))
        }
        _ => Err(directive(line, "expected a quoted string or an integer")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<ShotDocument, ShotError> {
        parse_shot_source(s, ShotLimits::default())
    }

    #[test]
    fn header_and_callouts() {
        let doc = parse(concat!(
            "\n@shot(title = 'a \\'b\\'', theme = \"dawn\", dpi = 2,)\n",
            "fn main() -> i64 {\n",
            "    let x = 1;\n",
            "    //  ^? i64\n",
            "    x //^! the result\n",
            "}\n\n"
        ))
        .unwrap();
        assert_eq!(doc.config.title.as_deref(), Some("a 'b'"));
        assert_eq!(doc.config.theme, Some(ShotTheme::Dawn));
        assert_eq!(doc.config.dpi, Some(2));
        assert_eq!(doc.display, "fn main() -> i64 {\n    let x = 1;\n    x\n}");
        assert_eq!(doc.source_lines, vec![3, 4, 6, 7]);
        assert_eq!(doc.callouts.len(), 2);
        assert_eq!(doc.callouts[0].display_line, 1);
        assert_eq!(doc.callouts[0].column_byte, Some(8));
        assert_eq!(
            doc.callouts[0].kind,
            CalloutKind::Query {
                author_text: Some("i64".into())
            }
        );
        assert_eq!(doc.callouts[1].display_line, 2);
        assert_eq!(doc.callouts[1].column_byte, None);
    }

    #[test]
    fn malformed_directives_are_typed_errors() {
        for (src, line) in [
            ("@shot(dpi = 5)\nx", 1),
            ("@shot(dpi = '2')\nx", 1),
            ("@shot(theme = 'neon')\nx", 1),
            ("@shot(color = 'x')\nx", 1),
            ("@shot(title = 'x'\nx", 1),
            ("@shot(title = 'x', title = 'y')\nx", 1),
            ("@shot(title = 'x') extra\nx", 1),
            ("x\n@shot(dpi = 1)", 2),
            ("@shot()\n@shot()\nx", 2),
            ("@shot(title = 'x\\n')\nx", 1),
        ] {
            match parse(src) {
                Err(ShotError::Directive { line: l, .. }) => assert_eq!(l, line, "{src:?}"),
                other => panic!("{src:?}: {other:?}"),
            }
        }
        assert!(matches!(
            parse("@shot()\n\n"),
            Err(ShotError::Directive { .. })
        ));
        assert!(matches!(parse("a\u{7}b"), Err(ShotError::Directive { .. })));
    }

    #[test]
    fn malformed_callouts_are_typed_errors() {
        for src in [
            "//^? nothing above",
            "x\n\n//^? blank above",
            "ab\n//    ^? past the end",
            "let x = 1; //^? trailing query",
            "x\n//^!",
        ] {
            assert!(
                matches!(parse(src), Err(ShotError::Callout { .. })),
                "{src:?}"
            );
        }
    }

    #[test]
    fn strings_and_plain_comments_are_code() {
        let doc = parse("let s = \"//^! no\"; // plain\n").unwrap();
        assert_eq!(doc.display, "let s = \"//^! no\"; // plain");
        assert!(doc.callouts.is_empty());
    }

    #[test]
    fn block_comments_and_multiline_strings_are_not_directives() {
        for src in [
            "fn main() { /* //^! ordinary comment */ }",
            "fn main() {\n    /* nested /* x */\n    //^! still inside */\n}",
            "let s = \"a\n//^! in a string\";",
            "/*\n@shot(dpi = 9)\n*/\nx",
        ] {
            let doc = parse(src).unwrap();
            assert!(doc.callouts.is_empty(), "{src:?}");
            assert_eq!(doc.config, ShotConfig::default(), "{src:?}");
            assert_eq!(doc.display, src.trim_end(), "{src:?}");
        }
        // After the block comment closes, callouts work again.
        let doc = parse("/* a */ x //^! note").unwrap();
        assert_eq!(doc.display, "/* a */ x");
        assert_eq!(doc.callouts.len(), 1);
    }

    #[test]
    fn limits() {
        let limits = ShotLimits {
            max_lines: 2,
            ..ShotLimits::default()
        };
        assert_eq!(
            parse_shot_source("a\nb\nc", limits),
            Err(ShotError::Limit("line count"))
        );
        let limits = ShotLimits {
            max_source_bytes: 3,
            ..ShotLimits::default()
        };
        assert_eq!(
            parse_shot_source("abcd", limits),
            Err(ShotError::Limit("source bytes"))
        );
        assert_eq!(parse("\tx").unwrap().display, "    x");
    }
}
