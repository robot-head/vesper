// The sugar expansion swap's words (SH1 lane 14, #1157), shared by
// src/swap/sugar.rs and tests/sugar.rs: `input` writes the text that
// modules/sugar's `expand_words` reads (`@vesper/sugar::
// sugar_expand_scoped_words`' layout), and `decode` reads its words back
// into what `vesper_syntax::sugar_expand::rust_expand_sugar` returns.

use std::collections::HashMap;

use vesper_core::ast::{Diagnostic, DiagnosticLabel, Effects, SugarEffectRegion, SugarPass};
use vesper_syntax::sugar_expand::{ExpandedSugar, Expansion, SugarSourceSpan};

/// The largest number the input's four base-128 bytes hold.
const MAX_NUMBER: usize = (1 << 28) - 1;

/// The input text of `expand_words` for one call of
/// `expand_sugar_with_scopes_for_execution`, or `None` when a length does
/// not fit its four bytes. Scopes the caller shares (the same slice) are
/// one scope, as the Rust driver binds them once.
pub fn input(
    source: &str,
    imports: &[(&str, &str)],
    scopes: &[&[(String, usize)]],
    visible: &[usize],
) -> Option<String> {
    let mut groups: Vec<&[(String, usize)]> = vec![];
    let mut seen = HashMap::new();
    let mut scope_of = Vec::with_capacity(scopes.len());
    for &scope in scopes {
        let key = (scope.as_ptr() as usize, scope.len());
        let id = *seen.entry(key).or_insert_with(|| {
            groups.push(scope);
            groups.len() - 1
        });
        scope_of.push(id);
    }
    let names: usize = groups.iter().map(|g| g.len()).sum();
    let mut text_len = source.len();
    for (alias, decl) in imports {
        text_len += alias.len() + decl.len();
    }
    for g in &groups {
        text_len += g.iter().map(|(n, _)| n.len()).sum::<usize>();
    }
    let numbers = 6 + 2 * imports.len() + scope_of.len() + groups.len() + 2 * names + visible.len();
    let mut text = String::with_capacity(4 * numbers + text_len);
    let mut ok = true;
    let mut number = |text: &mut String, n: usize| {
        ok &= n <= MAX_NUMBER;
        for k in 0..4 {
            text.push(char::from((n >> (7 * k)) as u8 & 127));
        }
    };
    number(&mut text, source.len());
    number(&mut text, imports.len());
    number(&mut text, scope_of.len());
    number(&mut text, groups.len());
    number(&mut text, names);
    number(&mut text, visible.len());
    for (alias, decl) in imports {
        number(&mut text, alias.len());
        number(&mut text, decl.len());
    }
    for &g in &scope_of {
        number(&mut text, g);
    }
    for g in &groups {
        number(&mut text, g.len());
    }
    for g in &groups {
        for (name, id) in g.iter() {
            number(&mut text, name.len());
            number(&mut text, *id);
        }
    }
    for &v in visible {
        number(&mut text, v);
    }
    text.push_str(source);
    for (alias, decl) in imports {
        text.push_str(alias);
        text.push_str(decl);
    }
    for g in &groups {
        for (name, _) in g.iter() {
            text.push_str(name);
        }
    }
    ok.then_some(text)
}

struct Words<'a>(&'a [u64], usize);

impl Words<'_> {
    fn next(&mut self) -> u64 {
        let w = self.0.get(self.1).copied().unwrap_or(u64::MAX);
        self.1 += 1;
        w
    }
    fn n(&mut self) -> usize {
        self.next() as usize
    }
    /// A count, bounded by the words left so a damaged stream cannot
    /// allocate without bound.
    fn count(&mut self) -> usize {
        self.n().min(self.0.len().saturating_sub(self.1))
    }
    fn span(&mut self) -> std::ops::Range<usize> {
        let start = self.n();
        start..self.n()
    }
    fn text(&mut self) -> String {
        let len = self.count();
        let bytes: Vec<u8> = (0..len).map(|_| self.next() as u8).collect();
        String::from_utf8(bytes)
            .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
    }
}

/// The effect row of an `emits` clause's text (`eff [..]`; empty for no
/// clause), as `read_definition` reads it from the declaration.
fn emits(row: &str, rows: &mut HashMap<String, Effects>) -> Effects {
    if row.is_empty() {
        return Effects::default();
    }
    rows.entry(row.to_owned())
        .or_insert_with(|| {
            let wrapped = format!("fn __sugar_row() {row} {{}}");
            match vesper_syntax::parser::rust_parse(&wrapped)
                .module
                .items
                .into_iter()
                .next()
            {
                Some(vesper_core::ast::Item::Function(f)) => f.effects,
                _ => Effects::default(),
            }
        })
        .clone()
}

fn pass(word: u64) -> SugarPass {
    match word {
        1 => SugarPass::In,
        2 => SugarPass::Inout,
        3 => SugarPass::Out,
        _ => SugarPass::Sink,
    }
}

/// `expand_words`' words: 0 (no sugar), 2 and a diagnostic, or 1 and the
/// expansion (driver.ves `expand_words`).
pub fn decode(words: &[u64]) -> Expansion {
    let mut w = Words(words, 0);
    match w.next() {
        0 => Ok(None),
        2 => {
            let code = w.text();
            let message = w.text();
            let span = w.span();
            let related = (0..w.count())
                .map(|_| {
                    let span = w.span();
                    DiagnosticLabel {
                        span,
                        message: w.text(),
                    }
                })
                .collect();
            Err(Diagnostic {
                code,
                message,
                span,
                related,
                help: None,
            })
        }
        _ => {
            let source = w.text();
            let fresh_prefix = w.text();
            let mut rows = HashMap::new();
            let n = w.count();
            let mut regions = Vec::with_capacity(n);
            let mut import_origins = Vec::with_capacity(n);
            for _ in 0..n {
                let expanded = w.span();
                let holes = (0..w.count()).map(|_| w.span()).collect();
                let places = (0..w.count())
                    .map(|_| {
                        let span = w.span();
                        (span, pass(w.next()))
                    })
                    .collect();
                let row = w.text();
                let call = w.span();
                let rule = w.span();
                let import = Some(w.next())
                    .filter(|&i| i != u64::MAX)
                    .map(|i| i as usize);
                regions.push(SugarEffectRegion {
                    expanded,
                    holes,
                    places,
                    emits: emits(&row, &mut rows),
                    call,
                    rule,
                });
                import_origins.push(import);
            }
            let mappings = (0..w.count())
                .map(|_| {
                    let expanded = w.span();
                    let original = w.span();
                    SugarSourceSpan {
                        expanded,
                        original,
                        copied: w.next() != 0,
                    }
                })
                .collect();
            Ok(Some(ExpandedSugar {
                source,
                fresh_prefix,
                regions,
                import_origins,
                mappings,
            }))
        }
    }
}
