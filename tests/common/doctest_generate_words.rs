//! The words `@vesper/test-runner` (packages/vesper-test-runner/src/generate.ves)
//! reads and writes for the generated module of a doctest example
//! (self-hosting SH5 lane 14, #1337): the input encoder and result decoder
//! of `vesper::doctest_generate::generate`.
//!
//! Words are little-endian `u64`s; a text is its byte length followed by
//! its bytes padded to whole words, as in tests/common/test_runner_words.rs.
#![allow(dead_code)]

use vesper::doctest_generate::{GenerateInput, Generated};

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

/// The `generate` input.
pub fn generate_input(input: GenerateInput<'_>) -> Vec<u8> {
    let mut w = Vec::new();
    text(&mut w, input.base);
    word(&mut w, input.lines.len() as u64);
    for l in input.lines {
        text(&mut w, l);
    }
    text(&mut w, input.wrapper);
    match input.entry {
        Some(e) => {
            word(&mut w, 1);
            text(&mut w, e);
        }
        None => word(&mut w, 0),
    }
    word(&mut w, input.expected as u64);
    match input.sim_effects {
        Some(names) => {
            word(&mut w, 1);
            word(&mut w, names.len() as u64);
            for n in names {
                text(&mut w, n);
            }
        }
        None => word(&mut w, 0),
    }
    w
}

/// The `generate` result.
pub fn decode_generate(words: &[u64]) -> Generated {
    let mut pos = 0;
    let mut next = || {
        let w = words[pos];
        pos += 1;
        w
    };
    assert_eq!(next(), 0, "generate status");
    let text = |next: &mut dyn FnMut() -> u64| {
        let len = next() as usize;
        let mut bytes = Vec::with_capacity(len + 8);
        for _ in 0..len.div_ceil(8) {
            bytes.extend_from_slice(&next().to_le_bytes());
        }
        bytes.truncate(len);
        String::from_utf8(bytes).expect("@vesper/test-runner wrote UTF-8")
    };
    let source = text(&mut next);
    let snippet_start = next() as usize;
    let code_start = next() as usize;
    let n = next() as usize;
    let segments = (0..n).map(|_| (next() as usize, next() as usize)).collect();
    let entry = (next() != 0).then(|| text(&mut next));
    assert_eq!(pos, words.len(), "trailing generate words");
    Generated {
        source,
        snippet_start,
        code_start,
        segments,
        entry,
    }
}
