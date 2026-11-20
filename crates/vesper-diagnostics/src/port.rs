//! The port build's side of the boundary (`cfg(vesper_port)`): the types the
//! generated binding (`port/binding.rs`) names as `crate::port::*`, the flat
//! encodings the Vesper functions read (documented in `vesper/src/lib.ves`)
//! and the calls behind `prepare`, `Prepared::lsp_json` and
//! `Prepared::terminal`.

pub use crate::port_binding::live_allocations;
use crate::{Error, Limits, Prepared};
use vesper_core::ast::Diagnostic;
use vesper_span::DiagnosticData;

/// `vesper_span::Resource`, mirrored: the binding can only implement its
/// trait for this crate's own types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Res {
    Source,
    Lines,
    Metadata,
    Work,
    Edits,
    Output,
    Diagnostics,
    Labels,
    Fixes,
    Text,
}

/// The errors the port raises; `error` maps them to `Error`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    SpanLimits,
    Budget(Res),
    ReversedRange,
    ByteOutOfBounds,
    Utf8Boundary,
    LineEndingInterior,
    InvalidDiagnostic,
    InvalidLimits,
    Name,
    Output,
    UnsupportedLineEnding,
}

/// `Limits`, flattened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caps {
    pub max_source_bytes: usize,
    pub max_lines: usize,
    pub max_metadata_bytes: usize,
    pub max_work: usize,
    pub max_edits: usize,
    pub max_output_bytes: usize,
    pub max_diagnostics: usize,
    pub max_labels: usize,
    pub max_fixes: usize,
    pub max_text_bytes: usize,
    pub max_name_bytes: usize,
}

/// `size_of` of the Rust records the budgets charge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sizes {
    pub diagnostic: usize,
    pub label: usize,
    pub value: usize,
    pub line: usize,
    pub report_label: usize,
}

/// The label and text totals of `prepare`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Totals {
    pub labels: usize,
    pub text: usize,
}

fn caps(l: Limits) -> Caps {
    let s = l.span;
    Caps {
        max_source_bytes: s.max_source_bytes,
        max_lines: s.max_lines,
        max_metadata_bytes: s.max_metadata_bytes,
        max_work: s.max_work,
        max_edits: s.max_edits,
        max_output_bytes: s.max_output_bytes,
        max_diagnostics: s.max_diagnostics,
        max_labels: s.max_labels,
        max_fixes: s.max_fixes,
        max_text_bytes: s.max_text_bytes,
        max_name_bytes: l.max_name_bytes,
    }
}

fn sizes() -> Sizes {
    Sizes {
        diagnostic: size_of::<DiagnosticData>(),
        label: size_of::<vesper_span::Label>(),
        value: size_of::<serde_json::Value>(),
        line: crate::ARIADNE_LINE_SIZE,
        report_label: crate::ARIADNE_LABEL_SIZE,
    }
}

fn error(f: Failure) -> Error {
    use vesper_span::{Error as S, Resource as R};
    let resource = |r| match r {
        Res::Source => R::Source,
        Res::Lines => R::Lines,
        Res::Metadata => R::Metadata,
        Res::Work => R::Work,
        Res::Edits => R::Edits,
        Res::Output => R::Output,
        Res::Diagnostics => R::Diagnostics,
        Res::Labels => R::Labels,
        Res::Fixes => R::Fixes,
        Res::Text => R::Text,
    };
    match f {
        Failure::SpanLimits => Error::Span(S::InvalidLimits),
        Failure::Budget(r) => Error::Span(S::Budget(resource(r))),
        Failure::ReversedRange => Error::Span(S::ReversedRange),
        Failure::ByteOutOfBounds => Error::Span(S::ByteOutOfBounds),
        Failure::Utf8Boundary => Error::Span(S::Utf8Boundary),
        Failure::LineEndingInterior => Error::Span(S::LineEndingInterior),
        Failure::InvalidDiagnostic => Error::Span(S::InvalidDiagnostic),
        Failure::InvalidLimits => Error::InvalidLimits,
        Failure::Name => Error::Name,
        Failure::Output => Error::Output,
        Failure::UnsupportedLineEnding => Error::UnsupportedLineEnding,
    }
}

/// The input of `prepare_check`: seven words per diagnostic (code, message
/// and help lengths, help presence, related count, primary span), then three
/// per related label (span, message length). It stops at the diagnostic
/// whose labels pass the label budget, where the check raises before it
/// reads a label, and is empty when the diagnostics pass their budget.
fn encode_input(diagnostics: &[Diagnostic], limits: Limits) -> Vec<u64> {
    if diagnostics.len() > limits.span.max_diagnostics {
        return Vec::new();
    }
    let mut words = Vec::with_capacity(diagnostics.len() * 7);
    let mut labels = 0usize;
    for d in diagnostics {
        words.extend_from_slice(&[
            d.code.len() as u64,
            d.message.len() as u64,
            d.help.is_some() as u64,
            d.help.as_ref().map_or(0, String::len) as u64,
            d.related.len() as u64,
            d.span.start as u64,
            d.span.end as u64,
        ]);
        labels = labels.saturating_add(1).saturating_add(d.related.len());
        if labels > limits.span.max_labels {
            break;
        }
        for l in &d.related {
            words.extend_from_slice(&[
                l.span.start as u64,
                l.span.end as u64,
                l.message.len() as u64,
            ]);
        }
    }
    words
}

/// The checks of `prepare`, before any copy.
pub fn prepare(text: &str, diagnostics: &[Diagnostic], limits: Limits) -> Result<Totals, Error> {
    let input = encode_input(diagnostics, limits);
    crate::port_binding::prepare_check(
        text,
        &input,
        diagnostics.len() as u64,
        caps(limits),
        sizes(),
    )
    .map_err(error)
}

/// The checked model: five words per diagnostic (code, message and help
/// lengths, help presence, label count), three per label (span, message
/// length), and the pool of every text in that order.
pub fn encode_model(data: &[DiagnosticData]) -> (Vec<u64>, String) {
    let words = data.iter().map(|d| 5 + 3 * d.labels.len()).sum();
    let text = data
        .iter()
        .map(|d| {
            d.code.len()
                + d.message.len()
                + d.help.as_ref().map_or(0, String::len)
                + d.labels.iter().map(|l| l.message.len()).sum::<usize>()
        })
        .sum();
    let mut model = Vec::with_capacity(words);
    let mut pool = String::with_capacity(text);
    for d in data {
        model.extend_from_slice(&[
            d.code.len() as u64,
            d.message.len() as u64,
            d.help.is_some() as u64,
            d.help.as_ref().map_or(0, String::len) as u64,
            d.labels.len() as u64,
        ]);
        pool.push_str(&d.code);
        pool.push_str(&d.message);
        if let Some(help) = &d.help {
            pool.push_str(help);
        }
        for l in &d.labels {
            model.extend_from_slice(&[
                l.span.start as u64,
                l.span.end as u64,
                l.message.len() as u64,
            ]);
            pool.push_str(&l.message);
        }
    }
    (model, pool)
}

pub fn lsp(p: &Prepared<'_>, uri: &str) -> Result<Vec<u8>, Error> {
    crate::port_binding::lsp(
        p.index.text(),
        &p.model.0,
        p.data().len() as u64,
        p.model.1.as_bytes(),
        uri,
        caps(p.limits),
        sizes(),
        p.label_count,
        p.text_bytes,
    )
    .map_err(error)
}

pub fn terminal(p: &Prepared<'_>, file: &str, color: bool) -> Result<Vec<u8>, Error> {
    crate::port_binding::terminal(
        p.index.text(),
        &p.model.0,
        p.data().len() as u64,
        p.model.1.as_bytes(),
        file,
        caps(p.limits),
        sizes(),
        p.label_count,
        p.text_bytes,
        color as u64,
    )
    .map_err(error)
}
