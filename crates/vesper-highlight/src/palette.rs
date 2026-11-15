//! Vesper Dusk and Vesper Dawn (§43.2-§43.3, §43.5) with contrast gates.
use crate::{
    ChromaError, VesperTokenClass,
    color::{apca_lc, channels_hex, contrast_ratio, oklch_to_srgb, quantize, relative_luminance},
};
use smol_str::SmolStr;

/// One palette colour: OKLCH coordinates (L and C in basis points of 1, hue
/// in whole degrees), its 8-bit sRGB fallback, and the fallback's WCAG
/// relative luminance in thousandths. Use [`ThemeSwatch::new`], which
/// computes the luminance from the fallback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThemeSwatch {
    pub oklch_l_bp: u16,
    pub oklch_c_bp: u16,
    pub oklch_hue_deg: u16,
    pub srgb_hex: u32,
    pub rel_lum_x1000: u32,
}

impl ThemeSwatch {
    /// A swatch whose `rel_lum_x1000` is computed (rounded to nearest) from
    /// `srgb_hex`.
    pub fn new(oklch_l_bp: u16, oklch_c_bp: u16, oklch_hue_deg: u16, srgb_hex: u32) -> Self {
        Self {
            oklch_l_bp,
            oklch_c_bp,
            oklch_hue_deg,
            srgb_hex,
            rel_lum_x1000: (relative_luminance(srgb_hex) * 1000.0).round() as u32,
        }
    }
    /// The sRGB colour the OKLCH coordinates give, and whether it was out of
    /// gamut before rounding.
    pub fn oklch_srgb(&self) -> (u32, bool) {
        let (c, clipped) = quantize(oklch_to_srgb(
            f64::from(self.oklch_l_bp) / 10_000.0,
            f64::from(self.oklch_c_bp) / 10_000.0,
            f64::from(self.oklch_hue_deg),
        ));
        (channels_hex(c), clipped)
    }
    /// Largest per-channel difference, in 8-bit steps, between the OKLCH
    /// colour and the sRGB fallback.
    pub fn fallback_step_error(&self) -> u8 {
        let (computed, _) = self.oklch_srgb();
        let a = crate::color::hex_channels(computed);
        let b = crate::color::hex_channels(self.srgb_hex);
        (0..3).map(|i| a[i].abs_diff(b[i])).max().unwrap_or(0)
    }
}

/// Which palette slot a swatch fills.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwatchRole {
    BgCanvas,
    BgCard,
    FgDefault,
    LinearAmber,
    InoutCyan,
    EffectIris,
    WireEmerald,
    KeywordRose,
    CommentSlate,
}

impl SwatchRole {
    pub fn name(self) -> &'static str {
        match self {
            Self::BgCanvas => "bg_canvas",
            Self::BgCard => "bg_card",
            Self::FgDefault => "fg_default",
            Self::LinearAmber => "linear_amber",
            Self::InoutCyan => "inout_cyan",
            Self::EffectIris => "effect_iris",
            Self::WireEmerald => "wire_emerald",
            Self::KeywordRose => "keyword_rose",
            Self::CommentSlate => "comment_slate",
        }
    }
    /// The CSS custom property that carries this colour (§43.3).
    pub fn css_var(self) -> &'static str {
        match self {
            Self::BgCanvas => "--vc-bg-canvas",
            Self::BgCard => "--vc-bg-card",
            Self::FgDefault => "--vc-fg-primary",
            Self::LinearAmber => "--vc-linear",
            Self::InoutCyan => "--vc-inout",
            Self::EffectIris => "--vc-effect",
            Self::WireEmerald => "--vc-wire-proof",
            Self::KeywordRose => "--vc-keyword",
            Self::CommentSlate => "--vc-comment",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ChromaPalette {
    pub name: SmolStr,
    pub bg_canvas: ThemeSwatch,
    pub bg_card: ThemeSwatch,
    pub fg_default: ThemeSwatch,
    pub linear_amber: ThemeSwatch,
    pub inout_cyan: ThemeSwatch,
    pub effect_iris: ThemeSwatch,
    pub wire_emerald: ThemeSwatch,
    pub keyword_rose: ThemeSwatch,
    pub comment_slate: ThemeSwatch,
}

/// The audit of one foreground swatch against `bg_card`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SwatchAudit {
    pub role: SwatchRole,
    pub srgb_hex: u32,
    /// WCAG 2.x contrast ratio against `bg_card`.
    pub wcag_ratio: f64,
    /// The gate: 7.0 for primary tokens, 4.5 for comments.
    pub required_ratio: f64,
    /// APCA-W3 Lc of the swatch as text on `bg_card`.
    pub apca_lc: f64,
}

/// Contrast and fallback audit of a palette.
#[derive(Clone, Debug, PartialEq)]
pub struct ContrastAudit {
    pub palette: SmolStr,
    pub foreground: Vec<SwatchAudit>,
    /// `(role, fallback, OKLCH-computed colour, steps apart, out of gamut)`.
    pub fallbacks: Vec<(SwatchRole, u32, u32, u8, bool)>,
}

impl ChromaPalette {
    /// Vesper Dusk (dark). The sRGB fallbacks are the §43.3 values; the OKLCH
    /// coordinates are the ones those fallbacks have (§43.5's listed OKLCH
    /// numbers do not reproduce its hex values; see the lane report).
    pub fn vesper_dusk() -> Self {
        Self {
            name: SmolStr::new_inline("vesper-dusk"),
            bg_canvas: ThemeSwatch::new(1653, 197, 270, 0x0B0E17),
            bg_card: ThemeSwatch::new(2031, 311, 269, 0x111625),
            fg_default: ThemeSwatch::new(9303, 187, 266, 0xE2E8F5),
            linear_amber: ThemeSwatch::new(8238, 1408, 70, 0xFFB454),
            inout_cyan: ThemeSwatch::new(7972, 1080, 213, 0x5CCFE6),
            effect_iris: ThemeSwatch::new(7970, 1307, 306, 0xD2A6FF),
            wire_emerald: ThemeSwatch::new(8452, 1299, 165, 0x6EE7B7),
            keyword_rose: ThemeSwatch::new(7734, 1354, 13, 0xFF8F9C),
            comment_slate: ThemeSwatch::new(6765, 437, 267, 0x8B97B3),
        }
    }

    /// Vesper Dawn (light). §43.3's background, foreground and comment
    /// colours are kept. Its five accent colours reach only 4.7-6.6:1 on the
    /// card, so each was darkened in OKLCH (same hue family, lower L, chroma
    /// reduced only to stay in gamut) until it clears 7:1; the lane report
    /// lists the design value next to each replacement.
    pub fn vesper_dawn() -> Self {
        Self {
            name: SmolStr::new_inline("vesper-dawn"),
            bg_canvas: ThemeSwatch::new(9531, 74, 81, 0xF2EFEA),
            bg_card: ThemeSwatch::new(9798, 45, 78, 0xFAF8F5),
            fg_default: ThemeSwatch::new(2464, 315, 271, 0x1B2030),
            linear_amber: ThemeSwatch::new(4550, 1234, 48, 0x8B3C00),
            inout_cyan: ThemeSwatch::new(4385, 786, 218, 0x015C6F),
            effect_iris: ThemeSwatch::new(4739, 2328, 292, 0x6627D0),
            wire_emerald: ThemeSwatch::new(4332, 1048, 156, 0x016137),
            keyword_rose: ThemeSwatch::new(4681, 1786, 18, 0xA71132),
            comment_slate: ThemeSwatch::new(4899, 441, 267, 0x55607A),
        }
    }

    /// `vesper-dusk`/`dusk` or `vesper-dawn`/`dawn`.
    pub fn by_name(name: &str) -> Option<Self> {
        match name {
            "dusk" | "vesper-dusk" => Some(Self::vesper_dusk()),
            "dawn" | "vesper-dawn" => Some(Self::vesper_dawn()),
            _ => None,
        }
    }

    pub fn swatches(&self) -> [(SwatchRole, ThemeSwatch); 9] {
        [
            (SwatchRole::BgCanvas, self.bg_canvas),
            (SwatchRole::BgCard, self.bg_card),
            (SwatchRole::FgDefault, self.fg_default),
            (SwatchRole::LinearAmber, self.linear_amber),
            (SwatchRole::InoutCyan, self.inout_cyan),
            (SwatchRole::EffectIris, self.effect_iris),
            (SwatchRole::WireEmerald, self.wire_emerald),
            (SwatchRole::KeywordRose, self.keyword_rose),
            (SwatchRole::CommentSlate, self.comment_slate),
        ]
    }

    /// The palette slot a token class is drawn with.
    pub fn role_for(class: VesperTokenClass) -> SwatchRole {
        use VesperTokenClass as C;
        match class {
            C::KeywordControl => SwatchRole::KeywordRose,
            C::ModalSpecifier | C::PassModeSink | C::LinearBinding => SwatchRole::LinearAmber,
            C::PassModeInOut | C::FieldLeafMut => SwatchRole::InoutCyan,
            C::EffectLinear | C::EffectAbortable => SwatchRole::EffectIris,
            C::WireEndianScalar | C::DocProofAnchor => SwatchRole::WireEmerald,
            C::CommentTrivia => SwatchRole::CommentSlate,
            _ => SwatchRole::FgDefault,
        }
    }

    pub fn swatch(&self, role: SwatchRole) -> ThemeSwatch {
        match role {
            SwatchRole::BgCanvas => self.bg_canvas,
            SwatchRole::BgCard => self.bg_card,
            SwatchRole::FgDefault => self.fg_default,
            SwatchRole::LinearAmber => self.linear_amber,
            SwatchRole::InoutCyan => self.inout_cyan,
            SwatchRole::EffectIris => self.effect_iris,
            SwatchRole::WireEmerald => self.wire_emerald,
            SwatchRole::KeywordRose => self.keyword_rose,
            SwatchRole::CommentSlate => self.comment_slate,
        }
    }

    pub fn swatch_for(&self, class: VesperTokenClass) -> ThemeSwatch {
        self.swatch(Self::role_for(class))
    }

    /// The integer WCAG gate of §43.5 over `rel_lum_x1000`: at least 7:1
    /// (`(max + 50) >= 7 * (min + 50)`) for the six primary token colours
    /// and at least 4.5:1 (`2 * (max + 50) >= 9 * (min + 50)`) for comments,
    /// both against `bg_card`. `lean/ChromaHighlightSafety.lean` proves this
    /// integer test implies the stated ratio bound on the integer model.
    pub fn verify_contrast_gates(&self) -> Result<(), ChromaError> {
        let bg = self.bg_card.rel_lum_x1000;
        let primary = [
            self.fg_default,
            self.linear_amber,
            self.inout_cyan,
            self.effect_iris,
            self.wire_emerald,
            self.keyword_rose,
        ];
        for sw in primary {
            if !gate(sw.rel_lum_x1000, bg, 7, 1) {
                return Err(ChromaError::InsufficientContrast {
                    srgb_hex: sw.srgb_hex,
                });
            }
        }
        if !gate(self.comment_slate.rel_lum_x1000, bg, 9, 2) {
            return Err(ChromaError::InsufficientContrast {
                srgb_hex: self.comment_slate.srgb_hex,
            });
        }
        Ok(())
    }

    /// Every swatch's OKLCH coordinates must reproduce its sRGB fallback
    /// within one 8-bit step per channel, in gamut.
    pub fn verify_oklch_fallbacks(&self) -> Result<(), ChromaError> {
        for (_, sw) in self.swatches() {
            let (computed, clipped) = sw.oklch_srgb();
            if clipped || sw.fallback_step_error() > 1 {
                return Err(ChromaError::OklchFallbackMismatch {
                    srgb_hex: sw.srgb_hex,
                    computed_hex: computed,
                });
            }
        }
        Ok(())
    }

    /// Floating-point WCAG ratios, APCA Lc values and fallback errors.
    pub fn audit(&self) -> ContrastAudit {
        let bg = self.bg_card.srgb_hex;
        let foreground = self
            .swatches()
            .into_iter()
            .filter(|(r, _)| !matches!(r, SwatchRole::BgCanvas | SwatchRole::BgCard))
            .map(|(role, sw)| SwatchAudit {
                role,
                srgb_hex: sw.srgb_hex,
                wcag_ratio: contrast_ratio(sw.srgb_hex, bg),
                required_ratio: if role == SwatchRole::CommentSlate {
                    4.5
                } else {
                    7.0
                },
                apca_lc: apca_lc(sw.srgb_hex, bg),
            })
            .collect();
        let fallbacks = self
            .swatches()
            .into_iter()
            .map(|(role, sw)| {
                let (computed, clipped) = sw.oklch_srgb();
                (
                    role,
                    sw.srgb_hex,
                    computed,
                    sw.fallback_step_error(),
                    clipped,
                )
            })
            .collect();
        ContrastAudit {
            palette: self.name.clone(),
            foreground,
            fallbacks,
        }
    }
}

/// `num/den` contrast gate on luminance thousandths:
/// `den * (max + 50) >= num * (min + 50)`, in u64 so it cannot overflow.
fn gate(a: u32, b: u32, num: u64, den: u64) -> bool {
    let (hi, lo) = (u64::from(a.max(b)), u64::from(a.min(b)));
    den * (hi + 50) >= num * (lo + 50)
}
