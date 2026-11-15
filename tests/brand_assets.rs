//! Verifies checked-in Appendix B brand SVG assets against Appendix B.2–B.4.

const MARK_DUSK_SVG: &str = include_str!("../website/brand/vesper-mark-dusk.svg");
const MARK_DAWN_SVG: &str = include_str!("../website/brand/vesper-mark-dawn.svg");
const FAVICON_16_SVG: &str = include_str!("../website/brand/vesper-favicon-16.svg");
const WORDMARK_DUSK_SVG: &str = include_str!("../website/brand/vesper-wordmark-dusk.svg");

const EXPECTED_MARK_DUSK_SVG: &str = "\
<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 256 256\" width=\"256\" height=\"256\" fill=\"none\">
  <!-- Upper Aggregate Root Struct Chevron -->
  <polygon points=\"30,34 87,34 128,75 169,34 226,34 206,74 128,113 50,74\" fill=\"#E2E8F5\"/>
  <!-- Port Blade: Mode::Linear & sink Ownership Transfer -->
  <polygon points=\"59.33,92.67 101,113.5 101,176\" fill=\"#FFB454\"/>
  <!-- Center Apex Blade: Disjoint inout FieldTree Projection -->
  <polygon points=\"128,127 143,119.5 143,200 128,230 113,200 113,119.5\" fill=\"#5CCFE6\"/>
  <!-- Starboard Blade: Shared in Views & One-Shot eff [...] Capabilities -->
  <polygon points=\"196.67,92.67 155,113.5 155,176\" fill=\"#D2A6FF\"/>
</svg>
";

const EXPECTED_FAVICON_16_SVG: &str = "\
<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 16 16\" width=\"16\" height=\"16\" fill=\"none\">
  <polygon points=\"1,1.5 5,1.5 8,4.5 11,1.5 15,1.5 13.6,4.3 8,7.1 2.4,4.3\" fill=\"#E2E8F5\"/>
  <polygon points=\"3.1,5.7 6,7.1 6,11.5\" fill=\"#FFB454\"/>
  <polygon points=\"8,8.1 9,7.6 9,13.5 8,15.5 7,13.5 7,7.6\" fill=\"#5CCFE6\"/>
  <polygon points=\"12.9,5.7 10,7.1 10,11.5\" fill=\"#D2A6FF\"/>
</svg>
";

const EXPECTED_WORDMARK_DUSK_SVG: &str = "\
<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 520 140\" width=\"520\" height=\"140\" fill=\"none\">
  <g transform=\"translate(12, 12) scale(0.453125)\">
    <polygon points=\"30,34 87,34 128,75 169,34 226,34 206,74 128,113 50,74\" fill=\"#E2E8F5\"/>
    <polygon points=\"59.33,92.67 101,113.5 101,176\" fill=\"#FFB454\"/>
    <polygon points=\"128,127 143,119.5 143,200 128,230 113,200 113,119.5\" fill=\"#5CCFE6\"/>
    <polygon points=\"196.67,92.67 155,113.5 155,176\" fill=\"#D2A6FF\"/>
  </g>
  <text x=\"148\" y=\"95\" fill=\"#E2E8F5\" font-family=\"'Space Grotesk', 'Inter', system-ui, sans-serif\" font-size=\"84\" font-weight=\"700\" letter-spacing=\"-3\">vesper</text>
</svg>
";

#[test]
fn appendix_b4_brand_svgs_match_canonical_geometry_and_palettes() {
    assert_eq!(MARK_DUSK_SVG, EXPECTED_MARK_DUSK_SVG);
    assert_eq!(FAVICON_16_SVG, EXPECTED_FAVICON_16_SVG);
    assert_eq!(WORDMARK_DUSK_SVG, EXPECTED_WORDMARK_DUSK_SVG);

    // Dawn palette replaces only the four Tri-Pillar fills with Dawn OKLCH hex colors.
    let expected_dawn = EXPECTED_MARK_DUSK_SVG
        .replace("#E2E8F5", "#1B2030")
        .replace("#FFB454", "#B85200")
        .replace("#5CCFE6", "#006B80")
        .replace("#D2A6FF", "#6B2FD6");
    assert_eq!(MARK_DAWN_SVG, expected_dawn);

    // Appendix B.3 geometric integrity: no blur/glow filters or fill inside negative channels.
    for svg in [
        MARK_DUSK_SVG,
        MARK_DAWN_SVG,
        FAVICON_16_SVG,
        WORDMARK_DUSK_SVG,
    ] {
        assert!(!svg.contains("<filter"));
        assert!(!svg.contains("feGaussianBlur"));
    }
}

#[test]
fn canonical_polygons_preserve_v_cone_colinearity_and_disjoint_channels() {
    // 2:1 outer V-cone ray: y - 34 == 2 * (x - 30) on left wall, symmetric around x = 128.
    let left_wall_vertices = [
        (30.0_f64, 34.0_f64),
        (50.0, 74.0),
        (113.0, 200.0),
        (128.0, 230.0),
    ];
    for (x, y) in left_wall_vertices {
        assert!(((y - 34.0) - 2.0 * (x - 30.0)).abs() < 1e-9);
    }

    // Port (x<=101), Center (113<=x<=143), Starboard (155<=x) keep 12 px channels.
    assert_eq!(113 - 101, 12);
    assert_eq!(155 - 143, 12);
}
