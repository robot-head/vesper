//! Vector slice loads and stores (P3b gaps lane 1, #1086):
//! `Vec::load[T, N](in s, at)`, `Vec::store(inout s, at, v)`,
//! `Vec::load_masked[T, N](in s, at)` and `Vec::store_masked(inout s, at, v)`
//! on `Slice[T]` and `ChunkSeq[T]` for every lane type. The unmasked forms
//! check `at + N <= len` once and raise `Fail[RuntimeError]`; the masked
//! forms read zero past the end and write only lanes inside the view. Every
//! program runs in the evaluator and the JIT (optimize on and off), builds
//! x86_64 and AArch64 objects and runs as an executable where a C driver
//! links one, and matches a Rust model of the sequence.
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::{check, native, parser, runtime::Runtime};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "vesper-vector-slice-io-{}-{}",
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

fn checked(source: &str) -> check::Checked {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{source}\n{:?}", p.diagnostics);
    check::check(&p.module)
}
fn accepted(source: &str) -> check::Checked {
    let c = checked(source);
    let shown: Vec<_> = c
        .diagnostics
        .iter()
        .map(|d| format!("{}: {} at `{}`", d.code, d.message, &source[d.span.clone()]))
        .collect();
    assert!(c.diagnostics.is_empty(), "{source}\n{shown:#?}");
    c
}
fn rejected(source: &str, code: &str, text: &str) {
    let c = checked(source);
    assert!(
        c.diagnostics
            .iter()
            .any(|d| d.code == code && d.message.contains(text)),
        "expected {code} containing {text:?}, got {:?}",
        c.diagnostics
    );
}
fn links_executables() -> bool {
    cfg!(unix)
        && Command::new("cc")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
}
fn executable_status(name: &str, source: &str) -> Option<i32> {
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
    Command::new(&exe).output().unwrap().status.code()
}
fn differential(name: &str, source: &str, expected: u64) {
    let c = accepted(source);
    for optimize in [true, false] {
        let mut runtime = Runtime::new(&c.world);
        runtime.optimize = optimize;
        let value = runtime
            .run("main", vec![])
            .unwrap_or_else(|e| panic!("{name}: evaluator (optimize {optimize}) failed: {e}"))
            .to_string();
        assert_eq!(
            value,
            expected.to_string(),
            "{name}: evaluator (optimize {optimize})"
        );
        let jit = native::jit_with_options(&c.world, "main", optimize)
            .unwrap_or_else(|e| panic!("{name}: JIT (optimize {optimize}) failed: {e}"))
            .to_string();
        assert_eq!(
            jit,
            expected.to_string(),
            "{name}: JIT (optimize {optimize})"
        );
    }
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        native::object_for_target(&c.world, "main", Some(target))
            .unwrap_or_else(|e| panic!("{name}: {target} object failed: {e}"));
    }
    if links_executables() {
        assert_eq!(
            executable_status(name, source),
            Some((expected & 0xff) as i32),
            "{name}: executable"
        );
    }
}

const OFFSET: u64 = 14695981039346656037;
fn mix(acc: u64, v: u64) -> u64 {
    acc.wrapping_mul(1099511628211).wrapping_add(v)
}

/// One lane type: its name, its lanes in 128 bits, the Vesper expressions
/// that make an element from a `u64` and read an element's bits, and the
/// element bits a `u64` makes.
struct Lane {
    name: &'static str,
    lanes: usize,
    make: &'static str,
    bits: &'static str,
    model: fn(u64) -> u64,
}
const LANES: [Lane; 6] = [
    Lane {
        name: "u8",
        lanes: 16,
        make: "wrapping_u8(x)",
        bits: "widen_u64(x)",
        model: |x| x & 0xff,
    },
    Lane {
        name: "u32",
        lanes: 4,
        make: "wrapping_u32(x)",
        bits: "widen_u64(x)",
        model: |x| x & 0xffff_ffff,
    },
    Lane {
        name: "u64",
        lanes: 2,
        make: "x",
        bits: "x",
        model: |x| x,
    },
    Lane {
        name: "i64",
        lanes: 2,
        make: "wrapping_i64(x)",
        bits: "wrapping_u64(x)",
        model: |x| x,
    },
    // Finite values only: the top two bits clear keep the exponent below
    // all ones, so the model compares bits.
    Lane {
        name: "f32",
        lanes: 4,
        make: "from_bits_f32(wrapping_u32(x & 1073741823))",
        bits: "widen_u64(to_bits_f32(x))",
        model: |x| x & 0x3fff_ffff,
    },
    Lane {
        name: "f64",
        lanes: 2,
        make: "from_bits_f64(x & 4611686018427387903)",
        bits: "to_bits_f64(x)",
        model: |x| x & 0x3fff_ffff_ffff_ffff,
    },
];

fn program(lane: &Lane, lanes: usize, isa: &str) -> String {
    let t = lane.name;
    let n = lanes;
    let lane_digest: String = (0..n)
        .map(|i| format!("    a = mix(a, bits(Vec::extract(v, {i})));\n"))
        .collect();
    format!(
        r#"
fn u(x: u64) -> u64 {{ x }}
fn mix(acc: u64, v: u64) -> u64 {{ wrapping_add(wrapping_mul(acc, 1099511628211), v) }}
fn elem(x: u64) -> {t} {{ {make} }}
fn bits(x: {t}) -> u64 {{ {bits} }}
fn vdigest(acc: u64, v: Vec[{t}, {n}]) -> u64 eff [Isa[{isa}]] {{
    let mut a = acc;
{lane_digest}    a
}}
fn load_from(in v: Slice[{t}], at: u64) -> Vec[{t}, {n}] eff [Isa[{isa}], Fail[RuntimeError]] {{ Vec::load[{t}, {n}](in v, at) }}
fn store_into(inout v: Slice[{t}], at: u64, x: Vec[{t}, {n}]) eff [Isa[{isa}], Fail[RuntimeError]] {{ Vec::store(inout v, at, x) }}
fn tail_load(in v: Slice[{t}], at: u64) -> Vec[{t}, {n}] eff [Isa[{isa}]] {{ Vec::load_masked[{t}, {n}](in v, at) }}
fn tail_store(inout v: Slice[{t}], at: u64, x: Vec[{t}, {n}]) eff [Isa[{isa}]] {{ Vec::store_masked(inout v, at, x) }}
fn main() -> u64 {{
    if Isa::supported[{isa}]() {{
        handle Fail[RuntimeError] in {{ run() }} with {{ return x => x, raise(e, k) => 7, }}
    }} else {{
        1
    }}
}}
fn run() -> u64 eff [Isa[{isa}], Fail[RuntimeError]] {{
    let len = u({len});
    let mut s: ChunkSeq[{t}] = ChunkSeq::new[{t}]();
    for i in u(0)..len {{ s.push(elem(wrapping_mul(wrapping_add(i, 1), 11400714819323198485))); }}
    let mut acc: u64 = {OFFSET};
    let v = Vec::load[{t}, {n}](in s, 1);
    acc = vdigest(acc, v);
    let w = load_from(in s[3..len], 2);
    acc = vdigest(acc, w);
    Vec::store(inout s, 0, w);
    store_into(inout s[2..len], {n}, v);
    acc = vdigest(acc, tail_load(in s.as_slice(), wrapping_sub(len, 3)));
    acc = vdigest(acc, tail_load(in s.as_slice(), wrapping_sub(len, 1)));
    acc = vdigest(acc, tail_load(in s[1..4], 0));
    acc = vdigest(acc, Vec::load_masked[{t}, {n}](in s, wrapping_add(len, 7)));
    acc = vdigest(acc, tail_load(in s[1..len], 3));
    tail_store(inout s.as_slice_mut(), wrapping_sub(len, 2), v);
    Vec::store_masked(inout s, 4, w);
    tail_store(inout s[0..3], 1, w);
    Vec::store_masked(inout s, wrapping_add(len, 100), v);
    for i in u(0)..len {{ acc = mix(acc, bits(s[i])); }}
    acc
}}
"#,
        make = lane.make,
        bits = lane.bits,
        len = 2 * n + 5,
    )
}

/// The Rust model of [`program`] over element bit patterns.
fn model(lane: &Lane, n: usize) -> u64 {
    let len = 2 * n + 5;
    let mut s: Vec<u64> = (0..len as u64)
        .map(|i| (lane.model)((i + 1).wrapping_mul(11400714819323198485)))
        .collect();
    let load = |s: &[u64], start: usize, end: usize, at: usize| -> Vec<u64> {
        (0..n)
            .map(|i| {
                let k = start + at + i;
                if at + i < end - start { s[k] } else { 0 }
            })
            .collect()
    };
    let digest = |acc: u64, v: &[u64]| -> u64 { v.iter().fold(acc, |a, x| mix(a, *x)) };
    let store = |s: &mut [u64], start: usize, end: usize, at: usize, v: &[u64]| {
        for (i, x) in v.iter().enumerate() {
            if at + i < end - start {
                s[start + at + i] = *x;
            }
        }
    };
    let mut acc = OFFSET;
    let v = load(&s, 0, len, 1);
    acc = digest(acc, &v);
    let w = load(&s, 3, len, 2);
    acc = digest(acc, &w);
    store(&mut s, 0, len, 0, &w);
    store(&mut s, 2, len, n, &v);
    acc = digest(acc, &load(&s, 0, len, len - 3));
    acc = digest(acc, &load(&s, 0, len, len - 1));
    acc = digest(acc, &load(&s, 1, 4, 0));
    acc = digest(acc, &vec![0; n]);
    acc = digest(acc, &load(&s, 1, len, 3));
    store(&mut s, 0, len, len - 2, &v);
    store(&mut s, 0, len, 4, &w);
    store(&mut s, 0, 3, 1, &w);
    for x in &s {
        acc = mix(acc, *x);
    }
    acc
}

#[test]
fn slice_loads_and_stores_every_lane_type() {
    for lane in &LANES {
        let source = program(lane, lane.lanes, "Baseline128");
        differential(
            &format!("io_{}", lane.name),
            &source,
            model(lane, lane.lanes),
        );
    }
}

#[test]
fn wide_slice_loads_and_stores() {
    // 256- and 512-bit shapes are chunked 128-bit code; they need the
    // profile statically, which the checked dispatch provides.
    for lane in &LANES {
        for (factor, isa) in [(2, "Avx2"), (4, "Avx512")] {
            let n = lane.lanes * factor;
            let source = program(lane, n, isa);
            let c = accepted(&source);
            let ran = Runtime::new(&c.world).run("main", vec![]).unwrap();
            let expected = if ran.to_string() == "1" {
                1
            } else {
                model(lane, n)
            };
            differential(&format!("io_{}_{n}", lane.name), &source, expected);
        }
    }
}

#[test]
fn unmasked_accesses_check_their_interval_once() {
    for (call, what) in [
        (
            "let v = Vec::load[u32, 4](in s, 7); Vec::extract(v, 0)",
            "load",
        ),
        (
            "Vec::store(inout s, 8, Vec::splat[u32, 4](1)); s[0]",
            "store",
        ),
        (
            "let v = Vec::load[u32, 4](in s, 18446744073709551615); Vec::extract(v, 0)",
            "load_past_u64",
        ),
    ] {
        let source = format!(
            "fn main() -> u32 eff [Isa[Baseline128], Fail[RuntimeError]] {{
    let mut s: ChunkSeq[u32] = ChunkSeq::new[u32]();
    for i in 0..10 {{ s.push(wrapping_u32(i)); }}
    {call}
}}"
        );
        let source = source.replace("fn main() -> u32", "fn run() -> u32")
            + "\nfn main() -> u32 { if Isa::supported[Baseline128]() { handle Fail[RuntimeError] in { run() } with { return x => x, raise(e, k) => match e { RuntimeError::IndexOutOfBounds => 77, RuntimeError::DivisionByZero => 1, }, } } else { 77 } }";
        differential(what, &source, 77);
    }
    // A failed store writes nothing.
    let source = "fn put(inout s: Slice[u32]) eff [Isa[Baseline128], Fail[RuntimeError]] { Vec::store(inout s, 8, Vec::splat[u32, 4](9)) }
fn main() -> u64 { if Isa::supported[Baseline128]() { handle Fail[RuntimeError] in { run() } with { return x => x, raise(e, k) => 5, } } else { 1045 } }
fn run() -> u64 eff [Isa[Baseline128], Fail[RuntimeError]] {
    let mut s: ChunkSeq[u32] = ChunkSeq::new[u32]();
    for i in 0..10 { s.push(wrapping_u32(i)); }
    let r: u64 = handle Fail[RuntimeError] in { put(inout s.as_slice_mut()); 0 } with { return x => x, raise(e, k) => 1, };
    let mut sum: u64 = 0;
    for i in 0..10 { sum = wrapping_add(sum, widen_u64(s[wrapping_u64(i)])); }
    wrapping_add(wrapping_mul(r, 1000), sum)
}";
    differential("failed_store", source, 1000 + 45);
}

#[test]
fn slice_access_typing() {
    let isa = "eff [Isa[Baseline128], Fail[RuntimeError]]";
    rejected(
        &format!("fn f(in s: Slice[u32]) -> Vec[u64, 2] {isa} {{ Vec::load[u64, 2](in s, 0) }}"),
        "E1402",
        "moves u64 lanes, but the sequence holds 'u32'",
    );
    rejected(
        &format!("fn f(in s: Slice[u32]) {isa} {{ Vec::store(s, 0, Vec::splat[u32, 4](1)) }}"),
        "E0308",
        "pass it by 'inout'",
    );
    rejected(
        &format!("fn f(in s: Slice[u32]) -> u64 {isa} {{ let v = Vec::load(in s, 0); 0 }}"),
        "E1401",
        "cannot infer the shape",
    );
    rejected(
        "fn f(in s: Slice[u32]) -> Vec[u32, 8] eff [Isa[Baseline128], Fail[RuntimeError]] { Vec::load[u32, 8](in s, 0) }",
        "E1403",
        "requires 'eff [Isa[Avx2]]'",
    );
    // The expected type gives the shape; the string form still works.
    accepted(&format!(
        "fn f(in s: Slice[f32]) -> Vec[f32, 4] {isa} {{ Vec::load(in s, 0) }}"
    ));
    accepted(&format!(
        "fn f(in s: ChunkSeq[f64]) -> Vec[f64, 2] {isa} {{ Vec::load_masked(in s, 0) }}"
    ));
    accepted(&format!(
        "fn f(s: String) -> Vec[u8, 16] {isa} {{ Vec::load_masked[u8, 16](s, 0) }}"
    ));
}
