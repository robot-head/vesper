//! Lower source sugar before the normal checker. Expression macros in its
//! output still require Forge's hygiene, checking and authority pipeline.
use super::*;
use crate::{ast::SugarEffectRegion, parser};
use vesper_syntax::sugar_expand::{ExpandedSugar, expand_sugar_mapped};

fn has_macro_declaration(source: &str) -> bool {
    source.contains("macro")
        && crate::lexer::lex(source).0.iter().any(|token| {
            token.kind == crate::lexer::TokenKind::Lower && &source[token.span.clone()] == "macro"
        })
}

fn parse(source: &str, include_tests: bool) -> parser::Parse {
    if include_tests {
        parser::parse_for_tests(source)
    } else {
        parser::parse(source)
    }
}

/// Parse a source module for execution. Ordinary macros retain their explicit
/// Forge entry point; macros composed with sugar go through the same checks.
pub fn parse_compiler_source(
    source: &str,
    include_tests: bool,
) -> Result<(std::borrow::Cow<'_, str>, parser::Parse), Diagnostic> {
    let (bytes, parsed, _) = parse_compiler_input(source, include_tests)?;
    Ok((bytes, parsed))
}

pub type CompilerInput<'a> = (
    std::borrow::Cow<'a, str>,
    parser::Parse,
    Vec<SourceMapSegment>,
);

/// Keep the generated-to-source correspondence for the CLI diagnostic renderer.
pub fn parse_compiler_input(
    source: &str,
    include_tests: bool,
) -> Result<CompilerInput<'_>, Diagnostic> {
    let parsed = parse(source, include_tests);
    if parsed.diagnostics.is_empty() {
        return Ok((source.into(), parsed, Vec::new()));
    }
    match expand_sugar_mapped(source)? {
        Some(expanded) => {
            let (bytes, parsed, map) = parse_resolved(expanded, include_tests, Some(source))?;
            Ok((bytes.into(), parsed, map))
        }
        None => Ok((source.into(), parsed, Vec::new())),
    }
}

/// Preserve original caller/definition labels reported by the effect checker.
pub fn original_diagnostic(map: &[SourceMapSegment], mut d: Diagnostic) -> Diagnostic {
    if !map.is_empty() && !matches!(d.code.as_str(), "E0143" | "E0144") {
        for span in
            std::iter::once(&mut d.span).chain(d.related.iter_mut().map(|label| &mut label.span))
        {
            if let Some(origin) = provenance::origin_of(map, span) {
                *span = origin.original.start..origin.original.end;
            }
        }
    }
    d
}

/// Check a source module using the compiler expansion pipeline and editor's
/// bounded checker. The checked artifact's source and spans are the expanded
/// bytes; raw editor snapshots retain their original-input semantics.
pub fn check_compiler_source(
    source: &str,
    limits: AnalysisLimits,
) -> Result<CheckedSource, AnalysisFailure> {
    limits.validate()?;
    if source.len() > limits.max_source_bytes {
        return Err(AnalysisFailure::Resource {
            reason: "analysis source byte budget exceeded".into(),
        });
    }
    let (expanded, parsed, map) =
        parse_compiler_input(source, false).map_err(|d| AnalysisFailure::Parse {
            diagnostics: vec![d],
        })?;
    check_source_with_sugar(&expanded, limits, parsed.module.sugar_origins).map_err(|failure| {
        let mapped = |errors: Vec<Diagnostic>| {
            errors
                .into_iter()
                .map(|d| original_diagnostic(&map, d))
                .collect()
        };
        match failure {
            AnalysisFailure::Parse { diagnostics } => AnalysisFailure::Parse {
                diagnostics: mapped(diagnostics),
            },
            AnalysisFailure::Semantic { diagnostics } => AnalysisFailure::Semantic {
                diagnostics: mapped(diagnostics),
            },
            other => other,
        }
    })
}

/// Accept a resolver's expansion, including the effect evidence for each
/// template. AST offsets refer to the returned source text.
pub fn parse_expanded_sugar(
    expanded: ExpandedSugar,
    include_tests: bool,
) -> Result<(String, parser::Parse), Diagnostic> {
    let (source, parsed, _) = parse_resolved(expanded, include_tests, None)?;
    Ok((source, parsed))
}

/// Forge validates a module of core declarations after expansion. Keep its
/// byte coordinates when exposing or dropping the declarations of a test block.
pub(super) fn macro_test_input(source: &str, include_tests: bool) -> std::borrow::Cow<'_, str> {
    if !source.contains("test") {
        return source.into();
    }
    let Ok(surface) = parser::parse_surface_with_limits(source, Default::default()) else {
        return source.into();
    };
    if !surface.diagnostics.is_empty() {
        return source.into();
    }
    let mut bytes = std::borrow::Cow::Borrowed(source.as_bytes());
    for item in surface
        .syntax()
        .children()
        .filter(|n| n.kind() == parser::ITEM)
    {
        let mut direct = item
            .children_with_tokens()
            .filter_map(|child| child.into_token())
            .filter(|token| {
                use crate::lexer::TokenKind as K;
                ![K::Whitespace, K::Comment, K::BlockComment]
                    .iter()
                    .any(|kind| *kind as u16 == token.kind().0)
            });
        let (Some(head), Some(open)) = (direct.next(), direct.next()) else {
            continue;
        };
        if head.text() != "test" || open.text() != "{" {
            continue;
        }
        let Some(close) = direct.last().filter(|token| token.text() == "}") else {
            continue;
        };
        let [head, open, close] = [head, open, close].map(|token| {
            let range = token.text_range();
            usize::from(range.start())..usize::from(range.end())
        });
        let mut clear = |span: Span| {
            for byte in &mut bytes.to_mut()[span] {
                if !matches!(*byte, b'\n' | b'\r') {
                    *byte = b' ';
                }
            }
        };
        if include_tests {
            clear(head);
            clear(open);
            clear(close);
        } else {
            clear(head.start..close.end);
        }
    }
    match bytes {
        std::borrow::Cow::Borrowed(_) => source.into(),
        std::borrow::Cow::Owned(text) => String::from_utf8(text)
            .expect("only complete token spans were blanked")
            .into(),
    }
}

fn parse_resolved(
    expanded: ExpandedSugar,
    include_tests: bool,
    original: Option<&str>,
) -> Result<(String, parser::Parse, Vec<SourceMapSegment>), Diagnostic> {
    let mut parsed = parse(&expanded.source, include_tests);
    if !parsed.diagnostics.is_empty() && has_macro_declaration(&expanded.source) {
        let input = macro_test_input(&expanded.source, include_tests);
        let artifact = compile_macros(
            &input,
            &CompileConfig::default(),
            CompileLimits::default(),
            Some(&expanded),
            original,
        )
        .map_err(|failure| {
            let mut d = failure
                .diagnostics
                .into_iter()
                .next()
                .unwrap_or_else(|| Diagnostic::new("FORGE_EXPAND", failure.message, 0..0));
            if !matches!(d.code.as_str(), "E0143" | "E0144") {
                d.span = match failure.origins.first().and_then(|o| o.as_ref()) {
                    Some(o) => o.original.start..o.original.end,
                    None => expanded.original_span(d.span),
                };
            }
            d
        })?;
        return Ok((
            artifact.expanded_source().to_owned(),
            artifact.checked().parsed().clone(),
            artifact.map,
        ));
    }
    parsed.module.sugar_origins = expanded.regions;
    let map = expanded
        .mappings
        .into_iter()
        .map(|m| SourceMapSegment {
            expanded: (&m.expanded).into(),
            original: (&m.original).into(),
            kind: if m.copied {
                MapKind::Source
            } else {
                MapKind::Generated
            },
            r#macro: None,
        })
        .collect();
    Ok((expanded.source, parsed, map))
}

/// Project a range in the macro input onto its emitted bytes. Copied pieces
/// retain byte offsets. A generated piece belongs to the invocation that
/// generated it, including when a sugar template supplied that invocation.
fn projected(span: &Span, map: &[SourceMapSegment]) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    for entry in map {
        let input = entry.original;
        let overlap = input.start.max(span.start)..input.end.min(span.end);
        if overlap.is_empty() {
            continue;
        }
        let output = entry.expanded;
        let mapped = if entry.kind == MapKind::Source
            && input.end - input.start == output.end - output.start
        {
            output.start + (overlap.start - input.start)..output.start + (overlap.end - input.start)
        } else if span.start <= input.start && input.end <= span.end {
            output.start..output.end
        } else {
            continue;
        };
        if let Some(previous) = out.last_mut()
            && previous.end == mapped.start
        {
            previous.end = mapped.end;
        } else {
            out.push(mapped);
        }
    }
    out
}

pub(super) fn project_regions(
    regions: &[SugarEffectRegion],
    map: &[SourceMapSegment],
    work: &mut Work,
) -> Result<Vec<SugarEffectRegion>, CompileFailure> {
    let mut out = Vec::with_capacity(regions.len());
    for region in regions {
        work.charge(mul(
            map.len(),
            1 + region.holes.len() + region.places.len(),
        )?)?;
        let pieces = projected(&region.expanded, map);
        let (Some(first), Some(last)) = (pieces.first(), pieces.last()) else {
            continue;
        };
        let mut holes: Vec<_> = pieces.windows(2).map(|w| w[0].end..w[1].start).collect();
        holes.extend(region.holes.iter().flat_map(|hole| projected(hole, map)));
        let places = region
            .places
            .iter()
            .flat_map(|(s, pass)| projected(s, map).into_iter().map(move |s| (s, *pass)))
            .collect();
        out.push(SugarEffectRegion {
            expanded: first.start..last.end,
            holes,
            places,
            ..region.clone()
        });
    }
    Ok(out)
}

pub(super) fn compose_maps(
    expansion: &ExpandedSugar,
    map: &[SourceMapSegment],
    original: &str,
    expanded: &str,
    limits: CompileLimits,
    work: &mut Work,
) -> Result<Vec<SourceMapSegment>, CompileFailure> {
    let mut result = Vec::new();
    for entry in map {
        let input = entry.original.start..entry.original.end;
        let output = entry.expanded.start..entry.expanded.end;
        if entry.kind == MapKind::Generated || input.len() != output.len() {
            append_map(
                &mut result,
                SourceMapSegment {
                    original: (&expansion.original_span(input)).into(),
                    expanded: entry.expanded,
                    kind: entry.kind,
                    r#macro: entry.r#macro.clone(),
                },
                limits,
                original,
                expanded,
                work,
            )?;
            continue;
        }
        let first = expansion
            .mappings
            .partition_point(|m| m.expanded.end <= input.start);
        for m in expansion.mappings[first..]
            .iter()
            .take_while(|m| m.expanded.start < input.end)
        {
            let span = input.start.max(m.expanded.start)..input.end.min(m.expanded.end);
            if span.is_empty() {
                continue;
            }
            let original_span = if m.copied {
                m.original.start + (span.start - m.expanded.start)
                    ..m.original.start + (span.end - m.expanded.start)
            } else {
                m.original.clone()
            };
            let dst =
                output.start + (span.start - input.start)..output.start + (span.end - input.start);
            append_map(
                &mut result,
                SourceMapSegment {
                    original: (&original_span).into(),
                    expanded: (&dst).into(),
                    kind: if m.copied {
                        MapKind::Source
                    } else {
                        MapKind::Generated
                    },
                    r#macro: entry.r#macro.clone(),
                },
                limits,
                original,
                expanded,
                work,
            )?;
        }
    }
    Ok(result)
}
