//! The words `@vesper/test-runner` (packages/vesper-test-runner/src/graph.ves)
//! reads and writes for the Anvil function graph (self-hosting SH5 lane 14,
//! #1337): the input encoder and result decoder of
//! `vesper::anvil_hash::FunctionGraph::from_module` over a source.
//!
//! Words are little-endian `u64`s; a text is its byte length followed by
//! its bytes padded to whole words, as in tests/common/test_runner_words.rs.
#![allow(dead_code)]

use std::collections::BTreeSet;
use vesper::anvil_hash::FunctionGraph;

/// The `graph` input: the source.
pub fn graph_input(source: &str) -> Vec<u8> {
    let mut w = Vec::new();
    w.extend_from_slice(&(source.len() as u64).to_le_bytes());
    w.extend_from_slice(source.as_bytes());
    while !w.len().is_multiple_of(8) {
        w.push(0);
    }
    w
}

/// The `graph` result, built into a [`FunctionGraph`] with the map
/// semantics of `FunctionGraph::from_module`: a later function of the same
/// key replaces the digest and the callees, and callers gather every edge.
pub fn decode_graph(words: &[u64]) -> FunctionGraph {
    let mut pos = 0;
    let mut word = || {
        let w = words[pos];
        pos += 1;
        w
    };
    assert_eq!(word(), 0, "graph status");
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
    let mut entries = Vec::with_capacity(n);
    for _ in 0..n {
        let key = text(&mut word);
        let hex = text(&mut word);
        let m = word() as usize;
        let callees: BTreeSet<String> = (0..m).map(|_| text(&mut word)).collect();
        entries.push((key, hex, callees));
    }
    assert_eq!(pos, words.len(), "trailing graph words");
    let mut graph = FunctionGraph::default();
    for (key, hex, _) in &entries {
        graph.fn_blake3.insert(key.clone(), hex.clone());
        graph.callees.entry(key.clone()).or_default();
        graph.callers.entry(key.clone()).or_default();
    }
    for (key, _, callees) in entries {
        for callee in &callees {
            graph
                .callers
                .entry(callee.clone())
                .or_default()
                .insert(key.clone());
        }
        graph.callees.insert(key, callees);
    }
    graph
}
