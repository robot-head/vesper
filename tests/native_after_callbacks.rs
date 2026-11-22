//! Native code after lent callbacks (self-hosting SH5 lane 7, #1330).
//!
//! SH4 lane 4 (#1278) left three gaps between a `with_ref` lookup and the
//! hand-written element view:
//!
//! 1. The `Option[R]` a lookup returns was built in a stack slot in each
//!    arm, joined by address, copied by the caller's `let` and loaded back
//!    by the next `match`. Small enums whose fields are integer or `bool`
//!    words now stay words (`native_aggregates::scalar_enums`): the tag and
//!    the fields are scalar values, block parameters and locals, and a
//!    specialized callee passes them to its caller's join directly.
//! 2. The receiver's header words were read again on every call: a
//!    specialized callee's graph reads `self.vals` of an `in self` that is
//!    the caller's `in` parameter, which nothing can change during the
//!    call. Those words are now loaded as read-only memory that Cranelift
//!    moves out of the caller's loop, and `stable_views` no longer gives up
//!    on `in` fields because a graph contains a closure, an indirect call
//!    or a method call.
//! 3. Scalars lent `in` to real impure calls kept a home slot; a cost rule
//!    now keeps them in registers when their other uses outweigh the loans.
//!
//! The shape tests check the emitted Cranelift functions; the differential
//! tests check that the evaluator, both JIT configurations (and that the
//! JIT frees every sequence once), x86-64 and AArch64 objects and an
//! executable agree.
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::{check, native, parser, runtime::Runtime, seq::native_runtime};

static NEXT: AtomicU64 = AtomicU64::new(0);

fn accepted(source: &str) -> check::Checked {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:#?}", c.diagnostics);
    c
}

/// Each emitted function named `function`: its stack slot sizes and its
/// Cranelift text.
fn emitted(source: &str, function: &str, optimize: bool) -> Vec<(Vec<u32>, String)> {
    let c = accepted(source);
    let mut out = vec![];
    native::object_with_observer(
        &c.world,
        "main",
        None,
        optimize,
        Some(&mut |f: native::EmittedFunction| {
            if f.source == function {
                out.push((
                    f.function
                        .sized_stack_slots
                        .values()
                        .map(|s| s.size)
                        .collect(),
                    f.function.display().to_string(),
                ));
            }
        }),
    )
    .unwrap();
    assert!(!out.is_empty(), "{function} was not emitted");
    out
}

fn links_executables() -> bool {
    cfg!(unix)
        && Command::new("cc")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
}

/// Runs `main` in the evaluator, both JIT configurations, x86-64 and
/// AArch64 objects and (where a C driver links) an executable.
fn differential(name: &str, source: &str) -> u64 {
    let c = accepted(source);
    let evaluated = Runtime::new(&c.world)
        .run("main", vec![])
        .unwrap_or_else(|e| panic!("{name}: evaluator failed: {e}"))
        .to_string();
    for optimize in [true, false] {
        let stray = native_runtime::stray_frees();
        let jit = native::jit_with_options(&c.world, "main", optimize)
            .unwrap_or_else(|e| panic!("{name}: JIT (optimize {optimize}) failed: {e}"))
            .to_string();
        assert_eq!(jit, evaluated, "{name}: JIT (optimize {optimize})");
        assert_eq!(
            native_runtime::stray_frees(),
            stray,
            "{name}: JIT (optimize {optimize}) freed a sequence twice"
        );
    }
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        let (obj, _) = native::object_with_options(&c.world, "main", Some(target), true)
            .unwrap_or_else(|e| panic!("{name}: {target}: {e}"));
        assert_eq!(&obj[..4], b"\x7fELF");
    }
    let expected: u64 = evaluated.parse().unwrap();
    if links_executables() {
        let dir = std::env::temp_dir().join(format!(
            "vesper-after-callbacks-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join(format!("{name}.ves"));
        fs::write(&file, source).unwrap();
        let exe: PathBuf = dir.join(name);
        let built = Command::new(env!("CARGO_BIN_EXE_vesper"))
            .arg("build")
            .arg(&file)
            .arg("-o")
            .arg(&exe)
            .output()
            .unwrap();
        assert!(
            built.status.success(),
            "{name}: build failed: {}",
            String::from_utf8_lossy(&built.stderr)
        );
        let run = Command::new(&exe).output().unwrap();
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(run.status.code(), Some((expected & 0xff) as i32), "{name}");
    }
    expected
}

const STATE: &str = "struct State { slots: ChunkSeq[u32], n: u64 }
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
struct Tab[V] { vals: ChunkSeq[V], keys: ChunkSeq[u64] }
impl [V: mod affine] Tab[V] {
    pub fn position(in self, k: u64) -> u64 {
        let mut i: u64 = 0;
        loop {
            match self.keys.try_get(i) {
                Option::Some(x) => {
                    if x == k {
                        return i;
                    }
                },
                Option::None => {
                    return 18446744073709551615;
                },
            }
            i = wrapping_add(i, 1);
        }
    }
    pub fn with_ref[R, e: eff](in self, k: u64, f: fn(in V) -> R eff [e]) -> Option[R] eff [e] {
        match self.vals.try_get(self.position(k)) {
            Option::Some(v) => Option::Some(f(in v)),
            Option::None => Option::None,
        }
    }
    // An early `return` of the result: it reaches the join through the
    // graph's exit, from storage.
    pub fn with_first[R, e: eff](in self, k: u64, f: fn(in V) -> R eff [e]) -> Option[R] eff [e] {
        if k == 0 {
            return Option::None;
        }
        match self.vals.try_get(self.position(k)) {
            Option::Some(v) => Option::Some(f(in v)),
            Option::None => Option::None,
        }
    }
    // The result passes through a call before it is returned: the callee
    // keeps it in storage and its caller reads the words back.
    pub fn with_passed[R: mod copy, e: eff](in self, k: u64, f: fn(in V) -> R eff [e]) -> Option[R] eff [e] {
        let r = match self.vals.try_get(self.position(k)) {
            Option::Some(v) => Option::Some(f(in v)),
            Option::None => Option::None,
        };
        same(r)
    }
}
fn same[T: mod copy](x: Option[T]) -> Option[T] {
    x
}
fn or0(x: Option[u64]) -> u64 {
    match x {
        Option::Some(v) => v,
        Option::None => 0,
    }
}
fn weight(in s: State) -> u64 {
    wrapping_add(wrapping_mul(s.n, 31), s.slots.len())
}
fn table(n: u64) -> Tab[State] {
    let mut t = Tab { vals: ChunkSeq::new[State](), keys: ChunkSeq::new[u64]() };
    for i in 0..n {
        t.vals.push(fresh(wrapping_mul(i, 7)));
        t.keys.push(wrapping_mul(i, 3));
    }
    t
}
";

/// The `flat_callbacks` shape of #1271, its hand-written `Option` form and
/// its element-view form.
fn lookups() -> String {
    format!(
        "{STATE}fn lookups(in t: Tab[State], in probe: State, n: u64) -> u64 {{
    let mut acc: u64 = 0;
    for k in 0..wrapping_add(n, 3) {{
        acc = wrapping_add(wrapping_mul(acc, 1000003), or0(t.with_ref(k, |in v| wrapping_add(weight(in v), probe.n))));
    }}
    acc
}}
fn options(in t: Tab[State], in probe: State, n: u64) -> u64 {{
    let mut acc: u64 = 0;
    for k in 0..wrapping_add(n, 3) {{
        let r = match t.vals.try_get(t.position(k)) {{
            Option::Some(v) => Option::Some(wrapping_add(weight(in v), probe.n)),
            Option::None => Option::None,
        }};
        acc = wrapping_add(wrapping_mul(acc, 1000003), or0(r));
    }}
    acc
}}
fn views(in t: Tab[State], in probe: State, n: u64) -> u64 {{
    let mut acc: u64 = 0;
    for k in 0..wrapping_add(n, 3) {{
        match t.vals.try_get(t.position(k)) {{
            Option::Some(v) => {{ acc = wrapping_add(wrapping_mul(acc, 1000003), wrapping_add(weight(in v), probe.n)); }},
            Option::None => {{ acc = wrapping_mul(acc, 1000003); }},
        }}
    }}
    acc
}}
fn main() -> u64 {{
    let t = table(12);
    let probe = fresh(5);
    let a = lookups(in t, in probe, 36);
    let b = options(in t, in probe, 36);
    let c = views(in t, in probe, 36);
    if a == b && b == c {{ wrapping_add(a, lookups(in t, in probe, 4)) }} else {{ 1 }}
}}"
    )
}

/// The `Option` of a lookup is never in memory on the optimize path: the
/// callback form and the hand-written `Option` form keep no 16-byte slot,
/// and the unoptimized path keeps its slots.
#[test]
fn option_results_stay_words() {
    let source = lookups();
    for function in ["lookups", "options"] {
        for (sizes, _) in emitted(&source, function, true) {
            assert!(
                !sizes.contains(&16),
                "optimized `{function}` keeps an Option slot: {sizes:?}"
            );
        }
        assert!(
            emitted(&source, function, false)
                .iter()
                .all(|(sizes, _)| sizes.contains(&16)),
            "unoptimized `{function}` lost its Option slot"
        );
    }
    differential("lookups", &source);
}

/// The receiver's handles and header words are read-only loads in the
/// optimized callback loop (Cranelift moves them out of the loop), as in
/// the element-view form, and ordinary loads unoptimized.
#[test]
fn callback_receivers_are_invariant() {
    let source = lookups();
    for function in ["lookups", "views"] {
        for (_, text) in emitted(&source, function, true) {
            assert!(
                text.contains("readonly"),
                "optimized `{function}` reads its receiver every lookup:\n{text}"
            );
        }
    }
    for (_, text) in emitted(&source, "lookups", false) {
        assert!(!text.contains("readonly"), "unoptimized `lookups`:\n{text}");
    }
}

/// An early `return` of the result and a result that passes through a call
/// reach the caller's join as words read back from storage.
#[test]
fn exits_and_stored_results_reach_the_join() {
    let source = format!(
        "{STATE}fn main() -> u64 {{
    let t = table(9);
    let probe = fresh(2);
    let mut acc: u64 = 0;
    for k in 0..30 {{
        let a = or0(t.with_first(k, |in v| wrapping_add(weight(in v), probe.n)));
        let b = or0(t.with_passed(k, |in v| wrapping_add(weight(in v), k)));
        let c = t.with_ref(k, |in v| v.n);
        let d = match same(c) {{
            Option::Some(x) => x,
            Option::None => 3,
        }};
        acc = wrapping_add(wrapping_mul(acc, 31), wrapping_add(wrapping_add(a, b), d));
    }}
    acc
}}"
    );
    differential("exits", &source);
}

/// Enums with several payload variants, `bool` and narrow fields,
/// wildcard patterns and a catch-all arm, joined through `if` and `match`
/// arms and a local.
#[test]
fn multi_variant_enums_stay_words() {
    let source = "enum Pick { A(u64), B(u32, bool), C }
fn pick(i: u64) -> u64 {
    let p = if i % 3 == 0 {
        Pick::A(wrapping_mul(i, 5))
    } else if i % 3 == 1 {
        Pick::B(wrapping_u32(i), i % 2 == 0)
    } else {
        Pick::C
    };
    let q = match p {
        Pick::A(x) => if x > 20 { Pick::B(7, true) } else { Pick::A(x) },
        Pick::B(_, b) => if b { Pick::C } else { Pick::B(9, false) },
        _ => Pick::A(i),
    };
    let r: Result[u64, u32] = match q {
        Pick::A(x) => Result::Ok(x),
        Pick::B(y, _) => Result::Err(y),
        Pick::C => Result::Ok(100),
    };
    match r {
        Result::Ok(v) => wrapping_add(v, 1),
        Result::Err(e) => wrapping_mul(wrapping_u64(e), 3),
    }
}
fn main() -> u64 {
    let mut acc: u64 = 0;
    for i in 0..40 {
        acc = wrapping_add(wrapping_mul(acc, 7), pick(i));
    }
    acc
}";
    for (sizes, _) in emitted(source, "pick", true) {
        assert!(sizes.is_empty(), "optimized `pick` keeps a slot: {sizes:?}");
    }
    differential("enums", source);
}

/// A callback that raises out of a lookup whose result is words, caught by
/// a handler around the loop; and a lookup whose result the caller passes
/// on whole.
#[test]
fn raising_callbacks_and_whole_results() {
    let source = format!(
        "{STATE}fn checked(x: u64) -> u64 eff [Fail[u64]] {{
    if x > 400 {{ Fail::raise(x) }}
    x
}}
fn sum(in t: Tab[State], n: u64) -> u64 eff [Fail[u64]] {{
    let mut acc: u64 = 0;
    for k in 0..n {{
        acc = wrapping_add(acc, or0(t.with_ref(k, |in v| checked(wrapping_add(weight(in v), k)))));
    }}
    acc
}}
fn main() -> u64 {{
    let t = table(20);
    let a = handle Fail[u64] in {{ sum(in t, 60) }} with {{ return v => v, raise(e, k) => wrapping_add(e, 1000), }};
    let b = handle Fail[u64] in {{ sum(in t, 9) }} with {{ return v => v, raise(e, k) => e, }};
    let whole = t.with_ref(4, |in v| v.n);
    wrapping_add(wrapping_add(a, b), or0(same(whole)))
}}"
    );
    differential("raising", &source);
}
