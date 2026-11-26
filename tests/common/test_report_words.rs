//! The words `@vesper/test-runner` (packages/vesper-test-runner/src/report.ves)
//! reads and writes for the text reports of `vesper test` and `vesper test
//! --doc` (self-hosting SH5 lane 14, #1337): the input encoders and result
//! decoder of `vesper::test_report`.
//!
//! Words are little-endian `u64`s; a text is its byte length followed by
//! its bytes padded to whole words, as in tests/common/test_runner_words.rs.
#![allow(dead_code)]

use vesper::test_report::{Diag, DocExample, Mutant, MutantCounts, MutantOutcome, UnitFile};

fn word(w: &mut Vec<u8>, x: u64) {
    w.extend_from_slice(&x.to_le_bytes());
}

fn text(w: &mut Vec<u8>, s: &str) {
    word(w, s.len() as u64);
    w.extend_from_slice(s.as_bytes());
    w.resize(w.len().next_multiple_of(8), 0);
}

fn optional(w: &mut Vec<u8>, s: Option<&str>) {
    match s {
        Some(s) => {
            word(w, 1);
            text(w, s);
        }
        None => word(w, 0),
    }
}

fn diags(w: &mut Vec<u8>, ds: &[Diag<'_>]) {
    word(w, ds.len() as u64);
    for d in ds {
        word(w, d.line as u64);
        word(w, d.column as u64);
        text(w, d.code);
        text(w, d.message);
    }
}

/// The `doc_text` input.
pub fn doc_input(file: &str, base: &[Diag<'_>], examples: &[DocExample<'_>]) -> Vec<u8> {
    let mut w = Vec::with_capacity(4096);
    text(&mut w, file);
    diags(&mut w, base);
    word(&mut w, examples.len() as u64);
    for e in examples {
        word(&mut w, e.index as u64);
        word(&mut w, e.line as u64);
        word(&mut w, e.column as u64);
        word(&mut w, e.passed as u64);
        text(&mut w, e.message);
        optional(&mut w, e.engine);
        diags(&mut w, e.diagnostics);
    }
    w
}

/// The `unit_text` and `baseline_text` input.
pub fn unit_input(files: &[UnitFile<'_>]) -> Vec<u8> {
    let mut w = Vec::with_capacity(4096);
    word(&mut w, files.len() as u64);
    for f in files {
        text(&mut w, f.display);
        optional(&mut w, f.error);
        word(&mut w, f.tests.len() as u64);
        for t in f.tests {
            text(&mut w, t.name);
            word(&mut w, t.line as u64);
            word(&mut w, t.column as u64);
            word(&mut w, t.passed as u64);
            text(&mut w, t.message);
            text(&mut w, t.engine);
            optional(&mut w, t.pin);
            optional(&mut w, t.capsule);
        }
    }
    w
}

/// The `mutants_text` input.
pub fn mutants_input(mutants: &[Mutant<'_>], counts: MutantCounts) -> Vec<u8> {
    let mut w = Vec::with_capacity(4096);
    word(&mut w, mutants.len() as u64);
    for m in mutants {
        text(&mut w, m.file);
        word(&mut w, m.line as u64);
        word(&mut w, m.column as u64);
        text(&mut w, m.function);
        text(&mut w, m.original);
        text(&mut w, m.replacement);
        match m.outcome {
            MutantOutcome::Killed(by) => {
                word(&mut w, 0);
                optional(&mut w, by);
            }
            MutantOutcome::Survived => word(&mut w, 1),
            MutantOutcome::IllTyped => word(&mut w, 2),
        }
    }
    for c in [
        counts.killed,
        counts.survived,
        counts.ill_typed,
        counts.tested,
        counts.total,
    ] {
        word(&mut w, c as u64);
    }
    w
}

/// Any report's result: 0 and the text.
pub fn decode_text(words: &[u64]) -> String {
    assert_eq!(words.first(), Some(&0), "report result tag");
    let len = words[1] as usize;
    assert_eq!(words.len(), 2 + len.div_ceil(8), "trailing report words");
    let mut bytes = Vec::with_capacity(len.next_multiple_of(8));
    for w in &words[2..] {
        bytes.extend_from_slice(&w.to_le_bytes());
    }
    bytes.truncate(len);
    String::from_utf8(bytes).expect("report text is UTF-8")
}
