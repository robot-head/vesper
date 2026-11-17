//! Golden-image encoding and comparison (no `unsafe`).
//!
//! Goldens are stored as Netpbm PAM files (`P7`, `TUPLTYPE RGB_ALPHA`, 8 bits
//! per channel): a plain, lossless format that needs no codec dependency and
//! that common image tools open. Decoding is strict and bounded; malformed or
//! oversized files are typed errors, never panics.

/// An 8-bit RGBA image, rows top to bottom.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Largest accepted golden dimension.
pub const MAX_DIMENSION: u32 = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GoldenError {
    /// Not a PAM file this module writes (the string says what is wrong).
    Malformed(&'static str),
    /// The image is larger than [`MAX_DIMENSION`] or its byte count is not
    /// `width * height * 4`.
    Size,
}

impl std::fmt::Display for GoldenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(why) => write!(f, "malformed PAM: {why}"),
            Self::Size => f.write_str("image size out of range"),
        }
    }
}
impl std::error::Error for GoldenError {}

impl Image {
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Result<Self, GoldenError> {
        if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
            return Err(GoldenError::Size);
        }
        if rgba.len() as u64 != u64::from(width) * u64::from(height) * 4 {
            return Err(GoldenError::Size);
        }
        Ok(Image {
            width,
            height,
            rgba,
        })
    }

    /// Converts BGRA bytes (a `B8G8R8A8` swapchain or X11 image) to RGBA.
    pub fn from_bgra(width: u32, height: u32, mut bgra: Vec<u8>) -> Result<Self, GoldenError> {
        for px in bgra.as_chunks_mut::<4>().0 {
            px.swap(0, 2);
        }
        Self::new(width, height, bgra)
    }

    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let i = ((y * self.width + x) * 4) as usize;
        self.rgba.get(i..i + 4)?.try_into().ok()
    }

    pub fn to_pam(&self) -> Vec<u8> {
        let mut out = format!(
            "P7\nWIDTH {}\nHEIGHT {}\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n",
            self.width, self.height
        )
        .into_bytes();
        out.extend_from_slice(&self.rgba);
        out
    }

    pub fn from_pam(bytes: &[u8]) -> Result<Self, GoldenError> {
        const END: &[u8] = b"ENDHDR\n";
        let header_len = bytes
            .windows(END.len())
            .take(1024)
            .position(|w| w == END)
            .ok_or(GoldenError::Malformed("no ENDHDR in the first 1 KiB"))?;
        let header = std::str::from_utf8(&bytes[..header_len])
            .map_err(|_| GoldenError::Malformed("header is not UTF-8"))?;
        let mut lines = header.lines();
        if lines.next() != Some("P7") {
            return Err(GoldenError::Malformed("missing P7 magic"));
        }
        let (mut w, mut h, mut depth, mut maxval, mut tuple) = (None, None, None, None, None);
        for line in lines {
            let mut parts = line.split_whitespace();
            let (Some(key), Some(value), None) = (parts.next(), parts.next(), parts.next()) else {
                return Err(GoldenError::Malformed("header line is not KEY VALUE"));
            };
            let number = || {
                value
                    .parse::<u32>()
                    .map_err(|_| GoldenError::Malformed("non-numeric header value"))
            };
            match key {
                "WIDTH" => w = Some(number()?),
                "HEIGHT" => h = Some(number()?),
                "DEPTH" => depth = Some(number()?),
                "MAXVAL" => maxval = Some(number()?),
                "TUPLTYPE" => tuple = Some(value),
                _ => return Err(GoldenError::Malformed("unknown header key")),
            }
        }
        if depth != Some(4) || maxval != Some(255) || tuple != Some("RGB_ALPHA") {
            return Err(GoldenError::Malformed("only 8-bit RGB_ALPHA is supported"));
        }
        let (Some(w), Some(h)) = (w, h) else {
            return Err(GoldenError::Malformed("missing WIDTH or HEIGHT"));
        };
        Self::new(w, h, bytes[header_len + END.len()..].to_vec())
    }
}

/// How two images differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Diff {
    /// Pixels with any channel differing by more than the tolerance.
    pub differing: usize,
    /// Largest per-channel difference seen.
    pub max_delta: u8,
    pub pixels: usize,
}

/// Compares two images of equal size channel by channel. `None` when the
/// sizes differ.
pub fn diff(actual: &Image, expected: &Image, tolerance: u8) -> Option<Diff> {
    if (actual.width, actual.height) != (expected.width, expected.height) {
        return None;
    }
    let mut out = Diff {
        differing: 0,
        max_delta: 0,
        pixels: (actual.width * actual.height) as usize,
    };
    for (a, e) in actual
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .zip(expected.rgba.as_chunks::<4>().0)
    {
        let delta = a
            .iter()
            .zip(e)
            .map(|(x, y)| x.abs_diff(*y))
            .max()
            .unwrap_or(0);
        out.max_delta = out.max_delta.max(delta);
        if delta > tolerance {
            out.differing += 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pam_round_trips_and_rejects_malformed_input() {
        let img = Image::new(2, 1, vec![1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
        let bytes = img.to_pam();
        assert_eq!(Image::from_pam(&bytes).unwrap(), img);
        assert!(Image::from_pam(b"P6\n1 1\n255\n\0\0\0").is_err());
        let mut short = bytes.clone();
        short.pop();
        assert_eq!(Image::from_pam(&short), Err(GoldenError::Size));
        let huge = b"P7\nWIDTH 99999\nHEIGHT 1\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n";
        assert_eq!(Image::from_pam(huge), Err(GoldenError::Size));
        assert!(Image::from_pam(&[0xff; 64]).is_err());
        let bgra = Image::from_bgra(1, 1, vec![3, 2, 1, 4]).unwrap();
        assert_eq!(bgra.pixel(0, 0), Some([1, 2, 3, 4]));
        let other = Image::new(2, 1, vec![1, 2, 3, 4, 5, 6, 7, 18]).unwrap();
        let d = diff(&img, &other, 2).unwrap();
        assert_eq!((d.differing, d.max_delta), (1, 10));
        assert!(diff(&img, &bgra, 0).is_none());
    }
}
