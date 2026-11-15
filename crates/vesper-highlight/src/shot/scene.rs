//! The resolution-independent code-card scene (§43.5 `CodeCardScene`): a
//! display list in CSS pixels at 1x that both the SVG writer and the PNG
//! rasteriser paint in order.

use super::directives::{CalloutKind, ShotDocument, line_starts};
use super::font::{PositionedGlyph, ShotFont};
use super::{ShotError, ShotLimits};
use crate::{ChromaPalette, HighlightSpan, VesperTokenClass, verify_lossless_spans};

/// An sRGB colour with straight alpha.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rgba {
    pub rgb: u32,
    pub alpha: u8,
}

impl Rgba {
    pub const fn opaque(rgb: u32) -> Self {
        Self { rgb, alpha: 255 }
    }
    pub const fn with_alpha(rgb: u32, alpha: u8) -> Self {
        Self { rgb, alpha }
    }
    pub fn rgb8(self) -> (u8, u8, u8) {
        (
            (self.rgb >> 16) as u8,
            (self.rgb >> 8) as u8,
            self.rgb as u8,
        )
    }
}

/// A shaped, positioned run of glyphs sharing one colour (§43.5
/// `PositionedGlyphRun`, with the font's real glyphs and advances).
#[derive(Clone, Debug, PartialEq)]
pub struct PositionedGlyphRun {
    /// Pen position of the first glyph and the baseline, in px at 1x.
    pub x_px: f32,
    pub y_px: f32,
    pub font_px: f32,
    /// Shaped advance width of the whole run.
    pub width_px: f32,
    /// The exact source text the run covers.
    pub text: String,
    pub srgb_hex: u32,
    /// A live `Mode::Linear` obligation (wire `semantic_flags` bit 0).
    pub underline_linear: bool,
    /// The font substituted at least one glyph (a `liga`/`calt` ligature;
    /// wire `semantic_flags` bit 2).
    pub ligature: bool,
    /// Token class for code runs; `None` for chrome, pin and legend text.
    pub class: Option<VesperTokenClass>,
    /// Absolute byte range in the displayed source, for code runs.
    pub source_range: Option<(usize, usize)>,
    pub glyphs: Vec<PositionedGlyph>,
}

/// One paint operation, in paint order.
#[derive(Clone, Debug, PartialEq)]
pub enum Primitive {
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        fill: Rgba,
    },
    /// One translucent elevation-shadow layer. The opaque card is painted
    /// over it, so only the part outside `card` (x, y, w, h, radius) shows.
    ShadowLayer {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        fill: Rgba,
        card: [f32; 5],
    },
    Circle {
        cx: f32,
        cy: f32,
        r: f32,
        fill: Rgba,
    },
    /// A filled triangle (callout pin pointer).
    Triangle {
        points: [(f32, f32); 3],
        fill: Rgba,
    },
    Text(PositionedGlyphRun),
    /// The wavy obligation underline under a live linear token.
    WavyUnderline {
        x0: f32,
        x1: f32,
        y: f32,
        color: Rgba,
    },
}

/// A mode-legend pill in the card footer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LegendPill {
    pub label: &'static str,
    pub srgb_hex: u32,
}

/// A resolved callout ready to lay out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CalloutPin {
    /// 0-based display line.
    pub line: usize,
    /// Absolute byte range in the displayed source the pin points at
    /// (empty at a line end for a trailing note).
    pub anchor: (usize, usize),
    pub text: String,
    /// `true` for `//^?` queries, `false` for `//^!` notes.
    pub query: bool,
    /// `true` when the pin sits under its line (caret form); `false` for a
    /// trailing note at the end of its line.
    pub below: bool,
    /// Class of the token pointed at (accent colour).
    pub class: Option<VesperTokenClass>,
}

/// Layout parameters. The defaults are the card design; the scene is in CSS
/// pixels and `dpi_scale` only scales rasterisation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SceneOptions {
    pub dpi_scale: u32,
    pub font_px: f32,
    pub line_height_px: f32,
    pub chrome: bool,
    pub shadow: bool,
    pub legend: bool,
    pub limits: ShotLimits,
}

impl Default for SceneOptions {
    fn default() -> Self {
        Self {
            dpi_scale: 1,
            font_px: 14.0,
            line_height_px: 22.0,
            chrome: true,
            shadow: true,
            legend: true,
            limits: ShotLimits::default(),
        }
    }
}

/// A complete card.
#[derive(Clone, Debug, PartialEq)]
pub struct CodeCardScene {
    pub title: String,
    pub theme: String,
    pub width_px: u32,
    pub height_px: u32,
    pub dpi_scale: u32,
    pub bg_canvas_hex: u32,
    pub bg_card_hex: u32,
    pub legend: Vec<LegendPill>,
    pub primitives: Vec<Primitive>,
}

impl CodeCardScene {
    /// Every glyph run (code, title, pins and legend) in paint order.
    pub fn runs(&self) -> impl Iterator<Item = &PositionedGlyphRun> {
        self.primitives.iter().filter_map(|p| match p {
            Primitive::Text(run) => Some(run),
            _ => None,
        })
    }

    /// The code runs only.
    pub fn code_runs(&self) -> impl Iterator<Item = &PositionedGlyphRun> {
        self.runs().filter(|r| r.class.is_some())
    }
}

const MARGIN: f32 = 32.0;
const PAD_X: f32 = 28.0;
const PAD_Y: f32 = 18.0;
const HEADER: f32 = 40.0;
const FOOTER: f32 = 34.0;
const MIN_CARD_W: f32 = 480.0;
const RADIUS: f32 = 12.0;
const TITLE_PX: f32 = 13.0;
const PIN_PX: f32 = 12.0;
const PIN_ROW: f32 = 30.0;
const PIN_H: f32 = 20.0;
const LEGEND_PX: f32 = 11.0;
const SHADOW_LAYERS: u8 = 8;
/// Ceiling on a resolved `//^?` text (the checker's printed type and mode).
const MAX_RESOLVED_CHARS: usize = 640;

fn span_index(spans: &[HighlightSpan], byte: usize) -> usize {
    spans
        .partition_point(|s| s.end <= byte)
        .min(spans.len().saturating_sub(1))
}

/// Resolves the document's callouts against the highlighted spans. `resolve`
/// gets the token text and its absolute byte range and returns the inferred
/// type and mode for a `//^?` query when the caller knows it; otherwise the
/// author's fallback text is used, and with neither the query is an error.
pub fn resolve_callouts(
    doc: &ShotDocument,
    spans: &[HighlightSpan],
    mut resolve: impl FnMut(&str, usize, usize) -> Option<String>,
) -> Result<Vec<CalloutPin>, ShotError> {
    verify_lossless_spans(doc.display.len(), spans)?;
    let starts = doc.line_starts();
    let mut pins = Vec::with_capacity(doc.callouts.len());
    for c in &doc.callouts {
        let line_start = starts[c.display_line];
        let line_end = starts
            .get(c.display_line + 1)
            .map_or(doc.display.len(), |s| s - 1);
        let (anchor, class) = match c.column_byte {
            Some(col) => {
                let at = line_start + col;
                let sp = spans[span_index(spans, at)];
                if matches!(sp.class, VesperTokenClass::Whitespace) {
                    return Err(ShotError::Callout {
                        line: c.source_line,
                        reason: "caret points at whitespace".into(),
                    });
                }
                // Clamp a multi-line span (block comment) to this line.
                (
                    (sp.start.max(line_start), sp.end.min(line_end)),
                    Some(sp.class),
                )
            }
            None => ((line_end, line_end), None),
        };
        let (text, query) = match &c.kind {
            CalloutKind::Query { author_text } => {
                let token = &doc.display[anchor.0..anchor.1];
                let text = resolve(token, anchor.0, anchor.1)
                    .or_else(|| author_text.clone())
                    .ok_or_else(|| ShotError::Callout {
                        line: c.source_line,
                        reason: format!(
                            "cannot infer the type of `{token}` and the query has no fallback text"
                        ),
                    })?;
                (text, true)
            }
            CalloutKind::Note { text } => (text.clone(), false),
        };
        if text.chars().count() > MAX_RESOLVED_CHARS {
            return Err(ShotError::Limit("callout text length"));
        }
        pins.push(CalloutPin {
            line: c.display_line,
            anchor,
            text,
            query,
            below: c.column_byte.is_some(),
            class,
        });
    }
    Ok(pins)
}

struct Layout<'f, 'a> {
    font: &'f ShotFont<'a>,
    scale_units: f32,
}

impl Layout<'_, '_> {
    /// Shapes `text` into a run whose pen starts at `x` on baseline `y`.
    fn run(&self, text: &str, x: f32, y: f32, font_px: f32, rgb: u32) -> PositionedGlyphRun {
        let k = font_px / f32::from(self.font.units_per_em());
        let mut pen = 0i64;
        let mut glyphs = Vec::new();
        let shaped = self.font.shape(text);
        for g in &shaped {
            glyphs.push(PositionedGlyph {
                glyph_id: g.glyph_id,
                x_px: x + (pen + i64::from(g.x_offset)) as f32 * k,
                y_px: y - g.y_offset as f32 * k,
            });
            pen += i64::from(g.x_advance);
        }
        let ligature = substituted(self.font, text, &shaped);
        PositionedGlyphRun {
            x_px: x,
            y_px: y,
            font_px,
            width_px: pen as f32 * k,
            text: text.to_string(),
            srgb_hex: rgb,
            underline_linear: false,
            ligature,
            class: None,
            source_range: None,
            glyphs,
        }
    }

    fn baseline_offset(&self, font_px: f32, box_h: f32) -> f32 {
        let asc = f32::from(self.font.ascender()) * font_px / self.scale_units;
        let desc = -f32::from(self.font.descender()) * font_px / self.scale_units;
        (box_h - (asc + desc)) / 2.0 + asc
    }
}

/// Whether shaping replaced any character's nominal `cmap` glyph (GSUB
/// `liga`/`calt`), which is how programming fonts form ligatures.
fn substituted(font: &ShotFont<'_>, text: &str, glyphs: &[super::font::ShapedGlyph]) -> bool {
    glyphs.iter().any(|g| {
        text.get(g.cluster as usize..)
            .and_then(|t| t.chars().next())
            .is_some_and(|c| font.nominal_glyph(c) != Some(g.glyph_id))
    })
}

fn legend_for(spans: &[HighlightSpan], palette: &ChromaPalette) -> Vec<LegendPill> {
    use VesperTokenClass as C;
    let has = |classes: &[C]| spans.iter().any(|s| classes.contains(&s.class));
    let mut pills = Vec::new();
    if has(&[C::ModalSpecifier, C::PassModeSink, C::LinearBinding]) {
        pills.push(LegendPill {
            label: "linear",
            srgb_hex: palette.linear_amber.srgb_hex,
        });
    }
    if has(&[C::PassModeInOut, C::FieldLeafMut]) {
        pills.push(LegendPill {
            label: "inout",
            srgb_hex: palette.inout_cyan.srgb_hex,
        });
    }
    if has(&[C::EffectLinear, C::EffectAbortable]) {
        pills.push(LegendPill {
            label: "eff",
            srgb_hex: palette.effect_iris.srgb_hex,
        });
    }
    if has(&[C::WireEndianScalar, C::DocProofAnchor]) {
        pills.push(LegendPill {
            label: "wire/proof",
            srgb_hex: palette.wire_emerald.srgb_hex,
        });
    }
    pills
}

/// Builds a card for the displayed source `src` (with its lossless
/// highlight `spans`) and resolved callout `pins`. Every advance comes from
/// shaping each whole line with `font`, so contextual ligatures (`->`, `=>`,
/// `::`) become the font's ligature glyphs.
pub fn build_code_card_scene(
    title: &str,
    src: &str,
    spans: &[HighlightSpan],
    pins: &[CalloutPin],
    palette: &ChromaPalette,
    font: &ShotFont<'_>,
    options: SceneOptions,
) -> Result<CodeCardScene, ShotError> {
    if !(1..=4).contains(&options.dpi_scale) {
        return Err(ShotError::Limit("dpi scale must be 1-4"));
    }
    if !(options.font_px >= 6.0 && options.font_px <= 48.0)
        || !(options.line_height_px >= options.font_px && options.line_height_px <= 96.0)
    {
        return Err(ShotError::Limit("font size or line height"));
    }
    palette.verify_contrast_gates()?;
    verify_lossless_spans(src.len(), spans)?;
    if src.len() > options.limits.max_source_bytes {
        return Err(ShotError::Limit("source bytes"));
    }
    if title.chars().count() > options.limits.max_title_chars {
        return Err(ShotError::Limit("title length"));
    }
    let starts = line_starts(src);
    if starts.len() > options.limits.max_lines {
        return Err(ShotError::Limit("line count"));
    }
    if pins.len() > options.limits.max_callouts {
        return Err(ShotError::Limit("callout count"));
    }
    let layout = Layout {
        font,
        scale_units: f32::from(font.units_per_em()),
    };
    let fg = palette.fg_default.srgb_hex;
    let card_x = MARGIN;
    let card_y = MARGIN;
    let code_x = card_x + PAD_X;
    let header = if options.chrome { HEADER } else { 0.0 };
    let line_h = options.line_height_px;
    let code_base = layout.baseline_offset(options.font_px, line_h);
    let pin_base = layout.baseline_offset(PIN_PX, PIN_H);

    let mut code: Vec<Primitive> = Vec::new();
    let mut pin_prims: Vec<Primitive> = Vec::new();
    let mut content_w = 0f32;
    let mut row_top = card_y + header + PAD_Y;
    for (line, &line_start) in starts.iter().enumerate() {
        let line_end = starts.get(line + 1).map_or(src.len(), |s| s - 1);
        let text = &src[line_start..line_end];
        if text.chars().count() > options.limits.max_line_chars {
            return Err(ShotError::Limit("line length"));
        }
        let baseline = row_top + code_base;
        let shaped = font.shape(text);
        let k = options.font_px / layout.scale_units;
        // Pen x (px) at each glyph and at the end of the line.
        let mut pen = Vec::with_capacity(shaped.len() + 1);
        let mut units = 0i64;
        for g in &shaped {
            pen.push(units as f32 * k);
            units += i64::from(g.x_advance);
        }
        let line_w = units as f32 * k;
        pen.push(line_w);
        let x_at = |byte: usize| -> f32 {
            // Pen position of the first glyph whose cluster is at or after `byte`.
            let rel = byte - line_start;
            let i = shaped.partition_point(|g| (g.cluster as usize) < rel);
            code_x + pen[i]
        };
        // Group glyphs by the span their cluster falls in.
        let mut i = 0;
        while i < shaped.len() {
            let abs = line_start + shaped[i].cluster as usize;
            let si = span_index(spans, abs);
            let sp = spans[si];
            let mut j = i + 1;
            while j < shaped.len() && line_start + (shaped[j].cluster as usize) < sp.end {
                j += 1;
            }
            let run_end = if j < shaped.len() {
                line_start + shaped[j].cluster as usize
            } else {
                line_end
            };
            if !matches!(sp.class, VesperTokenClass::Whitespace) {
                let mut run = layout.run(
                    &src[abs..run_end],
                    code_x + pen[i],
                    baseline,
                    options.font_px,
                    0,
                );
                // Keep the line-level shaping (identical glyphs for whole-token
                // runs; context-dependent ones come from the line).
                run.glyphs = shaped[i..j]
                    .iter()
                    .enumerate()
                    .map(|(n, g)| PositionedGlyph {
                        glyph_id: g.glyph_id,
                        x_px: code_x + pen[i + n] + g.x_offset as f32 * k,
                        y_px: baseline - g.y_offset as f32 * k,
                    })
                    .collect();
                run.width_px = pen[j] - pen[i];
                run.ligature = substituted(font, text, &shaped[i..j]);
                run.srgb_hex = palette.swatch_for(sp.class).srgb_hex;
                run.underline_linear = sp.is_live_linear;
                run.class = Some(sp.class);
                run.source_range = Some((abs, run_end));
                if sp.is_live_linear {
                    code.push(Primitive::WavyUnderline {
                        x0: run.x_px,
                        x1: run.x_px + run.width_px,
                        y: baseline + 3.5,
                        color: Rgba::opaque(palette.linear_amber.srgb_hex),
                    });
                }
                code.push(Primitive::Text(run));
            }
            i = j;
        }
        content_w = content_w.max(line_w);
        // Trailing notes sit after the code of their line.
        let mut note_x = code_x + line_w + 20.0;
        for pin in pins.iter().filter(|p| p.line == line && !p.below) {
            let box_top = row_top + (line_h - PIN_H) / 2.0;
            let label = layout.run(&pin.text, note_x + 20.0, box_top + pin_base, PIN_PX, fg);
            let w = label.width_px + 30.0;
            pin_prims.push(Primitive::Rect {
                x: note_x,
                y: box_top,
                w,
                h: PIN_H,
                radius: PIN_H / 2.0,
                fill: Rgba::opaque(palette.bg_canvas.srgb_hex),
            });
            pin_prims.push(Primitive::Circle {
                cx: note_x + 11.0,
                cy: box_top + PIN_H / 2.0,
                r: 3.5,
                fill: Rgba::opaque(palette.effect_iris.srgb_hex),
            });
            pin_prims.push(Primitive::Text(label));
            note_x += w + 8.0;
            content_w = content_w.max(note_x - 8.0 - code_x);
        }
        row_top += line_h;
        // Caret pins open a row under their line.
        for pin in pins.iter().filter(|p| p.line == line && p.below) {
            let token_x = x_at(pin.anchor.0);
            let token_end = x_at(pin.anchor.1);
            let accent = pin.class.map_or(palette.comment_slate.srgb_hex, |c| {
                palette.swatch_for(c).srgb_hex
            });
            let box_x = (token_x - 8.0).max(code_x - 8.0);
            let box_top = row_top + 7.0;
            let prefix = if pin.query { "" } else { "note: " };
            let text = format!("{prefix}{}", pin.text);
            let label = layout.run(&text, box_x + 10.0, box_top + pin_base, PIN_PX, fg);
            let w = label.width_px + 20.0;
            let mid = (token_x + token_end) / 2.0;
            pin_prims.push(Primitive::Triangle {
                points: [
                    (mid - 5.0, box_top),
                    (mid + 5.0, box_top),
                    (mid, box_top - 5.0),
                ],
                fill: Rgba::opaque(accent),
            });
            pin_prims.push(Primitive::Rect {
                x: box_x,
                y: box_top,
                w,
                h: PIN_H,
                radius: 5.0,
                fill: Rgba::opaque(accent),
            });
            pin_prims.push(Primitive::Rect {
                x: box_x + 1.0,
                y: box_top + 1.0,
                w: w - 2.0,
                h: PIN_H - 2.0,
                radius: 4.0,
                fill: Rgba::opaque(palette.bg_canvas.srgb_hex),
            });
            pin_prims.push(Primitive::Text(label));
            content_w = content_w.max(box_x + w - code_x);
            row_top += PIN_ROW;
        }
    }
    let code_bottom = row_top;

    let legend = if options.legend {
        legend_for(spans, palette)
    } else {
        Vec::new()
    };
    let title_run = (options.chrome && !title.is_empty()).then(|| {
        let mut r = layout.run(title, 0.0, 0.0, TITLE_PX, palette.comment_slate.srgb_hex);
        r.class = None;
        r
    });
    let legend_runs: Vec<PositionedGlyphRun> = legend
        .iter()
        .map(|p| layout.run(p.label, 0.0, 0.0, LEGEND_PX, p.srgb_hex))
        .collect();
    let legend_w: f32 = legend_runs.iter().map(|r| r.width_px + 30.0).sum();
    let title_w = title_run.as_ref().map_or(0.0, |r| r.width_px + 2.0 * 80.0);
    let card_w = (content_w + 2.0 * PAD_X)
        .max(MIN_CARD_W)
        .max(title_w)
        .max(legend_w + 2.0 * PAD_X)
        .ceil();
    let card_h =
        (code_bottom - card_y + PAD_Y + if legend.is_empty() { 0.0 } else { FOOTER }).ceil();
    let width = card_w + 2.0 * MARGIN;
    let height = card_h + 2.0 * MARGIN;
    let max_edge = options.limits.max_card_edge_px as f32;
    if width > max_edge || height > max_edge {
        return Err(ShotError::Limit("card size"));
    }

    let mut prims = Vec::with_capacity(code.len() + pin_prims.len() + 32);
    prims.push(Primitive::Rect {
        x: 0.0,
        y: 0.0,
        w: width,
        h: height,
        radius: 0.0,
        fill: Rgba::opaque(palette.bg_canvas.srgb_hex),
    });
    if options.shadow {
        // Ambient elevation: concentric translucent layers, offset downwards.
        // Light cards need a lighter shadow to read as the same elevation.
        let dark = luminance_x1000(palette.bg_card.srgb_hex) < 500;
        let alpha = if dark { 12 } else { 5 };
        for i in (1..=SHADOW_LAYERS).rev() {
            let grow = f32::from(i) * 2.0;
            prims.push(Primitive::ShadowLayer {
                x: card_x - grow,
                y: card_y - grow + 10.0,
                w: card_w + 2.0 * grow,
                h: card_h + 2.0 * grow,
                radius: RADIUS + grow,
                fill: Rgba::with_alpha(0x000000, alpha),
                card: [card_x, card_y, card_w, card_h, RADIUS],
            });
        }
    }
    prims.push(Primitive::Rect {
        x: card_x,
        y: card_y,
        w: card_w,
        h: card_h,
        radius: RADIUS,
        fill: Rgba::opaque(palette.bg_card.srgb_hex),
    });
    if options.chrome {
        for (n, rgb) in [
            palette.keyword_rose.srgb_hex,
            palette.linear_amber.srgb_hex,
            palette.wire_emerald.srgb_hex,
        ]
        .into_iter()
        .enumerate()
        {
            prims.push(Primitive::Circle {
                cx: card_x + 20.0 + n as f32 * 18.0,
                cy: card_y + HEADER / 2.0,
                r: 6.0,
                fill: Rgba::opaque(rgb),
            });
        }
        if let Some(r) = title_run {
            let x = card_x + (card_w - r.width_px) / 2.0;
            let y = card_y + layout.baseline_offset(TITLE_PX, HEADER);
            prims.push(Primitive::Text(
                layout.run(title, x, y, TITLE_PX, r.srgb_hex),
            ));
        }
        prims.push(Primitive::Rect {
            x: card_x,
            y: card_y + HEADER - 1.0,
            w: card_w,
            h: 1.0,
            radius: 0.0,
            fill: Rgba::with_alpha(fg, 20),
        });
    }
    prims.extend(code);
    prims.extend(pin_prims);
    if !legend.is_empty() {
        let pill_h = 18.0;
        let top = card_y + card_h - FOOTER + (FOOTER - pill_h) / 2.0 - 4.0;
        let mut x = card_x + card_w - PAD_X - legend_w + 8.0;
        let base = layout.baseline_offset(LEGEND_PX, pill_h);
        for (pill, label) in legend.iter().zip(&legend_runs) {
            let w = label.width_px + 22.0;
            prims.push(Primitive::Rect {
                x,
                y: top,
                w,
                h: pill_h,
                radius: pill_h / 2.0,
                fill: Rgba::with_alpha(pill.srgb_hex, 36),
            });
            prims.push(Primitive::Circle {
                cx: x + 9.0,
                cy: top + pill_h / 2.0,
                r: 3.0,
                fill: Rgba::opaque(pill.srgb_hex),
            });
            prims.push(Primitive::Text(layout.run(
                pill.label,
                x + 16.0,
                top + base,
                LEGEND_PX,
                pill.srgb_hex,
            )));
            x += w + 8.0;
        }
    }
    Ok(CodeCardScene {
        title: title.to_string(),
        theme: palette.name.to_string(),
        width_px: width as u32,
        height_px: height as u32,
        dpi_scale: options.dpi_scale,
        bg_canvas_hex: palette.bg_canvas.srgb_hex,
        bg_card_hex: palette.bg_card.srgb_hex,
        legend,
        primitives: prims,
    })
}

/// Rough relative luminance (x1000) of an sRGB colour from its channel sum;
/// only used to pick the shadow strength.
fn luminance_x1000(rgb: u32) -> u32 {
    let (r, g, b) = ((rgb >> 16) & 0xFF, (rgb >> 8) & 0xFF, rgb & 0xFF);
    (r * 213 + g * 715 + b * 72) / 255
}

/// The wavy obligation underline as quadratic segments `(control, end)`
/// starting at `(x0, y)`; shared by the SVG writer and the rasteriser.
pub(crate) fn wavy_segments(x0: f32, x1: f32, y: f32) -> Vec<[(f32, f32); 2]> {
    const HALF: f32 = 2.0;
    const AMP: f32 = 1.5;
    let mut out = Vec::new();
    let mut x = x0;
    let mut up = true;
    while x + HALF <= x1 + 0.01 {
        let dy = if up { -AMP } else { AMP };
        out.push([(x + HALF / 2.0, y + dy), (x + HALF, y)]);
        x += HALF;
        up = !up;
    }
    out
}
