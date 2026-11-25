//! The words `@vesper/test-runner` (packages/vesper-test-runner/src/mutants.ves)
//! reads and writes for the mutation sites of `vesper test --mutate`
//! (self-hosting SH5 lane 14, #1337): the input encoder and result decoder
//! of `vesper::test_mutants::collect`. Shared by
//! tests/self_host_test_mutants.rs and the hybrid build.
//!
//! Words are little-endian `u64`s; a text is its byte length followed by
//! its bytes padded to whole words, as in tests/common/test_runner_words.rs.
#![allow(dead_code)]

use std::collections::BTreeSet;
use vesper::test_mutants::MutantSite;

/// The `collect` input: the source and the test names.
pub fn collect_input(source: &str, test_names: &BTreeSet<&str>) -> Vec<u8> {
    let mut w = Vec::new();
    let text = |w: &mut Vec<u8>, s: &str| {
        w.extend_from_slice(&(s.len() as u64).to_le_bytes());
        w.extend_from_slice(s.as_bytes());
        while !w.len().is_multiple_of(8) {
            w.push(0);
        }
    };
    text(&mut w, source);
    w.extend_from_slice(&(test_names.len() as u64).to_le_bytes());
    for name in test_names {
        text(&mut w, name);
    }
    w
}

/// The `collect` result.
pub fn decode_collect(words: &[u64]) -> Vec<MutantSite> {
    let mut pos = 0;
    let mut word = || {
        let w = words[pos];
        pos += 1;
        w
    };
    assert_eq!(word(), 0, "mutants status");
    let n = word() as usize;
    let text = |word: &mut dyn FnMut() -> u64| {
        let len = word() as usize;
        let mut bytes = Vec::with_capacity(len + 8);
        for _ in 0..len.div_ceil(8) {
            bytes.extend_from_slice(&word().to_le_bytes());
        }
        bytes.truncate(len);
        String::from_utf8(bytes).expect("@vesper/test-runner wrote UTF-8")
    };
    let mut sites = Vec::with_capacity(n);
    for _ in 0..n {
        let function = text(&mut word);
        let start = word() as usize;
        let end = word() as usize;
        let original = text(&mut word);
        let replacement = text(&mut word);
        sites.push(MutantSite {
            function,
            start,
            end,
            original,
            replacement,
        });
    }
    assert_eq!(pos, words.len(), "trailing mutants words");
    sites
}
