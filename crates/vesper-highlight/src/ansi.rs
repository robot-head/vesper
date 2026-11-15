//! `vesper highlight --ansi`: 24-bit SGR colours for terminals.
//!
//! Every span is wrapped in `ESC[38;2;R;G;Bm ... ESC[0m` (plus `ESC[4m` for a
//! live linear obligation, `ESC[3m` for an abortable effect); whitespace is
//! emitted bare. Removing the escape sequences gives the source unchanged.
use crate::{ChromaPalette, Highlight, VesperTokenClass, color::hex_channels};

pub fn render(src: &str, h: &Highlight, palette: &ChromaPalette) -> String {
    let mut out = String::with_capacity(src.len() * 3);
    for sp in &h.spans {
        let text = &src[sp.start..sp.end];
        if sp.class == VesperTokenClass::Whitespace {
            out.push_str(text);
            continue;
        }
        let [r, g, b] = hex_channels(palette.swatch_for(sp.class).srgb_hex);
        out.push_str(&format!("\x1b[38;2;{r};{g};{b}m"));
        if sp.is_live_linear {
            out.push_str("\x1b[4m");
        }
        if sp.class == VesperTokenClass::EffectAbortable {
            out.push_str("\x1b[3m");
        }
        out.push_str(text);
        out.push_str("\x1b[0m");
    }
    out
}

/// Removes the SGR sequences [`render`] writes (`ESC [ ... m`).
pub fn strip(ansi: &str) -> String {
    let mut out = String::with_capacity(ansi.len());
    let mut chars = ansi.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for d in chars.by_ref() {
                if d == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}
