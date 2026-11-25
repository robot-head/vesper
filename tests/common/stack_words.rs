//! The word streams of the stack-bound swap (self-hosting SH2 lane 10,
//! #1182): `stack_bound::stack_bound_report` from `@vesper/check-loops-atomics`.
//!
//! [`stack_input`] writes one call graph and its policy in the format
//! `read_ctx` and `read_graph` read (an empty world, as the differential of
//! tests/self_host_check_loops_atomics.rs writes it), and
//! [`decode_stack_report`] reads `report_words`' result back into the Rust
//! report. The hybrid build (crates/vesper-hybrid/src/hybrid.rs) and
//! tests/hybrid_stack_bound.rs include this file.
#![allow(dead_code)]

use std::collections::HashMap;
use vesper::check::World;
use vesper::stack_bound::{
    GraphOrigin, StackBoundPolicy, StackBoundReport, StackFinding, StackGraph, StackRootBound,
};

/// Names interned in first-use order (the harness's `Enc::name`), borrowed
/// from the graph and policy. A graph has a few dozen names, so a linear
/// scan beats hashing until the list grows.
#[derive(Default)]
struct Names<'a> {
    index: HashMap<&'a str, u64>,
    list: Vec<&'a str>,
}

impl<'a> Names<'a> {
    const SCAN: usize = 32;

    fn name(&mut self, s: &'a str) -> u64 {
        if self.list.len() <= Self::SCAN {
            if let Some(i) = self.list.iter().position(|n| *n == s) {
                return i as u64;
            }
        } else if let Some(i) = self.index.get(s) {
            return *i;
        }
        let i = self.list.len() as u64;
        self.list.push(s);
        if self.list.len() == Self::SCAN + 1 {
            for (k, n) in self.list.iter().enumerate() {
                self.index.insert(n, k as u64);
            }
        } else if self.list.len() > Self::SCAN + 1 {
            self.index.insert(s, i);
        }
        i
    }
}

/// The input of `stack_bound_words`: the names, no types, an empty world,
/// then the graph and the policy.
pub fn stack_input(g: &StackGraph, p: &StackBoundPolicy) -> Vec<u8> {
    let mut e = Names::default();
    let mut t: Vec<u64> = Vec::with_capacity(
        16 + 3 * g.functions.len()
            + 2 * g.calls.len()
            + 2 * g.indirect.len()
            + 3 * g.external.len()
            + 2 * (p.roots.len() + p.external_bounds.len() + p.indirect_targets.len())
            + p.indirect_targets.values().map(Vec::len).sum::<usize>(),
    );
    t.push(u64::from(g.origin == GraphOrigin::Ebpf));
    t.push(g.functions.len() as u64);
    for f in &g.functions {
        t.extend([u64::from(f.id), e.name(&f.name), u64::from(f.frame_bytes)]);
    }
    t.push(g.calls.len() as u64);
    for (a, b) in &g.calls {
        t.extend([u64::from(*a), u64::from(*b)]);
    }
    t.push(g.indirect.len() as u64);
    for (c, inst) in &g.indirect {
        t.extend([u64::from(*c), inst.map_or(0, |i| u64::from(i) + 1)]);
    }
    t.push(g.external.len() as u64);
    for x in &g.external {
        let s = e.name(&x.symbol);
        t.extend([
            u64::from(x.caller),
            s,
            x.inst.map_or(0, |i| u64::from(i) + 1),
        ]);
    }
    t.push(p.roots.len() as u64);
    for r in &p.roots {
        let n = e.name(&r.function);
        t.extend([n, u64::from(r.budget_bytes)]);
    }
    t.push(p.external_bounds.len() as u64);
    for (s, b) in &p.external_bounds {
        let n = e.name(s);
        t.extend([n, u64::from(*b)]);
    }
    t.push(p.indirect_targets.len() as u64);
    for (c, ts) in &p.indirect_targets {
        let n = e.name(c);
        t.extend([n, ts.len() as u64]);
        for x in ts {
            let n = e.name(x);
            t.push(n);
        }
    }
    // The world section of an empty world (`Enc::world`): fip, ADTs,
    // effects, expression types, partial and overflow spans.
    static FIP: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    let fip = *FIP.get_or_init(|| u64::from(vesper::fip::active(&World::default())));
    let world = [fip, 0, 0, 0, 0, 0];
    let text: usize = e.list.iter().map(|n| 8 + n.len().next_multiple_of(8)).sum();
    let mut out: Vec<u8> = Vec::with_capacity(text + 8 * (t.len() + world.len() + 2));
    let put = |out: &mut Vec<u8>, x: u64| out.extend(x.to_le_bytes());
    put(&mut out, e.list.len() as u64);
    for n in &e.list {
        put(&mut out, n.len() as u64);
        out.extend(n.as_bytes());
        while !out.len().is_multiple_of(8) {
            out.push(0);
        }
    }
    // No types.
    put(&mut out, 0);
    for x in world.iter().chain(&t) {
        put(&mut out, *x);
    }
    out
}

/// A cursor over a result stream; reading past the end yields 0, so a
/// corrupt stream decodes to garbage the differential tests catch, never a
/// panic of the compiler.
struct Words<'a> {
    words: &'a [u64],
    pos: usize,
}

impl Words<'_> {
    fn next(&mut self) -> u64 {
        let x = self.words.get(self.pos).copied().unwrap_or(0);
        self.pos += 1;
        x
    }

    fn count(&mut self) -> usize {
        (self.next() as usize).min(self.words.len().saturating_sub(self.pos))
    }

    fn text(&mut self) -> String {
        let len = (self.next() as usize).min(8 * self.words.len().saturating_sub(self.pos));
        let mut bytes = Vec::with_capacity(len);
        // A truncated stream leaves the cursor past the end, with `len` 0.
        let start = self.pos.min(self.words.len());
        let end = (start + len.div_ceil(8)).min(self.words.len());
        for w in &self.words[start..end] {
            bytes.extend_from_slice(&w.to_le_bytes()[..(len - bytes.len()).min(8)]);
        }
        self.pos = self.pos.max(end);
        String::from_utf8(bytes)
            .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
    }

    fn optional(&mut self) -> Option<u64> {
        self.next().checked_sub(1)
    }
}

/// Reads `report_words`' result (packages/vesper-check-loops-atomics,
/// stack.ves). `origin` is the graph's, which the stream does not repeat.
pub fn decode_stack_report(origin: GraphOrigin, words: &[u64]) -> Result<StackBoundReport, String> {
    let mut r = Words { words, pos: 0 };
    if r.next() != 0 {
        return Err(r.text());
    }
    let n = r.count();
    let mut roots = Vec::with_capacity(n);
    for _ in 0..n {
        let root = r.text();
        let budget_bytes = r.next() as u32;
        let worst_case_bytes = r.optional();
        let within_budget = r.next() != 0;
        let k = r.count();
        let worst_path = (0..k).map(|_| r.text()).collect();
        roots.push(StackRootBound {
            root,
            budget_bytes,
            worst_case_bytes,
            worst_path,
            within_budget,
        });
    }
    let n = r.count();
    let mut findings = Vec::with_capacity(n);
    for _ in 0..n {
        let function = r.text();
        let inst = r.optional().map(|i| i as u32);
        let rule = r.text();
        let message = r.text();
        findings.push(StackFinding {
            function,
            inst,
            rule,
            message,
        });
    }
    Ok(StackBoundReport {
        origin,
        roots,
        findings,
    })
}
