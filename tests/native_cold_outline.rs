//! Tests for `native_cold_outline`: outlining cold traps/branches,
//! asymmetric fast/slow tails, and loop early-exits into cold helpers, plus
//! compatibility with Stage-5 capability verifiers.
use std::collections::BTreeSet;
use vesper::{
    check, native,
    native_verifiers::{self, NativeVerifier, NativeVerifierPolicy},
    parser, runtime,
};

fn check_world(source: &str) -> vesper::check::World {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:#?}", c.diagnostics);
    c.world
}

fn clif_function<'a>(clif: &'a str, name: &str) -> &'a str {
    let marker = format!("; {name}\n");
    let start = clif
        .find(&marker)
        .unwrap_or_else(|| panic!("missing '{name}' in CLIF:\n{clif}"));
    let rest = &clif[start + marker.len()..];
    match rest.find("\n; ") {
        Some(end) => &rest[..end],
        None => rest,
    }
}

#[test]
fn asymmetric_tail_and_loop_exit_outline_cold_helpers() {
    let source = "struct Pair { a: u64, b: u64, c: u64, d: u64 }

fn slow_fallback(x: u64, y: u64, z: u64) -> u64 {
    let mut p = Pair { a: x, b: y, c: z, d: wrapping_add(x, y) };
    let mut acc: u64 = 0;
    for i in 0..8 {
        acc = wrapping_add(acc, wrapping_add(p.a, p.b));
        p.a = wrapping_add(p.a, i);
        p.c = wrapping_add(p.c, p.d);
    }
    wrapping_add(acc, p.c)
}

fn asym_dispatch(x: u64, y: u64, z: u64) -> u64 {
    if x < 100 {
        wrapping_add(wrapping_mul(x, 3), y)
    } else {
        let a = slow_fallback(x, y, z);
        let b = slow_fallback(y, z, x);
        wrapping_add(a, b)
    }
}

fn loop_early_exit(n: u64, x: u64, y: u64) -> u64 {
    let mut acc: u64 = 0;
    for i in 0..n {
        if wrapping_add(i, x) > 50 {
            let mut tmp: u64 = 0;
            for j in 0..4 {
                tmp = wrapping_add(tmp, slow_fallback(i, j, y));
            }
            return tmp;
        }
        acc = wrapping_add(acc, wrapping_add(i, y));
    }
    acc
}

fn main() -> u64 {
    let mut sum: u64 = 0;
    for i in 0..120 {
        if (i & 31) == 31 {
            sum = wrapping_add(sum, asym_dispatch(i, 7, 11));
        } else {
            sum = wrapping_add(sum, asym_dispatch(i % 90, 7, 11));
        }
    }
    sum = wrapping_add(sum, loop_early_exit(20, 5, 9));
    sum = wrapping_add(sum, loop_early_exit(80, 5, 9));
    sum
}";
    let world = check_world(source);
    let eval = runtime::Runtime::new(&world).run("main", vec![]).unwrap();
    let jit_opt = native::jit_with_options(&world, "main", true).unwrap();
    let jit_noopt = native::jit_with_options(&world, "main", false).unwrap();
    assert_eq!(eval.to_string(), jit_opt.to_string());
    assert_eq!(eval.to_string(), jit_noopt.to_string());

    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        let (_, clif) = native::object_with_options(&world, "main", Some(target), true).unwrap();
        assert!(
            clif.contains("; asym_dispatch::cold#0\n"),
            "{target}: expected asym_dispatch::cold#0 to be outlined:\n{clif}"
        );
        assert!(
            clif.contains("; loop_early_exit::cold#0\n"),
            "{target}: expected loop_early_exit::cold#0 to be outlined:\n{clif}"
        );
        // Verify that the fast/loop path no longer hosts the two direct calls inside `asym_dispatch`
        // (only the cold stub calls `cold#0`).
        let asym_body = clif_function(&clif, "asym_dispatch");
        assert_eq!(
            asym_body.matches(" call ").count(),
            1,
            "{target}: expected only 1 cold call in asym_dispatch:\n{asym_body}"
        );
    }
}

#[test]
fn verifiers_accept_outlined_detfloat_realtime_and_stack_functions() {
    let source = "fn det_cold_helper(x: f64, y: f64) -> f64 eff [DetFloat] {
    let mut acc = x;
    let mut f = 0.0;
    for i in 0..6 {
        f = f + 0.25;
        acc = (acc + f) * (y + 1.0);
    }
    acc
}

fn det_entry(x: f64) -> f64 eff [DetFloat] {
    if x < 1.0 {
        x * 0.5 + 1.0
    } else {
        let a = det_cold_helper(x, x + 1.0);
        let b = det_cold_helper(a, x + 2.0);
        a + b
    }
}

fn rt_cold(x: u64) -> u64 eff [Realtime] {
    let a = wrapping_mul(x, 31);
    let b = wrapping_add(a, 17);
    let c = wrapping_mul(b, x);
    let d = wrapping_add(c, 101);
    let e = wrapping_mul(d, 7);
    let f = wrapping_sub(e, a);
    let g = wrapping_mul(f, b);
    let h = wrapping_add(g, c);
    let i = wrapping_mul(h, 13);
    let j = wrapping_add(i, d);
    let k = wrapping_mul(j, e);
    wrapping_add(k, f)
}

fn rt_entry(x: u64) -> u64 eff [Realtime] {
    if x < 10 {
        wrapping_add(x, 1)
    } else {
        let a = rt_cold(x);
        let b = rt_cold(a);
        wrapping_add(a, b)
    }
}

fn main() -> u64 {
    let d = det_entry(0.5) + det_entry(2.5);
    let r = wrapping_add(rt_entry(3), rt_entry(30));
    if d > 1.0 { r } else { wrapping_add(r, 1) }
}";
    let world = check_world(source);
    let verifiers = BTreeSet::from([
        NativeVerifier::DetFloat,
        NativeVerifier::Realtime,
        NativeVerifier::Stack,
    ]);
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        let (_bytes, clif, report) = native_verifiers::verify_native_with_capabilities(
            &world,
            "main",
            Some(target),
            true,
            &verifiers,
            &NativeVerifierPolicy::default(),
        )
        .unwrap();
        assert!(
            report.accepted(),
            "{target}: verifier rejected outlined capability functions: {:?}",
            report.findings
        );
        assert!(
            clif.contains("::cold#0\n"),
            "{target}: expected cold outlining in detfloat/realtime test:\n{clif}"
        );
    }
}

#[test]
fn fail_guard_does_not_invert_hot_tail() {
    let source = "fn step(x: u64, y: u64) -> u64 {
    wrapping_add(wrapping_mul(x, 3), y)
}

fn checked_hot_body(n: u64) -> u64 eff [Fail[ArithOverflow]] {
    let m = n - 1;
    let mut acc: u64 = 0;
    for i in 0..m {
        acc = wrapping_add(acc, step(i, m));
    }
    acc
}

fn main() -> u64 {
    handle Fail[ArithOverflow] in {
        checked_hot_body(10)
    } with {
        return x => x,
        raise(_, _) => 0,
    }
}";
    let world = check_world(source);
    let eval = runtime::Runtime::new(&world).run("main", vec![]).unwrap();
    let jit_opt = native::jit_with_options(&world, "main", true).unwrap();
    assert_eq!(eval.to_string(), jit_opt.to_string());
    let (_, clif) = native::object_with_options(&world, "main", None, true).unwrap();
    assert!(
        !clif.contains("checked_hot_body::cold#"),
        "Fail error-return guard must not outline the hot body as ::cold#0:\n{clif}"
    );
}
