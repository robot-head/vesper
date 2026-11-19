//! UTF-8 `String` and `Str` (§13, port lane F2): the evaluator, the
//! Cranelift JIT and AOT executables agree on ASCII and multi-byte text,
//! UTF-8 validation, byte-order comparison and boundary faults; views are
//! second class; Lean extraction refuses string operations with a typed
//! error.
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::{check, lean, native, parser, runtime::Runtime};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "vesper-strings-test-{}-{}",
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

fn fixture(name: &str) -> String {
    fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/strings")
            .join(format!("{name}.ves")),
    )
    .unwrap()
}

fn checked(source: &str) -> check::Checked {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    check::check(&p.module)
}

fn accepted(source: &str) -> check::Checked {
    let c = checked(source);
    assert!(c.diagnostics.is_empty(), "{:#?}", c.diagnostics);
    c
}

fn links_executables() -> bool {
    cfg!(unix)
        && Command::new("cc")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
}

fn executable(name: &str, source: &str) -> (Option<i32>, String) {
    let temp = Temp::new();
    let file = temp.0.join(format!("{name}.ves"));
    fs::write(&file, source).unwrap();
    let exe = temp.0.join(name);
    let built = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .arg("build")
        .arg(&file)
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{name}: vesper build failed\n{}{}",
        String::from_utf8_lossy(&built.stdout),
        String::from_utf8_lossy(&built.stderr)
    );
    let run = Command::new(&exe).output().unwrap();
    (
        run.status.code(),
        String::from_utf8_lossy(&run.stderr).into_owned(),
    )
}

/// Runs `main` in the evaluator, both JIT configurations and (where a C
/// driver links executables) an AOT executable, and returns the result.
fn differential(name: &str, source: &str) -> String {
    let c = accepted(source);
    let evaluated = Runtime::new(&c.world)
        .run("main", vec![])
        .unwrap_or_else(|e| panic!("{name}: evaluator failed: {e}"))
        .to_string();
    for optimize in [true, false] {
        let jit = native::jit_with_options(&c.world, "main", optimize)
            .unwrap_or_else(|e| panic!("{name}: JIT (optimize {optimize}) failed: {e}"))
            .to_string();
        assert_eq!(
            jit, evaluated,
            "{name}: JIT (optimize {optimize}) vs evaluator"
        );
    }
    if links_executables() {
        let expected: u64 = evaluated.parse().unwrap();
        let (status, stderr) = executable(name, source);
        assert_eq!(
            status,
            Some((expected & 0xff) as i32),
            "{name}: executable stderr={stderr}"
        );
    }
    evaluated
}

/// The fixtures' polynomial hash of the code points of `s`.
fn chars_hash(s: &str) -> u64 {
    s.chars().fold(7u64, |acc, c| {
        acc.wrapping_mul(31).wrapping_add(u64::from(c))
    })
}

fn mix(values: &[u64]) -> u64 {
    values
        .iter()
        .fold(0u64, |t, v| t.wrapping_mul(1_000_003).wrapping_add(*v))
}

#[test]
fn ascii_and_multibyte_text_agree_on_every_backend() {
    let order_bits = |a: &str, b: &str| -> u64 {
        let (a, b) = (a.as_bytes(), b.as_bytes());
        u64::from(a == b)
            | u64::from(a != b) << 1
            | u64::from(a < b) << 2
            | u64::from(a <= b) << 3
            | u64::from(a > b) << 4
            | u64::from(a >= b) << 5
    };
    let mixed = "h\u{e9}llo, \u{4e16}\u{754c} \u{1f600}";
    let ascii = "plain ascii";
    let joined = format!("{mixed} and {ascii}");
    let bytes = mixed.as_bytes();
    let bytes_hash = bytes.iter().fold(11u64, |acc, b| {
        acc.wrapping_mul(257).wrapping_add(u64::from(*b))
    });
    let scratch = format!("temporary{}", char::from_u32(0x10FFFF).unwrap());
    let expected = mix(&[
        joined.len() as u64,
        joined.chars().count() as u64,
        chars_hash(&joined),
        chars_hash(&mixed[1..3]),
        chars_hash(&mixed[8..14]),
        bytes_hash,
        u64::from(bytes[1]),
        0xe9,
        0x4e16,
        1,
        2,
        1,
        // push_char: a surrogate and 0x110000 are refused, 0x10FFFF is not.
        1,
        scratch.len() as u64,
        chars_hash("h"),
        order_bits("abc", "abd"),
        order_bits("abc", "ab"),
        order_bits("", ""),
        order_bits("z", "\u{e9}"),
        order_bits(mixed, &joined),
        order_bits("h", "h"),
        1,
    ]);
    assert_eq!(differential("text", &fixture("text")), expected.to_string());
}

#[test]
fn utf8_validation_matches_rfc_3629_on_every_backend() {
    let cases: [&[u8]; 20] = [
        b"",
        b"A",
        &[0xC3, 0xA9],
        &[0xC0, 0x80],
        &[0xC1, 0xBF],
        &[0xE0, 0x80, 0x80],
        &[0xE0, 0xA0, 0x80],
        &[0xED, 0xA0, 0x80],
        &[0xED, 0x9F, 0xBF],
        &[0xF0, 0x8F, 0xBF, 0xBF],
        &[0xF0, 0x90, 0x80, 0x80],
        &[0xF4, 0x8F, 0xBF, 0xBF],
        &[0xF4, 0x90, 0x80, 0x80],
        &[0xF5, 0x80, 0x80, 0x80],
        &[0x80],
        &[0xE2, 0x82],
        &[0x61, 0xE2, 0x82, 0xAC, 0x62],
        &[0xFF],
        &[0xC3, 0x28],
        &[0xEF, 0xBB, 0xBF],
    ];
    // Rust's `str::from_utf8` is the RFC 3629 decoder.
    let values: Vec<u64> = cases
        .iter()
        .map(|b| match std::str::from_utf8(b) {
            Ok(s) => chars_hash(s).wrapping_add(b.len() as u64),
            Err(_) => 0,
        })
        .collect();
    assert_eq!(
        differential("utf8", &fixture("utf8")),
        mix(&values).to_string()
    );
}

#[test]
fn byte_and_char_reads_raise_index_failures_on_every_backend() {
    let source = "fn at(in s: Str, i: u64) -> u64 eff [Fail[RuntimeError]] { widen_u64(s.byte_at(i)) }
fn ch(in s: Str, i: u64) -> u64 eff [Fail[RuntimeError]] { widen_u64(s.char_at(i)) }
fn guarded(k: u64) -> u64 {
    let s = \"a\u{e9}\";
    handle Fail[RuntimeError] in {
        if k == 0 { at(s, 1) } else if k == 1 { at(s, 3) } else if k == 2 { ch(s, 1) } else { ch(s, 2) }
    } with {
        return x => x,
        raise(e, k) => match e {
            RuntimeError::IndexOutOfBounds => 1000,
            RuntimeError::DivisionByZero => 2000,
        },
    }
}
fn main() -> u64 {
    wrapping_add(wrapping_add(guarded(0), guarded(1)), wrapping_add(guarded(2), guarded(3)))
}";
    assert_eq!(
        differential("reads", source),
        (0xC3 + 1000 + 0xE9 + 1000).to_string()
    );
}

#[test]
fn view_intervals_off_char_boundaries_are_faults() {
    for (name, interval, needle) in [
        ("boundary_fault", "1..2", "char boundaries"),
        ("range_fault", "1..9", "out of bounds"),
    ] {
        let source = format!(
            "fn total(in s: Str) -> u64 {{ s.len() }}\nfn main() -> u64 {{ let s = \"a\u{e9}b\"; total(s[{interval}]) }}"
        );
        let c = accepted(&source);
        let e = Runtime::new(&c.world).run("main", vec![]).unwrap_err();
        assert!(e.contains(needle), "{name} evaluator: {e}");
        let j = native::jit(&c.world, "main").unwrap_err();
        assert!(
            j.contains(needle) || j.contains("out of bounds"),
            "{name} JIT: {j}"
        );
        if links_executables() {
            let (status, stderr) = executable(name, &source);
            assert_eq!(status, Some(71), "{name}: {stderr}");
        }
    }
    // A view on char boundaries is fine.
    let ok = accepted(
        "fn total(in s: Str) -> u64 { s.len() }\nfn main() -> u64 { let s = \"a\u{e9}b\"; total(s[1..3]) }",
    );
    assert_eq!(native::jit(&ok.world, "main").unwrap().to_string(), "2");
}

#[test]
fn empty_views_never_read_past_the_bytes() {
    // `""` and `String::new()` have null data; the boundary check at
    // `i == len` must not load from it.
    let source = "fn total(in s: Str) -> u64 { wrapping_add(s.len(), 1) }
fn main() -> u64 {
    let e = \"\";
    let n = String::new();
    let s = \"a\u{e9}\";
    let low = wrapping_add(total(e[0..0]), wrapping_mul(total(n[0..0]), 10));
    let high = wrapping_add(wrapping_mul(total(s[3..3]), 100), wrapping_mul(total(s[0..3]), 1000));
    wrapping_add(low, high)
}";
    assert_eq!(differential("empty_views", source), "4111");
}

#[test]
fn grown_strings_return_from_the_evaluator_and_the_jit() {
    let source = "fn main() -> String {
    let mut s = String::new();
    for i in 0..3 {
        s.push_str(\"ab\");
        s.push_char(955);
    }
    s.push_str(\"!\");
    s
}";
    let c = accepted(source);
    let expected = "\"ab\u{3bb}ab\u{3bb}ab\u{3bb}!\"";
    assert_eq!(
        Runtime::new(&c.world)
            .run("main", vec![])
            .unwrap()
            .to_string(),
        expected
    );
    assert_eq!(native::jit(&c.world, "main").unwrap().to_string(), expected);
}

fn rejected(source: &str, code: &str, needle: &str) {
    let c = checked(source);
    assert!(
        c.diagnostics
            .iter()
            .any(|d| d.code == code && d.message.contains(needle)),
        "expected {code} containing {needle:?}, got {:#?}",
        c.diagnostics
    );
}

#[test]
fn views_are_second_class() {
    rejected(
        "fn f(inout s: String) -> u64 { let v = s.as_str(); 0 }",
        "E1531",
        "second-class",
    );
    rejected(
        "fn f(inout s: String) -> u64 { let v = s[0..1]; 0 }",
        "E1531",
        "second-class",
    );
    rejected("fn f(inout s: Str) -> u64 { 0 }", "E1530", "inout");
    rejected("fn f(sink s: Str) -> u64 { 0 }", "E1530", "sink");
    rejected("fn f(in s: String) -> Str { s }", "E1530", "result");
    rejected(
        "struct Holder { s: Str }\nfn main() -> u64 { 0 }",
        "E1530",
        "field",
    );
    rejected(
        "fn f(in s: Str) -> u64 { s.push_str(\"x\"); 0 }",
        "E1532",
        "read-only",
    );
    rejected(
        "fn f() -> u64 { let mut s = \"ab\"; s.push_str(s.as_str()); 0 }",
        "E0301",
        "grows",
    );
    rejected(
        "fn size(in s: Str) -> u64 { s.len() }\nfn f(in s: String) -> u64 { size(s.as_str(1)) }",
        "E0207",
        "expects 0 arguments",
    );
    rejected(
        "fn size(in s: Str) -> u64 { s.len() }\nfn f(in s: String) -> u64 { size(s.as_str(undefined_name)) }",
        "E0201",
        "undefined_name",
    );
    rejected(
        "fn f(in s: String) -> bool { s == 3 }",
        "E0200",
        "compares only",
    );
}

#[test]
fn comparisons_borrow_their_operands() {
    let source = "fn main() -> u64 {
    let a = \"apple\";
    let b = \"banana\";
    let first: u64 = if a < b { 1 } else { 0 };
    let again: u64 = if a == b { 10 } else { 20 };
    wrapping_add(wrapping_add(first, again), wrapping_add(a.len(), b.len()))
}";
    assert_eq!(differential("borrowed_compare", source), "32");
}

#[test]
fn lean_extraction_refuses_string_operations() {
    let c = accepted("fn main() -> u64 { let s = \"abc\"; s.len() }");
    let e = lean::extract(&c.world).unwrap_err().to_string();
    assert!(e.contains("string operation 'len'"), "{e}");
}

/// Executables link with the MSVC toolchain on Windows hosts.
#[cfg(windows)]
#[test]
fn windows_executables_run_strings() {
    for name in ["text", "utf8"] {
        let source = fixture(name);
        let c = accepted(&source);
        let expected: u64 = Runtime::new(&c.world)
            .run("main", vec![])
            .unwrap()
            .to_string()
            .parse()
            .unwrap();
        let temp = Temp::new();
        let (object, _) =
            native::object_for_target(&c.world, "main", Some(native::windows::MSVC_TARGET))
                .unwrap_or_else(|e| panic!("{name}: {e}"));
        let path = temp.0.join(format!("{name}.vesper.o"));
        fs::write(&path, object).unwrap();
        let exe =
            native::windows::link_executable(&path, &temp.0.join(format!("{name}.exe")), None)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
        let status = Command::new(&exe).status().unwrap();
        assert_eq!(status.code(), Some((expected & 0xff) as i32), "{name}");
    }
}
