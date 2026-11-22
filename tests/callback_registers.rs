//! Lent callbacks keep their parameters in registers (self-hosting SH4
//! lane 4, #1278, fixes #1271).
//!
//! On the native optimize path a call to `with_ref`-style method is
//! specialized into its caller with the closure literal it is passed. Before
//! this lane every name in scope at the closure literal kept a stack slot
//! (`register_locals` treated the closure's `aliases`, which list every name
//! in scope, as captures), the parameters the specialized call bound for the
//! inlined graph (the key, the closure tag) kept slots too, an `in`
//! aggregate argument was copied twice on the way to the closure body, and
//! a lent capture's address went through a stack slot. The shape test
//! checks the emitted Cranelift function of the caller has no word-sized
//! stack slot left; the differential tests check that the evaluator, both
//! JIT configurations, x86-64 and AArch64 objects and an executable agree
//! on callbacks that capture loop variables, snapshot names that change
//! later, write `inout` scalars, are forwarded through lent parameters,
//! raise, and pick one of two closures.
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

/// The sizes of the explicit stack slots of each emitted function named
/// `function`.
fn slots(source: &str, function: &str, optimize: bool) -> Vec<Vec<u32>> {
    let c = accepted(source);
    let mut out = vec![];
    native::object_with_observer(
        &c.world,
        "main",
        None,
        optimize,
        Some(&mut |f: native::EmittedFunction| {
            if f.source == function {
                out.push(
                    f.function
                        .sized_stack_slots
                        .values()
                        .map(|s| s.size)
                        .collect(),
                );
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

/// Runs `main` in the evaluator, both JIT configurations (checking that
/// the JIT frees every sequence once), x86-64 and AArch64 objects and
/// (where a C driver links) an executable.
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
            "vesper-callback-registers-test-{}-{}",
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
struct Tab[V] { vals: ChunkSeq[V] }
impl [V: mod affine] Tab[V] {
    pub fn with_ref[R, e: eff](in self, i: u64, f: fn(in V) -> R eff [e]) -> Option[R] eff [e] {
        match self.vals.try_get(i) {
            Option::Some(v) => Option::Some(f(in v)),
            Option::None => Option::None,
        }
    }
    pub fn with_mut[R, e: eff](inout self, i: u64, f: fn(inout V) -> R eff [e]) -> Option[R] eff [e] {
        match self.vals.try_get_mut(i) {
            Option::Some(v) => Option::Some(f(inout v)),
            Option::None => Option::None,
        }
    }
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
    let mut t = Tab { vals: ChunkSeq::new[State]() };
    for i in 0..n {
        t.vals.push(fresh(wrapping_mul(i, 7)));
    }
    t
}
";

/// The `flat_callbacks` shape of #1271: a loop of `with_ref` lookups whose
/// lent closure reads the element, a borrowed parameter and the loop index.
fn lookups() -> String {
    format!(
        "{STATE}fn lookups(in t: Tab[State], in probe: State, n: u64) -> u64 {{
    let mut acc: u64 = 0;
    for k in 0..wrapping_add(n, 3) {{
        acc = wrapping_add(wrapping_mul(acc, 1000003), or0(t.with_ref(k, |in v| wrapping_add(weight(in v), wrapping_add(probe.n, k)))));
    }}
    acc
}}
fn main() -> u64 {{
    let t = table(12);
    let probe = fresh(5);
    wrapping_add(lookups(in t, in probe, 12), lookups(in t, in probe, 4))
}}"
    )
}

#[test]
fn callback_loops_keep_their_words_in_registers() {
    let source = lookups();
    // Optimized: no word-sized home is left (the accumulator, the index,
    // `n`, the inlined key and closure tag, the lent capture's address).
    for sizes in slots(&source, "lookups", true) {
        assert!(
            !sizes.contains(&8),
            "optimized `lookups` keeps a word slot: {sizes:?}"
        );
    }
    // The unoptimized path keeps its home slots.
    assert!(
        slots(&source, "lookups", false)
            .iter()
            .all(|sizes| sizes.contains(&8))
    );
    differential("lookups", &source);
}

/// A closure value made in a loop copies its capture when it is made: a
/// later change of the captured name does not reach it, and the closure
/// made last is the one called.
#[test]
fn closures_snapshot_names_that_change_later() {
    let source = "fn main() -> u64 {
    let mut base: u64 = 3;
    let mut total: u64 = 0;
    for i in 0..6 {
        let h = |x: u64| wrapping_add(wrapping_mul(x, 10), base);
        base = wrapping_add(base, wrapping_mul(i, 5));
        total = wrapping_add(wrapping_mul(total, 3), h(i));
    }
    wrapping_add(total, base)
}";
    differential("snapshot", source);
}

/// `inout` callbacks: an element lent in place, and a scalar local of the
/// caller lent to a closure parameter and restored after each call, both
/// next to register locals the closure captures.
#[test]
fn inout_callbacks_write_back() {
    let source = format!(
        "{STATE}fn main() -> u64 {{
    let mut t = table(6);
    let mut c: u64 = 1;
    let step: u64 = 4;
    let bump = |inout x: u64| {{ x = wrapping_add(x, 2); }};
    for i in 0..8 {{
        let got = or0(t.with_mut(i, |inout v| {{ v.n = wrapping_add(v.n, wrapping_add(step, i)); v.slots.push(1); v.n }}));
        c = wrapping_add(wrapping_mul(c, 7), got);
    }}
    bump(inout c);
    let mut seen: u64 = 0;
    for i in 0..8 {{
        seen = wrapping_add(seen, or0(t.with_ref(i, |in v| weight(in v))));
    }}
    wrapping_add(c, seen)
}}"
    );
    differential("inout", &source);
}

/// A lent function parameter forwarded into another lent closure, called
/// in a loop of a specialized callee, and passed on again.
#[test]
fn forwarded_callbacks_in_specialized_loops() {
    let source = format!(
        "{STATE}fn each(in t: Tab[State], f: fn(in State) -> u64) -> u64 {{
    let mut acc: u64 = 0;
    for i in 0..wrapping_add(t.vals.len(), 1) {{
        acc = wrapping_add(wrapping_mul(acc, 31), or0(t.with_ref(i, |in v| wrapping_add(f(in v), i))));
    }}
    acc
}}
fn twice(in t: Tab[State], f: fn(in State) -> u64) -> u64 {{
    wrapping_add(each(in t, f), each(in t, |in v| wrapping_mul(f(in v), 3)))
}}
fn main() -> u64 {{
    let t = table(9);
    let probe = fresh(2);
    let k: u64 = 11;
    wrapping_add(twice(in t, |in v| wrapping_add(weight(in v), wrapping_add(probe.n, k))), each(in t, |in v| v.n))
}}"
    );
    differential("forwarded", &source);
}

/// A raising callback: the handler outside the call catches it, and the
/// loop's register locals the closure captured are still right afterwards.
#[test]
fn raising_callbacks_keep_captured_locals() {
    let source = format!(
        "{STATE}fn main() -> u64 {{
    let t = table(5);
    let mut total: u64 = 0;
    for i in 0..6 {{
        let r = handle Fail[RuntimeError] in {{
            widen_u64(match t.with_ref(3, |in v| v.slots[i]) {{ Option::Some(x) => x, Option::None => 0 }})
        }} with {{ return x => x, raise(e, k) => 500, }};
        total = wrapping_add(wrapping_mul(total, 13), wrapping_add(r, i));
    }}
    total
}}"
    );
    differential("raising", &source);
}
