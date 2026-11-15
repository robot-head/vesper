//! `vesper shot` acceptance (§43.1 item 3, §43.2, §43.5): golden SVG bytes and
//! PNG pixel hashes in both themes at 1x and 4x, bit-identical re-rendering,
//! ligature shaping from the bundled font, callout pins, `--verify`
//! rejections and content-addressed cache hits.
//!
//! Set `VESPER_BLESS_SHOT=1` to rewrite the goldens after an intended change
//! (and bump `SHOT_CACHE_SCHEMA`).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use vesper_highlight::HighlightSpan;
use vesper_highlight::shot::{
    self, RasterOptions, SceneOptions, ShotDocument, ShotError, ShotFont, ShotLimits, ShotTheme,
    SvgOptions,
};

const FONT: &[u8] = include_bytes!("../assets/fonts/jetbrains-mono/JetBrainsMono-Regular.ttf");
const CARDS: [&str; 3] = ["wal_txn", "transaction", "counter"];

fn root() -> PathBuf {
    std::fs::canonicalize(env!("CARGO_MANIFEST_DIR")).unwrap()
}

fn fixture(name: &str) -> PathBuf {
    root().join("tests/fixtures/shot").join(name)
}

fn spans(display: &str) -> Vec<HighlightSpan> {
    vesper_highlight::highlight_tier0(display).unwrap()
}

fn load(name: &str) -> (ShotDocument, Vec<HighlightSpan>) {
    let src = std::fs::read_to_string(fixture(&format!("{name}.vsp"))).unwrap();
    let doc = shot::parse_shot_source(&src, ShotLimits::default()).unwrap();
    let spans = spans(&doc.display);
    (doc, spans)
}

fn scene(name: &str, theme: ShotTheme, font: &ShotFont<'_>) -> shot::CodeCardScene {
    let (doc, spans) = load(name);
    // The library test has no checker; queries use the authors' fallback text
    // or, for a bare `//^?`, a fixed stand-in (the CLI test covers the checker).
    let pins = shot::resolve_callouts(&doc, &spans, |token, _, _| {
        Some(format!("{token} : (query)"))
    })
    .unwrap();
    let title = doc.config.title.clone().unwrap_or_default();
    shot::build_code_card_scene(
        &title,
        &doc.display,
        &spans,
        &pins,
        &theme.palette(),
        font,
        SceneOptions::default(),
    )
    .unwrap()
}

fn bless() -> bool {
    std::env::var_os("VESPER_BLESS_SHOT").is_some()
}

#[test]
fn golden_svg_bytes_and_png_pixel_hashes() {
    let font = ShotFont::parse(FONT).unwrap();
    let mut hashes = String::new();
    for name in CARDS {
        for theme in [ShotTheme::Dusk, ShotTheme::Dawn] {
            let scene = scene(name, theme, &font);
            assert_eq!(
                scene,
                self::scene(name, theme, &font),
                "scene is deterministic"
            );
            let svg = shot::render_svg(&scene, &font, SvgOptions::default());
            assert_eq!(svg, shot::render_svg(&scene, &font, SvgOptions::default()));
            let golden = fixture(&format!("golden/{name}-{}.svg", theme.id()));
            if bless() {
                std::fs::create_dir_all(golden.parent().unwrap()).unwrap();
                std::fs::write(&golden, &svg).unwrap();
            } else {
                let expected = std::fs::read_to_string(&golden).unwrap();
                assert!(
                    svg == expected,
                    "{name} {} SVG differs from {}",
                    theme.id(),
                    golden.display()
                );
            }
            for dpi in [1, 4] {
                let options = RasterOptions {
                    dpi_scale: Some(dpi),
                    ..RasterOptions::default()
                };
                let (w, h, rgba) = shot::render_rgba(&scene, &font, options).unwrap();
                assert_eq!((w, h), (scene.width_px * dpi, scene.height_px * dpi));
                let again = shot::render_rgba(&scene, &font, options).unwrap();
                assert!(again.2 == rgba, "re-rendering is bit-identical");
                let png = shot::render_png(&scene, &font, options).unwrap();
                assert_eq!(png, shot::render_png(&scene, &font, options).unwrap());
                assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
                hashes.push_str(&format!(
                    "{name} {} {dpi}x {w}x{h} {}\n",
                    theme.id(),
                    blake3::hash(&rgba).to_hex()
                ));
            }
        }
    }
    let golden = fixture("golden/pixels.txt");
    if bless() {
        std::fs::write(&golden, &hashes).unwrap();
    } else {
        assert_eq!(
            hashes,
            std::fs::read_to_string(&golden).unwrap(),
            "PNG pixel hashes"
        );
    }
}

#[test]
fn ligatures_use_the_fonts_ligature_glyphs() {
    let font = ShotFont::parse(FONT).unwrap();
    assert_eq!(font.family(), "JetBrains Mono");
    for (text, liga) in [
        ("->", "hyphen_greater.liga"),
        ("=>", "equal_greater.liga"),
        ("::", "colon_colon.liga"),
    ] {
        let glyphs = font.shape(text);
        let names: Vec<_> = glyphs
            .iter()
            .map(|g| font.glyph_name(g.glyph_id).unwrap())
            .collect();
        assert!(names.contains(&liga), "{text}: {names:?}");
        // Neither glyph is the character's nominal glyph.
        for (g, c) in glyphs.iter().zip(text.chars()) {
            assert_ne!(font.nominal_glyph(c), Some(g.glyph_id));
        }
    }
    // In a card, the `->` of a signature is one run holding the ligature glyph.
    let scene = scene("transaction", ShotTheme::Dusk, &font);
    let arrow = scene
        .code_runs()
        .find(|r| r.text == "->")
        .expect("an -> run");
    assert!(arrow.ligature);
    let liga = font.shape("->")[1].glyph_id;
    assert!(arrow.glyphs.iter().any(|g| g.glyph_id == liga));
    let svg = shot::render_svg(&scene, &font, SvgOptions::default());
    assert!(svg.contains("data-ligature=\"1\">-&gt;</text>"));
    let scene = self::scene("counter", ShotTheme::Dusk, &font);
    for text in ["::", "=>"] {
        assert!(
            scene.code_runs().any(|r| r.text == text && r.ligature),
            "{text}"
        );
    }
}

#[test]
fn advances_come_from_the_font() {
    let font = ShotFont::parse(FONT).unwrap();
    let scene = scene("transaction", ShotTheme::Dusk, &font);
    let upem = f32::from(font.units_per_em());
    for run in scene.code_runs() {
        let shaped = font.shape(&run.text);
        let units: i32 = shaped.iter().map(|g| g.x_advance).sum();
        let expected = units as f32 * run.font_px / upem;
        assert!((run.width_px - expected).abs() < 0.01, "{:?}", run.text);
        assert_eq!(run.glyphs.len(), shaped.len());
    }
    // A run starts where the previous text on its line ends.
    let commit = scene.code_runs().find(|r| r.text == "commit").unwrap();
    let fn_kw = scene
        .code_runs()
        .find(|r| r.text == "fn" && (r.y_px - commit.y_px).abs() < 0.01)
        .unwrap();
    let gap = font.measure("fn ", 14.0);
    assert!((commit.x_px - fn_kw.x_px - gap).abs() < 0.01);
}

#[test]
fn callout_pins_and_window_chrome_are_drawn() {
    let font = ShotFont::parse(FONT).unwrap();
    let scene = scene("wal_txn", ShotTheme::Dusk, &font);
    let texts: Vec<_> = scene.runs().map(|r| r.text.as_str()).collect();
    // This test's resolver answers every query; the CLI prefers the checker
    // and falls back to the author's text.
    assert!(texts.contains(&"tx : (query)"));
    assert!(texts.contains(&"Discharges linear tx across uncancellable Storage + Fail"));
    assert!(texts.contains(&"wal_txn.vsp — Disjoint Projections & Linear Exhaustion"));
    // The displayed code has no directive or callout comment left in it.
    assert!(
        !texts
            .iter()
            .any(|t| t.contains("@shot") || t.contains("//^") || t.contains("^?"))
    );
    let circles = scene
        .primitives
        .iter()
        .filter(|p| matches!(p, shot::Primitive::Circle { .. }))
        .count();
    assert!(circles >= 3, "window chrome dots");
    assert!(
        scene
            .primitives
            .iter()
            .any(|p| matches!(p, shot::Primitive::ShadowLayer { .. })),
        "elevation shadow"
    );
}

#[test]
fn bad_inputs_are_typed_errors() {
    let font = ShotFont::parse(FONT).unwrap();
    assert!(matches!(
        ShotFont::parse(b"not a font"),
        Err(ShotError::Font(_))
    ));
    let (doc, spans) = load("transaction");
    let mut broken = spans.clone();
    broken.pop();
    assert!(matches!(
        shot::build_code_card_scene(
            "t",
            &doc.display,
            &broken,
            &[],
            &ShotTheme::Dusk.palette(),
            &font,
            SceneOptions::default()
        ),
        Err(ShotError::Chroma(_))
    ));
    let options = SceneOptions {
        dpi_scale: 5,
        ..SceneOptions::default()
    };
    assert_eq!(
        shot::build_code_card_scene(
            "t",
            &doc.display,
            &spans,
            &[],
            &ShotTheme::Dusk.palette(),
            &font,
            options
        ),
        Err(ShotError::Limit("dpi scale must be 1-4"))
    );
    let wide = "x".repeat(300);
    let doc = shot::parse_shot_source(&wide, ShotLimits::default()).unwrap();
    let spans = self::spans(&doc.display);
    let limits = ShotLimits {
        max_card_edge_px: 1000,
        ..ShotLimits::default()
    };
    let options = SceneOptions {
        limits,
        ..SceneOptions::default()
    };
    assert_eq!(
        shot::build_code_card_scene(
            "t",
            &doc.display,
            &spans,
            &[],
            &ShotTheme::Dusk.palette(),
            &font,
            options
        ),
        Err(ShotError::Limit("card size"))
    );
    // A query with neither a checker answer nor fallback text is refused.
    let doc = shot::parse_shot_source("let x = 1;\n//  ^?", ShotLimits::default()).unwrap();
    let spans = self::spans(&doc.display);
    assert!(matches!(
        shot::resolve_callouts(&doc, &spans, |_, _, _| None),
        Err(ShotError::Callout { line: 2, .. })
    ));
    let doc = shot::parse_shot_source("let  x = 1;\n//  ^?", ShotLimits::default()).unwrap();
    assert!(matches!(
        shot::resolve_callouts(&doc, &self::spans(&doc.display), |_, _, _| Some("t".into())),
        Err(ShotError::Callout { .. })
    ));
}

fn temp_dir(tag: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "vesper-shot-{tag}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn vesper(args: &[&std::ffi::OsStr]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_vesper"))
        .arg("shot")
        .args(args)
        .output()
        .unwrap()
}

fn shot_json(input: &Path, output: &Path, extra: &[&str]) -> serde_json::Value {
    let mut args: Vec<&std::ffi::OsStr> = vec![
        input.as_os_str(),
        "-o".as_ref(),
        output.as_os_str(),
        "--json".as_ref(),
    ];
    args.extend(extra.iter().map(|a| std::ffi::OsStr::new(*a)));
    let out = vesper(&args);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn verify_rejects_examples_that_fail_to_check_or_to_run() {
    let dir = temp_dir("verify");
    for (name, needle) in [
        ("reject_check", "E0302"),
        ("reject_runtime", "evaluator execution failed"),
    ] {
        let out_path = dir.join(format!("{name}.svg"));
        let out = vesper(&[
            fixture(&format!("{name}.vsp")).as_os_str(),
            "-o".as_ref(),
            out_path.as_os_str(),
            "--verify".as_ref(),
            "--no-cache".as_ref(),
        ]);
        assert!(!out.status.success(), "{name} rendered");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("not rendering it") && err.contains(needle),
            "{name}: {err}"
        );
        assert!(!out_path.exists(), "{name}: no card is written");
        // Without --verify the same file renders.
        shot_json(&fixture(&format!("{name}.vsp")), &out_path, &["--no-cache"]);
        assert!(out_path.exists());
    }
    let ok = shot_json(
        &fixture("transaction.vsp"),
        &dir.join("t.svg"),
        &["--verify", "--no-cache"],
    );
    assert_eq!(ok["verified"], true);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn checker_answers_bare_queries() {
    let dir = temp_dir("query");
    let out = dir.join("t.svg");
    let json = shot_json(&fixture("transaction.vsp"), &out, &["--no-cache"]);
    assert_eq!(json["callouts"], 3);
    let svg = std::fs::read_to_string(&out).unwrap();
    // The query under the `let tx` binding is answered by Tier 1's hover
    // table (it overrides the author's fallback text), and the bare `//  ^?`
    // under `tx.commit()` by the checker's hover at that expression.
    assert!(svg.contains(">tx : WriteTxn (mod linear)</text>"), "{svg}");
    assert!(svg.contains(">WriteTxn : mod linear</text>"), "{svg}");
    assert!(
        svg.contains(">consumes the linear transaction</text>"),
        "{svg}"
    );
    // Tier 1 marks the live linear binding with the wavy obligation underline.
    assert!(svg.contains("class=\"vc-lin-live\""), "{svg}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn cache_hits_return_the_stored_bytes() {
    let dir = temp_dir("cache");
    let store = dir.join("store");
    let store_arg = store.to_str().unwrap();
    for ext in ["svg", "png"] {
        let first = dir.join(format!("first.{ext}"));
        let second = dir.join(format!("second.{ext}"));
        let a = shot_json(
            &fixture("transaction.vsp"),
            &first,
            &["--store", store_arg, "--dpi", "1"],
        );
        let b = shot_json(
            &fixture("transaction.vsp"),
            &second,
            &["--store", store_arg, "--dpi", "1"],
        );
        assert_eq!(a["cache"], "miss");
        assert_eq!(b["cache"], "hit");
        assert_eq!(a["key"], b["key"]);
        assert_eq!(
            std::fs::read(&first).unwrap(),
            std::fs::read(&second).unwrap()
        );
        // A different theme is a different key.
        let c = shot_json(
            &fixture("transaction.vsp"),
            &second,
            &["--store", store_arg, "--dpi", "1", "--theme", "dusk"],
        );
        assert_eq!(c["cache"], "miss");
        assert_ne!(a["key"], c["key"]);
    }
    // Different dpi, different key.
    let d = shot_json(
        &fixture("transaction.vsp"),
        &dir.join("x.png"),
        &["--store", store_arg, "--dpi", "2"],
    );
    assert_eq!(d["cache"], "miss");
    assert_eq!(
        (d["width"].as_u64(), d["dpi"].as_u64()),
        (Some(1088), Some(2))
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn cli_rejects_bad_arguments_and_directives() {
    let dir = temp_dir("args");
    let bad = dir.join("bad.vsp");
    std::fs::write(&bad, "@shot(dpi = 9)\nfn main() -> i64 { 1 }\n").unwrap();
    for args in [
        vec![
            bad.as_os_str(),
            "-o".as_ref(),
            dir.join("x.svg").as_os_str(),
        ],
        vec![
            fixture("transaction.vsp").as_os_str(),
            "-o".as_ref(),
            dir.join("x.gif").as_os_str(),
        ],
        vec![
            fixture("transaction.vsp").as_os_str(),
            "-o".as_ref(),
            dir.join("x.png").as_os_str(),
            "--dpi".as_ref(),
            "8".as_ref(),
        ],
    ] {
        let out = vesper(&args);
        assert!(!out.status.success());
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn query_pins_report_only_unambiguous_actual_checker_diagnostics() {
    let dir = temp_dir("diagnostic-query");
    let source = dir.join("diagnostic.vsp");
    let output = dir.join("diagnostic.svg");
    std::fs::write(&source, "linear struct Tx {}\nfn bad(sink tx: Tx) eff [Fail[i64]] {\n    Fail::raise(1);\n  //^? author fallback\n    let Tx {} = tx;\n}\n").unwrap();
    let report = shot_json(&source, &output, &["--no-cache"]);
    assert_eq!(report["callouts"], 1);
    let svg = std::fs::read_to_string(&output).unwrap();
    assert!(svg.contains("E0402:"), "{svg}");
    assert!(svg.contains("live linear binding introduced here"), "{svg}");
    assert!(!svg.contains("author fallback"), "{svg}");

    // `--verify` still refuses to render an invalid program.
    let verify_out = dir.join("verify.svg");
    let result = vesper(&[
        source.as_os_str(),
        "-o".as_ref(),
        verify_out.as_os_str(),
        "--verify".as_ref(),
        "--no-cache".as_ref(),
    ]);
    assert!(!result.status.success());
    assert!(!verify_out.exists());

    // Two linear obligations yield two diagnostics on the same call; the
    // query uses its author's fallback instead of picking one arbitrarily.
    std::fs::write(&source, "linear struct Tx {}\nfn bad(sink tx: Tx, sink ty: Tx) eff [Fail[i64]] {\n    Fail::raise(1);\n  //^? ambiguous fallback\n    let Tx {} = tx;\n    let Tx {} = ty;\n}\n").unwrap();
    shot_json(&source, &output, &["--no-cache"]);
    let svg = std::fs::read_to_string(&output).unwrap();
    assert!(svg.contains("ambiguous fallback"), "{svg}");
    assert!(!svg.contains("E0402:"), "{svg}");

    // A broad enclosing block diagnostic must not be attributed to an
    // unrelated token. Author text still goes through SVG escaping.
    std::fs::write(&source, "linear struct Tx {}\nfn bad(sink tx: Tx) eff [Fail[i64]] {\n    let z = 1;\n  //^? fallback <tag> & text\n    Fail::raise(z);\n    let Tx {} = tx;\n}\n").unwrap();
    shot_json(&source, &output, &["--no-cache"]);
    let svg = std::fs::read_to_string(&output).unwrap();
    assert!(svg.contains("fallback &lt;tag&gt; &amp; text"), "{svg}");
    assert!(!svg.contains("E0402:"), "{svg}");
    assert!(!svg.contains("E0302:"), "{svg}");
    std::fs::remove_dir_all(dir).unwrap();
}
