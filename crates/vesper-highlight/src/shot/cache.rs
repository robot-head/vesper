//! Content-addressed cache keys for rendered cards (§43.1 item 5):
//! BLAKE3 over the lossless token partition digest, the theme id and the shot
//! configuration. The CLI stores outputs under this key in the Loom CAS.

use super::font::ShotFont;
use super::scene::CalloutPin;
use crate::HighlightSpan;

/// Version of the key derivation; bump it whenever renderer output changes.
pub const SHOT_CACHE_SCHEMA: &str = "vesper-shot-cache/1";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShotOutputFormat {
    Svg { embed_font: bool },
    Png { dpi_scale: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ShotCacheKey(pub [u8; 32]);

impl ShotCacheKey {
    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// Digest of the displayed source and its lossless highlight partition (every
/// span's range, class and live-linear flag). This stands for the CST digest:
/// the partition is derived from the lossless CST and covers every byte.
pub fn source_digest(display: &str, spans: &[HighlightSpan]) -> [u8; 32] {
    let mut h = blake3::Hasher::new_derive_key("vesper-shot source partition v1");
    h.update(&(display.len() as u64).to_le_bytes());
    h.update(display.as_bytes());
    h.update(&(spans.len() as u64).to_le_bytes());
    for s in spans {
        h.update(&(s.start as u64).to_le_bytes());
        h.update(&(s.end as u64).to_le_bytes());
        h.update(&(s.class as u16).to_le_bytes());
        h.update(&[u8::from(s.is_live_linear)]);
    }
    *h.finalize().as_bytes()
}

fn field(h: &mut blake3::Hasher, bytes: &[u8]) {
    h.update(&(bytes.len() as u64).to_le_bytes());
    h.update(bytes);
}

/// `BLAKE3(source digest + theme id + shot config)`. The config covers the
/// title, the resolved callout pins, the output format and scale, and the
/// font's digest, so any change to what is drawn changes the key.
pub fn shot_cache_key(
    source_digest: [u8; 32],
    theme_id: &str,
    title: &str,
    pins: &[CalloutPin],
    format: ShotOutputFormat,
    font: &ShotFont<'_>,
) -> ShotCacheKey {
    let mut h = blake3::Hasher::new_derive_key(SHOT_CACHE_SCHEMA);
    h.update(&source_digest);
    field(&mut h, theme_id.as_bytes());
    field(&mut h, title.as_bytes());
    h.update(&(pins.len() as u64).to_le_bytes());
    for p in pins {
        h.update(&(p.line as u64).to_le_bytes());
        h.update(&(p.anchor.0 as u64).to_le_bytes());
        h.update(&(p.anchor.1 as u64).to_le_bytes());
        h.update(&[u8::from(p.query), u8::from(p.below)]);
        h.update(&p.class.map_or(u16::MAX, |c| c as u16).to_le_bytes());
        field(&mut h, p.text.as_bytes());
    }
    match format {
        ShotOutputFormat::Svg { embed_font } => h.update(&[b's', u8::from(embed_font)]),
        ShotOutputFormat::Png { dpi_scale } => h.update(&[b'p', dpi_scale as u8]),
    };
    h.update(&font.digest());
    ShotCacheKey(*h.finalize().as_bytes())
}
