//! Deterministic Disjoint Field-Tree Prism brand asset emitter (Appendix B).
//!
//! Synthesizes the canonical `256x256` mark, `16x16` pixel-snapped `.vsp`
//! favicon, and `520x140` horizontal wordmark lockup directly from
//! [`ChromaPalette`] tokens without external asset files.

use crate::ChromaPalette;
use core::fmt::Write;

/// Color rendering mode for the Disjoint Field-Tree Prism mark (Appendix B.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrandColorMode {
    TriPillarOklch,
    SolarAmberAccent,
    MonochromeStencil,
}

/// Parametric proportions `Theta = (g_chan, w_center, w_root, g_split)` on the
/// canonical `256x256` grid (Appendix B.2, B.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrismProportions {
    pub channel_gap_px: u16,
    pub center_width_px: u16,
    pub root_weight_px: u16,
    pub split_gap_px: u16,
    pub evening_star_glint: bool,
}

impl PrismProportions {
    /// Canonical master proportions `Theta_master = (12, 30, 38, 14)`.
    pub const CANONICAL_MASTER: Self = Self {
        channel_gap_px: 12,
        center_width_px: 30,
        root_weight_px: 38,
        split_gap_px: 14,
        evening_star_glint: false,
    };

    /// Bold app-tile proportions `Theta = (16, 34, 42, 16)`.
    pub const BOLD_APP_TILE: Self = Self {
        channel_gap_px: 16,
        center_width_px: 34,
        root_weight_px: 42,
        split_gap_px: 16,
        evening_star_glint: false,
    };

    /// Release star-seal proportions (`CANONICAL_MASTER` with glint enabled).
    pub const RELEASE_STAR_SEAL: Self = Self {
        evening_star_glint: true,
        ..Self::CANONICAL_MASTER
    };
}

/// Geometric validation error for parametric prism proportions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrandGeometryError {
    ZeroChannelGapBreaksDisjointness,
    CenterBladeExceedsVConeEnvelope,
}

/// Audits that parametric proportions preserve positive disjoint channels,
/// avoid chevron overlap, and stay strictly inside the `2:1` V-cone envelope.
pub fn verify_prism_geometry(p: PrismProportions) -> Result<(), BrandGeometryError> {
    if p.channel_gap_px == 0 {
        return Err(BrandGeometryError::ZeroChannelGapBreaksDisjointness);
    }
    let w_center = u32::from(p.center_width_px);
    let g_chan = u32::from(p.channel_gap_px);
    let w_root = u32::from(p.root_weight_px);
    let g_split = u32::from(p.split_gap_px);

    // Require positive blade/split dimensions and root chevron crossing inside x > 30.
    if w_center == 0 || g_split == 0 || w_root <= 8 {
        return Err(BrandGeometryError::CenterBladeExceedsVConeEnvelope);
    }
    // Center blade shoulders (`y_refract - w_center / 4`) must stay below `y_root_bot`.
    if g_split * 4 <= w_center {
        return Err(BrandGeometryError::CenterBladeExceedsVConeEnvelope);
    }
    // Keep inner channels strictly inside outer V-cone `[30, 226]`.
    if w_center + g_chan * 2 >= 196 {
        return Err(BrandGeometryError::CenterBladeExceedsVConeEnvelope);
    }
    let y_refract = 75 + w_root + g_split;
    if y_refract >= 230 {
        return Err(BrandGeometryError::CenterBladeExceedsVConeEnvelope);
    }
    // Compare exact `x_outer_l = (y_refract - 38) / 1.5` against `x_inner_l = 128 - w_center/2 - g_chan`
    // scaled by 3 (and doubled by 2 -> scaled by 6 to handle odd widths cleanly).
    let x_outer_top_x6 = (y_refract - 38) * 4;
    let x_inner_left_x6 = (256 - w_center - g_chan * 2) * 3;
    if x_outer_top_x6 >= x_inner_left_x6 {
        return Err(BrandGeometryError::CenterBladeExceedsVConeEnvelope);
    }
    Ok(())
}

fn format_coord(value: f64) -> String {
    let rounded = (value * 100.0).round() / 100.0;
    let s = format!("{rounded:.2}");
    if let Some(stripped) = s.strip_suffix(".00") {
        stripped.to_string()
    } else if let Some(stripped) = s.strip_suffix('0') {
        stripped.to_string()
    } else {
        s
    }
}

fn mode_colors(palette: &ChromaPalette, mode: BrandColorMode) -> (u32, u32, u32, u32) {
    match mode {
        BrandColorMode::TriPillarOklch => (
            palette.fg_default.srgb_hex,
            palette.linear_amber.srgb_hex,
            palette.inout_cyan.srgb_hex,
            palette.effect_iris.srgb_hex,
        ),
        BrandColorMode::SolarAmberAccent => (
            palette.fg_default.srgb_hex,
            palette.linear_amber.srgb_hex,
            palette.linear_amber.srgb_hex,
            palette.comment_slate.srgb_hex,
        ),
        BrandColorMode::MonochromeStencil => (
            palette.fg_default.srgb_hex,
            palette.fg_default.srgb_hex,
            palette.fg_default.srgb_hex,
            palette.fg_default.srgb_hex,
        ),
    }
}

fn push_prism_polygons(
    out: &mut String,
    indent: &str,
    palette: &ChromaPalette,
    mode: BrandColorMode,
    prop: PrismProportions,
    include_comments: bool,
) {
    let (root_hex, left_hex, center_hex, right_hex) = mode_colors(palette, mode);

    let y_valley = 75.0_f64;
    let y_root_bot = y_valley + f64::from(prop.root_weight_px);
    let x_root_split_l = (y_root_bot - 38.0) / 1.5;
    let y_root_split_l = 34.0 + (x_root_split_l - 30.0) * 2.0;
    let x_root_split_r = 256.0 - x_root_split_l;
    let x_shoulder_l = 128.0 - (y_valley - 34.0);
    let x_shoulder_r = 128.0 + (y_valley - 34.0);

    let y_refract = y_root_bot + f64::from(prop.split_gap_px);
    let half_c = f64::from(prop.center_width_px) * 0.5;
    let x_center_l = 128.0 - half_c;
    let x_center_r = 128.0 + half_c;
    let y_center_top = y_refract - half_c * 0.5;
    let y_center_bot = 34.0 + (x_center_l - 30.0) * 2.0;

    let x_outer_l = (y_refract - 38.0) / 1.5;
    let y_outer_l = 34.0 + (x_outer_l - 30.0) * 2.0;
    let x_inner_l = x_center_l - f64::from(prop.channel_gap_px);
    let y_inner_top = y_refract - (128.0 - x_inner_l) * 0.5;
    let y_inner_bot = 34.0 + (x_inner_l - 30.0) * 2.0;
    let x_outer_r = 256.0 - x_outer_l;
    let x_inner_r = x_center_r + f64::from(prop.channel_gap_px);

    if include_comments {
        let _ = writeln!(out, "{indent}<!-- Upper Aggregate Root Struct Chevron -->");
    }
    let _ = writeln!(
        out,
        "{indent}<polygon points=\"30,34 {},34 128,{} {},34 226,34 {},{} 128,{} {},{}\" fill=\"#{root_hex:06X}\"/>",
        format_coord(x_shoulder_l),
        format_coord(y_valley),
        format_coord(x_shoulder_r),
        format_coord(x_root_split_r),
        format_coord(y_root_split_l),
        format_coord(y_root_bot),
        format_coord(x_root_split_l),
        format_coord(y_root_split_l),
    );
    if include_comments {
        let _ = writeln!(
            out,
            "{indent}<!-- Port Blade: Mode::Linear & sink Ownership Transfer -->"
        );
    }
    let _ = writeln!(
        out,
        "{indent}<polygon points=\"{},{} {},{} {},{}\" fill=\"#{left_hex:06X}\"/>",
        format_coord(x_outer_l),
        format_coord(y_outer_l),
        format_coord(x_inner_l),
        format_coord(y_inner_top),
        format_coord(x_inner_l),
        format_coord(y_inner_bot),
    );
    if include_comments {
        let _ = writeln!(
            out,
            "{indent}<!-- Center Apex Blade: Disjoint inout FieldTree Projection -->"
        );
    }
    let _ = writeln!(
        out,
        "{indent}<polygon points=\"128,{} {},{} {},{} 128,230 {},{} {},{}\" fill=\"#{center_hex:06X}\"/>",
        format_coord(y_refract),
        format_coord(x_center_r),
        format_coord(y_center_top),
        format_coord(x_center_r),
        format_coord(y_center_bot),
        format_coord(x_center_l),
        format_coord(y_center_bot),
        format_coord(x_center_l),
        format_coord(y_center_top),
    );
    if include_comments {
        let _ = writeln!(
            out,
            "{indent}<!-- Starboard Blade: Shared in Views & One-Shot eff [...] Capabilities -->"
        );
    }
    let _ = writeln!(
        out,
        "{indent}<polygon points=\"{},{} {},{} {},{}\" fill=\"#{right_hex:06X}\"/>",
        format_coord(x_outer_r),
        format_coord(y_outer_l),
        format_coord(x_inner_r),
        format_coord(y_inner_top),
        format_coord(x_inner_r),
        format_coord(y_inner_bot),
    );

    if prop.evening_star_glint {
        let glint_hex = match mode {
            BrandColorMode::MonochromeStencil => palette.fg_default.srgb_hex,
            _ => palette.wire_emerald.srgb_hex,
        };
        let _ = writeln!(
            out,
            "{indent}<polygon points=\"128,26 131.5,34.5 140,38 131.5,41.5 128,50 124.5,41.5 116,38 124.5,34.5\" fill=\"#{glint_hex:06X}\"/>"
        );
    }
}

/// Deterministically emits the `256x256` Disjoint Field-Tree Prism SVG mark from [`ChromaPalette`].
pub fn emit_prism_mark_svg(
    palette: &ChromaPalette,
    mode: BrandColorMode,
    prop: PrismProportions,
) -> Result<String, BrandGeometryError> {
    verify_prism_geometry(prop)?;
    let mut svg = String::with_capacity(768);
    svg.push_str(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 256 256\" width=\"256\" height=\"256\" fill=\"none\">\n",
    );
    push_prism_polygons(&mut svg, "  ", palette, mode, prop, true);
    svg.push_str("</svg>\n");
    Ok(svg)
}

/// Emits the pixel-snapped `16x16` micro icon for `.vsp` IDE file trees and browser favicons.
pub fn emit_favicon_16_svg(palette: &ChromaPalette) -> String {
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 16 16\" width=\"16\" height=\"16\" fill=\"none\">\n\
         \x20 <polygon points=\"1,1.5 5,1.5 8,4.5 11,1.5 15,1.5 13.6,4.3 8,7.1 2.4,4.3\" fill=\"#{:06X}\"/>\n\
         \x20 <polygon points=\"3.1,5.7 6,7.1 6,11.5\" fill=\"#{:06X}\"/>\n\
         \x20 <polygon points=\"8,8.1 9,7.6 9,13.5 8,15.5 7,13.5 7,7.6\" fill=\"#{:06X}\"/>\n\
         \x20 <polygon points=\"12.9,5.7 10,7.1 10,11.5\" fill=\"#{:06X}\"/>\n\
         </svg>\n",
        palette.fg_default.srgb_hex,
        palette.linear_amber.srgb_hex,
        palette.inout_cyan.srgb_hex,
        palette.effect_iris.srgb_hex,
    )
}

/// Emits the `520x140` horizontal wordmark lockup (`Appendix B.4`) from [`ChromaPalette`].
pub fn emit_wordmark_svg(
    palette: &ChromaPalette,
    mode: BrandColorMode,
    prop: PrismProportions,
) -> Result<String, BrandGeometryError> {
    verify_prism_geometry(prop)?;
    let mut svg = String::with_capacity(1024);
    svg.push_str(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 520 140\" width=\"520\" height=\"140\" fill=\"none\">\n",
    );
    svg.push_str("  <g transform=\"translate(12, 12) scale(0.453125)\">\n");
    push_prism_polygons(&mut svg, "    ", palette, mode, prop, false);
    svg.push_str("  </g>\n");
    let _ = writeln!(
        svg,
        "  <text x=\"148\" y=\"95\" fill=\"#{:06X}\" font-family=\"'Space Grotesk', 'Inter', system-ui, sans-serif\" font-size=\"84\" font-weight=\"700\" letter-spacing=\"-3\">vesper</text>",
        palette.fg_default.srgb_hex
    );
    svg.push_str("</svg>\n");
    Ok(svg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_dusk_mark_matches_checked_in_website_svg() {
        let palette = ChromaPalette::vesper_dusk();
        let emitted = emit_prism_mark_svg(
            &palette,
            BrandColorMode::TriPillarOklch,
            PrismProportions::CANONICAL_MASTER,
        )
        .unwrap();
        let checked_in = include_str!("../../../website/brand/vesper-mark-dusk.svg");
        assert_eq!(emitted, checked_in);
    }

    #[test]
    fn geometry_verifier_rejects_zero_gap_and_vcone_violations() {
        assert_eq!(
            verify_prism_geometry(PrismProportions {
                channel_gap_px: 0,
                ..PrismProportions::CANONICAL_MASTER
            }),
            Err(BrandGeometryError::ZeroChannelGapBreaksDisjointness)
        );
        assert_eq!(
            verify_prism_geometry(PrismProportions {
                center_width_px: 0,
                ..PrismProportions::CANONICAL_MASTER
            }),
            Err(BrandGeometryError::CenterBladeExceedsVConeEnvelope)
        );
        assert_eq!(
            verify_prism_geometry(PrismProportions {
                center_width_px: 180,
                ..PrismProportions::CANONICAL_MASTER
            }),
            Err(BrandGeometryError::CenterBladeExceedsVConeEnvelope)
        );
        assert_eq!(
            verify_prism_geometry(PrismProportions {
                root_weight_px: 140,
                ..PrismProportions::CANONICAL_MASTER
            }),
            Err(BrandGeometryError::CenterBladeExceedsVConeEnvelope)
        );
        assert_eq!(
            verify_prism_geometry(PrismProportions::BOLD_APP_TILE),
            Ok(())
        );
        assert_eq!(
            verify_prism_geometry(PrismProportions::RELEASE_STAR_SEAL),
            Ok(())
        );
    }

    #[test]
    fn all_color_modes_favicon_and_wordmark_emit_valid_svgs() {
        let dawn = ChromaPalette::vesper_dawn();
        let mono = emit_prism_mark_svg(
            &dawn,
            BrandColorMode::MonochromeStencil,
            PrismProportions::BOLD_APP_TILE,
        )
        .unwrap();
        assert!(mono.contains("fill=\"#1B2030\""));
        assert!(!mono.contains("fill=\"#8B3C00\""));

        let seal = emit_prism_mark_svg(
            &dawn,
            BrandColorMode::SolarAmberAccent,
            PrismProportions::RELEASE_STAR_SEAL,
        )
        .unwrap();
        assert!(seal.contains("128,26 131.5,34.5"));

        let favicon = emit_favicon_16_svg(&ChromaPalette::vesper_dusk());
        assert!(favicon.contains("viewBox=\"0 0 16 16\""));
        assert!(favicon.contains("fill=\"#FFB454\""));
        assert!(favicon.contains("fill=\"#5CCFE6\""));
        assert!(favicon.contains("fill=\"#D2A6FF\""));

        let wordmark = emit_wordmark_svg(
            &ChromaPalette::vesper_dusk(),
            BrandColorMode::TriPillarOklch,
            PrismProportions::CANONICAL_MASTER,
        )
        .unwrap();
        assert!(wordmark.contains("viewBox=\"0 0 520 140\""));
        assert!(wordmark.contains("transform=\"translate(12, 12) scale(0.453125)\""));
        assert!(wordmark.contains(">vesper</text>"));
    }
}
