//! The card font: OpenType shaping with `rustybuzz` and outlines with
//! `ttf-parser`. Advances and ligatures come from the font, never from a fixed
//! per-character width.

use super::ShotError;

/// One glyph produced by shaping, in font units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShapedGlyph {
    pub glyph_id: u16,
    /// Byte offset, within the shaped text, of the first character this glyph
    /// belongs to (HarfBuzz cluster).
    pub cluster: u32,
    pub x_advance: i32,
    pub x_offset: i32,
    pub y_offset: i32,
}

/// A glyph placed in a scene, in CSS pixels at 1x.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PositionedGlyph {
    pub glyph_id: u16,
    pub x_px: f32,
    pub y_px: f32,
}

/// A parsed font face borrowed from caller-owned bytes.
pub struct ShotFont<'a> {
    bytes: &'a [u8],
    face: rustybuzz::Face<'a>,
    family: String,
    digest: [u8; 32],
}

impl std::fmt::Debug for ShotFont<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShotFont")
            .field("family", &self.family)
            .field("bytes", &self.bytes.len())
            .finish_non_exhaustive()
    }
}

impl<'a> ShotFont<'a> {
    /// Parses face 0 of an OpenType/TrueType font.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, ShotError> {
        let face = rustybuzz::Face::from_slice(bytes, 0)
            .ok_or(ShotError::Font("not a parseable OpenType face"))?;
        let upem = face.units_per_em();
        if !(16..=16384).contains(&upem) {
            return Err(ShotError::Font("units per em out of range"));
        }
        if face.ascender() <= 0 || face.descender() > 0 {
            return Err(ShotError::Font("ascender/descender metrics missing"));
        }
        let family = family_name(&face).ok_or(ShotError::Font("font has no family name"))?;
        Ok(Self {
            bytes,
            face,
            family,
            digest: *blake3::hash(bytes).as_bytes(),
        })
    }

    /// The font's typographic family name (name ID 16, else ID 1).
    pub fn family(&self) -> &str {
        &self.family
    }

    /// The raw font bytes (for SVG embedding).
    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    /// BLAKE3 of the font bytes; part of the cache key.
    pub fn digest(&self) -> [u8; 32] {
        self.digest
    }

    pub fn units_per_em(&self) -> u16 {
        self.face.units_per_em() as u16
    }

    pub fn ascender(&self) -> i16 {
        self.face.ascender()
    }

    pub fn descender(&self) -> i16 {
        self.face.descender()
    }

    /// The PostScript glyph name from the `post` table, when present.
    pub fn glyph_name(&self, glyph_id: u16) -> Option<&str> {
        self.face.glyph_name(ttf_parser::GlyphId(glyph_id))
    }

    /// The glyph the `cmap` table maps `c` to before any substitution.
    pub fn nominal_glyph(&self, c: char) -> Option<u16> {
        self.face.glyph_index(c).map(|g| g.0)
    }

    /// Shapes one line of text with the font's default features (including
    /// `liga` and `calt`, which is where programming ligatures live).
    pub fn shape(&self, text: &str) -> Vec<ShapedGlyph> {
        if text.is_empty() {
            return Vec::new();
        }
        let mut buffer = rustybuzz::UnicodeBuffer::new();
        buffer.push_str(text);
        buffer.set_direction(rustybuzz::Direction::LeftToRight);
        buffer.guess_segment_properties();
        let shaped = rustybuzz::shape(&self.face, &[], buffer);
        shaped
            .glyph_infos()
            .iter()
            .zip(shaped.glyph_positions())
            .map(|(info, pos)| ShapedGlyph {
                glyph_id: u16::try_from(info.glyph_id).unwrap_or(0),
                cluster: info.cluster,
                x_advance: pos.x_advance,
                x_offset: pos.x_offset,
                y_offset: pos.y_offset,
            })
            .collect()
    }

    /// Width of `text` in pixels at `font_px`.
    pub fn measure(&self, text: &str, font_px: f32) -> f32 {
        let units: i64 = self
            .shape(text)
            .iter()
            .map(|g| i64::from(g.x_advance))
            .sum();
        units as f32 * font_px / f32::from(self.units_per_em())
    }

    /// Feeds the glyph outline (font units, y up) to `sink`. Returns false for
    /// glyphs without an outline (spaces).
    pub(crate) fn outline(&self, glyph_id: u16, sink: &mut dyn ttf_parser::OutlineBuilder) -> bool {
        self.face
            .outline_glyph(ttf_parser::GlyphId(glyph_id), sink)
            .is_some()
    }
}

fn family_name(face: &ttf_parser::Face<'_>) -> Option<String> {
    let names = face.names();
    let mut fallback = None;
    for i in 0..names.len() {
        let Some(name) = names.get(i) else { continue };
        if !name.is_unicode() {
            continue;
        }
        if name.name_id == ttf_parser::name_id::TYPOGRAPHIC_FAMILY {
            if let Some(s) = name.to_string() {
                return Some(s);
            }
        } else if name.name_id == ttf_parser::name_id::FAMILY && fallback.is_none() {
            fallback = name.to_string();
        }
    }
    fallback
}
