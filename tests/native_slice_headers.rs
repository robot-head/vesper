//! Slice and sequence headers of borrowed parameters (P3b gaps lane 3,
//! #1088). A borrowed slice is second-class and never changes its view, and
//! a borrowed sequence the function never resizes, rebinds or lends other
//! than `in` keeps its handle, so the native optimize path reads each such
//! parameter once at function entry and loads its header words as read-only
//! memory ([`vesper::bounds_hoist::stable_views`]): a store through one
//! slice no longer reloads every other slice's header. The plan tests check
//! which parameters qualify; the differential tests check that every backend
//! agrees, including parameters that are resized or lent to callees.
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::{check, ir, ir_opt, native, parser, runtime::Runtime};

static NEXT: AtomicU64 = AtomicU64::new(0);

fn accepted(source: &str) -> check::Checked {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:#?}", c.diagnostics);
    c
}

/// The parameters of `function` read once at entry, after the native
/// optimize pipeline's passes (without inlining, so `function` keeps its
/// own body).
fn entry_views(source: &str, function: &str) -> Vec<String> {
    let c = accepted(source);
    let mut program = ir::lower_world(&c.world);
    ir_opt::optimize(&c.world, &mut program).unwrap();
    vesper::fip::elaborate_non_fip_drops(&c.world, &mut program);
    vesper::bounds_hoist::run(&mut program);
    let mut places: Vec<String> = vesper::bounds_hoist::header_hoists(&program.functions[function])
        .into_iter()
        .filter(|h| h.entry)
        .map(|h| h.place.display())
        .collect();
    places.sort();
    places
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
        let jit = native::jit_with_options(&c.world, "main", optimize)
            .unwrap_or_else(|e| panic!("{name}: JIT (optimize {optimize}) failed: {e}"))
            .to_string();
        assert_eq!(jit, evaluated, "{name}: JIT (optimize {optimize})");
    }
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        let (obj, _) = native::object_with_options(&c.world, "main", Some(target), true)
            .unwrap_or_else(|e| panic!("{name}: {target}: {e}"));
        assert_eq!(&obj[..4], b"\x7fELF");
    }
    let expected: u64 = evaluated.parse().unwrap();
    if links_executables() {
        let dir = std::env::temp_dir().join(format!(
            "vesper-slice-headers-test-{}-{}",
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

fn program(body: &str) -> String {
    format!(
        "fn fill(inout s: ChunkSeq[u32], n: u64) {{
    for i in 0..n {{
        s.push(wrapping_u32(wrapping_mul(i, 7)));
    }}
}}
{body}
fn main() -> u64 {{
    handle Fail[RuntimeError] in {{
        handle Fail[ArithOverflow] in {{
            run()
        }} with {{
            return x => x,
            raise(e, k) => 7778,
        }}
    }} with {{
        return x => x,
        raise(e, k) => 7777,
    }}
}}
"
    )
}

/// The Resonance repro: two `inout` slices written in turn.
const SWAP_LANES: &str =
    "fn k(inout a: Slice[u32], inout b: Slice[u32]) eff [Fail[RuntimeError], Fail[ArithOverflow]] {
    let n: u64 = 16;
    for v in 0..n {
        a.set(v, b[v]);
        b.set(v, wrapping_add(a[v], 1));
    }
}
";

#[test]
fn borrowed_slices_are_read_once() {
    let source = program(&format!(
        "{SWAP_LANES}fn run() -> u64 eff [Fail[RuntimeError], Fail[ArithOverflow]] {{
    let mut x: ChunkSeq[u32] = ChunkSeq::new[u32]();
    let mut y: ChunkSeq[u32] = ChunkSeq::new[u32]();
    fill(inout x, 16);
    fill(inout y, 20);
    for r in 0..3 {{
        k(inout x.as_slice_mut(), inout y.as_slice_mut());
    }}
    let mut acc: u64 = 0;
    for i in 0..x.len() {{
        acc = wrapping_add(wrapping_mul(acc, 3), widen_u64(x[i]));
    }}
    for i in 0..y.len() {{
        acc = wrapping_add(wrapping_mul(acc, 3), widen_u64(y[i]));
    }}
    acc
}}"
    ));
    assert_eq!(entry_views(&source, "k"), ["a", "b"]);
    let mut x: Vec<u32> = (0..16).map(|i| i * 7).collect();
    let mut y: Vec<u32> = (0..20).map(|i| i * 7).collect();
    for _ in 0..3 {
        for v in 0..16 {
            x[v] = y[v];
            y[v] = x[v].wrapping_add(1);
        }
    }
    let expected = x.iter().chain(&y).fold(0u64, |acc, v| {
        acc.wrapping_mul(3).wrapping_add(u64::from(*v))
    });
    assert_eq!(differential("headers_swap", &source), expected);
    // A short slice still faults in the checked loop.
    let short = source.replace("fill(inout y, 20);", "fill(inout y, 9);");
    assert_eq!(differential("headers_swap_short", &short), 7777);
}

#[test]
fn slices_lent_to_callees_keep_their_view() {
    // `bump` writes elements of `a` through its own loan; `a`'s view and
    // buffer are unchanged, and the reads after the call see the writes.
    let source = program(
        "fn bump(inout s: Slice[u32], i: u64) {
    s.try_set(i, 99);
}
fn f(inout a: Slice[u32], in b: Slice[u32]) -> u64 eff [Fail[RuntimeError], Fail[ArithOverflow]] {
    let mut acc: u64 = 0;
    for i in 0..a.len() {
        let before = widen_u64(a[i]);
        bump(inout a, i);
        acc = wrapping_add(acc, wrapping_add(before, wrapping_add(widen_u64(a[i]), widen_u64(b[i]))));
    }
    acc
}
fn run() -> u64 eff [Fail[RuntimeError], Fail[ArithOverflow]] {
    let mut x: ChunkSeq[u32] = ChunkSeq::new[u32]();
    let mut y: ChunkSeq[u32] = ChunkSeq::new[u32]();
    fill(inout x, 12);
    fill(inout y, 12);
    f(inout x.as_slice_mut(), in y.as_slice())
}",
    );
    assert_eq!(entry_views(&source, "f"), ["a", "b"]);
    let expected: u64 = (0..12u64).map(|i| i * 7 + 99 + i * 7).sum();
    assert_eq!(differential("headers_lent", &source), expected);
}

#[test]
fn resized_or_lent_sequences_are_not_invariant() {
    // `grow` pushes onto its `inout` sequence and `pass` lends it on, so
    // neither handle is read once; `keep` only writes elements.
    let source = program(
        "fn grow(inout s: ChunkSeq[u32]) -> u64 eff [Fail[RuntimeError], Fail[ArithOverflow]] {
    let mut acc: u64 = 0;
    for i in 0..4 {
        acc = wrapping_add(acc, widen_u64(s[0]));
        s.push(5);
    }
    wrapping_add(acc, s.len())
}
fn pass(inout s: ChunkSeq[u32]) -> u64 eff [Fail[RuntimeError], Fail[ArithOverflow]] {
    let a = widen_u64(s[1]);
    fill(inout s, 3);
    wrapping_add(a, widen_u64(s[s.len() - 1]))
}
fn keep(inout s: ChunkSeq[u32]) -> u64 {
    let mut acc: u64 = 0;
    for i in 0..s.len() {
        s.try_set(i, wrapping_u32(i));
        acc = wrapping_add(acc, match s.try_get(i) {
            Option::Some(v) => widen_u64(v),
            Option::None => 1000,
        });
    }
    acc
}
fn run() -> u64 eff [Fail[RuntimeError], Fail[ArithOverflow]] {
    let mut x: ChunkSeq[u32] = ChunkSeq::new[u32]();
    fill(inout x, 3);
    let a = grow(inout x);
    let b = pass(inout x);
    let c = keep(inout x);
    wrapping_add(wrapping_mul(a, 1000000), wrapping_add(wrapping_mul(b, 1000), c))
}",
    );
    assert!(entry_views(&source, "grow").is_empty());
    assert!(entry_views(&source, "pass").is_empty());
    assert_eq!(entry_views(&source, "keep"), ["s"]);
    // grow: s[0] = 0 four times, len 7; pass: s[1] = 7, then 0, 7, 14
    // appended, last 14; keep: 0 + 1 + .. + 9.
    let expected = 7 * 1_000_000 + (7 + 14) * 1000 + (0..10).sum::<u64>();
    assert_eq!(differential("headers_resized", &source), expected);
}
