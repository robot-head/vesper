//! `vesper-chroma.css` (§43.3): both themes as CSS custom properties.
//!
//! Dusk is the default (`:root`) and `[data-vesper-theme=dusk]`; Dawn is
//! `[data-vesper-theme=dawn]` and, without JavaScript, the default under
//! `prefers-color-scheme: light` when no theme attribute is set. Colours are
//! the palettes' sRGB fallbacks, generated from [`ChromaPalette`].
use crate::{ChromaPalette, SwatchRole, VesperTokenClass};

/// The design's size target for the sheet, in bytes (1.2 KiB).
pub const TARGET_BYTES: usize = 1229;

/// The CSS class of a token class, or `None` for whitespace.
pub fn class_name(class: VesperTokenClass) -> Option<&'static str> {
    use VesperTokenClass as C;
    Some(match class {
        C::KeywordControl => "vc-k",
        C::ModalSpecifier => "vc-m",
        C::PassModeIn => "vc-i",
        C::PassModeInOut => "vc-io",
        C::PassModeSink => "vc-s",
        C::LinearBinding => "vc-l",
        C::FieldLeafMut => "vc-fm",
        C::FieldLeafRead => "vc-fr",
        C::EffectLinear => "vc-el",
        C::EffectAbortable => "vc-ea",
        C::TypeNominal => "vc-t",
        C::WireEndianScalar => "vc-w",
        C::FunctionIdent => "vc-f",
        C::LiteralScalar => "vc-n",
        C::DocProofAnchor => "vc-p",
        C::PunctLigature => "vc-o",
        C::CommentTrivia => "vc-c",
        C::Whitespace => return None,
    })
}

const ROLES: [SwatchRole; 8] = [
    SwatchRole::BgCard,
    SwatchRole::FgDefault,
    SwatchRole::LinearAmber,
    SwatchRole::InoutCyan,
    SwatchRole::EffectIris,
    SwatchRole::WireEmerald,
    SwatchRole::KeywordRose,
    SwatchRole::CommentSlate,
];

fn vars(p: &ChromaPalette, out: &mut String) {
    for (i, role) in ROLES.iter().enumerate() {
        if i > 0 {
            out.push(';');
        }
        out.push_str(role.css_var());
        out.push_str(":#");
        out.push_str(&hex6(p.swatch(*role).srgb_hex));
    }
}

pub(crate) fn hex6(hex: u32) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    (0..6)
        .rev()
        .map(|i| DIGITS[((hex >> (i * 4)) & 0xF) as usize] as char)
        .collect()
}

/// The complete stylesheet.
pub fn stylesheet() -> String {
    let (dusk, dawn) = (ChromaPalette::vesper_dusk(), ChromaPalette::vesper_dawn());
    let mut s = String::with_capacity(TARGET_BYTES);
    s.push_str(":root,[data-vesper-theme=dusk]{");
    vars(&dusk, &mut s);
    s.push_str("}[data-vesper-theme=dawn]{");
    vars(&dawn, &mut s);
    s.push_str("}@media(prefers-color-scheme:light){:root:not([data-vesper-theme]){");
    vars(&dawn, &mut s);
    s.push_str("}}");
    s.push_str(concat!(
        ".vc{white-space:pre;background:var(--vc-bg-card);color:var(--vc-fg-primary)}",
        ".vc-k{color:var(--vc-keyword)}",
        ".vc-m,.vc-s,.vc-l{color:var(--vc-linear)}",
        ".vc-io,.vc-fm{color:var(--vc-inout)}",
        ".vc-el,.vc-ea{color:var(--vc-effect)}",
        ".vc-ea{font-style:italic}",
        ".vc-w,.vc-p{color:var(--vc-wire-proof)}",
        ".vc-c,.vc-gutter{color:var(--vc-comment)}",
        ".vc-lin-live{text-decoration:underline wavy var(--vc-linear) 1.5px;text-underline-offset:4px}",
        ".vc-gutter,.vc-twoslash-pin{user-select:none}",
        ".vc-cr{font-size:0;line-height:0}",
        ".vc-cr-br::after{content:\"\\A\"}",
    ));
    s
}
