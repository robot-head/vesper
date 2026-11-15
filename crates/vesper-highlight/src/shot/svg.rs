//! Deterministic vector SVG export of a [`CodeCardScene`]: text stays text
//! (`<text>` per glyph run, selectable and searchable), positioned by the
//! font's shaped advances. Output bytes depend only on the scene, the font
//! family name and the options.

use super::font::ShotFont;
use super::scene::{CodeCardScene, Primitive, Rgba, wavy_segments};
use base64::Engine as _;
use std::fmt::Write as _;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SvgOptions {
    /// Embed the font as a base64 `@font-face` so the SVG renders identically
    /// without the font installed (adds the font's size to the file).
    pub embed_font: bool,
}

/// Formats a coordinate with at most two decimals and no trailing zeros.
fn num(out: &mut String, v: f32) {
    let v = if v.abs() < 0.005 { 0.0 } else { v };
    let s = format!("{v:.2}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    out.push_str(s);
}

fn color(out: &mut String, attr: &str, c: Rgba) {
    let _ = write!(out, " {attr}=\"#{:06X}\"", c.rgb & 0xFF_FFFF);
    if c.alpha != 255 {
        let _ = write!(out, " {attr}-opacity=\"");
        num(out, f32::from(c.alpha) / 255.0);
        out.push('"');
    }
}

fn escape(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
}

fn attr(out: &mut String, name: &str, v: f32) {
    let _ = write!(out, " {name}=\"");
    num(out, v);
    out.push('"');
}

/// Writes the scene as an SVG document.
pub fn render_svg(scene: &CodeCardScene, font: &ShotFont<'_>, options: SvgOptions) -> String {
    let mut out = String::with_capacity(4096 + scene.primitives.len() * 96);
    let (w, h) = (scene.width_px, scene.height_px);
    let _ = write!(
        out,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\" viewBox=\"0 0 {w} {h}\" role=\"img\" aria-label=\""
    );
    escape(&mut out, &scene.title);
    out.push_str("\" font-family=\"'");
    escape(&mut out, font.family());
    out.push_str("', ui-monospace, monospace\" xml:space=\"preserve\" data-vesper-theme=\"");
    escape(&mut out, &scene.theme);
    out.push_str("\">\n");
    if !scene.title.is_empty() {
        out.push_str("<title>");
        escape(&mut out, &scene.title);
        out.push_str("</title>\n");
    }
    out.push_str("<style>text{white-space:pre;font-kerning:none;font-variant-ligatures:common-ligatures contextual}");
    if options.embed_font {
        out.push_str("@font-face{font-family:'");
        escape(&mut out, font.family());
        out.push_str("';src:url(data:font/ttf;base64,");
        out.push_str(&base64::engine::general_purpose::STANDARD.encode(font.bytes()));
        out.push_str(") format('truetype')}");
    }
    out.push_str("</style>\n");
    for p in &scene.primitives {
        match p {
            Primitive::Rect {
                x,
                y,
                w,
                h,
                radius,
                fill,
            }
            | Primitive::ShadowLayer {
                x,
                y,
                w,
                h,
                radius,
                fill,
                ..
            } => {
                out.push_str("<rect");
                attr(&mut out, "x", *x);
                attr(&mut out, "y", *y);
                attr(&mut out, "width", *w);
                attr(&mut out, "height", *h);
                if *radius > 0.0 {
                    attr(&mut out, "rx", *radius);
                }
                color(&mut out, "fill", *fill);
                out.push_str("/>\n");
            }
            Primitive::Circle { cx, cy, r, fill } => {
                out.push_str("<circle");
                attr(&mut out, "cx", *cx);
                attr(&mut out, "cy", *cy);
                attr(&mut out, "r", *r);
                color(&mut out, "fill", *fill);
                out.push_str("/>\n");
            }
            Primitive::Triangle { points, fill } => {
                out.push_str("<path d=\"M");
                for (i, (x, y)) in points.iter().enumerate() {
                    if i > 0 {
                        out.push_str(" L");
                    }
                    num(&mut out, *x);
                    out.push(' ');
                    num(&mut out, *y);
                }
                out.push_str("Z\"");
                color(&mut out, "fill", *fill);
                out.push_str("/>\n");
            }
            Primitive::WavyUnderline {
                x0,
                x1,
                y,
                color: c,
            } => {
                out.push_str("<path class=\"vc-lin-live\" d=\"M");
                num(&mut out, *x0);
                out.push(' ');
                num(&mut out, *y);
                for [(cx, cy), (ex, ey)] in wavy_segments(*x0, *x1, *y) {
                    out.push_str(" Q");
                    for v in [cx, cy, ex, ey] {
                        num(&mut out, v);
                        out.push(' ');
                    }
                    out.pop();
                }
                out.push_str("\" fill=\"none\" stroke-width=\"1.2\"");
                color(&mut out, "stroke", *c);
                out.push_str("/>\n");
            }
            Primitive::Text(run) => {
                out.push_str("<text");
                attr(&mut out, "x", run.x_px);
                attr(&mut out, "y", run.y_px);
                attr(&mut out, "font-size", run.font_px);
                if run.width_px > 0.0 {
                    attr(&mut out, "textLength", run.width_px);
                    out.push_str(" lengthAdjust=\"spacingAndGlyphs\"");
                }
                color(&mut out, "fill", Rgba::opaque(run.srgb_hex));
                if let Some(class) = run.class {
                    let _ = write!(out, " data-class=\"{}\"", class as u16);
                }
                if run.ligature {
                    out.push_str(" data-ligature=\"1\"");
                }
                out.push('>');
                escape(&mut out, &run.text);
                out.push_str("</text>\n");
            }
        }
    }
    out.push_str("</svg>\n");
    out
}
