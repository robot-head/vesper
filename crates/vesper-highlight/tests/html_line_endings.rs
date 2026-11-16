use std::path::PathBuf;
use std::process::Command;
use vesper_highlight::{
    FallbackReason, Highlight, HighlightSpan, Tier, VesperTokenClass, css, html, tier0_fallback,
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
        "<span class=\"vc-gutter\" aria-hidden=\"true\">1 </span>A<span class=\"vc-cr vc-cr-br\">&#13;</span>",
        "<span class=\"vc-gutter\" aria-hidden=\"true\">2 </span>B\n",
        "<span class=\"vc-gutter\" aria-hidden=\"true\">3 </span>C<span class=\"vc-cr\">&#13;</span>\n",
        "<span class=\"vc-gutter\" aria-hidden=\"true\">4 </span>D&lt;&amp;&gt;&quot;",
        "</code></pre>\n",
    );
    let actual = render(source, true);
    assert_eq!(actual, expected);
    assert!(
        !actual.contains('\r'),
        "raw CR would be normalized by HTML parsing"
    );
    assert_eq!(source_text(&actual), source);
    assert_eq!(
        render(source, false),
        concat!(
            "<pre class=\"vc\" data-vc-tier=\"0\"><code>",
            "A<span class=\"vc-cr vc-cr-br\">&#13;</span>",
            "B\n",
            "C<span class=\"vc-cr\">&#13;</span>\n",
            "D&lt;&amp;&gt;&quot;",
            "</code></pre>\n",
        )
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
        assert_eq!(source_text(&rendered), source);
    }
    let rendered = render("\r\n\rx\nz\ry", true);
    assert_eq!(rendered.matches("class=\"vc-gutter\"").count(), 5);
    for line in 1..=5 {
        assert!(rendered.contains(&format!("aria-hidden=\"true\">{line} </span>")));
    }
    // A CRLF split across two spans must not insert a gutter or visual CR break between bytes.
    assert!(rendered.contains("<span class=\"vc-cr\">&#13;</span>\n<span class=\"vc-gutter\""));
    assert_eq!(rendered.matches("vc-cr-br").count(), 2);
    assert_eq!(source_text(&rendered), "\r\n\rx\nz\ry");
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
        "<span class=\"vc-k\">x<span class=\"vc-cr\">&#13;</span></span><span class=\"vc-n\">\n</span><span class=\"vc-gutter\""
    ));
    assert!(rendered.contains(
        "<span class=\"vc-n\">y<span class=\"vc-cr vc-cr-br\">&#13;</span></span><span class=\"vc-gutter\""
    ));
    assert_eq!(source_text(&rendered), source);
}

#[test]
fn stylesheet_preserves_visual_breaks_without_crlf_extra_space() {
    let sheet = css::stylesheet();
    assert!(sheet.contains(".vc{white-space:pre;"));
    assert!(sheet.contains(".vc-cr{font-size:0;line-height:0}"));
    assert!(sheet.contains(".vc-cr-br::after{content:\"\\A\"}"));
    assert!(sheet.len() <= css::TARGET_BYTES, "{} bytes", sheet.len());
}

#[test]
fn escape_into_still_escapes_carriage_returns_for_attributes() {
    let mut out = String::new();
    html::escape_into("a\rb<c>&\"", &mut out);
    assert_eq!(out, "a&#13;b&lt;c&gt;&amp;&quot;");
}

#[test]
fn browser_layout_matches_lf_for_crlf_and_lone_cr() {
    let Some(browser) = ["chromium", "google-chrome", "chromium-browser"]
        .into_iter()
        .find(|bin| {
            Command::new(bin)
                .arg("--version")
                .output()
                .is_ok_and(|o| o.status.success())
        })
    else {
        return;
    };

    let css = css::stylesheet();
    let cases = [
        ("mixed", "A\rB\nC\r\nD", "A\nB\nC\nD"),
        ("empty_lines", "\r\n\rx\nz\ry", "\n\nx\nz\ny"),
        ("trailing_cr", "alpha\r", "alpha\n"),
        ("trailing_crlf", "alpha\r\n", "alpha\n"),
    ];
    let mut blocks = String::new();
    let mut meta = String::from("[");
    for (i, (name, src, lf_equiv)) in cases.iter().enumerate() {
        for line_numbers in [true, false] {
            let opts = html::HtmlOptions {
                theme: Some("dusk"),
                line_numbers,
            };
            let h_src = tier0_fallback(src, FallbackReason::NotRequested).unwrap();
            let h_ref = tier0_fallback(lf_equiv, FallbackReason::NotRequested).unwrap();
            let rendered = html::render(src, &h_src, &opts);
            let reference = html::render(lf_equiv, &h_ref, &opts);
            let test_id = format!("t_{name}_{line_numbers}");
            let ref_id = format!("r_{name}_{line_numbers}");
            blocks.push_str(&format!("<div id=\"{test_id}\">{rendered}</div>"));
            blocks.push_str(&format!("<div id=\"{ref_id}\">{reference}</div>"));
            if i > 0 || !line_numbers {
                meta.push(',');
            }
            let expected_sel = src.strip_suffix('\n').unwrap_or(src);
            meta.push_str(&format!(
                "{{\"name\":\"{name}_{line_numbers}\",\"testId\":\"{test_id}\",\"refId\":\"{ref_id}\",\"src\":{:?},\"expectedSel\":{:?}}}",
                src, expected_sel
            ));
        }
    }
    meta.push(']');

    let doc = format!(
        r#"<!doctype html><html><head><style>
{css}
.vc {{ font-family: monospace; font-size: 14px; line-height: 20px; width: max-content; }}
</style></head><body>{blocks}<pre id="out"></pre><script>
const cases = {meta};
const results = cases.map(c => {{
  const tPre = document.getElementById(c.testId).querySelector('pre');
  const rPre = document.getElementById(c.refId).querySelector('pre');
  const tCode = tPre.querySelector('code');
  const tRect = tPre.getBoundingClientRect();
  const rRect = rPre.getBoundingClientRect();
  const tGutters = [...tCode.querySelectorAll('.vc-gutter')].map(g => {{
    const gr = g.getBoundingClientRect();
    return {{ text: g.textContent, dy: Math.round((gr.top - tRect.top) * 10) / 10, dx: Math.round((gr.left - tRect.left) * 10) / 10 }};
  }});
  const rGutters = [...rPre.querySelectorAll('.vc-gutter')].map(g => {{
    const gr = g.getBoundingClientRect();
    return {{ text: g.textContent, dy: Math.round((gr.top - rRect.top) * 10) / 10, dx: Math.round((gr.left - rRect.left) * 10) / 10 }};
  }});
  const sel = window.getSelection();
  sel.removeAllRanges();
  const range = document.createRange();
  range.selectNodeContents(tCode);
  sel.addRange(range);
  const selected = sel.toString();
  const domText = [...tCode.childNodes]
    .filter(n => !(n.nodeType === 1 && n.classList.contains('vc-gutter')))
    .map(n => n.textContent)
    .join('');
  return {{
    name: c.name,
    widthMatch: Math.abs(tRect.width - rRect.width) < 0.01,
    heightMatch: Math.abs(tRect.height - rRect.height) < 0.01,
    guttersMatch: JSON.stringify(tGutters) === JSON.stringify(rGutters),
    domMatch: domText === c.src,
    selMatch: selected === c.expectedSel,
  }};
}});
document.getElementById('out').textContent = JSON.stringify(results);
</script></body></html>"#
    );

    let path: PathBuf = std::env::temp_dir().join(format!(
        "vesper_highlight_layout_{}.html",
        std::process::id()
    ));
    std::fs::write(&path, doc).unwrap();
    let url = format!("file://{}", path.display());
    let output = Command::new(browser)
        .args([
            "--headless",
            "--no-sandbox",
            "--disable-gpu",
            "--dump-dom",
            &url,
        ])
        .output();
    let _ = std::fs::remove_file(&path);
    let Ok(output) = output else {
        return;
    };
    if !output.status.success() {
        return;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json = stdout
        .split_once("<pre id=\"out\">")
        .and_then(|(_, rest)| rest.split_once("</pre>"))
        .map(|(body, _)| body)
        .unwrap_or_else(|| panic!("missing layout output in {stdout}"));
    for (name, _, _) in &cases {
        for line_numbers in [true, false] {
            let expected = format!(
                "{{\"name\":\"{name}_{line_numbers}\",\"widthMatch\":true,\"heightMatch\":true,\"guttersMatch\":true,\"domMatch\":true,\"selMatch\":true}}"
            );
            assert!(
                json.contains(&expected),
                "browser layout mismatch for {name}_{line_numbers}: {json}"
            );
        }
    }
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
