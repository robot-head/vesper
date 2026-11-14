//! RGBA8 screenshots and golden comparison with a stated tolerance.
//!
//! The comparison is pure so it is tested without GTK; capturing and PNG
//! encoding go through GDK (`GtkBackend::screenshot`, `RgbaImage::save_png`,
//! `RgbaImage::load_png` with the `gtk` feature).

/// Largest accepted image side, in pixels.
pub const MAX_SIDE: u32 = 8192;

/// Unpremultiplied RGBA, 8 bits per channel, rows top to bottom, no padding.
#[derive(Clone, PartialEq, Eq)]
pub struct RgbaImage {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl std::fmt::Debug for RgbaImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RgbaImage({}x{})", self.width, self.height)
    }
}

/// Malformed image data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImageError {
    /// Zero or above [`MAX_SIDE`].
    BadSize { width: u32, height: u32 },
    /// `pixels.len()` is not `width * height * 4`.
    BadLength { expected: usize, actual: usize },
    /// Decoding or encoding failed (GDK's message).
    Codec(String),
}

impl RgbaImage {
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self, ImageError> {
        if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
            return Err(ImageError::BadSize { width, height });
        }
        // Both sides are at most 8192, so this cannot overflow usize on any
        // 64-bit or 32-bit target (8192 * 8192 * 4 = 2^28).
        let expected = width as usize * height as usize * 4;
        if pixels.len() != expected {
            return Err(ImageError::BadLength {
                expected,
                actual: pixels.len(),
            });
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }
    /// The RGBA value at `(x, y)`, or `None` outside the image.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let i = (y as usize * self.width as usize + x as usize) * 4;
        Some([
            self.pixels[i],
            self.pixels[i + 1],
            self.pixels[i + 2],
            self.pixels[i + 3],
        ])
    }
}

/// How far a screenshot may drift from its golden and still match.
///
/// A pixel *differs* when any channel differs by more than
/// `max_channel_delta`. The images match when at most
/// `max_differing_per_million` pixels per million (rounded down, of the
/// golden's pixel count) differ. Sizes must be equal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tolerance {
    pub max_channel_delta: u8,
    pub max_differing_per_million: u32,
}

impl Tolerance {
    pub const EXACT: Tolerance = Tolerance {
        max_channel_delta: 0,
        max_differing_per_million: 0,
    };
}

/// Statistics of one comparison.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageDiff {
    pub pixels: u64,
    pub differing: u64,
    /// Largest per-channel difference over all pixels.
    pub max_channel_delta: u8,
    /// The differing-pixel budget the tolerance allowed.
    pub allowed: u64,
}

/// Why a screenshot did not match.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mismatch {
    Size {
        golden: (u32, u32),
        actual: (u32, u32),
    },
    Pixels(ImageDiff),
}

/// Compare `actual` with `golden` under `tolerance`.
pub fn compare(
    golden: &RgbaImage,
    actual: &RgbaImage,
    tolerance: Tolerance,
) -> Result<ImageDiff, Mismatch> {
    if (golden.width, golden.height) != (actual.width, actual.height) {
        return Err(Mismatch::Size {
            golden: (golden.width, golden.height),
            actual: (actual.width, actual.height),
        });
    }
    let pixels = u64::from(golden.width) * u64::from(golden.height);
    let mut differing = 0u64;
    let mut max_delta = 0u8;
    let (golden_px, _) = golden.pixels.as_chunks::<4>();
    let (actual_px, _) = actual.pixels.as_chunks::<4>();
    for (g, a) in golden_px.iter().zip(actual_px) {
        let delta = g
            .iter()
            .zip(a)
            .map(|(x, y)| x.abs_diff(*y))
            .max()
            .unwrap_or(0);
        max_delta = max_delta.max(delta);
        if delta > tolerance.max_channel_delta {
            differing += 1;
        }
    }
    let allowed = pixels * u64::from(tolerance.max_differing_per_million) / 1_000_000;
    let diff = ImageDiff {
        pixels,
        differing,
        max_channel_delta: max_delta,
        allowed,
    };
    if differing > allowed {
        Err(Mismatch::Pixels(diff))
    } else {
        Ok(diff)
    }
}

#[cfg(feature = "gtk")]
mod gdk_io {
    use super::{ImageError, RgbaImage};
    use gtk4::{gdk, glib, prelude::*};
    use std::path::Path;

    /// Download any GDK texture as unpremultiplied RGBA8.
    pub(crate) fn download(texture: &gdk::Texture) -> Result<RgbaImage, ImageError> {
        let (width, height) = (texture.width(), texture.height());
        let bad = || ImageError::BadSize {
            width: u32::try_from(width).unwrap_or(0),
            height: u32::try_from(height).unwrap_or(0),
        };
        let w = u32::try_from(width).map_err(|_| bad())?;
        let h = u32::try_from(height).map_err(|_| bad())?;
        let mut downloader = gdk::TextureDownloader::new(texture);
        downloader.set_format(gdk::MemoryFormat::R8g8b8a8);
        let (bytes, stride) = downloader.download_bytes();
        let row = w as usize * 4;
        if stride < row || bytes.len() < stride * (h as usize).saturating_sub(1) + row {
            return Err(ImageError::BadLength {
                expected: stride * h as usize,
                actual: bytes.len(),
            });
        }
        let mut pixels = Vec::with_capacity(row * h as usize);
        for y in 0..h as usize {
            pixels.extend_from_slice(&bytes[y * stride..y * stride + row]);
        }
        RgbaImage::new(w, h, pixels)
    }

    impl RgbaImage {
        /// Encode as PNG through GDK.
        pub fn save_png(&self, path: &Path) -> Result<(), ImageError> {
            let texture = gdk::MemoryTexture::new(
                self.width as i32,
                self.height as i32,
                gdk::MemoryFormat::R8g8b8a8,
                &glib::Bytes::from(self.pixels.as_slice()),
                self.width as usize * 4,
            );
            texture
                .save_to_png(path)
                .map_err(|e| ImageError::Codec(e.to_string()))
        }

        /// Decode a PNG through GDK.
        pub fn load_png(path: &Path) -> Result<RgbaImage, ImageError> {
            let texture =
                gdk::Texture::from_filename(path).map_err(|e| ImageError::Codec(e.to_string()))?;
            download(&texture)
        }
    }
}
#[cfg(feature = "gtk")]
pub(crate) use gdk_io::download;

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, rgba: [u8; 4]) -> RgbaImage {
        RgbaImage::new(w, h, rgba.repeat((w * h) as usize)).unwrap()
    }

    #[test]
    fn tolerance_bounds_channel_delta_and_pixel_count() {
        assert!(RgbaImage::new(0, 1, vec![]).is_err());
        assert!(RgbaImage::new(MAX_SIDE + 1, 1, vec![]).is_err());
        assert_eq!(
            RgbaImage::new(1, 1, vec![0; 3]),
            Err(ImageError::BadLength {
                expected: 4,
                actual: 3
            })
        );
        let golden = solid(100, 100, [10, 20, 30, 255]);
        assert_eq!(golden.pixel(99, 99), Some([10, 20, 30, 255]));
        assert_eq!(golden.pixel(100, 0), None);
        let mut actual = golden.clone();
        // 5 pixels off by 3, 1 pixel off by 200.
        for i in 0..5 {
            actual.pixels[i * 4] += 3;
        }
        actual.pixels[400] = 210;
        let exact = compare(&golden, &actual, Tolerance::EXACT).unwrap_err();
        assert!(
            matches!(exact, Mismatch::Pixels(d) if d.differing == 6 && d.max_channel_delta == 200)
        );
        let loose = Tolerance {
            max_channel_delta: 4,
            max_differing_per_million: 100, // 1 pixel of 10 000
        };
        let diff = compare(&golden, &actual, loose).unwrap();
        assert_eq!((diff.differing, diff.allowed), (1, 1));
        actual.pixels[800] = 0;
        assert!(compare(&golden, &actual, loose).is_err());
        assert!(matches!(
            compare(&golden, &solid(100, 99, [0; 4]), loose),
            Err(Mismatch::Size { .. })
        ));
    }
}
