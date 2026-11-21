//! Self-hosting SH2 lane 5 (#1177): `@vesper/ir-check` against
//! `src/ir_check.rs` over the corpus and the census's IR mutants (which make
//! the checker's diagnostics fire; the clean corpus has none).
//!
//! Both run in one pass per case, so Rust validates each case once.
//!
//! - Text: every kept diagnostic as one line (code, span, message, every
//!   related label and the help), Rust against Vesper.
//! - Words: the hybrid build's path. The world and the program are encoded
//!   by `tests/common/check_words.rs`, and the Vesper `ir_check_words`
//!   stream must decode to exactly `ir_check::validate`'s diagnostics.
#[path = "common/check_harness.rs"]
mod check_harness;
#[path = "common/check_words.rs"]
mod check_words;

use check_harness::*;
use std::fmt::Write;
use std::sync::OnceLock;
use vesper::ast::Diagnostic;

fn package() -> &'static Package {
    static P: OnceLock<Package> = OnceLock::new();
    P.get_or_init(|| Package::load("packages/vesper-ir-check"))
}

/// `check_ir_text` of packages/vesper-ir-check/src/lib.ves.
fn diagnostics_text(ds: &[Diagnostic]) -> String {
    let mut o = String::new();
    for d in ds {
        write!(
            o,
            "{} {}-{} {}",
            d.code, d.span.start, d.span.end, d.message
        )
        .unwrap();
        for l in &d.related {
            write!(o, " [{}-{} {}]", l.span.start, l.span.end, l.message).unwrap();
        }
        if let Some(h) = d.help.as_deref().filter(|h| !h.is_empty()) {
            write!(o, " help: {h}").unwrap();
        }
        o.push('\n');
    }
    o
}

/// The hybrid build's input for `c`, or `None` when Rust answers for it
/// (a world with element views, which `@vesper/ir-check` does not have yet).
fn input(c: &IrCase) -> Option<Vec<u8>> {
    check_words::ir_input(c.world, &c.program)
}

/// Every case (each world's IR and `mutants` mutants of it, every `step`-th
/// case): Rust's diagnostics against the Vesper text entry and, when
/// `words` is given, against the hybrid build's decoded word stream.
/// Returns (differing cases, Rust diagnostics, cases).
fn differential_cases(
    text: &Entry,
    words: Option<&Entry>,
    mutants: usize,
    step: usize,
) -> (usize, usize, usize) {
    let (mut bad, mut diags, mut cases, mut rust_only) = (0, 0, 0, 0);
    let start = std::time::Instant::now();
    for c in ir_cases(mutants).iter().step_by(step) {
        let Some(input) = input(c) else {
            rust_only += 1;
            continue;
        };
        let rust = vesper::ir_check::validate(c.world, &c.program);
        diags += rust.len();
        cases += 1;
        let r = diagnostics_text(&rust);
        let mut differs = false;
        if let Some(d) = first_difference(&r, &text.text(&input)) {
            differs = true;
            if bad < 5 {
                eprintln!("ir check text: {} differs at {d}", c.name);
            }
        }
        if let Some(words) = words {
            let decoded = check_words::decode_diagnostics(&words.words(&input));
            if decoded != rust {
                differs = true;
                if bad < 5 {
                    eprintln!(
                        "ir check words: {}:\n  rust:   {r}\n  vesper: {}",
                        c.name,
                        diagnostics_text(&decoded)
                    );
                }
            }
        }
        bad += differs as usize;
    }
    eprintln!(
        "ir check: {bad} of {cases} cases differ ({diags} Rust diagnostics, \
         {rust_only} left to Rust, {:.1} s)",
        start.elapsed().as_secs_f64()
    );
    assert!(
        rust_only * 10 < cases,
        "{rust_only} cases left to Rust against {cases} compared"
    );
    (bad, diags, cases)
}

/// Every `n`-th case (`HYBRID_IR_CHECK_STEP`, default `default`). The Rust
/// check of all 1,710 cases takes about 25 minutes in a debug build, so the
/// default runs a slice of them; `HYBRID_IR_CHECK_STEP=1` runs every case
/// (the report's numbers).
fn step(default: usize) -> usize {
    std::env::var("HYBRID_IR_CHECK_STEP")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(default)
}

/// Text and the hybrid build's word stream, over every world and five
/// mutants of each (every fifth case by default).
#[test]
fn ir_check_matches_rust_with_mutants() {
    let text = package().entry("check_ir_text", true);
    let words = package().entry("check_ir_words", true);
    let (bad, diags, _) = differential_cases(&text, Some(&words), IR_MUTANTS, step(5));
    assert!(
        diags > 1000,
        "the mutants produced only {diags} diagnostics"
    );
    assert_eq!(bad, 0);
}

/// The optimizer off agrees with it on (every 29th case by default, a
/// different slice from the optimized test's).
#[test]
fn ir_check_unoptimized_matches_rust() {
    let text = package().entry("check_ir_text", false);
    let (bad, _, _) = differential_cases(&text, None, IR_MUTANTS, step(29));
    assert_eq!(bad, 0);
}

/// Origin labels past Rust's indexing limits (`Origins::try_build`): every
/// parameter renamed to a name over 1,024 bytes, so Rust keeps no origins
/// for the function and reports its diagnostics without related labels or
/// help. Over the first two mutants of every tenth world.
#[test]
fn ir_check_origin_limits_match_rust() {
    use vesper::ir::Op;
    let words = package().entry("check_ir_words", true);
    let long = |n: &str| format!("{n}_{}", "x".repeat(1100));
    let (mut bad, mut diags, mut unlabeled) = (0, 0, 0);
    for c in ir_cases(2).iter().step_by(10) {
        let mut program = c.program.clone();
        for f in program.functions.values_mut() {
            let names: std::collections::BTreeSet<String> =
                f.params.iter().map(|p| p.name.clone()).collect();
            for p in &mut f.params {
                p.name = long(&p.name);
            }
            for b in &mut f.blocks {
                for i in &mut b.instructions {
                    match &mut i.op {
                        Op::Read { root, .. } if names.contains(root.as_str()) => {
                            *root = long(root)
                        }
                        Op::Write { place, .. } => {
                            let (root, rest) = place
                                .split_once('.')
                                .map_or((place.as_str(), ""), |(a, b)| (a, b));
                            if names.contains(root) {
                                let renamed = long(root);
                                *place = if rest.is_empty() {
                                    renamed
                                } else {
                                    format!("{renamed}.{rest}")
                                };
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        let Some(input) = check_words::ir_input(c.world, &program) else {
            continue;
        };
        let rust = vesper::ir_check::validate(c.world, &program);
        diags += rust.len();
        unlabeled += rust.iter().filter(|d| d.related.is_empty()).count();
        if check_words::decode_diagnostics(&words.words(&input)) != rust {
            bad += 1;
            if bad <= 3 {
                eprintln!("origin limits: {} differs", c.name);
            }
        }
    }
    eprintln!(
        "origin limits: {bad} cases differ ({diags} diagnostics, {unlabeled} without labels)"
    );
    assert!(unlabeled > 0, "no diagnostic lost its labels");
    assert_eq!(bad, 0);
}

/// Element view matches (#1221) are not in `@vesper/ir-check` yet, so the
/// hybrid build leaves their programs to Rust: `ir_input` refuses them, and
/// the Rust check of the valid linear-element program reports nothing.
#[test]
fn element_views_stay_on_rust() {
    let source = "linear struct Ticket { id: u64 }
fn id(in t: Ticket) -> u64 { t.id }
fn stamp(inout t: Ticket) { t.id = wrapping_add(t.id, 10); }
fn body() -> u64 eff [Fail[RuntimeError]] {
    let mut s = ChunkSeq::new[Ticket]();
    s.push(Ticket { id: 1 });
    match s.try_get_mut(0) { Some(t) => stamp(inout t), None => {} }
    let seen = match s.try_get(0) { Some(t) => id(in t), None => 0 };
    let back = match s.pop() { Some(Ticket { id }) => id, None => 0 };
    ChunkSeq::finish(sink s);
    wrapping_add(wrapping_mul(seen, 100), back)
}
fn main() -> u64 { handle Fail[RuntimeError] in { body() } with { return x => x, raise(e, k) => 999, } }
";
    let parsed = vesper::parser::parse(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let checked = vesper::check::check(&parsed.module);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    let w = &checked.world;
    assert!(!w.element_views.is_empty(), "no element views recorded");
    let program = vesper::ir::lower_world(w);
    assert!(check_words::ir_input(w, &program).is_none());
    assert_eq!(vesper::ir_check::validate(w, &program), vec![]);
}

/// Lent closures and lent function parameters (#1261): calling a lent
/// `fn` parameter reads it, and a lent closure borrows its captures. The
/// programs of tests/closure_modes.rs that use them check clean in Rust and
/// in Vesper (`lent_parameters_pass_on` and a lent closure literal).
#[test]
fn lent_functions_match_rust() {
    let state = "struct State { slots: ChunkSeq[u32], n: u64 }
fn fresh(n: u64) -> State {
    let mut s = State { slots: ChunkSeq::new[u32](), n: n };
    let mut i: u64 = 0;
    loop {
        if i >= n % 4 {
            break;
        }
        s.slots.push(wrapping_u32(i));
        i = wrapping_add(i, 1);
    }
    s
}
";
    let words = package().entry("check_ir_words", true);
    for body in [
        "fn apply(f: fn(u64) -> u64) -> u64 { f(1) }
fn twice(f: fn(u64) -> u64) -> u64 { wrapping_add(apply(f), apply(|x: u64| f(f(x)))) }
fn main() -> u64 {
    let s = fresh(5);
    wrapping_add(twice(|x: u64| wrapping_add(x, s.slots.len())), twice(|x: u64| wrapping_add(x, s.n)))
}",
        "fn apply(f: fn(u64) -> u64) -> u64 { wrapping_add(f(1), f(2)) }
fn main() -> u64 { let s = fresh(3); apply(|x: u64| wrapping_add(x, s.n)) }",
    ] {
        let source = format!("{state}{body}");
        let parsed = vesper::parser::parse(&source);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = vesper::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        let w = &checked.world;
        let program = vesper::ir::lower_world(w);
        let rust = vesper::ir_check::validate(w, &program);
        assert_eq!(rust, vec![], "{body}");
        let input = check_words::ir_input(w, &program).expect("encodable");
        assert_eq!(
            check_words::decode_diagnostics(&words.words(&input)),
            rust,
            "{body}"
        );
    }
}

/// ABBA: `validate` in Rust against the Vesper check with its input
/// encoding and result decoding (the hybrid build's cost per call).
#[test]
#[ignore = "timing; CHECK_HARNESS_SPEED=1"]
fn ir_check_speed() {
    if !speed() {
        return;
    }
    let f = package().entry("check_ir_words", true);
    let cases = ir_cases(IR_MUTANTS);
    let cases: Vec<_> = cases.into_iter().filter(|c| input(c).is_some()).collect();
    let inputs: Vec<Vec<u8>> = cases.iter().filter_map(input).collect();
    abba(
        "ir check per call (encode + Vesper + decode)",
        &mut || {
            time(|| {
                for c in &cases {
                    std::hint::black_box(vesper::ir_check::validate(c.world, &c.program));
                }
            })
        },
        &mut || {
            time(|| {
                for c in &cases {
                    std::hint::black_box(check_words::decode_diagnostics(
                        &f.words(&input(c).unwrap()),
                    ));
                }
            })
        },
    );
    abba(
        "ir check, Vesper alone (input encoded beforehand)",
        &mut || {
            time(|| {
                for c in &cases {
                    std::hint::black_box(vesper::ir_check::validate(c.world, &c.program));
                }
            })
        },
        &mut || {
            time(|| {
                for i in &inputs {
                    std::hint::black_box(f.words(i));
                }
            })
        },
    );
}
