//! `vesper shot`: hermetic vector SVG and Retina PNG code cards (§43.1 item 3,
//! §43.2, §43.5).
//!
//! The pipeline is
//!
//! 1. [`parse_shot_source`] reads the `@shot(...)` header directive and the
//!    Twoslash-style `//^?` / `//^!` callout comments out of the raw source
//!    text and returns the text that is displayed. These are directives that
//!    Chroma reads from text; they are not attribute syntax and are never
//!    compiled (attribute syntax belongs to §16.4).
//! 2. The caller highlights the displayed text with this crate's Tier 0/Tier 1
//!    highlighter and resolves each `//^?` query to a type/mode string (the CLI
//!    asks the checker; a library cannot depend on the root package).
//! 3. [`build_code_card_scene`] shapes every line with the bundled OpenType
//!    font (`rustybuzz`), so `->`, `=>` and `::` become the font's contextual
//!    ligature glyphs and every advance comes from the font, and lays out the
//!    window chrome, elevation shadow, callout pins and mode-legend pills as a
//!    resolution-independent display list.
//! 4. [`render_svg`] writes that display list as deterministic SVG bytes, and
//!    [`render_png`] rasterises it with `tiny-skia` at 1x-4x.
//! 5. [`shot_cache_key`] is the BLAKE3 key under which the CLI caches outputs
//!    in the content-addressed store (§43.1 item 5).
//!
//! The renderer takes the font as bytes; it embeds no font itself. Nothing
//! here launches a browser or reads ambient state.

mod cache;
mod directives;
mod font;
mod raster;
mod scene;
mod svg;

pub use cache::{SHOT_CACHE_SCHEMA, ShotCacheKey, ShotOutputFormat, shot_cache_key, source_digest};
pub use directives::{
    Callout, CalloutKind, ShotConfig, ShotDocument, ShotTheme, parse_shot_source,
};
pub use font::{PositionedGlyph, ShapedGlyph, ShotFont};
pub use raster::{RasterOptions, render_png, render_rgba};
pub use scene::{
    CalloutPin, CodeCardScene, LegendPill, PositionedGlyphRun, Primitive, Rgba, SceneOptions,
    build_code_card_scene, resolve_callouts,
};
pub use svg::{SvgOptions, render_svg};

use crate::ChromaError;
use std::fmt;

/// Finite ceilings for untrusted shot sources. Exceeding any of them is a
/// typed [`ShotError::Limit`], never a panic or a truncated card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShotLimits {
    pub max_source_bytes: usize,
    pub max_lines: usize,
    pub max_line_chars: usize,
    pub max_callouts: usize,
    pub max_callout_chars: usize,
    pub max_title_chars: usize,
    /// Largest card edge at 1x, in CSS pixels.
    pub max_card_edge_px: u32,
    /// Largest raster, in device pixels (width times height).
    pub max_raster_pixels: u64,
}

impl Default for ShotLimits {
    fn default() -> Self {
        Self {
            max_source_bytes: 64 * 1024,
            max_lines: 400,
            max_line_chars: 320,
            max_callouts: 32,
            max_callout_chars: 160,
            max_title_chars: 160,
            max_card_edge_px: 4096,
            max_raster_pixels: 64 * 1024 * 1024,
        }
    }
}

/// Every way a shot can fail. Malformed directives, unresolvable callouts and
/// over-budget inputs are errors; no partial card is produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShotError {
    /// The highlighter's spans or the palette failed lane 1's gates.
    Chroma(ChromaError),
    /// The font bytes are not a usable OpenType/TrueType face.
    Font(&'static str),
    /// A malformed `@shot(...)` header directive (1-based source line).
    Directive { line: usize, reason: String },
    /// A malformed or unresolvable `//^?` / `//^!` callout (1-based source line).
    Callout { line: usize, reason: String },
    /// A finite ceiling was exceeded.
    Limit(&'static str),
    /// The rasteriser could not allocate or encode the image.
    Raster(&'static str),
}

impl fmt::Display for ShotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Chroma(e) => write!(f, "chroma: {e:?}"),
            Self::Font(r) => write!(f, "shot font: {r}"),
            Self::Directive { line, reason } => {
                write!(f, "@shot directive at line {line}: {reason}")
            }
            Self::Callout { line, reason } => write!(f, "callout at line {line}: {reason}"),
            Self::Limit(r) => write!(f, "shot limit exceeded: {r}"),
            Self::Raster(r) => write!(f, "shot raster: {r}"),
        }
    }
}

impl std::error::Error for ShotError {}

impl From<ChromaError> for ShotError {
    fn from(e: ChromaError) -> Self {
        Self::Chroma(e)
    }
}
