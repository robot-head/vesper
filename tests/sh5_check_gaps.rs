//! Self-hosting SH5 lane 6 (#1329): the checker and JIT gaps the SH4 lanes
//! reported, each with its repro. Every repro checks, and the evaluator and
//! both JIT builds return the same value.
//!
//! - `Err(e)?` in a `Fail` function (SH4 lane 15, #1311): the JIT refused it
//!   with "native layout is not yet defined for _".
//! - A handler clause capturing a plain `u32` parameter (lane 15): E0404.
//! - `ChunkSeq::last` and `Slice::last` (lane 16, #1308): E1525.
//! - `p.as_bytes().len()` on a `Str` parameter (lane 12, #1299): E1531.
//! - Debug native lowering on a 2 MB stack (lane 17, #1319): five corpus
//!   programs overflowed it inside inlined effect specialization.
use vesper::{check, native, parser, runtime::Runtime};

fn accepted(source: &str) -> check::Checked {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{source}\n{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{source}\n{:?}", c.diagnostics);
    c
}

/// The evaluator and the optimized and unoptimized JIT all return
/// `expected` from `main`.
fn agree(source: &str, expected: &str) {
    let c = accepted(source);
    let eval = Runtime::new(&c.world).run("main", vec![]).unwrap();
    assert_eq!(eval.to_string(), expected, "evaluator");
    for optimize in [true, false] {
        let jit = native::jit_with_options(&c.world, "main", optimize).unwrap();
        assert_eq!(jit.to_string(), expected, "JIT optimize={optimize}");
    }
}

#[test]
fn err_try_in_a_fail_function() {
    agree(
        "copy struct E { k: u8 }
fn f(x: u64) -> u64 eff [Fail[E]] {
    if x == 0 {
        Err(E { k: 1 })?;
    }
    x
}
fn main() -> u64 {
    let a = handle Fail[E] in { f(3) } with {
        return v => v,
        raise(e, resume) => 99,
    };
    let b = handle Fail[E] in { f(0) } with {
        return v => v,
        raise(e, resume) => wrapping_add(90, widen_u64(e.k)),
    };
    wrapping_add(wrapping_mul(a, 100), b)
}",
        "391",
    );
}

/// `let x = Err(e)?;` leaves nothing to infer the binding's type from
/// either: it is `Unit`.
#[test]
fn err_try_bound_to_a_local() {
    agree(
        "copy struct E { k: u8 }
fn f(x: u64) -> u64 eff [Fail[E]] {
    if x == 0 {
        let u = Err(E { k: 7 })?;
        u
    }
    x
}
fn main() -> u64 {
    handle Fail[E] in { f(0) } with {
        return v => v,
        raise(e, resume) => 5,
    }
}",
        "5",
    );
}

#[test]
fn handler_clause_captures_an_in_copy_parameter() {
    agree(
        "copy struct E { k: u8 }
fn g(t: u32) -> u32 eff [Fail[E]] { if t == 0 { Fail::raise(E { k: 0 }); } t }
fn f(t: u32, d: u32) -> u32 {
    handle Fail[E] in { g(t) } with {
        return v => wrapping_add(v, d),
        raise(e, resume) => wrapping_add(t, d),
    }
}
fn main() -> u32 { wrapping_add(wrapping_mul(f(0, 4), 100), f(5, 1)) }",
        "406",
    );
}

/// An `inout` parameter is still not a capture: a clause would read a
/// snapshot of a place the body may change.
#[test]
fn handler_clause_still_refuses_an_inout_capture() {
    let p = parser::parse(
        "copy struct E { k: u8 }
fn g(t: u32) -> u32 eff [Fail[E]] { if t == 0 { Fail::raise(E { k: 0 }); } t }
fn f(inout t: u32) -> u32 {
    handle Fail[E] in { g(t) } with {
        return v => v,
        raise(e, resume) => t,
    }
}
fn main() -> u32 { let mut x: u32 = 1; f(inout x) }",
    );
    let c = check::check(&p.module);
    assert!(
        c.diagnostics.iter().any(|d| d.code == "E0404"),
        "{:?}",
        c.diagnostics
    );
}

#[test]
fn chunk_seq_and_slice_last() {
    agree(
        "fn top(in s: ChunkSeq[u64]) -> u64 {
    match s.last() {
        Option::Some(x) => x,
        Option::None => 0,
    }
}
fn tail(in s: Slice[u64]) -> u64 {
    match s.last() {
        Option::Some(x) => x,
        Option::None => 1,
    }
}
fn main() -> u64 {
    let mut s: ChunkSeq[u64] = ChunkSeq::new();
    let a = top(in s);
    let b = tail(in s.as_slice());
    s.push(4);
    s.push(9);
    let c = top(in s);
    let d = tail(in s.as_slice());
    let e = tail(in s[0..1]);
    let f = tail(in s[1..1]);
    let n = s.len();
    wrapping_add(wrapping_add(wrapping_add(a, wrapping_mul(b, 10)), wrapping_mul(c, 100)),
        wrapping_add(wrapping_add(wrapping_mul(d, 1000), wrapping_mul(e, 10000)),
            wrapping_add(wrapping_mul(f, 100000), wrapping_mul(n, 1000000))))
}",
        "2149910",
    );
}

/// `last` copies its element out, so a non-copy element is E1522, as for
/// `try_get`.
#[test]
fn last_refuses_non_copy_elements() {
    let p = parser::parse(
        "fn f(in s: ChunkSeq[String]) -> u64 {
    match s.last() {
        Option::Some(x) => 1,
        Option::None => 0,
    }
}
fn main() -> u64 { 0 }",
    );
    let c = check::check(&p.module);
    assert!(
        c.diagnostics.iter().any(|d| d.code == "E1522"),
        "{:?}",
        c.diagnostics
    );
}

#[test]
fn str_as_bytes_len() {
    agree(
        "fn n(in p: Str) -> u64 { p.as_bytes().len() }
fn m(in s: String) -> u64 { s.as_bytes().len() }
fn main() -> u64 {
    let mut s = String::with_capacity(12);
    s.push_str(\"hello, world\");
    wrapping_add(wrapping_mul(n(in \"hello\"), 100), m(in s))
}",
        "512",
    );
}

/// Corpus programs whose debug lowering overflowed a 2 MB stack: each
/// lowers (optimized and not) on a thread of exactly that size.
#[test]
fn deep_specialization_lowers_on_a_2_mib_stack() {
    for (file, source) in [
        (
            "native_handlers/abort_through_handlers.ves",
            include_str!("fixtures/native_handlers/abort_through_handlers.ves"),
        ),
        (
            "native_handlers/deep_recursion.ves",
            include_str!("fixtures/native_handlers/deep_recursion.ves"),
        ),
        (
            "native_handlers/unresumed.ves",
            include_str!("fixtures/native_handlers/unresumed.ves"),
        ),
        (
            "verified_codegen/region_exhausted.ves",
            include_str!("fixtures/verified_codegen/region_exhausted.ves"),
        ),
        (
            "verified_codegen/regions.ves",
            include_str!("fixtures/verified_codegen/regions.ves"),
        ),
    ] {
        let world = std::sync::Arc::new(accepted(source).world);
        let lower = |stack: usize| {
            let world = world.clone();
            std::thread::Builder::new()
                .stack_size(stack)
                .spawn(move || {
                    [false, true].map(|optimize| {
                        vesper::native_verifiers::emit_and_collect(&world, "main", None, optimize)
                            .map(|(bytes, _, _)| bytes)
                    })
                })
                .unwrap()
                .join()
                .unwrap_or_else(|_| panic!("{file}"))
        };
        // The same objects, or the same refusal (some of these programs
        // pass the 64-level specialization limit), as on a 256 MiB stack.
        assert_eq!(lower(2 << 20), lower(256 << 20), "{file}");
    }
}
