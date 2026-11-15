//! PNG rasterisation of a [`CodeCardScene`] with `tiny-skia` at 1x-4x.
//!
//! Glyphs are filled from the font's own outlines (no system font lookup), so
//! the raster depends only on the scene, the font bytes and the scale. The
//! crate is built without tiny-skia's SIMD feature so every target runs the
//! same scalar pipeline.

use super::font::ShotFont;
use super::scene::{CodeCardScene, Primitive, Rgba, wavy_segments};
use super::{ShotError, ShotLimits};
use std::collections::HashMap;
use tiny_skia::{FillRule, Paint, Path, PathBuilder, Pixmap, Stroke, Transform};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RasterOptions {
    /// Device pixels per CSS pixel (1-4). `None` uses the scene's `dpi_scale`.
    pub dpi_scale: Option<u32>,
    pub limits: ShotLimits,
}

struct Outline(PathBuilder);

impl ttf_parser::OutlineBuilder for Outline {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.move_to(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.line_to(x, y);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.0.quad_to(x1, y1, x, y);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.0.cubic_to(x1, y1, x2, y2, x, y);
    }
    fn close(&mut self) {
        self.0.close();
    }
}

fn paint(c: Rgba) -> Paint<'static> {
    let (r, g, b) = c.rgb8();
    let mut p = Paint::default();
    p.set_color_rgba8(r, g, b, c.alpha);
    p.anti_alias = true;
    p
}

fn rounded_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<Path> {
    let mut pb = PathBuilder::new();
    push_rounded_rect(&mut pb, x, y, w, h, r);
    pb.finish()
}

fn push_rounded_rect(pb: &mut PathBuilder, x: f32, y: f32, w: f32, h: f32, r: f32) {
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    if r == 0.0 {
        if let Some(rect) = tiny_skia::Rect::from_xywh(x, y, w, h) {
            pb.push_rect(rect);
        }
        return;
    }
    // Quarter circles as cubics (k = 4/3 (sqrt 2 - 1)).
    let k = r * 0.552_284_8;
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
}

/// Fills a rounded rectangle. Opaque fills paint the straight interior with
/// `fill_rect` and only the four corners as paths, which keeps 4x cards cheap.
fn fill_rounded(pixmap: &mut Pixmap, [x, y, w, h, r]: [f32; 5], fill: Rgba, base: Transform) {
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    let p = paint(fill);
    if fill.alpha == 255 && r > 0.0 {
        for (rx, ry, rw, rh) in [
            (x + r, y, w - 2.0 * r, h),
            (x, y + r, r, h - 2.0 * r),
            (x + w - r, y + r, r, h - 2.0 * r),
        ] {
            if let Some(rect) = tiny_skia::Rect::from_xywh(rx, ry, rw, rh) {
                // Pixel-aligned rectangles need no coverage computation.
                let s = base.sx;
                let aligned = [rx, ry, rw, rh].iter().all(|v| (v * s).fract() == 0.0);
                let mut q = p.clone();
                q.anti_alias = !aligned;
                pixmap.fill_rect(rect, &q, base, None);
            }
        }
        let k = r * 0.552_284_8;
        for (cx, cy, sx, sy) in [
            (x + r, y + r, -1.0, -1.0),
            (x + w - r, y + r, 1.0, -1.0),
            (x + r, y + h - r, -1.0, 1.0),
            (x + w - r, y + h - r, 1.0, 1.0),
        ] {
            let mut pb = PathBuilder::new();
            pb.move_to(cx, cy);
            pb.line_to(cx + sx * r, cy);
            pb.cubic_to(
                cx + sx * r,
                cy + sy * k,
                cx + sx * k,
                cy + sy * r,
                cx,
                cy + sy * r,
            );
            pb.close();
            if let Some(path) = pb.finish() {
                pixmap.fill_path(&path, &p, FillRule::Winding, base, None);
            }
        }
    } else if let Some(path) = rounded_rect(x, y, w, h, r) {
        pixmap.fill_path(&path, &p, FillRule::Winding, base, None);
    }
}

/// Paints the scene and returns `(width, height, premultiplied RGBA8)` at the
/// chosen scale. The canvas is opaque, so premultiplied equals straight RGBA.
pub fn render_rgba(
    scene: &CodeCardScene,
    font: &ShotFont<'_>,
    options: RasterOptions,
) -> Result<(u32, u32, Vec<u8>), ShotError> {
    let pixmap = paint_scene(scene, font, options)?;
    Ok((pixmap.width(), pixmap.height(), pixmap.data().to_vec()))
}

/// Paints the scene and encodes it as PNG.
pub fn render_png(
    scene: &CodeCardScene,
    font: &ShotFont<'_>,
    options: RasterOptions,
) -> Result<Vec<u8>, ShotError> {
    let pixmap = paint_scene(scene, font, options)?;
    if !pixmap
        .data()
        .as_chunks::<4>()
        .0
        .iter()
        .all(|px| px[3] == 255)
    {
        // Translucent pixels need tiny-skia's demultiplying encoder.
        return pixmap
            .encode_png()
            .map_err(|_| ShotError::Raster("PNG encoding failed"));
    }
    // Opaque: premultiplied RGBA is straight RGBA, so encode in place without
    // tiny-skia's full-size demultiplied copy.
    let mut out = Vec::with_capacity(pixmap.data().len() / 8);
    let mut encoder = png::Encoder::new(&mut out, pixmap.width(), pixmap.height());
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .and_then(|mut w| w.write_image_data(pixmap.data()))
        .map_err(|_| ShotError::Raster("PNG encoding failed"))?;
    Ok(out)
}

fn paint_scene(
    scene: &CodeCardScene,
    font: &ShotFont<'_>,
    options: RasterOptions,
) -> Result<Pixmap, ShotError> {
    let scale = options.dpi_scale.unwrap_or(scene.dpi_scale);
    if !(1..=4).contains(&scale) {
        return Err(ShotError::Limit("dpi scale must be 1-4"));
    }
    let w = scene
        .width_px
        .checked_mul(scale)
        .ok_or(ShotError::Limit("raster size"))?;
    let h = scene
        .height_px
        .checked_mul(scale)
        .ok_or(ShotError::Limit("raster size"))?;
    if u64::from(w) * u64::from(h) > options.limits.max_raster_pixels {
        return Err(ShotError::Limit("raster pixels"));
    }
    let mut pixmap = Pixmap::new(w, h).ok_or(ShotError::Raster("cannot allocate the pixmap"))?;
    let s = scale as f32;
    let base = Transform::from_scale(s, s);
    let units = f32::from(font.units_per_em());
    let mut glyphs: HashMap<u16, Option<Path>> = HashMap::new();
    for p in &scene.primitives {
        match p {
            Primitive::Rect {
                x,
                y,
                w,
                h,
                radius,
                fill,
            } => {
                let covers = *x <= 0.0
                    && *y <= 0.0
                    && *w >= scene.width_px as f32
                    && *h >= scene.height_px as f32;
                if covers && fill.alpha == 255 {
                    let (r, g, b) = fill.rgb8();
                    pixmap.fill(tiny_skia::Color::from_rgba8(r, g, b, 255));
                } else {
                    fill_rounded(&mut pixmap, [*x, *y, *w, *h, *radius], *fill, base);
                }
            }
            Primitive::ShadowLayer {
                x,
                y,
                w,
                h,
                radius,
                fill,
                card,
            } => {
                // Only the ring outside the (later, opaque) card is visible.
                let mut pb = PathBuilder::new();
                push_rounded_rect(&mut pb, *x, *y, *w, *h, *radius);
                push_rounded_rect(&mut pb, card[0], card[1], card[2], card[3], card[4]);
                if let Some(path) = pb.finish() {
                    pixmap.fill_path(&path, &paint(*fill), FillRule::EvenOdd, base, None);
                }
            }
            Primitive::Circle { cx, cy, r, fill } => {
                if let Some(path) = PathBuilder::from_circle(*cx, *cy, *r) {
                    pixmap.fill_path(&path, &paint(*fill), FillRule::Winding, base, None);
                }
            }
            Primitive::Triangle { points, fill } => {
                let mut pb = PathBuilder::new();
                pb.move_to(points[0].0, points[0].1);
                pb.line_to(points[1].0, points[1].1);
                pb.line_to(points[2].0, points[2].1);
                pb.close();
                if let Some(path) = pb.finish() {
                    pixmap.fill_path(&path, &paint(*fill), FillRule::Winding, base, None);
                }
            }
            Primitive::WavyUnderline { x0, x1, y, color } => {
                let mut pb = PathBuilder::new();
                pb.move_to(*x0, *y);
                for [(cx, cy), (ex, ey)] in wavy_segments(*x0, *x1, *y) {
                    pb.quad_to(cx, cy, ex, ey);
                }
                if let Some(path) = pb.finish() {
                    let stroke = Stroke {
                        width: 1.2,
                        ..Stroke::default()
                    };
                    pixmap.stroke_path(&path, &paint(*color), &stroke, base, None);
                }
            }
            Primitive::Text(run) => {
                let k = run.font_px / units;
                let fill = paint(Rgba::opaque(run.srgb_hex));
                for g in &run.glyphs {
                    let path = glyphs.entry(g.glyph_id).or_insert_with(|| {
                        let mut outline = Outline(PathBuilder::new());
                        if font.outline(g.glyph_id, &mut outline) {
                            outline.0.finish()
                        } else {
                            None
                        }
                    });
                    if let Some(path) = path {
                        let t =
                            base.pre_concat(Transform::from_row(k, 0.0, 0.0, -k, g.x_px, g.y_px));
                        pixmap.fill_path(path, &fill, FillRule::Winding, t, None);
                    }
                }
            }
        }
    }
    Ok(pixmap)
}
