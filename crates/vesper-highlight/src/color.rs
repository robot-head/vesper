//! Colour science used by the palette audit, computed in code.
//!
//! * OKLab/OKLCH <-> linear sRGB uses Björn Ottosson's published matrices
//!   (2020-12, the form CSS Color 4 cites).
//! * Relative luminance and contrast ratio follow WCAG 2.x (sRGB transfer
//!   with the 0.04045 knee, `(L1 + 0.05) / (L2 + 0.05)`).
//! * APCA Lc follows APCA-W3 0.0.98G-4g (the constants of the `apca-w3`
//!   reference implementation, version 0.1.9).

/// Splits `0xRRGGBB` into 8-bit channels.
pub fn hex_channels(hex: u32) -> [u8; 3] {
    [(hex >> 16) as u8, (hex >> 8) as u8, hex as u8]
}

/// Joins 8-bit channels into `0xRRGGBB`.
pub fn channels_hex(c: [u8; 3]) -> u32 {
    (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2])
}

/// sRGB transfer function: encoded `0..=1` to linear light.
pub fn srgb_to_linear(c: f64) -> f64 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Inverse sRGB transfer function: linear light to encoded `0..=1`.
pub fn linear_to_srgb(x: f64) -> f64 {
    if x <= 0.003_130_8 {
        12.92 * x
    } else {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    }
}

/// OKLCH (L in 0..=1, C, hue in degrees) to unclamped encoded sRGB in
/// 0..=255 per channel. Values outside 0..=255 are out of gamut.
pub fn oklch_to_srgb(l: f64, c: f64, hue_deg: f64) -> [f64; 3] {
    let (sin, cos) = hue_deg.to_radians().sin_cos();
    let (a, b) = (c * cos, c * sin);
    let l_ = l + 0.396_337_777_4 * a + 0.215_803_757_3 * b;
    let m_ = l - 0.105_561_345_8 * a - 0.063_854_172_8 * b;
    let s_ = l - 0.089_484_177_5 * a - 1.291_485_548_0 * b;
    let (l3, m3, s3) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    let r = 4.076_741_662_1 * l3 - 3.307_711_591_3 * m3 + 0.230_969_929_2 * s3;
    let g = -1.268_438_004_6 * l3 + 2.609_757_401_1 * m3 - 0.341_319_396_5 * s3;
    let bl = -0.004_196_086_3 * l3 - 0.703_418_614_7 * m3 + 1.707_612_701_0 * s3;
    [r, g, bl].map(|x| linear_to_srgb(x) * 255.0)
}

/// Encoded sRGB `0xRRGGBB` to OKLCH (L, C, hue in degrees 0..360).
pub fn srgb_to_oklch(hex: u32) -> (f64, f64, f64) {
    let [r, g, b] = hex_channels(hex).map(|c| srgb_to_linear(f64::from(c) / 255.0));
    let l = 0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b;
    let m = 0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b;
    let s = 0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b;
    let (l_, m_, s_) = (l.cbrt(), m.cbrt(), s.cbrt());
    let ll = 0.210_454_255_3 * l_ + 0.793_617_785_0 * m_ - 0.004_072_046_8 * s_;
    let a = 1.977_998_495_1 * l_ - 2.428_592_205_0 * m_ + 0.450_593_709_9 * s_;
    let bb = 0.025_904_037_1 * l_ + 0.782_771_766_2 * m_ - 0.808_675_766_0 * s_;
    let hue = bb.atan2(a).to_degrees().rem_euclid(360.0);
    (ll, a.hypot(bb), hue)
}

/// Rounds unclamped sRGB channels into gamut, reporting whether clamping
/// moved any channel by more than half a step.
pub fn quantize(rgb: [f64; 3]) -> ([u8; 3], bool) {
    let mut clipped = false;
    let c = rgb.map(|x| {
        clipped |= !(-0.5..=255.5).contains(&x);
        x.round().clamp(0.0, 255.0) as u8
    });
    (c, clipped)
}

/// WCAG 2.x relative luminance of `0xRRGGBB`.
pub fn relative_luminance(hex: u32) -> f64 {
    let [r, g, b] = hex_channels(hex).map(|c| srgb_to_linear(f64::from(c) / 255.0));
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// WCAG 2.x contrast ratio of two colours (1..=21).
pub fn contrast_ratio(a: u32, b: u32) -> f64 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// APCA-W3 0.0.98G-4g lightness contrast Lc of `text` on `background`:
/// positive for dark text on a light background, negative for light text on
/// a dark one. |Lc| 75 is APCA's body-text level, 60 its minimum for content
/// text, 90 its preferred level.
pub fn apca_lc(text: u32, background: u32) -> f64 {
    const BLK_THRS: f64 = 0.022;
    const BLK_CLMP: f64 = 1.414;
    fn y(hex: u32) -> f64 {
        let [r, g, b] = hex_channels(hex).map(|c| (f64::from(c) / 255.0).powf(2.4));
        let y = 0.212_672_9 * r + 0.715_152_2 * g + 0.072_175 * b;
        if y < BLK_THRS {
            y + (BLK_THRS - y).powf(BLK_CLMP)
        } else {
            y
        }
    }
    let (yt, yb) = (y(text), y(background));
    if (yb - yt).abs() < 0.0005 {
        return 0.0;
    }
    let lc = if yb > yt {
        let sapc = (yb.powf(0.56) - yt.powf(0.57)) * 1.14;
        if sapc < 0.1 { 0.0 } else { sapc - 0.027 }
    } else {
        let sapc = (yb.powf(0.65) - yt.powf(0.62)) * 1.14;
        if sapc > -0.1 { 0.0 } else { sapc + 0.027 }
    };
    lc * 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn luminance_endpoints() {
        assert_eq!(relative_luminance(0x000000), 0.0);
        assert!((relative_luminance(0xFFFFFF) - 1.0).abs() < 1e-12);
        assert!((contrast_ratio(0x000000, 0xFFFFFF) - 21.0).abs() < 1e-9);
        // WCAG's worked example: #777 on white is 4.48:1.
        assert!((contrast_ratio(0x777777, 0xFFFFFF) - 4.48).abs() < 0.01);
    }

    #[test]
    fn oklch_round_trip() {
        for hex in [
            0x000000, 0xFFFFFF, 0xFF0000, 0x00FF00, 0x0000FF, 0xFFB454, 0x111625,
        ] {
            let (l, c, h) = srgb_to_oklch(hex);
            let (rgb, clipped) = quantize(oklch_to_srgb(l, c, h));
            assert!(!clipped, "{hex:06X}");
            assert_eq!(channels_hex(rgb), hex);
        }
        // OKLab white is L = 1, C = 0.
        let (l, c, _) = srgb_to_oklch(0xFFFFFF);
        assert!((l - 1.0).abs() < 1e-4 && c < 1e-4);
    }

    #[test]
    fn apca_reference_values() {
        // apca-w3 0.1.9 README: #888 on #fff is Lc 63.06; #fff on #888 is -68.54.
        assert!((apca_lc(0x888888, 0xFFFFFF) - 63.056).abs() < 0.01);
        assert!((apca_lc(0xFFFFFF, 0x888888) + 68.541).abs() < 0.01);
        assert!((apca_lc(0x000000, 0xFFFFFF) - 106.04).abs() < 0.01);
        assert!((apca_lc(0xFFFFFF, 0x000000) + 107.88).abs() < 0.01);
    }
}
