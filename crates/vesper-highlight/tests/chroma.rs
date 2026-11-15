//! Acceptance tests for the Chroma library: Tier 0 losslessness, Tier 1
//! decoration, the wire record, palettes and contrast, and the renderers.
use std::time::Instant;
use vesper_highlight::{
    ChromaError, ChromaPalette, FallbackReason, FieldLeafFact, HighlightSpan, HoverEntry,
    LinearBindingFact, SemanticFacts, SwatchRole, ThemeSwatch, Tier, VesperTokenClass as C, ansi,
    color, css, flags, highlight_tier0, highlight_tier0_tokens, highlight_tier0_tokens_with_limit,
    highlight_tier0_with_limit, highlight_tier1, html, json, tier0_fallback, verify_lossless_spans,
    wire::{self, ChromaTokenSpanWire},
};

const SAMPLE: &str = r#"// A §43.2-style snippet.
linear effect Storage { fn begin_txn(in n: u64) -> WriteTxn; }
wire struct Header { len: u32be, tag: u16le }
fn append(inout broker.{index, metrics}: Broker, in config: Config, sink batch: Batch) -> u64 eff [Storage, Fail[IoError]] {
    let tx: WriteTxn = Storage::begin_txn(config.max);
    match validate(in batch) {
        Ok(()) => { broker.metrics += 1; tx.commit() }
        Err(e) => { tx.abort(); Fail::raise(e) }
    }
}
/* block /* nested */ comment */ @proof fn f() -> mod linear Unit { "str\"ing" }
"#;

fn text<'a>(src: &'a str, s: &HighlightSpan) -> &'a str {
    &src[s.start..s.end]
}

fn class_of(src: &str, spans: &[HighlightSpan], token: &str, nth: usize) -> C {
    spans
        .iter()
        .filter(|s| text(src, s) == token)
        .nth(nth)
        .unwrap_or_else(|| panic!("token {token:?} #{nth} missing"))
        .class
}

#[test]
fn appended_operators_keep_real_tokens_and_parser_recovery() {
    let source = "fn f() -> u64 { ~1 }";
    let cst = highlight_tier0(source).unwrap();
    let tokens = highlight_tier0_tokens(source).unwrap();
    for spans in [&cst, &tokens] {
        verify_lossless_spans(source.len(), spans).unwrap();
        let tilde = spans.iter().find(|s| text(source, s) == "~").unwrap();
        assert_eq!(tilde.class, C::PunctLigature);
        assert_eq!(tilde.flags & flags::RECOVERED, 0);
    }
    let source = "fn f() { x |= 1; y &= 2; }";
    let cst = highlight_tier0(source).unwrap();
    let tokens = highlight_tier0_tokens(source).unwrap();
    for spans in [&cst, &tokens] {
        verify_lossless_spans(source.len(), spans).unwrap();
        for operator in ["|=", "&="] {
            let span = spans.iter().find(|s| text(source, s) == operator).unwrap();
            assert_eq!(span.class, C::PunctLigature);
        }
    }
    // These lexical operators are not parser support; their tokens must not
    // acquire a spurious lexer-error marker merely from the discriminant map.
    for operator in ["|=", "&="] {
        let span = cst.iter().find(|s| text(source, s) == operator).unwrap();
        assert_eq!(span.flags & flags::RECOVERED, 0);
    }
    assert!(tokens.iter().all(|span| span.flags & flags::RECOVERED == 0));
    let invalid = "fn # x";
    for spans in [
        highlight_tier0(invalid).unwrap(),
        highlight_tier0_tokens(invalid).unwrap(),
    ] {
        let hash = spans.iter().find(|s| text(invalid, s) == "#").unwrap();
        assert_ne!(hash.flags & flags::RECOVERED, 0);
    }
}

#[test]
fn tier0_is_lossless_and_classifies_the_design_snippet() {
    let spans = highlight_tier0(SAMPLE).unwrap();
    verify_lossless_spans(SAMPLE.len(), &spans).unwrap();
    let joined: String = spans.iter().map(|s| text(SAMPLE, s)).collect();
    assert_eq!(joined, SAMPLE);
    let c = |t, n| class_of(SAMPLE, &spans, t, n);
    assert_eq!(c("linear", 0), C::ModalSpecifier);
    assert_eq!(c("effect", 0), C::KeywordControl);
    assert_eq!(c("Storage", 0), C::EffectLinear);
    assert_eq!(c("Storage", 2), C::EffectLinear);
    assert_eq!(c("Fail", 0), C::EffectAbortable);
    assert_eq!(c("Fail", 1), C::EffectAbortable);
    assert_eq!(c("IoError", 0), C::TypeNominal);
    assert_eq!(c("u32be", 0), C::WireEndianScalar);
    assert_eq!(c("u16le", 0), C::WireEndianScalar);
    assert_eq!(c("wire", 0), C::KeywordControl);
    assert_eq!(c("fn", 0), C::KeywordControl);
    assert_eq!(c("begin_txn", 0), C::FunctionIdent);
    assert_eq!(c("append", 0), C::FunctionIdent);
    assert_eq!(c("inout", 0), C::PassModeInOut);
    assert_eq!(c("index", 0), C::FieldLeafMut);
    assert_eq!(c("metrics", 0), C::FieldLeafMut);
    assert_eq!(c("broker", 0), C::PassModeIn);
    assert_eq!(c("in", 0), C::PassModeIn);
    assert_eq!(c("sink", 0), C::PassModeSink);
    assert_eq!(c("u64", 0), C::TypeNominal);
    assert_eq!(c("1", 0), C::LiteralScalar);
    assert_eq!(c("\"str\\\"ing\"", 0), C::LiteralScalar);
    assert_eq!(c("@", 0), C::DocProofAnchor);
    assert_eq!(c("proof", 0), C::DocProofAnchor);
    assert_eq!(c("mod", 0), C::ModalSpecifier);
    assert_eq!(c("// A §43.2-style snippet.", 0), C::CommentTrivia);
    assert_eq!(c("/* block /* nested */ comment */", 0), C::CommentTrivia);
    let arrow = spans.iter().find(|s| text(SAMPLE, s) == "->").unwrap();
    assert_eq!(arrow.class, C::PunctLigature);
    assert_ne!(arrow.flags & flags::LIGATURE, 0);
    let fat = spans.iter().find(|s| text(SAMPLE, s) == "=>").unwrap();
    assert_ne!(fat.flags & flags::LIGATURE, 0);
    let path = spans.iter().find(|s| text(SAMPLE, s) == "::").unwrap();
    assert_ne!(path.flags & flags::LIGATURE, 0);
    assert!(
        spans.iter().all(|s| !s.is_live_linear),
        "Tier 0 has no liveness"
    );
}

#[test]
fn tier0_is_lossless_on_every_prefix_and_odd_inputs() {
    for cut in 0..=SAMPLE.len() {
        if !SAMPLE.is_char_boundary(cut) {
            continue;
        }
        let prefix = &SAMPLE[..cut];
        let spans = highlight_tier0(prefix).unwrap_or_else(|e| panic!("prefix {cut}: {e}"));
        verify_lossless_spans(prefix.len(), &spans).unwrap();
    }
    for odd in [
        "",
        " ",
        "\n\n",
        "#",
        "fn (",
        "\"unterminated",
        "/* open",
        "é ü 日本 🦀",
        "fn f() { let x = 1 }}}}}",
        "\u{0}\u{7f}",
        &"(".repeat(4000),
        &"{".repeat(200),
    ] {
        let spans = highlight_tier0(odd).unwrap();
        verify_lossless_spans(odd.len(), &spans).unwrap();
        let joined: String = spans.iter().map(|s| text(odd, s)).collect();
        assert_eq!(joined, odd);
    }
    let lexer_error = highlight_tier0("fn # x").unwrap();
    assert!(lexer_error.iter().any(|s| s.flags & flags::RECOVERED != 0));
}

#[test]
fn token_mode_matches_cst_mode_boundaries_and_classes() {
    for src in [SAMPLE, "", "fn (", "/* open", "é 🦀 #"] {
        let cst = highlight_tier0(src).unwrap();
        let tok = highlight_tier0_tokens(src).unwrap();
        verify_lossless_spans(src.len(), &tok).unwrap();
        assert_eq!(cst.len(), tok.len());
        for (a, b) in cst.iter().zip(&tok) {
            assert_eq!((a.start, a.end), (b.start, b.end));
            assert_eq!(a.class, b.class, "{:?}", text(src, a));
        }
    }
    for cut in (0..SAMPLE.len()).filter(|c| SAMPLE.is_char_boundary(*c)) {
        let spans = highlight_tier0_tokens(&SAMPLE[..cut]).unwrap();
        verify_lossless_spans(cut, &spans).unwrap();
    }
    assert_eq!(
        highlight_tier0_tokens_with_limit("abc", 2),
        Err(ChromaError::SourceTooLarge { len: 3, limit: 2 })
    );
}

#[test]
fn unclosed_effect_rows_stay_linear_time() {
    // Each `eff [` opens a bracket that never closes; a rescan per row would
    // be quadratic in the number of rows.
    let src = "fn f() -> u64 eff [ A, ".repeat(20_000);
    for mode in [highlight_tier0_tokens, highlight_tier0] {
        let start = Instant::now();
        let spans = mode(&src).unwrap();
        verify_lossless_spans(src.len(), &spans).unwrap();
        assert!(start.elapsed().as_secs() < 10, "{:?}", start.elapsed());
    }
    // Many distinct effect names: classification stays bounded and lossless.
    let names: String = (0..5000).map(|i| format!("E{i}, ")).collect();
    let src = format!("fn f() eff [{names}] {{}}");
    let spans = highlight_tier0_tokens(&src).unwrap();
    assert_eq!(class_of(&src, &spans, "E0", 0), C::EffectAbortable);
    assert_eq!(class_of(&src, &spans, "E4999", 0), C::TypeNominal);
    // 20,000 effect declarations, each used: the name table is capped, so
    // lookups stay constant-time per token.
    let src: String = (0..20_000)
        .map(|i| format!("effect E{i} {{ }}\nfn f{i}() eff [E{i}] {{ }}\n"))
        .collect();
    for mode in [highlight_tier0_tokens, highlight_tier0] {
        let start = Instant::now();
        let spans = mode(&src).unwrap();
        verify_lossless_spans(src.len(), &spans).unwrap();
        assert!(start.elapsed().as_secs() < 10, "{:?}", start.elapsed());
    }
}

#[test]
fn oversized_input_is_a_typed_error() {
    let big = "a".repeat(64);
    assert_eq!(
        highlight_tier0_with_limit(&big, 16),
        Err(ChromaError::SourceTooLarge { len: 64, limit: 16 })
    );
}

#[test]
fn lossless_verifier_rejects_gaps_overlaps_and_short_coverage() {
    let s = |a, b| HighlightSpan::new(a, b, C::Whitespace);
    assert_eq!(
        verify_lossless_spans(4, &[s(0, 2), s(3, 4)]),
        Err(ChromaError::DiscontiguousTokenSpan {
            expected_offset: 2,
            actual_start: 3
        })
    );
    assert_eq!(
        verify_lossless_spans(4, &[s(0, 2), s(1, 4)]),
        Err(ChromaError::DiscontiguousTokenSpan {
            expected_offset: 2,
            actual_start: 1
        })
    );
    assert_eq!(
        verify_lossless_spans(4, &[s(0, 3), s(3, 2)]),
        Err(ChromaError::DiscontiguousTokenSpan {
            expected_offset: 3,
            actual_start: 3
        })
    );
    assert_eq!(
        verify_lossless_spans(4, &[s(0, 3)]),
        Err(ChromaError::IncompleteSourceCoverage {
            covered_end: 3,
            src_len: 4
        })
    );
    verify_lossless_spans(0, &[]).unwrap();
}

#[test]
fn wire_record_layout_is_16_bytes_little_endian() {
    let span = HighlightSpan {
        start: 0x0403_0201,
        end: 0x0403_0201 + 0x0807_0605,
        class: C::LinearBinding,
        is_live_linear: true,
        flags: flags::LIGATURE,
        hover_sym_id: Some(0x0D0C_0B0A),
    };
    let w = ChromaTokenSpanWire::from_span(&span).unwrap();
    let b = w.to_bytes();
    assert_eq!(b.len(), wire::RECORD_BYTES);
    assert_eq!(wire::RECORD_BYTES, 16);
    assert_eq!(&b[wire::OFFSET_BYTE_START..4], &[1, 2, 3, 4]);
    assert_eq!(&b[wire::OFFSET_BYTE_LEN..8], &[5, 6, 7, 8]);
    assert_eq!(&b[wire::OFFSET_TOKEN_CLASS..10], &[5, 0]);
    assert_eq!(&b[wire::OFFSET_SEMANTIC_FLAGS..12], &[0b101, 0]);
    assert_eq!(&b[wire::OFFSET_HOVER_SYM_ID..16], &[0x0A, 0x0B, 0x0C, 0x0D]);
    assert_eq!(ChromaTokenSpanWire::from_bytes(&b), w);
    assert_eq!(w.to_span().unwrap(), span);
    // Frozen discriminants.
    for (i, class) in C::ALL.iter().enumerate() {
        assert_eq!(*class as u16 as usize, i);
        assert_eq!(C::from_u16(i as u16).unwrap(), *class);
    }
    assert_eq!(C::Whitespace as u16, 17);
    assert_eq!(
        C::from_u16(18),
        Err(ChromaError::UnknownTokenClass { raw: 18 })
    );
}

#[test]
fn wire_round_trip_and_rejections() {
    let spans = highlight_tier0(SAMPLE).unwrap();
    let bytes = wire::encode(&spans).unwrap();
    assert_eq!(bytes.len(), spans.len() * 16);
    assert_eq!(wire::decode(&bytes).unwrap(), spans);
    assert_eq!(
        wire::decode(&bytes[..15]),
        Err(ChromaError::InvalidWireLength { len: 15 })
    );
    let mut bad = bytes[..16].to_vec();
    bad[8] = 99;
    assert_eq!(
        wire::decode(&bad),
        Err(ChromaError::UnknownTokenClass { raw: 99 })
    );
    let mut bad = bytes[..16].to_vec();
    bad[11] = 0x80;
    assert!(matches!(
        wire::decode(&bad),
        Err(ChromaError::UnknownSemanticFlags { .. })
    ));
    let mut bad = [0u8; 16];
    bad[0..4].copy_from_slice(&u32::MAX.to_le_bytes());
    bad[4..8].copy_from_slice(&2u32.to_le_bytes());
    assert!(matches!(
        wire::decode(&bad),
        Err(ChromaError::WireFieldOverflow { .. })
    ));
    #[cfg(target_pointer_width = "64")]
    {
        let huge = HighlightSpan::new(1 << 33, (1 << 33) + 1, C::Whitespace);
        let mut out = vec![7u8];
        assert!(wire::encode_into(&[huge], &mut out).is_err());
        assert_eq!(out, vec![7u8], "failed encode leaves the buffer unchanged");
    }
}

#[test]
fn shipped_palettes_pass_contrast_and_fallback_audits() {
    for p in [ChromaPalette::vesper_dusk(), ChromaPalette::vesper_dawn()] {
        p.verify_contrast_gates().unwrap();
        p.verify_oklch_fallbacks().unwrap();
        let audit = p.audit();
        eprintln!("{} (WCAG 2.x ratio on bg_card, APCA-W3 Lc):", p.name);
        for a in &audit.foreground {
            eprintln!(
                "  {:<14} #{:06X}  {:>6.2}:1 (gate {:.1})  Lc {:>7.2}",
                a.role.name(),
                a.srgb_hex,
                a.wcag_ratio,
                a.required_ratio,
                a.apca_lc
            );
            assert!(a.wcag_ratio >= a.required_ratio, "{a:?}");
            // APCA: every token colour reaches Lc 60 (APCA's content-text floor).
            assert!(a.apca_lc.abs() >= 45.0, "{a:?}");
        }
        for (role, hex, computed, steps, clipped) in &audit.fallbacks {
            eprintln!(
                "  oklch {:<14} #{hex:06X} -> #{computed:06X} ({steps} step, clipped {clipped})",
                role.name()
            );
            assert!(*steps <= 1 && !clipped);
        }
        // rel_lum_x1000 is computed from the fallback, not copied.
        for (_, sw) in p.swatches() {
            let expect = (color::relative_luminance(sw.srgb_hex) * 1000.0).round() as u32;
            assert_eq!(sw.rel_lum_x1000, expect);
        }
    }
}

/// §43.5 lists OKLCH numbers for Vesper Dusk. Computing them shows they do
/// not reproduce the listed hex values within one sRGB step; this test
/// records each mismatch (the lane report quotes this output).
#[test]
fn design_dusk_oklch_values_are_reported_against_their_hex() {
    let design = [
        ("bg_canvas", 1450, 220, 265, 0x0B0E17, 9),
        ("bg_card", 1850, 260, 265, 0x111625, 16),
        ("fg_default", 9200, 180, 255, 0xE2E8F5, 790),
        ("linear_amber", 8350, 1580, 72, 0xFFB454, 552),
        ("inout_cyan", 8450, 1320, 205, 0x5CCFE6, 548),
        ("effect_iris", 8100, 1490, 298, 0xD2A6FF, 472),
        ("wire_emerald", 8500, 1450, 158, 0x6EE7B7, 660),
        ("keyword_rose", 7900, 1620, 18, 0xFF8F9C, 435),
        ("comment_slate", 6750, 280, 258, 0x8B97B3, 305),
    ];
    let mut mismatches = 0;
    for (name, l, c, h, hex, listed_lum) in design {
        let sw = ThemeSwatch {
            oklch_l_bp: l,
            oklch_c_bp: c,
            oklch_hue_deg: h,
            srgb_hex: hex,
            rel_lum_x1000: listed_lum,
        };
        let (computed, clipped) = sw.oklch_srgb();
        let steps = sw.fallback_step_error();
        let lum = (color::relative_luminance(hex) * 1000.0).round() as u32;
        eprintln!(
            "design {name:<14} oklch({l} {c} {h}) -> #{computed:06X} vs #{hex:06X}: {steps} steps{}; luminance listed {listed_lum}, computed {lum}",
            if clipped { ", out of gamut" } else { "" }
        );
        if steps > 1 || clipped {
            mismatches += 1;
        }
    }
    // Recorded, not hidden: at the design revision of 2026-11-14 all nine differ.
    assert_eq!(mismatches, 9);
    // The design's Dawn accent hex values (§43.3) fail the 7:1 gate.
    let mut dawn = ChromaPalette::vesper_dawn();
    dawn.linear_amber = ThemeSwatch::new(0, 0, 0, 0xB85200);
    assert_eq!(
        dawn.verify_contrast_gates(),
        Err(ChromaError::InsufficientContrast { srgb_hex: 0xB85200 })
    );
    for hex in [0xB85200, 0x006B80, 0x6B2FD6, 0x087443, 0xB4233C] {
        let r = color::contrast_ratio(hex, 0xFAF8F5);
        eprintln!("design dawn #{hex:06X}: {r:.2}:1 on #FAF8F5");
        assert!(r < 7.0);
    }
}

#[test]
fn low_contrast_palettes_are_rejected() {
    let mut p = ChromaPalette::vesper_dusk();
    p.keyword_rose = ThemeSwatch::new(3000, 100, 20, 0x3A2A30);
    assert_eq!(
        p.verify_contrast_gates(),
        Err(ChromaError::InsufficientContrast { srgb_hex: 0x3A2A30 })
    );
    let mut p = ChromaPalette::vesper_dawn();
    p.comment_slate = ThemeSwatch::new(8000, 100, 260, 0xB0B4BE);
    assert_eq!(
        p.verify_contrast_gates(),
        Err(ChromaError::InsufficientContrast { srgb_hex: 0xB0B4BE })
    );
    // A comment colour that passes 4.5:1 but not 7:1 is accepted as a comment.
    let mut p = ChromaPalette::vesper_dusk();
    p.comment_slate = ThemeSwatch::new(6000, 300, 260, 0x7A85A0);
    assert!(color::contrast_ratio(0x7A85A0, 0x111625) < 7.0);
    p.verify_contrast_gates().unwrap();
    // An OKLCH definition that does not match its fallback is rejected.
    let mut p = ChromaPalette::vesper_dusk();
    p.linear_amber.oklch_l_bp = 5000;
    assert!(matches!(
        p.verify_oklch_fallbacks(),
        Err(ChromaError::OklchFallbackMismatch { .. })
    ));
}

#[test]
fn swatch_mapping_follows_the_design() {
    let p = ChromaPalette::vesper_dusk();
    assert_eq!(p.swatch_for(C::LinearBinding), p.linear_amber);
    assert_eq!(p.swatch_for(C::PassModeSink), p.linear_amber);
    assert_eq!(p.swatch_for(C::FieldLeafMut), p.inout_cyan);
    assert_eq!(p.swatch_for(C::EffectAbortable), p.effect_iris);
    assert_eq!(p.swatch_for(C::DocProofAnchor), p.wire_emerald);
    assert_eq!(p.swatch_for(C::CommentTrivia), p.comment_slate);
    assert_eq!(p.swatch_for(C::FieldLeafRead), p.fg_default);
    assert_eq!(
        ChromaPalette::role_for(C::KeywordControl),
        SwatchRole::KeywordRose
    );
}

/// Removes the elements `user-select: none` hides from a copy (`vc-gutter`,
/// `vc-twoslash-pin`), strips the remaining tags and decodes entities.
fn copied_text(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    let mut hidden_depth = 0usize;
    while let Some(lt) = rest.find('<') {
        if hidden_depth == 0 {
            out.push_str(&rest[..lt]);
        }
        let gt = rest[lt..].find('>').unwrap() + lt;
        let tag = &rest[lt..=gt];
        if hidden_depth > 0 {
            if tag.starts_with("</") {
                hidden_depth -= 1;
            } else if !tag.ends_with("/>") {
                hidden_depth += 1;
            }
        } else if tag.contains("vc-gutter") || tag.contains("vc-twoslash-pin") {
            hidden_depth = 1;
        }
        rest = &rest[gt + 1..];
    }
    out.push_str(rest);
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
}

#[test]
fn html_copy_is_byte_exact_and_styled_only_by_custom_properties() {
    for src in [SAMPLE, "", "x", "a\n", "\n\nfn f() {}\n\n", "<&>\"'"] {
        let h = tier0_fallback(src, FallbackReason::NotRequested).unwrap();
        for line_numbers in [true, false] {
            let out = html::render(
                src,
                &h,
                &html::HtmlOptions {
                    theme: Some("dawn"),
                    line_numbers,
                },
            );
            let inner = out
                .strip_prefix(
                    "<pre class=\"vc\" data-vesper-theme=\"dawn\" data-vc-tier=\"0\"><code>",
                )
                .and_then(|s| s.strip_suffix("</code></pre>\n"))
                .unwrap();
            assert_eq!(copied_text(inner), src);
            assert!(!out.contains("style="), "no inline styles");
            assert!(!out.contains('#'), "no literal colours in HTML");
        }
    }
    let sheet = css::stylesheet();
    assert!(sheet.contains(".vc-gutter,.vc-twoslash-pin{user-select:none}"));
    for var in [
        "--vc-bg-card",
        "--vc-fg-primary",
        "--vc-linear",
        "--vc-inout",
        "--vc-effect",
        "--vc-wire-proof",
        "--vc-keyword",
        "--vc-comment",
    ] {
        assert_eq!(sheet.matches(&format!("{var}:#")).count(), 3, "{var}");
    }
    for class in C::ALL {
        if let Some(name) = css::class_name(class) {
            assert!(name.starts_with("vc-"));
        }
    }
    eprintln!(
        "vesper-chroma.css: {} bytes (target {})",
        sheet.len(),
        css::TARGET_BYTES
    );
    assert!(sheet.len() <= css::TARGET_BYTES, "{} bytes", sheet.len());
}

#[test]
fn ansi_strip_round_trips() {
    let h = tier0_fallback(SAMPLE, FallbackReason::NotRequested).unwrap();
    for p in [ChromaPalette::vesper_dusk(), ChromaPalette::vesper_dawn()] {
        let out = ansi::render(SAMPLE, &h, &p);
        assert!(out.contains("\x1b[38;2;"));
        assert_eq!(ansi::strip(&out), SAMPLE);
    }
}

fn wal_facts(src: &str) -> SemanticFacts {
    let at = |needle: &str, nth: usize| {
        let mut from = 0;
        let mut found = 0;
        let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        loop {
            let i = src[from..].find(needle).unwrap() + from;
            let j = i + needle.len();
            let whole = (i == 0 || !ident(src.as_bytes()[i - 1]))
                && (j == src.len() || !ident(src.as_bytes()[j]));
            if whole {
                if found == nth {
                    return i..j;
                }
                found += 1;
            }
            from = i + 1;
        }
    };
    SemanticFacts {
        linear_bindings: vec![LinearBindingFact {
            name: "tx".into(),
            mint: at("tx", 0),
            occurrences: vec![at("tx", 1), at("tx", 2)],
            discharge: Some(at("tx", 2)),
            hover: Some(0),
        }],
        field_leaves: vec![
            FieldLeafFact {
                span: at("metrics", 1),
                mutated: true,
            },
            FieldLeafFact {
                span: at("max", 0),
                mutated: false,
            },
        ],
        effects: vec![vesper_highlight::EffectFact {
            name: "Storage".into(),
            linear: true,
        }],
        hovers: vec![HoverEntry {
            span: at("tx", 0),
            text: "tx : WriteTxn (mod linear)".into(),
        }],
    }
}

#[test]
fn tier1_decorates_linear_inout_and_effect_classes() {
    let facts = wal_facts(SAMPLE);
    let h = highlight_tier1(SAMPLE, &facts).unwrap();
    assert_eq!(h.tier, Tier::Semantic1);
    verify_lossless_spans(SAMPLE.len(), &h.spans).unwrap();
    let tx: Vec<_> = h.spans.iter().filter(|s| text(SAMPLE, s) == "tx").collect();
    assert_eq!(tx.len(), 3);
    for s in &tx {
        assert_eq!(s.class, C::LinearBinding);
        assert!(s.is_live_linear);
        assert_eq!(s.hover_sym_id, Some(0));
        assert_ne!(s.semantic_flags() & flags::LINEAR_UNDERLINE, 0);
    }
    let metrics: Vec<_> = h
        .spans
        .iter()
        .filter(|s| text(SAMPLE, s) == "metrics")
        .collect();
    assert_eq!(metrics[1].class, C::FieldLeafMut);
    assert_ne!(metrics[1].flags & flags::MUTATED_LEAF, 0);
    let max = h.spans.iter().find(|s| text(SAMPLE, s) == "max").unwrap();
    assert_eq!(max.class, C::FieldLeafRead);
    // HTML carries the underline class and the hover row.
    let out = html::render(SAMPLE, &h, &html::HtmlOptions::default());
    assert!(out.contains(
        "<span class=\"vc-l vc-lin-live\" title=\"tx : WriteTxn (mod linear)\">tx</span>"
    ));
    // JSON carries the wire records and the hover table.
    let j = json::render(&h, "vesper-dusk").unwrap();
    assert!(j.contains("\"tier\":1"));
    assert!(j.contains("\"text\":\"tx : WriteTxn (mod linear)\""));
    let hex: String = wire::encode(&h.spans)
        .unwrap()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert!(j.contains(&format!("\"wire_hex\":\"{hex}\"")));
}

#[test]
fn tier1_rejects_facts_outside_the_source() {
    let mut facts = wal_facts(SAMPLE);
    facts.field_leaves.push(FieldLeafFact {
        span: SAMPLE.len()..SAMPLE.len() + 3,
        mutated: true,
    });
    assert!(matches!(
        highlight_tier1(SAMPLE, &facts),
        Err(ChromaError::InvalidFact { .. })
    ));
    // Off a UTF-8 boundary ('§' is two bytes at offset 5).
    let mut facts = SemanticFacts::default();
    facts.hovers.push(HoverEntry {
        span: 5..6,
        text: "x".into(),
    });
    assert!(matches!(
        highlight_tier1(SAMPLE, &facts),
        Err(ChromaError::InvalidFact { start: 5, end: 6 })
    ));
}

fn two_hundred_lines() -> String {
    let unit = "fn step(inout s.{a, b}: State, in k: u64) -> u64 eff [Fail[E]] {\n    let t: Tx = Storage::begin(k);\n    s.a = wrapping_add(s.a, k); // comment\n    t.commit()\n}\n";
    let mut src = String::new();
    while src.lines().count() < 200 {
        src.push_str(unit);
    }
    src
}

#[test]
fn tier0_latency_on_a_200_line_file_is_measured() {
    let src = two_hundred_lines();
    for _ in 0..20 {
        highlight_tier0(&src).unwrap();
    }
    let runs = 200;
    let start = Instant::now();
    for _ in 0..runs {
        std::hint::black_box(highlight_tier0(std::hint::black_box(&src)).unwrap());
    }
    let per = start.elapsed() / runs;
    let start = Instant::now();
    for _ in 0..runs {
        std::hint::black_box(highlight_tier0_tokens(std::hint::black_box(&src)).unwrap());
    }
    let per_tokens = start.elapsed() / runs;
    eprintln!(
        "Tier 0, {} lines / {} bytes: CST mode {:?}, token mode {:?} per highlight ({} build)",
        src.lines().count(),
        src.len(),
        per,
        per_tokens,
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );
    // A loose sanity bound only; the report quotes the measured number.
    assert!(per.as_millis() < 250);
}
