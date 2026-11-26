//! The dyn lowering swap's words (front-end wiring slice 4, #1976): the
//! module goes to `@vesper/check-dyn` as the `ast` section of the
//! `parse_words` stream (`ast_words.rs`), and the outcome comes back in the
//! layout packages/vesper-check-dyn/src/lib.ves documents ("the hybrid
//! swap"). The lowered module is read with `parse_words::read_ast_section`
//! (crates/vesper-hybrid/src/parse_words.rs), which the includer declares
//! next to this module as `parse_words`.
//!
//! Shared by the hybrid's `src/swap/check_dyn.rs` and the root package's
//! tests/self_host_check_dyn.rs, so keep it free of test-only dependencies.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use vesper::ast::{Diagnostic, DiagnosticLabel, Module, Span, Type};
use vesper::dyn_trait::{Callee, Forward};
use vesper::{dyn_fn, dyn_trait};

use super::parse_words::{AstSection, read_ast_section};

/// The cursor over an outcome.
struct Words<'a> {
    w: &'a [u64],
    at: usize,
}

impl Words<'_> {
    fn next(&mut self) -> u64 {
        let v = self.w.get(self.at).copied().unwrap_or(0);
        self.at += 1;
        v
    }

    fn count(&mut self) -> usize {
        self.next() as usize
    }

    fn span(&mut self) -> Span {
        let start = self.count();
        let end = self.count();
        start..end
    }

    /// `put_text_words`: the byte length, then the bytes eight per word.
    fn text(&mut self) -> String {
        let len = self.count();
        let words = len.div_ceil(8);
        let end = (self.at + words).min(self.w.len());
        let mut bytes = Vec::with_capacity(words * 8);
        for w in &self.w[self.at.min(end)..end] {
            bytes.extend_from_slice(&w.to_le_bytes());
        }
        self.at += words;
        bytes.truncate(len);
        String::from_utf8(bytes)
            .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
    }

    fn diagnostics(&mut self) -> Vec<Diagnostic> {
        let n = self.count();
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            let code = self.text();
            let message = self.text();
            let span = self.span();
            let help = self.text();
            let k = self.count();
            let related = (0..k)
                .map(|_| {
                    let span = self.span();
                    DiagnosticLabel {
                        span,
                        message: self.text(),
                    }
                })
                .collect();
            out.push(Diagnostic {
                code,
                message,
                span,
                related,
                help: (!help.is_empty()).then_some(help),
            });
        }
        out
    }

    /// The `ok` word and the diagnostics; `Err` when the pass failed.
    fn head(&mut self) -> Result<(), Vec<Diagnostic>> {
        let ok = self.next() == 1;
        let diagnostics = self.diagnostics();
        if ok { Ok(()) } else { Err(diagnostics) }
    }

    /// The lowered module, keeping `input`'s `sugar_origins` (the `ast`
    /// section does not carry them, and neither pass changes them).
    fn module(&mut self, input: &Module) -> AstSection {
        let mut section = read_ast_section(self.w, &mut self.at);
        section.module.sugar_origins = input.sugar_origins.clone();
        section
    }
}

/// `dyn_fn::lower`'s result from `dyn_fn_words(module_words(input), base)`.
pub fn decode_dyn_fn(
    input: &Module,
    base: usize,
    words: &[u64],
) -> Result<dyn_fn::Lowered, Vec<Diagnostic>> {
    let mut r = Words { w: words, at: 0 };
    r.head()?;
    let module = r.module(input).module;
    let end = r.count();
    let n = r.count();
    let origins = (0..n)
        .map(|_| {
            let first = r.count();
            (first, r.span())
        })
        .collect();
    Ok(dyn_fn::Lowered {
        module,
        base,
        end,
        origins,
    })
}

/// `dyn_trait::lower`'s result from `dyn_trait_words(module_words(input))`.
pub fn decode_dyn_trait(
    input: &Module,
    words: &[u64],
) -> Result<dyn_trait::Lowered, Vec<Diagnostic>> {
    let mut r = Words { w: words, at: 0 };
    r.head()?;
    let section = r.module(input);
    let name = |sym: u64| section.name(sym).to_owned();
    // Each hidden generic's trait is its bound's trait reference: the pass
    // pushes one bound per lowered parameter, and the last one for an
    // (owner, generic) pair is the parameter `params` kept.
    let mut traits: BTreeMap<(&str, &str), Type> = BTreeMap::new();
    for b in &section.module.trait_bounds {
        traits.insert(
            (&b.owner, &b.param),
            Type::Named(b.trait_ref.name.clone(), b.trait_ref.args.clone()),
        );
    }
    let mut params: BTreeMap<String, BTreeMap<String, Type>> = BTreeMap::new();
    for _ in 0..r.count() {
        let owner = name(r.next());
        let g = name(r.next());
        let ty = traits
            .get(&(owner.as_str(), g.as_str()))
            .cloned()
            .unwrap_or_else(|| Type::named(&g));
        params.entry(owner).or_default().insert(g, ty);
    }
    let n = r.count();
    let mut forwards = Vec::with_capacity(n);
    for _ in 0..n {
        let param = name(r.next());
        let call = r.span();
        let kind = r.next();
        let callee = r.next();
        let base = r.span();
        let callee = if kind == 1 {
            Callee::Name(name(callee))
        } else {
            Callee::Method(base, name(callee))
        };
        let index = r.count();
        let span = r.span();
        forwards.push(Forward {
            name: param,
            call,
            callee,
            index,
            span,
        });
    }
    let boxed: BTreeSet<String> = (0..r.count()).map(|_| name(r.next())).collect();
    Ok(dyn_trait::Lowered {
        module: section.module,
        params,
        forwards,
        boxed,
    })
}
