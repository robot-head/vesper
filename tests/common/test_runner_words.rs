//! The words `@vesper/test-runner` (packages/vesper-test-runner) reads and
//! writes for test discovery (self-hosting SH5 lane 14, #1337): the input
//! encoders and result decoders of `vesper::test_discovery`'s two swapped
//! functions. Shared by the hybrid's `test_runner` module (crates/vesper-hybrid/src/hybrid.rs) and
//! the root package's tests/self_host_test_runner.rs.
//!
//! Words are little-endian `u64`s. A text is its byte length followed by
//! its bytes, padded with zeros to whole words (util.ves `Reader::text` and
//! `put_text`).
#![allow(dead_code)]

use vesper::ast::{Item, Module, Span};
use vesper::test_attrs::{AttrValue, TestCaseArg, TestCaseAttr as RawCase};
use vesper::test_discovery::{CaseRow, CaseRows, Discovered};
use vesper_test::{BudgetGate, ExpectErrorSpec, FaultProfile, HarnessPlan, TestCaseAttr, TestMode};

/// The word format version (`format()` in lib.ves).
pub const FORMAT: u64 = 1;

/// An input being written.
#[derive(Default)]
pub struct Words(Vec<u8>);

impl Words {
    pub fn word(&mut self, w: u64) {
        self.0.extend_from_slice(&w.to_le_bytes());
    }

    pub fn text(&mut self, s: &str) {
        self.word(s.len() as u64);
        self.0.extend_from_slice(s.as_bytes());
        while !self.0.len().is_multiple_of(8) {
            self.0.push(0);
        }
    }

    pub fn finish(self) -> Vec<u8> {
        self.0
    }
}

/// The `case_rows` input of `source` and its parse.
pub fn case_rows_input(source: &str, module: &Module) -> Vec<u8> {
    let mut w = Words::default();
    w.text(source);
    let mut names: Vec<&str> = module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Function(f) => Some(f.name.as_str()),
            _ => None,
        })
        .collect();
    names.extend(
        module
            .uses
            .iter()
            .flat_map(|import| import.names.iter().map(|name| name.local())),
    );
    w.word(names.len() as u64);
    for name in names {
        w.text(name);
    }
    let functions: Vec<_> = module
        .items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| match item {
            Item::Function(f) => Some((i, f)),
            _ => None,
        })
        .collect();
    w.word(functions.len() as u64);
    for (i, f) in functions {
        w.word(i as u64);
        w.word(f.span.start as u64);
        w.word(!f.generics.is_empty() as u64);
        w.word(f.body.is_some() as u64);
        w.text(&f.name);
    }
    w.finish()
}

/// The `discover` input.
pub fn discover_input(source: &str, items: &[Item], cases: &[CaseRow]) -> Vec<u8> {
    let mut w = Words::default();
    w.text(source);
    w.word(items.len() as u64);
    for item in items {
        let Item::Function(f) = item else {
            w.word(0);
            continue;
        };
        w.word(1);
        w.text(&f.name);
        w.word(f.span.start as u64);
        w.word(f.span.end as u64);
        w.word(!f.generics.is_empty() as u64);
        w.word(f.body.is_some() as u64);
        w.word(f.params.len() as u64);
        for p in &f.params {
            w.text(&p.name);
            w.text(&p.ty.to_string());
        }
        w.word(f.effects.instances.len() as u64);
        for e in &f.effects.instances {
            w.text(&e.name);
        }
    }
    w.word(cases.len() as u64);
    for c in cases {
        w.text(&c.entry);
        w.text(&c.name);
        w.word(c.span.start as u64);
        w.word(c.span.end as u64);
        w.word(c.function_index as u64);
    }
    w.finish()
}

/// A result being read.
pub struct Reader<'a> {
    words: &'a [u64],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(words: &'a [u64]) -> Self {
        Self { words, pos: 0 }
    }

    pub fn word(&mut self) -> u64 {
        let w = self.words[self.pos];
        self.pos += 1;
        w
    }

    fn usize(&mut self) -> usize {
        self.word() as usize
    }

    fn flag(&mut self) -> bool {
        self.word() != 0
    }

    pub fn text(&mut self) -> String {
        let len = self.usize();
        let mut bytes = Vec::with_capacity(len + 8);
        for _ in 0..len.div_ceil(8) {
            bytes.extend_from_slice(&self.word().to_le_bytes());
        }
        bytes.truncate(len);
        String::from_utf8(bytes).expect("@vesper/test-runner wrote UTF-8")
    }

    fn span(&mut self) -> Span {
        let start = self.usize();
        start..self.usize()
    }

    fn opt(&mut self) -> Option<u64> {
        let has = self.flag();
        let v = self.word();
        has.then_some(v)
    }

    /// The error of a failed call, or `None` after an ok status.
    fn status(&mut self) -> Option<String> {
        (self.word() != 0).then(|| self.text())
    }

    pub fn done(&self) -> bool {
        self.pos == self.words.len()
    }
}

/// The `case_rows` result: the rows and the source with every wrapper
/// appended.
pub fn decode_case_rows(source: &str, words: &[u64]) -> Result<CaseRows, String> {
    let mut r = Reader::new(words);
    if let Some(e) = r.status() {
        return Err(e);
    }
    let n = r.usize();
    let mut rows = Vec::with_capacity(n);
    for _ in 0..n {
        let name = r.text();
        let span = r.span();
        let entry = r.text();
        let generated = r.span();
        let function_index = r.usize();
        rows.push(CaseRow {
            name,
            span,
            entry,
            generated,
            function_index,
        });
    }
    let expanded = r.flag();
    let suffix = r.text();
    assert!(r.done(), "trailing case_rows words");
    Ok(CaseRows {
        rows,
        expanded: expanded.then(|| format!("{source}{suffix}")),
    })
}

fn mode(w: u64) -> TestMode {
    match w {
        0 => TestMode::Unit,
        1 => TestMode::Property,
        2 => TestMode::SimPulse,
        3 => TestMode::BudgetGate,
        4 => TestMode::ExpectError,
        _ => panic!("test mode {w}"),
    }
}

fn plan(r: &mut Reader) -> HarnessPlan {
    let name = r.text();
    let primary_mode = mode(r.word());
    let has_property_params = r.flag();
    let uses_simpulse = r.flag();
    let max_stack = r.opt();
    let max_cycles = r.opt();
    let budget = BudgetGate {
        max_stack,
        max_cycles,
        zero_alloc: r.flag(),
        const_time: r.flag(),
        realtime: r.flag(),
    };
    let expect_error = r.flag().then(|| {
        let code = r.text();
        let primary = r.flag().then(|| r.text());
        let related = r.flag().then(|| r.text());
        let message = r.flag().then(|| r.text());
        ExpectErrorSpec {
            code,
            primary,
            related,
            message,
        }
    });
    let property_cases = r.word() as u32;
    let sim_schedules = r.word() as u32;
    let fault_profile = match r.word() {
        0 => None,
        1 => Some(FaultProfile::TornWalSector),
        2 => Some(FaultProfile::NetPartition),
        3 => Some(FaultProfile::ClockSkew),
        _ => Some(FaultProfile::Custom(r.text())),
    };
    let seed = r.word();
    let pins = r.usize();
    let pinned_cases = (0..pins)
        .map(|_| {
            let n = r.usize();
            TestCaseAttr {
                args: (0..n).map(|_| r.text()).collect(),
                label: None,
            }
        })
        .collect();
    let fuel_limit = (r.word() != 0).then(|| r.word());
    let timeout_ms = (r.word() != 0).then(|| r.word());
    let ignore = r.flag();
    HarnessPlan {
        name,
        primary_mode,
        has_property_params,
        uses_simpulse,
        budget,
        expect_error,
        property_cases,
        sim_schedules,
        fault_profile,
        seed,
        pinned_cases,
        fuel_limit,
        timeout_ms,
        ignore,
        uses_nondeterministic_ffi: r.flag(),
        uses_ambient_host_io: r.flag(),
    }
}

fn raw_cases(r: &mut Reader) -> Vec<RawCase> {
    let n = r.usize();
    (0..n)
        .map(|_| {
            let span = r.span();
            let args = r.usize();
            let args = (0..args)
                .map(|_| {
                    let name = r.flag().then(|| r.text());
                    let span = r.span();
                    let value = match r.word() {
                        0 => AttrValue::Ident(r.text()),
                        1 => AttrValue::String(r.text()),
                        2 => {
                            let neg = r.flag();
                            let magnitude = ((r.word() as u128) << 64) | r.word() as u128;
                            let v = magnitude as i128;
                            AttrValue::Int(if neg { -v } else { v })
                        }
                        3 => AttrValue::Float(r.text()),
                        4 => AttrValue::Bool(r.flag()),
                        _ => AttrValue::Raw(r.text()),
                    };
                    TestCaseArg { name, value, span }
                })
                .collect();
            RawCase { args, span }
        })
        .collect()
}

/// The `discover` result.
pub fn decode_discover(words: &[u64]) -> Result<Vec<Discovered>, String> {
    let mut r = Reader::new(words);
    if let Some(e) = r.status() {
        return Err(e);
    }
    let n = r.usize();
    let mut tests = Vec::with_capacity(n);
    for _ in 0..n {
        let index = r.usize();
        let name = r.text();
        let span = r.span();
        let plan = plan(&mut r);
        let raw_cases = raw_cases(&mut r);
        tests.push(Discovered {
            index,
            name,
            span,
            plan,
            raw_cases,
        });
    }
    assert!(r.done(), "trailing discover words");
    Ok(tests)
}
