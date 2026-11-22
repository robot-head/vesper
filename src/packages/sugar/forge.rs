//! Package functions outside the expression-macro call graph are checked in
//! the assembled package. Forge still checks every macro quotation, invoking
//! function, and transitively referenced local function in its closed program.
use super::*;
use crate::forge_frontend::{MapKind, parse_package_sugar};
use vesper_syntax::macro_source;

/// Mask only functions with no path from a macro-bearing body, quotation or
/// supporting declaration. Conservative token references keep plain callees,
/// recursive functions and functions referenced as values in the audit.
fn independent_functions(source: &str) -> Vec<Span> {
    let Ok(module) = macro_source::parse(source, Default::default()) else {
        return Vec::new();
    };
    let functions = module.functions();
    let ids: BTreeMap<_, _> = functions
        .iter()
        .enumerate()
        .map(|(index, f)| (f.header.name.as_str(), index))
        .collect();
    // Let Forge diagnose duplicate function declarations.
    if ids.len() != functions.len() {
        return Vec::new();
    }
    let mut edges = vec![Vec::new(); functions.len()];
    let mut pending: Vec<_> = functions
        .iter()
        .enumerate()
        .filter(|(_, f)| !f.invocations.is_empty())
        .map(|(i, _)| i)
        .collect();
    let mut position = 0;
    for token in module.tokens() {
        while functions
            .get(position)
            .is_some_and(|f| f.span.end <= token.span.start)
        {
            position += 1;
        }
        let Some(&target) = ids.get(&source[token.span.clone()]) else {
            continue;
        };
        if functions
            .get(position)
            .is_some_and(|f| f.span.start <= token.span.start)
        {
            edges[position].push(target);
        } else {
            pending.push(target);
        }
    }
    let mut required = vec![false; functions.len()];
    while let Some(i) = pending.pop() {
        if !std::mem::replace(&mut required[i], true) {
            pending.extend(&edges[i]);
        }
    }
    functions
        .iter()
        .zip(required)
        .filter(|(_, keep)| !keep)
        .map(|(f, _)| f.span.clone())
        .collect()
}

pub(super) fn parse(
    mut expanded: ExpandedSugar,
    tests: bool,
    audited: &mut Option<BTreeSet<String>>,
) -> Result<(String, parser::Parse), Diagnostic> {
    // A test block can introduce more macro-bearing functions in the test
    // compilation. The normal Forge test projection handles their authority.
    let spans = if tests && expanded.source.contains("test") {
        vec![]
    } else {
        independent_functions(&expanded.source)
    };
    let mut functions = Vec::new();
    for span in spans {
        let body = expanded.source[span.clone()].to_owned();
        expanded
            .source
            .replace_range(span.clone(), &" ".repeat(span.len()));
        functions.push((span, body));
    }
    let (mut text, checked, map) = parse_package_sugar(expanded, tests)?;
    *audited = Some(
        checked
            .module
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Function(f) => Some(f.name.clone()),
                _ => None,
            })
            .collect(),
    );
    if functions.is_empty() {
        return Ok((text, checked));
    }
    // Forge copies these byte ranges without edits. Verify that property
    // before restoring each original function for strict package parsing.
    let mut segments = map.iter().peekable();
    for (span, body) in functions {
        while segments
            .peek()
            .is_some_and(|m| m.original.end <= span.start)
        {
            segments.next();
        }
        let Some(segment) = segments.peek().filter(|m| {
            m.kind == MapKind::Source
                && m.original.start <= span.start
                && m.original.end >= span.end
                && m.original.end - m.original.start == m.expanded.end - m.expanded.start
        }) else {
            return Err(Diagnostic::new(
                "FORGE_EXPAND",
                "missing copied package function in macro source map",
                span,
            ));
        };
        let start = segment.expanded.start + span.start - segment.original.start;
        text.replace_range(start..start + body.len(), &body);
    }
    let mut parsed = if tests {
        parser::parse_for_tests(&text)
    } else {
        parser::parse(&text)
    };
    parsed.module.sugar_origins = checked.module.sugar_origins;
    Ok((text, parsed))
}
