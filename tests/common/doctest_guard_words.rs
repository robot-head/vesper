//! The words `@vesper/test-runner` (packages/vesper-test-runner/src/guard.ves)
//! reads and writes for the doctest source guard (self-hosting SH5 lane 14,
//! #1337): the input encoder and result decoder of
//! `vesper::doctest_guard::guard`.
//!
//! Words are little-endian `u64`s; a text is its byte length followed by
//! its bytes padded to whole words, as in tests/common/test_runner_words.rs.
#![allow(dead_code)]

use vesper::doctest_guard::GuardLimits;

/// The `guard` input: the source and the two limits.
pub fn guard_input(source: &str, limits: GuardLimits) -> Vec<u8> {
    let mut w = Vec::new();
    w.extend_from_slice(&(source.len() as u64).to_le_bytes());
    w.extend_from_slice(source.as_bytes());
    while !w.len().is_multiple_of(8) {
        w.push(0);
    }
    w.extend_from_slice(&(limits.max_declarations as u64).to_le_bytes());
    w.extend_from_slice(&(limits.max_nominal_expansion as u64).to_le_bytes());
    w
}

/// The `guard` result: `Ok(())` or the error message.
pub fn decode_guard(words: &[u64]) -> Result<(), String> {
    match words {
        [0] => Ok(()),
        [1, len, rest @ ..] => {
            let len = *len as usize;
            assert_eq!(rest.len(), len.div_ceil(8), "guard message words");
            let mut bytes: Vec<u8> = rest.iter().flat_map(|w| w.to_le_bytes()).collect();
            bytes.truncate(len);
            Err(String::from_utf8(bytes).expect("@vesper/test-runner wrote UTF-8"))
        }
        _ => panic!("malformed guard result {words:?}"),
    }
}
