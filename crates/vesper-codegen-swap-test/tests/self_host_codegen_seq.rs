//! Self-hosting SH4 lane 9 (#1283), part 2: native emission of sequences,
//! bytes, strings, bounds hoisting, fused reads and drop glue from
//! `@vesper/codegen-seq-emit`, through the `seq` hook of `vesper::codegen_swap`.
//!
//! Over the corpus and targeted test worlds, with the optimizer on and off,
//! every function's CLIF text, every object and every sampled JIT run must
//! equal the Rust build's, and the objects for the other native targets
//! (Windows, AArch64 Linux/macOS, and freestanding where applicable) must
//! be byte-identical too.
//! `CODEGEN_SEQ_STEP=n` compares every n-th world (default 1 under Cargo; under Bazel every fourth, up to four runtime worlds).
#[path = "../../../tests/common/check_harness.rs"]
mod check_harness;

use check_harness::repo;
use std::sync::OnceLock;
use std::sync::atomic::Ordering;
use vesper::codegen_swap::{self, CodegenHooks, Lent};
use vesper::native::WordFunction;
use vesper_codegen_swap_test::*;

fn hook() -> &'static WordFunction {
    static HOOK: OnceLock<WordFunction> = OnceLock::new();
    // Compiled with no hook in effect (see the pilot test's note).
    HOOK.get_or_init(|| {
        codegen_swap::with_hooks(CodegenHooks::default(), || {
            let checked = foreign_package(&repo(), "packages/vesper-codegen-seq/emit");
            hook_function(&checked, "seq_hook")
        })
    })
}

fn seq(lent: &mut Lent<'_, '_>) -> Result<bool, String> {
    vesper_hook(lent, hook())
}

/// Every request the Vesper hook has answered, indexed by `codegen_swap::seq` op.
const MAX_OP: usize = 14;
static OP_COUNTS: [std::sync::atomic::AtomicU64; MAX_OP] =
    [const { std::sync::atomic::AtomicU64::new(0) }; MAX_OP];

fn count_op(op: u32) -> u64 {
    OP_COUNTS[op as usize].load(Ordering::Relaxed)
}

fn counted(lent: &mut Lent<'_, '_>) -> Result<bool, String> {
    let op = lent.op;
    let handled = seq(lent)?;
    if handled {
        assert!((op as usize) < MAX_OP, "unexpected seq op {op}");
        OP_COUNTS[op as usize].fetch_add(1, Ordering::Relaxed);
    }
    Ok(handled)
}

/// Every `step()`-th world: `CODEGEN_SEQ_STEP`, else 1 under Cargo and 4
/// under Bazel, whose CI run shares one time limit with the whole suite.
fn step() -> usize {
    std::env::var("CODEGEN_SEQ_STEP")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(if std::env::var_os("TEST_SRCDIR").is_some() {
            4
        } else {
            1
        })
}

fn big_stack<R: Send>(f: impl FnOnce() -> R + Send) -> R {
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(256 << 20)
            .spawn_scoped(s, f)
            .expect("spawn a compile thread")
            .join()
            .unwrap_or_else(|e| std::panic::resume_unwind(e))
    })
}

/// The corpus worlds with a `main` whose host object defines the
/// sequence runtime (its live counter symbol), every `step()`-th.
fn worlds() -> &'static [&'static check_harness::Case] {
    static W: OnceLock<Vec<&'static check_harness::Case>> = OnceLock::new();
    W.get_or_init(|| {
        check_harness::corpus()
            .iter()
            .filter(|c| c.world.functions.contains_key("main"))
            .step_by(step())
            // Cheap first: a world whose signatures and bodies never name
            // a sequence or string type cannot need the runtime.
            .filter(|c| {
                let text = format!("{:?}", c.world.functions);
                text.contains("ChunkSeq") || text.contains("String")
            })
            .filter(|c| {
                build(&c.world, "main", true).is_ok_and(|b| {
                    b.object
                        .windows(b"vesper_seq_live".len())
                        .any(|w| w == b"vesper_seq_live")
                })
            })
            .take(if under_bazel() {
                BAZEL_WORLDS
            } else {
                usize::MAX
            })
            .collect()
    })
}

fn under_bazel() -> bool {
    std::env::var_os("TEST_SRCDIR").is_some()
}

/// Under Bazel, the first few runtime worlds only: the CI run shares one
/// time limit with the whole suite, and a Cargo run covers the corpus.
const BAZEL_WORLDS: usize = 4;

fn runs(index: usize, case: &check_harness::Case) -> bool {
    let f = &case.world.functions["main"];
    index.is_multiple_of(4)
        && f.params.is_empty()
        && f.effects.instances.is_empty()
        && f.effects.tail.is_none()
}

/// The runtime swap on the host: 0 CLIF differences, byte-identical
/// objects and equal runs, optimize on and off.
#[test]
fn the_runtime_matches_the_rust_emitter() {
    big_stack(host_swap);
}

fn host_swap() {
    let hooks = CodegenHooks {
        seq: Some(counted),
        ..CodegenHooks::default()
    };
    let before = count_op(codegen_swap::seq::RUNTIME);
    let declined = DECLINED.load(Ordering::Relaxed);
    let mut total = Diff::default();
    // Builds whose runtime did not come from the hook.
    let mut elsewhere = vec![];
    for (i, case) in worlds().iter().enumerate() {
        for optimize in [true, false] {
            let at = count_op(codegen_swap::seq::RUNTIME);
            total.add(compare(
                &case.name,
                &case.world,
                "main",
                hooks,
                optimize,
                runs(i, case),
            ));
            if count_op(codegen_swap::seq::RUNTIME) == at {
                elsewhere.push(format!("{}@{optimize}", case.name));
            }
        }
    }
    let runtimes = count_op(codegen_swap::seq::RUNTIME) - before;
    let declined = DECLINED.load(Ordering::Relaxed) - declined;
    eprintln!(
        "codegen seq: {}; {runtimes} runtimes defined from Vesper, {declined} requests declined; \
         runtime not requested in {elsewhere:?}",
        total.summary()
    );
    assert!(total.worlds > 0, "no corpus world compiled");
    assert!(runtimes > 0, "the runtime hook never ran");
    assert!(total.is_clean(), "{}", total.summary());
}

/// The other native targets: Windows (no fault message, no `write`
/// import) and AArch64 Linux and macOS.
#[test]
fn the_runtime_matches_on_other_targets() {
    big_stack(other_targets);
}

fn other_targets() {
    const TARGETS: [&str; 3] = [
        "x86_64-pc-windows-msvc",
        "aarch64-unknown-linux-gnu",
        "aarch64-apple-darwin",
    ];
    let before = count_op(codegen_swap::seq::RUNTIME);
    let hooks = CodegenHooks {
        seq: Some(counted),
        ..CodegenHooks::default()
    };
    let (mut compared, mut differ) = (0, vec![]);
    // Every fourth world: the runtime is the same in each object, and
    // only the code around it changes.
    for case in worlds().iter().step_by(4) {
        for target in TARGETS {
            let rust = vesper::native::object_with_options(&case.world, "main", Some(target), true);
            let hooked = codegen_swap::with_hooks(hooks, || {
                vesper::native::object_with_options(&case.world, "main", Some(target), true)
            });
            match (rust, hooked) {
                (Ok((r, rc)), Ok((h, hc))) => {
                    compared += 1;
                    if r != h || rc != hc {
                        differ.push(format!("{} {target}", case.name));
                    }
                }
                (Err(_), Err(_)) => {}
                (r, h) => differ.push(format!(
                    "{} {target}: {:?} vs {:?}",
                    case.name,
                    r.err(),
                    h.err()
                )),
            }
        }
    }
    let runtimes = count_op(codegen_swap::seq::RUNTIME) - before;
    eprintln!(
        "codegen seq, other targets: {compared} objects, {} differ, {runtimes} runtimes from Vesper",
        differ.len()
    );
    assert!(compared > 0 && runtimes > 0);
    assert!(differ.is_empty(), "{differ:?}");
}

/// A hook that answers the runtime request without defining anything is
/// caught: the object (or the compile) differs.
#[test]
fn a_missing_runtime_is_caught() {
    big_stack(missing_runtime);
}

fn missing_runtime() {
    fn nothing(lent: &mut Lent<'_, '_>) -> Result<bool, String> {
        Ok(lent.op == codegen_swap::seq::RUNTIME)
    }
    let hooks = CodegenHooks {
        seq: Some(nothing),
        ..CodegenHooks::default()
    };
    let caught = worlds().iter().take(20).any(|case| {
        let rust = build(&case.world, "main", true);
        let hooked = codegen_swap::with_hooks(hooks, || build(&case.world, "main", true));
        match (rust, hooked) {
            (Ok(r), Ok(h)) => r.object != h.object,
            (Ok(_), Err(_)) => true,
            _ => false,
        }
    });
    assert!(caught, "a hook that defines no runtime went unnoticed");
}

fn check_source(name: &str, src: &str) -> vesper::check::World {
    let parsed = vesper::parser::parse(src);
    assert!(
        parsed.diagnostics.is_empty(),
        "{name} parse errors: {:?}",
        parsed.diagnostics
    );
    let c = vesper::check::check(&parsed.module);
    assert!(
        c.diagnostics.is_empty(),
        "{name} check errors: {:?}",
        c.diagnostics
    );
    c.world
}

fn targeted_worlds() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "seq_core_ops",
            r#"
fn bump(inout x: u32) { x = wrapping_add(x, 1); }
fn run() -> u64 eff [Fail[RuntimeError]] {
    let mut s = ChunkSeq::with_capacity[u32](4);
    s.push(10);
    s.push(20);
    s.push(30);
    let mut t = ChunkSeq::new[u32]();
    t.extend(in s.as_slice());
    t.extend_reversed(in s.as_slice());
    let _ = t.try_set(1, 99);
    let _ = t.try_set(100, 7);
    t.set(2, 44);
    bump(inout t[0]);
    let fg = match s.try_get(1) {
        Option::Some(v) => widen_u64(v),
        Option::None => 0,
    };
    let a = match t[1..4].try_get(0) {
        Option::Some(v) => widen_u64(v),
        Option::None => 0,
    };
    let b = match t.last() {
        Option::Some(v) => widen_u64(v),
        Option::None => 0,
    };
    let c = match t.pop() {
        Option::Some(v) => widen_u64(v),
        Option::None => 0,
    };
    t.resize(6, 5);
    t.extend_from_within(1, 3);
    let _ = t.try_extend_from_within(0, 2);
    t.truncate(4);
    t.reserve(8);
    t.clear();
    let len = t.len();
    let mut r = wrapping_add(a, b);
    r = wrapping_add(r, c);
    r = wrapping_add(r, len);
    r = wrapping_add(r, widen_u64(s[1]));
    r = wrapping_add(r, fg);
    wrapping_add(r, s.as_slice().len())
}
fn main() -> u64 { handle Fail[RuntimeError] in { run() } with { return x => x, raise(e, k) => 0 } }
"#,
        ),
        (
            "seq_strings",
            r#"
fn check_sub(in sub: Str) -> u64 {
    let cmp = if sub == "Hello" && sub < "World" && "Abc" != sub { 10 } else { 0 };
    wrapping_add(cmp, sub.as_bytes().len())
}
fn read_bytes(in st: Str) -> u64 eff [Fail[RuntimeError]] {
    let cmp_len = check_sub(in st[0..5]);
    let b0 = st.byte_at(1);
    let b1 = match st.try_byte_at(99) {
        Option::Some(v) => v,
        Option::None => 4,
    };
    wrapping_add(wrapping_add(widen_u64(b0), widen_u64(b1)), cmp_len)
}
fn run() -> u64 eff [Fail[RuntimeError]] {
    let mut s = String::new();
    s.push_str(in "");
    s.push_str(in "Hello, ");
    let _ = s.push_char(87);
    s.push_str(in "orld!");
    s.reserve(16);
    let rb = read_bytes(in s.as_str());
    s.clear();
    wrapping_add(rb, s.len())
}
fn main() -> u64 { handle Fail[RuntimeError] in { run() } with { return x => x, raise(e, k) => 0 } }
"#,
        ),
        (
            "seq_bytes_simd",
            r#"
fn scan_bytes(in sa: Slice[u8], in sb: Slice[u8], in lit1: Slice[u8], in lit2: Slice[u8], in ws: Slice[u8]) -> u64 {
    let sum = sa.sum_be16();
    let eq1 = if sa.eq(in sa) { 1 } else { 0 };
    let eq2 = if lit1.eq_ignore_ascii_case(in lit2) { 2 } else { 0 };
    let cmp = if sa.cmp(in sb) != 0 { 4 } else { 0 };
    let f1 = match sa.find_byte(68) { Option::Some(p) => p, Option::None => 0 };
    let f2 = match sa.find_either(67, 68) { Option::Some(p) => p, Option::None => 0 };
    let f3 = match ws.find_not_in(0x100000000, 1 << 32, false) { Option::Some(p) => p, Option::None => 0 };
    let mut r = sum;
    r = wrapping_add(r, eq1);
    r = wrapping_add(r, eq2);
    r = wrapping_add(r, cmp);
    r = wrapping_add(r, f1);
    r = wrapping_add(r, f2);
    wrapping_add(r, f3)
}
fn run() -> u64 eff [Fail[RuntimeError]] {
    let mut a = ChunkSeq::with_capacity[u8](32);
    for i in 0..24 {
        a.push(wrapping_u8(wrapping_add(65, i & 7)));
    }
    let mut b = ChunkSeq::with_capacity[u8](32);
    b.resize(24, 0);
    b.copy_from(0, in a.as_slice());
    let _ = b.try_copy_from(2, in a[0..4]);
    b.copy_within(0, 8, 4);
    let _ = b.try_copy_within(1, 5, 2);
    b.fill(20, 24, 66);
    let _ = b.try_fill(0, 2, 67);
    b.write_u16_be(0, 0x1234);
    b.write_u32_le(4, 0x11223344);
    let _ = b.try_write_u64_be(8, 0x0102030405060708);
    let w1 = b.read_u16_be(0);
    let w2 = match b.try_read_u32_le(4) { Option::Some(v) => widen_u64(v), Option::None => 0 };
    let mut lower = ChunkSeq::new[u8]();
    lower.extend_ascii_lowercase(in "HelloWorld1234567890!".as_bytes());
    lower.make_ascii_lowercase(0, 10);
    let _ = lower.try_make_ascii_lowercase(0, 5);
    let sc = scan_bytes(
        in a.as_slice(),
        in b.as_slice(),
        in "AbCdEfGhIjKlMnOpQrSt".as_bytes(),
        in "abcdefghijklmnopqrst".as_bytes(),
        in "    \t\nhello".as_bytes(),
    );
    let mut r = widen_u64(w1);
    r = wrapping_add(r, w2);
    r = wrapping_add(r, sc);
    wrapping_add(r, lower.len())
}
fn main() -> u64 { handle Fail[RuntimeError] in { run() } with { return x => x, raise(e, k) => 0 } }
"#,
        ),
        (
            "seq_hoist_and_drops",
            r#"
struct Pair { s: String, seq: ChunkSeq[u8] }
struct Holder { data: ChunkSeq[u64] }
impl Holder {
    pub fn with_ref[R](in self, k: u64, f: fn(in u64) -> R) -> Option[R] {
        match self.data.try_get(k) {
            Option::Some(v) => Option::Some(f(in v)),
            Option::None => Option::None,
        }
    }
}
enum Item { Empty, Full(String, ChunkSeq[String]) }
fn sum_hoisted(in xs: ChunkSeq[u64]) -> u64 eff [Fail[RuntimeError]] {
    let mut acc: u64 = 0;
    let n = xs.len();
    for i in 0..n {
        acc = wrapping_add(acc, xs[i]);
    }
    if n > 0 {
        acc = wrapping_add(acc, xs[0]);
    }
    acc
}
fn sum_with_ref(in hh: Holder) -> u64 {
    let mut acc: u64 = 0;
    for i in 0..4 {
        match hh.with_ref(i, |in v| v) {
            Option::Some(v) => { acc = wrapping_add(acc, v); },
            Option::None => {},
        }
    }
    acc
}
fn run() -> u64 eff [Fail[RuntimeError]] {
    let mut xs = ChunkSeq::new[u64]();
    for i in 0..12 { xs.push(i); }
    let h = sum_hoisted(in xs);
    let holder = Holder { data: xs };
    let hf = sum_with_ref(in holder);
    let mut p = Pair { s: String::new(), seq: ChunkSeq::new[u8]() };
    p.s.push_str(in "abc");
    p.seq.push(1);
    let mut nested = ChunkSeq::new[String]();
    let mut s1 = String::new();
    s1.push_str(in "one");
    nested.push(s1);
    let it = Item::Full(p.s, nested);
    let n = match it {
        Item::Empty => 0,
        Item::Full(st, ns) => wrapping_add(st.len(), ns.len()),
    };
    wrapping_add(wrapping_add(wrapping_add(h, hf), n), p.seq.len())
}
fn main() -> u64 { handle Fail[RuntimeError] in { run() } with { return x => x, raise(e, k) => 0 } }
"#,
        ),
    ]
}

/// All `codegen_swap::seq` ops (`1..=13`) match Rust across corpus and
/// targeted worlds, with 0 CLIF diffs, 0 object diffs and equal JIT runs.
#[test]
fn all_seq_ops_match_the_rust_emitter() {
    big_stack(|| {
        let hooks = CodegenHooks {
            seq: Some(counted),
            ..CodegenHooks::default()
        };
        let mut before = [0u64; MAX_OP];
        for (op, slot) in before.iter_mut().enumerate().skip(1) {
            *slot = count_op(op as u32);
        }
        let mut total = Diff::default();
        for (name, src) in targeted_worlds() {
            let world = check_source(name, src);
            for optimize in [true, false] {
                let diff = compare(name, &world, "main", hooks, optimize, true);
                assert!(
                    diff.worlds == 1,
                    "{name} (optimize={optimize}) failed to build: {:?}",
                    vesper_codegen_swap_test::build(&world, "main", optimize).err()
                );
                total.add(diff);
            }
        }
        for (i, case) in worlds().iter().enumerate() {
            for optimize in [true, false] {
                total.add(compare(
                    &case.name,
                    &case.world,
                    "main",
                    hooks,
                    optimize,
                    runs(i, case),
                ));
            }
        }
        let mut summary = String::new();
        for (op, prev) in before.iter().enumerate().skip(1) {
            let delta = count_op(op as u32) - *prev;
            summary.push_str(&format!(" op{op}={delta}"));
        }
        eprintln!("codegen seq part 2b: {};{}", total.summary(), summary);
        assert!(total.is_clean(), "{}", total.summary());
        for (op, prev) in before.iter().enumerate().skip(1) {
            let delta = count_op(op as u32) - *prev;
            assert!(delta > 0, "seq op {op} was never exercised ({summary})");
        }
    });
}

/// A wrong emission in `seq` is caught by `compare`.
#[test]
fn a_wrong_seq_emit_is_caught() {
    big_stack(|| {
        fn wrong_literal(lent: &mut Lent<'_, '_>) -> Result<bool, String> {
            if lent.op == codegen_swap::seq::STR_OP
                && lent.args.first().copied() == Some(codegen_swap::seq::STR_LEN)
            {
                lent.results.push(lent.args[1]);
                return Ok(true);
            }
            Ok(false)
        }
        let hooks = CodegenHooks {
            seq: Some(wrong_literal),
            ..CodegenHooks::default()
        };
        let world = check_source(
            "wrong_seq",
            r#"
fn main() -> u64 { "hello".len() }
"#,
        );
        let diff = compare("wrong_seq", &world, "main", hooks, true, false);
        assert!(!diff.is_clean(), "a wrong seq emission went unnoticed");
    });
}
