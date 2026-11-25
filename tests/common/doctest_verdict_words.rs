//! The words `@vesper/test-runner` (packages/vesper-test-runner/src/verdict.ves)
//! reads and writes for the doctest wrapper names and verdicts (self-hosting
//! SH5 lane 14, #1337): the input encoders and result decoders of
//! `vesper::doctest_verdict::{wrappers, verdict}`.
//!
//! Words are little-endian `u64`s; a text is its byte length followed by
//! its bytes padded to whole words, as in tests/common/test_runner_words.rs.
#![allow(dead_code)]

use vesper::doctest_verdict::{ExampleNames, Located, Verdict, VerdictInput};

fn word(w: &mut Vec<u8>, x: u64) {
    w.extend_from_slice(&x.to_le_bytes());
}

fn text(w: &mut Vec<u8>, s: &str) {
    word(w, s.len() as u64);
    w.extend_from_slice(s.as_bytes());
    while !w.len().is_multiple_of(8) {
        w.push(0);
    }
}

struct Reader<'a> {
    words: &'a [u64],
    pos: usize,
}

impl Reader<'_> {
    fn word(&mut self) -> u64 {
        let w = self.words[self.pos];
        self.pos += 1;
        w
    }

    fn text(&mut self) -> String {
        let len = self.word() as usize;
        let mut bytes = Vec::with_capacity(len + 8);
        for _ in 0..len.div_ceil(8) {
            bytes.extend_from_slice(&self.word().to_le_bytes());
        }
        bytes.truncate(len);
        String::from_utf8(bytes).expect("@vesper/test-runner wrote UTF-8")
    }

    fn done(&self) {
        assert_eq!(self.pos, self.words.len(), "trailing verdict words");
    }
}

/// The `wrappers` input.
pub fn wrappers_input(functions: &[&str], examples: &[ExampleNames<'_>]) -> Vec<u8> {
    let mut w = Vec::new();
    word(&mut w, functions.len() as u64);
    for f in functions {
        text(&mut w, f);
    }
    word(&mut w, examples.len() as u64);
    for e in examples {
        match e.entry {
            Some(name) => {
                word(&mut w, 1);
                text(&mut w, name);
            }
            None => word(&mut w, 0),
        }
        word(&mut w, e.lines.len() as u64);
        for l in e.lines {
            text(&mut w, l);
        }
    }
    w
}

/// The `wrappers` result.
pub fn decode_wrappers(words: &[u64]) -> Result<Vec<String>, String> {
    let mut r = Reader { words, pos: 0 };
    let out = if r.word() == 0 {
        let n = r.word() as usize;
        Ok((0..n).map(|_| r.text()).collect())
    } else {
        Err(r.text())
    };
    r.done();
    out
}

/// The `verdict` input.
pub fn verdict_input(input: VerdictInput<'_>) -> Vec<u8> {
    let mut w = Vec::new();
    text(&mut w, input.source);
    word(&mut w, input.offset as u64);
    word(&mut w, input.lines.len() as u64);
    for &(offset, len) in input.lines {
        word(&mut w, offset as u64);
        word(&mut w, len as u64);
    }
    word(&mut w, input.snippet_start as u64);
    word(&mut w, input.code_start as u64);
    word(&mut w, input.segments.len() as u64);
    for &(start, end) in input.segments {
        word(&mut w, start as u64);
        word(&mut w, end as u64);
    }
    word(&mut w, input.errors.len() as u64);
    for e in input.errors {
        text(&mut w, e.code);
        text(&mut w, e.message);
        word(&mut w, e.start as u64);
        word(&mut w, e.end as u64);
    }
    match input.expected {
        Some(code) => {
            word(&mut w, 1);
            text(&mut w, code);
        }
        None => word(&mut w, 0),
    }
    word(&mut w, input.own_entry as u64);
    w
}

/// The `verdict` result.
pub fn decode_verdict(words: &[u64]) -> Verdict {
    let mut r = Reader { words, pos: 0 };
    let line = r.word() as usize;
    let column = r.word() as usize;
    let n = r.word() as usize;
    let diagnostics = (0..n)
        .map(|_| Located {
            code: r.text(),
            message: r.text(),
            byte_start: r.word() as usize,
            byte_end: r.word() as usize,
            line: r.word() as usize,
            column: r.word() as usize,
        })
        .collect();
    let decided = if r.word() != 0 {
        let passed = r.word() != 0;
        Some((passed, r.text()))
    } else {
        None
    };
    r.done();
    Verdict {
        line,
        column,
        diagnostics,
        decided,
    }
}
