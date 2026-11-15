//! `vesper highlight --html`: semantic, accessible HTML styled only through
//! the `vesper-chroma.css` custom properties.
//!
//! Source text appears in document order, HTML-escaped and otherwise
//! unchanged, with carriage returns represented by character references so
//! HTML input newline normalization does not turn them into line feeds. Line
//! numbers are `vc-gutter` elements marked `user-select: none` and
//! `aria-hidden`. Clipboard newline conventions depend on the browser and OS.
//! Hover rows become `title` attributes (not copied with a selection).
use crate::{Highlight, css::class_name};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HtmlOptions {
    /// `data-vesper-theme` on the block; `None` follows the page.
    pub theme: Option<&'static str>,
    pub line_numbers: bool,
}

impl Default for HtmlOptions {
    fn default() -> Self {
        Self {
            theme: None,
            line_numbers: true,
        }
    }
}

/// Appends `text` HTML-escaped (`&`, `<`, `>`, `"`, and carriage return).
pub fn escape_into(text: &str, out: &mut String) {
    let mut last = 0;
    for (i, b) in text.bytes().enumerate() {
        let rep = match b {
            b'&' => "&amp;",
            b'<' => "&lt;",
            b'>' => "&gt;",
            b'"' => "&quot;",
            b'\r' => "&#13;",
            _ => continue,
        };
        out.push_str(&text[last..i]);
        out.push_str(rep);
        last = i + 1;
    }
    out.push_str(&text[last..]);
}

/// Renders a `<pre class="vc">` block. `src` must be the text `h` was
/// computed from.
pub fn render(src: &str, h: &Highlight, opts: &HtmlOptions) -> String {
    let mut lines = 1usize;
    let mut previous_cr = false;
    for byte in src.bytes() {
        if byte == b'\r' || (byte == b'\n' && !previous_cr) {
            lines += 1;
        }
        previous_cr = byte == b'\r';
    }
    let width = lines.to_string().len();
    let mut out = String::with_capacity(src.len() * 4 + 64);
    out.push_str("<pre class=\"vc\"");
    if let Some(theme) = opts.theme {
        out.push_str(" data-vesper-theme=\"");
        escape_into(theme, &mut out);
        out.push('"');
    }
    out.push_str(match h.tier {
        crate::Tier::Lossless0 => " data-vc-tier=\"0\"",
        crate::Tier::Semantic1 => " data-vc-tier=\"1\"",
    });
    out.push_str("><code>");
    let mut line = 1usize;
    let gutter = |line: usize, out: &mut String| {
        if opts.line_numbers {
            out.push_str("<span class=\"vc-gutter\" aria-hidden=\"true\">");
            let n = line.to_string();
            for _ in n.len()..width {
                out.push(' ');
            }
            out.push_str(&n);
            out.push_str(" </span>");
        }
    };
    if !src.is_empty() {
        gutter(line, &mut out);
    }
    for sp in &h.spans {
        let text = &src[sp.start..sp.end];
        let open = class_name(sp.class).map(|class| {
            let mut tag = String::from("<span class=\"");
            tag.push_str(class);
            if sp.is_live_linear {
                tag.push_str(" vc-lin-live");
            }
            tag.push('"');
            if let Some(row) = sp.hover_sym_id.and_then(|id| h.hovers.get(id as usize)) {
                tag.push_str(" title=\"");
                escape_into(&row.text, &mut tag);
                tag.push('"');
            }
            tag.push('>');
            tag
        });
        let mut rest = text;
        let mut offset = sp.start;
        while !rest.is_empty() {
            let (segment, terminator) = match rest.find(['\r', '\n']) {
                Some(i) => (&rest[..=i], Some(rest.as_bytes()[i])),
                None => (rest, None),
            };
            rest = &rest[segment.len()..];
            offset += segment.len();
            if let Some(open) = &open {
                out.push_str(open);
                escape_into(segment, &mut out);
                out.push_str("</span>");
            } else {
                escape_into(segment, &mut out);
            }
            // A CRLF is one terminator even when the two bytes belong to
            // different highlight spans. Defer its gutter until after LF.
            let line_end = match terminator {
                Some(b'\n') => true,
                Some(b'\r') => src.as_bytes().get(offset) != Some(&b'\n'),
                _ => false,
            };
            if line_end {
                line += 1;
                if offset < src.len() {
                    gutter(line, &mut out);
                }
            }
        }
    }
    out.push_str("</code></pre>\n");
    out
}
