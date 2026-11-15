use vesper_highlight::{
    FallbackReason, Highlight, HighlightSpan, Tier, VesperTokenClass, html, tier0_fallback,
};

// Deliberately split every ASCII byte into a separate span, including the CR
// and LF of a CRLF pair. This can happen with independently supplied spans.
fn split_highlight(source: &str) -> Highlight {
    assert!(source.is_ascii());
    Highlight {
        spans: (0..source.len())
            .map(|i| HighlightSpan::new(i, i + 1, VesperTokenClass::Whitespace))
            .collect(),
        hovers: vec![],
        tier: Tier::Lossless0,
        fallback: None,
    }
}

fn render(source: &str, line_numbers: bool) -> String {
    html::render(
        source,
        &split_highlight(source),
        &html::HtmlOptions {
            theme: None,
            line_numbers,
        },
    )
}

#[test]
fn mixed_line_endings_keep_text_and_number_each_logical_line() {
    let source = "A\rB\nC\r\nD<&>\"";
    let expected = concat!(
        "<pre class=\"vc\" data-vc-tier=\"0\"><code>",
        "<span class=\"vc-gutter\" aria-hidden=\"true\">1 </span>A&#13;",
        "<span class=\"vc-gutter\" aria-hidden=\"true\">2 </span>B\n",
        "<span class=\"vc-gutter\" aria-hidden=\"true\">3 </span>C&#13;\n",
        "<span class=\"vc-gutter\" aria-hidden=\"true\">4 </span>D&lt;&amp;&gt;&quot;",
        "</code></pre>\n",
    );
    let actual = render(source, true);
    assert_eq!(actual, expected);
    assert!(
        !actual.contains('\r'),
        "raw CR would be normalized by HTML parsing"
    );
    assert_eq!(
        render(source, false),
        "<pre class=\"vc\" data-vc-tier=\"0\"><code>A&#13;B\nC&#13;\nD&lt;&amp;&gt;&quot;</code></pre>\n"
    );
}

#[test]
fn empty_and_trailing_lines_do_not_gain_phantom_gutters() {
    assert_eq!(
        render("", true),
        "<pre class=\"vc\" data-vc-tier=\"0\"><code></code></pre>\n"
    );
    for source in ["a\r", "a\n", "a\r\n"] {
        let rendered = render(source, true);
        assert_eq!(
            rendered.matches("class=\"vc-gutter\"").count(),
            1,
            "{source:?}"
        );
        assert!(!rendered.contains('\r'));
    }
    let rendered = render("\r\n\rx\nz\ry", true);
    assert_eq!(rendered.matches("class=\"vc-gutter\"").count(), 5);
    for line in 1..=5 {
        assert!(rendered.contains(&format!("aria-hidden=\"true\">{line} </span>")));
    }
    // A CRLF split across two spans must not insert a gutter between bytes.
    assert!(rendered.contains("&#13;\n<span class=\"vc-gutter\""));
}

#[test]
fn line_number_width_counts_lone_carriage_returns() {
    let rendered = render(&"x\r".repeat(9), true);
    assert_eq!(rendered.matches("class=\"vc-gutter\"").count(), 9);
    assert!(rendered.contains("aria-hidden=\"true\"> 1 </span>"));
    assert!(rendered.contains("aria-hidden=\"true\"> 9 </span>"));
    assert!(!rendered.contains("aria-hidden=\"true\">10 </span>"));
}

#[test]
fn styled_spans_and_real_highlighting_preserve_mixed_ending_text() {
    let source = "fn f() {\rlet x = 1;\r\nx\n}";
    let actual = tier0_fallback(source, FallbackReason::NotRequested).unwrap();
    let rendered = html::render(source, &actual, &html::HtmlOptions::default());
    assert_eq!(rendered.matches("class=\"vc-gutter\"").count(), 4);
    assert!(!rendered.contains('\r'));
    assert_eq!(source_text(&rendered), source);

    // The CR and LF are split across differently styled spans; gutters must
    // remain outside both spans and must appear after, not inside, the pair.
    let source = "x\r\ny\rz";
    let highlight = Highlight {
        spans: vec![
            HighlightSpan::new(0, 2, VesperTokenClass::KeywordControl),
            HighlightSpan::new(2, source.len(), VesperTokenClass::LiteralScalar),
        ],
        hovers: vec![],
        tier: Tier::Lossless0,
        fallback: None,
    };
    let rendered = html::render(source, &highlight, &html::HtmlOptions::default());
    assert!(rendered.contains(
        "<span class=\"vc-k\">x&#13;</span><span class=\"vc-n\">\n</span><span class=\"vc-gutter\""
    ));
    assert_eq!(source_text(&rendered), source);
}

// Extract selectable text from this renderer's known markup, decoding the
// character references after removing nonselectable gutter elements.
fn source_text(html: &str) -> String {
    let mut rest = html
        .split_once("<code>")
        .unwrap()
        .1
        .split_once("</code>")
        .unwrap()
        .0;
    let mut result = String::new();
    while let Some(start) = rest.find('<') {
        result.push_str(&rest[..start]);
        rest = &rest[start..];
        if rest.starts_with("<span class=\"vc-gutter\"") {
            rest = rest.split_once("</span>").unwrap().1;
        } else {
            rest = rest.split_once('>').unwrap().1;
        }
    }
    result.push_str(rest);
    result
        .replace("&#13;", "\r")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
}
