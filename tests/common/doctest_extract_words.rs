//! The words `@vesper/test-runner` (packages/vesper-test-runner/src/doctest.ves)
//! reads and writes for doctest fence extraction and the token preflight
//! (self-hosting SH5 lane 14, #1337): the input encoders and result
//! decoders of `vesper::doctest_extract`'s two swapped functions. Shared by
//! tests/self_host_doctest_extract.rs (and the hybrid build once it installs
//! the table).
//!
//! Words are little-endian `u64`s. A text is its byte length followed by
//! its bytes, padded with zeros to whole words (util.ves `Reader::text` and
//! `put_text`), as in tests/common/test_runner_words.rs.
#![allow(dead_code)]

use std::collections::BTreeSet;
use vesper::doctest_extract::{Example, ExtractLimits, Line, PreflightLimits};
use vesper_doc::sim::{Device, SimSpec};

#[derive(Default)]
struct Words(Vec<u8>);

impl Words {
    fn word(&mut self, w: u64) {
        self.0.extend_from_slice(&w.to_le_bytes());
    }

    fn text(&mut self, s: &str) {
        self.word(s.len() as u64);
        self.0.extend_from_slice(s.as_bytes());
        while !self.0.len().is_multiple_of(8) {
            self.0.push(0);
        }
    }
}

/// The `preflight` input.
pub fn preflight_input(source: &str, limits: PreflightLimits) -> Vec<u8> {
    let mut w = Words::default();
    w.text(source);
    w.word(limits.max_tokens as u64);
    w.word(limits.max_nesting as u64);
    w.word(limits.max_segment_tokens as u64);
    w.word(limits.max_declarations as u64);
    w.0
}

/// The `extract` input.
pub fn extract_input(source: &str, limits: ExtractLimits) -> Vec<u8> {
    let mut w = Words::default();
    w.text(source);
    w.word(limits.max_snippet_bytes as u64);
    w.word(limits.max_examples as u64);
    w.0
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

    fn usize(&mut self) -> usize {
        self.word() as usize
    }

    fn text(&mut self) -> String {
        let len = self.usize();
        let mut bytes = Vec::with_capacity(len + 8);
        for _ in 0..len.div_ceil(8) {
            bytes.extend_from_slice(&self.word().to_le_bytes());
        }
        bytes.truncate(len);
        String::from_utf8(bytes).expect("@vesper/test-runner wrote UTF-8")
    }

    fn status(&mut self) -> Result<(), String> {
        if self.word() != 0 {
            Err(self.text())
        } else {
            Ok(())
        }
    }

    fn done(&self) -> bool {
        self.pos == self.words.len()
    }
}

/// The `preflight` result.
pub fn decode_preflight(words: &[u64]) -> Result<(), String> {
    let mut r = Reader { words, pos: 0 };
    r.status()?;
    assert!(r.done(), "trailing preflight words");
    Ok(())
}

/// The `extract` result: each example's lines are ranges of `source`.
pub fn decode_extract(source: &str, words: &[u64]) -> Result<Vec<Example>, String> {
    let mut r = Reader { words, pos: 0 };
    r.status()?;
    let n = r.usize();
    let mut examples = Vec::with_capacity(n);
    for _ in 0..n {
        let offset = r.usize();
        let has_expected = r.word() != 0;
        let expected = r.text();
        let has_entry = r.word() != 0;
        let entry = r.text();
        let has_sim = r.word() != 0;
        let seed = r.word();
        let bits = r.word();
        let lines = (0..r.usize())
            .map(|_| {
                let start = r.usize();
                let end = r.usize();
                Line {
                    text: source[start..end].to_string(),
                    offset: start,
                }
            })
            .collect();
        let devices: BTreeSet<Device> = [(1, Device::Clock), (2, Device::Net), (4, Device::Disk)]
            .into_iter()
            .filter(|(bit, _)| bits & bit != 0)
            .map(|(_, d)| d)
            .collect();
        examples.push(Example {
            lines,
            offset,
            expected: has_expected.then_some(expected),
            entry: has_entry.then_some(entry),
            sim: has_sim.then_some(SimSpec { seed, devices }),
        });
    }
    assert!(r.done(), "trailing extract words");
    Ok(examples)
}
