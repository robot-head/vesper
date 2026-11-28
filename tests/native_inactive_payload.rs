//! Inactive payload words of an enum value are unspecified (manuscript
//! §16.6, port speed lane 4, #1024).
//!
//! On the native optimize path a variant no longer zeroes the words past
//! its own fields. Each program here builds narrow variants into storage
//! that last held a wider one (a reused local, a loop-carried value, a
//! sequence slot, a value replaced through `inout`), so their inactive
//! words hold the wider variant's fields. Equality, `match`, drops and the
//! export boundary must read only the active variant. Every program runs
//! in the evaluator, the JIT with the optimizer on and off and, where a C
//! compiler links, an executable; all must agree, and the JIT must free
//! every sequence exactly once.
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::{check, native, parser, runtime::Runtime, seq::native_runtime};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "vesper-inactive-payload-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn accepted(source: &str) -> check::Checked {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:#?}", c.diagnostics);
    c
}

fn links() -> bool {
    cfg!(unix)
        && Command::new("cc")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
}

fn vesper(args: &[&std::ffi::OsStr]) {
    let out = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "vesper {args:?} failed\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Runs `main` everywhere and returns the agreed result.
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
            native_runtime::teardown_frees(),
            0,
            "{name}: JIT (optimize {optimize}) left sequences for teardown"
        );
        assert_eq!(
            native_runtime::stray_frees(),
            stray,
            "{name}: JIT (optimize {optimize}) freed a sequence that was not live"
        );
    }
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        let (obj, _) = native::object_with_options(&c.world, "main", Some(target), true)
            .unwrap_or_else(|e| panic!("{name}: {target}: {e}"));
        assert_eq!(&obj[..4], b"\x7fELF");
    }
    let expected: u64 = evaluated.parse().unwrap();
    if links() {
        let temp = Temp::new();
        let file = temp.0.join(format!("{name}.ves"));
        fs::write(&file, source).unwrap();
        let exe = temp.0.join(name);
        vesper(&[
            "build".as_ref(),
            file.as_os_str(),
            "-o".as_ref(),
            exe.as_os_str(),
        ]);
        let run = Command::new(&exe).output().unwrap();
        assert_eq!(
            run.status.code(),
            Some((expected & 0xff) as i32),
            "{name}: executable stderr={}",
            String::from_utf8_lossy(&run.stderr)
        );
    }
    expected
}

/// The Ignis `check` shape: one wide variant and three one-word ones.
const CHECK: &str = r#"
copy struct Rec {
    a: u64,
    b: u64,
    c: u32,
    d: bool
}

copy enum Check {
    Ok(Rec),
    BadA(u64),
    BadB(u64),
    BadC(u32)
}

fn check(a: u64, b: u64, c: u32, depth: u64) -> Check {
    if depth > 0 {
        return check(a, b, c, wrapping_sub(depth, 1));
    }
    if a == 0 {
        return Check::BadA(a);
    }
    if b == 0 {
        return Check::BadB(b);
    }
    if c == 0 {
        return Check::BadC(c);
    }
    Check::Ok(Rec {
        a: a,
        b: b,
        c: c,
        d: true
    })
}

fn score(x: Check) -> u64 {
    match x {
        Check::Ok(r) => {
            let d: u64 = if r.d { 1000 } else { 0 };
            wrapping_add(wrapping_add(wrapping_mul(r.a, 3), wrapping_mul(r.b, 5)), wrapping_add(widen_u64(r.c), d))
        }
        Check::BadA(v) => wrapping_add(v, 11),
        Check::BadB(v) => wrapping_add(v, 13),
        Check::BadC(v) => wrapping_add(widen_u64(v), 17),
    }
}
"#;

#[test]
fn equality_and_match_ignore_inactive_words() {
    // `x` alternates between `Ok` (four payload words) and the one-word
    // variants, so each narrow value lands on the previous `Ok` fields.
    // Equal narrow values built through different histories compare equal,
    // and unequal ones compare unequal.
    let source = format!(
        "{CHECK}
fn main() -> u64 {{
    let mut total: u64 = 0;
    let end: u64 = 24;
    let mut prev = Check::Ok(Rec {{ a: 7, b: 8, c: 9, d: true }});
    for i in 0..end {{
        let k: u64 = i % 4;
        let a: u64 = if k == 1 {{ 0 }} else {{ wrapping_add(i, 1) }};
        let b: u64 = if k == 2 {{ 0 }} else {{ 77 }};
        let c: u32 = if k == 3 {{ 0 }} else {{ 5 }};
        let x = check(a, b, c, i % 3);
        if x == Check::BadA(0) {{ total = wrapping_add(total, 100); }}
        if x == Check::BadB(0) {{ total = wrapping_add(total, 200); }}
        if x == Check::BadC(0) {{ total = wrapping_add(total, 300); }}
        if x == prev {{ total = wrapping_add(total, 7); }}
        let fresh = check(a, b, c, 0);
        if x == fresh {{ total = wrapping_add(total, 1); }}
        total = wrapping_add(total, score(x));
        prev = x;
    }}
    total
}}
"
    );
    let got = differential("equality_and_match", &source);
    assert!(got > 0);
}

#[test]
fn sequence_slots_keep_only_the_active_variant() {
    // Each slot first holds `Ok`, then a one-word variant is written over
    // it; the elements compare and match by their active variant only.
    let source = format!(
        "{CHECK}
fn main() -> u64 {{
    let mut s: ChunkSeq[Check] = ChunkSeq::new[Check]();
    let end: u64 = 12;
    for i in 0..end {{
        let n: u64 = i;
        s.push(Check::Ok(Rec {{ a: wrapping_add(n, 1), b: 2, c: 3, d: false }}));
    }}
    for i in 0..end {{
        let n: u64 = i;
        if n % 2 == 0 {{
            let _ = s.try_set(n, check(0, 1, 1, n % 3));
        }}
    }}
    let mut total: u64 = 0;
    for i in 0..end {{
        let n: u64 = i;
        match s.try_get(n) {{
            Some(x) => {{
                if x == Check::BadA(0) {{ total = wrapping_add(total, 1000); }}
                total = wrapping_add(total, score(x));
            }}
            None => {{}}
        }}
    }}
    total
}}
"
    );
    let got = differential("sequence_slots", &source);
    assert!(got > 0);
}

#[test]
fn drops_read_only_the_active_variant() {
    // `Two` owns two sequences in its payload words; `One` is one word. A
    // `One` built over a dropped `Two` keeps the freed pointers in its
    // inactive words, and dropping it must not free them again.
    let source = r#"
enum Bag {
    Two(ChunkSeq[u64], ChunkSeq[u64]),
    One(u64),
    Nothing
}

fn fill(n: u64) -> ChunkSeq[u64] {
    let mut s: ChunkSeq[u64] = ChunkSeq::new[u64]();
    let four: u64 = 4;
    for i in 0..four {
        s.push(wrapping_add(n, i));
    }
    s
}

fn make(k: u64) -> Bag {
    if k % 3 == 0 {
        return Bag::Two(fill(k), fill(wrapping_add(k, 10)));
    }
    if k % 3 == 1 {
        return Bag::One(k);
    }
    Bag::Nothing
}

fn weigh(sink b: Bag) -> u64 {
    match b {
        Bag::Two(x, y) => wrapping_add(x.len(), wrapping_mul(y.len(), 10)),
        Bag::One(v) => wrapping_add(v, 100),
        Bag::Nothing => 1,
    }
}

fn replace(inout b: Bag, k: u64) {
    b = make(k);
}

fn main() -> u64 {
    let mut total: u64 = 0;
    let end: u64 = 18;
    let mut held = make(0);
    for i in 0..end {
        let k: u64 = i;
        replace(inout held, k);
        total = wrapping_add(total, weigh(make(k)));
    }
    wrapping_add(total, weigh(held))
}
"#;
    let got = differential("drops", source);
    assert!(got > 0);
}
