//! `ChunkSeq[T]` and `Slice[T]` (§13, port lane F1): evaluator, Cranelift JIT
//! and AOT executables agree; bounds failures raise `Fail[RuntimeError]`;
//! interval loans are disjoint by construction; element modes are enforced.
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
            "vesper-seq-test-{}-{}",
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
            .join("tests/fixtures/seq")
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

#[test]
fn growth_reads_writes_pop_and_extend_agree() {
    let expected = {
        let mut s: Vec<u64> = (0..100).map(|i| i * 3).collect();
        s[7] = 1000;
        let total: u64 = s.iter().sum();
        let head: u64 = s[10..14].iter().sum();
        total + 250 + 9 + (11 + 12) + 1 + head
    };
    assert_eq!(
        differential("growth", &fixture("growth")),
        expected.to_string()
    );
}

#[test]
fn disjoint_inout_sub_slices_agree() {
    // [0..4] scaled by 10, [4..10] by 100.
    let s: Vec<u64> = (0..10)
        .map(|i| if i < 4 { i * 10 } else { i * 100 })
        .collect();
    let all: u64 = s.iter().sum();
    let both: u64 = s[0..6].iter().sum::<u64>() + s[3..10].iter().sum::<u64>();
    let expected = all + both + s[9];
    assert_eq!(
        differential("disjoint_slices", &fixture("disjoint_slices")),
        expected.to_string()
    );
}

#[test]
fn linear_and_affine_elements_agree() {
    // pop order 3, 2, 1 -> 321, plus 40.
    assert_eq!(
        differential("linear_elements", &fixture("linear_elements")),
        "361"
    );
}

#[test]
fn out_of_bounds_raises_index_failure_on_every_backend() {
    assert_eq!(
        differential("out_of_bounds", &fixture("out_of_bounds")),
        (6 + 100 + 30).to_string()
    );
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

const PRELUDE: &str = "linear struct Ticket { id: u64 }\nfn take(inout a: Slice[u64], inout b: Slice[u64]) { () }\nfn look(in a: Slice[u64], in b: Slice[u64]) { () }\nfn mixed(inout a: Slice[u64], in b: Slice[u64]) { () }\n";

fn with_prelude(body: &str) -> String {
    format!("{PRELUDE}{body}")
}

#[test]
fn overlapping_interval_loans_are_rejected() {
    // Constant intervals that overlap.
    rejected(
        &with_prelude(
            "fn main() -> u64 { let mut s: ChunkSeq[u64] = ChunkSeq::new[u64](); take(inout s[0..3], inout s[2..5]); 0 }",
        ),
        "E0301",
        "may overlap",
    );
    // Bounds that are not shared or known.
    rejected(
        &with_prelude(
            "fn f(a: u64, b: u64) -> u64 { let mut s: ChunkSeq[u64] = ChunkSeq::new[u64](); take(inout s[0..a], inout s[b..4]); 0 }",
        ),
        "E0301",
        "may overlap",
    );
    // The same interval twice.
    rejected(
        &with_prelude(
            "fn f(m: u64) -> u64 { let mut s: ChunkSeq[u64] = ChunkSeq::new[u64](); take(inout s[0..m], inout s[0..m]); 0 }",
        ),
        "E0301",
        "overlapping arguments",
    );
    // A whole view and an interval.
    rejected(
        &with_prelude(
            "fn main() -> u64 { let mut s: ChunkSeq[u64] = ChunkSeq::new[u64](); take(inout s.as_slice_mut(), inout s[0..1]); 0 }",
        ),
        "E0301",
        "may overlap",
    );
    // An inout view and a shared view of the same interval.
    rejected(
        &with_prelude(
            "fn main() -> u64 { let mut s: ChunkSeq[u64] = ChunkSeq::new[u64](); mixed(inout s[0..2], in s[1..3]); 0 }",
        ),
        "E0301",
        "may overlap",
    );
    // An interval and the whole sequence place.
    rejected(
        "fn g(inout a: Slice[u64], in s: ChunkSeq[u64]) { () }\nfn main() -> u64 { let mut s: ChunkSeq[u64] = ChunkSeq::new[u64](); g(inout s[0..1], in s); 0 }",
        "E0301",
        "overlapping arguments",
    );
    // Shared views may overlap; shared-bound splits are disjoint.
    accepted(&with_prelude(
        "fn f(m: u64, n: u64) -> u64 { let mut s: ChunkSeq[u64] = ChunkSeq::new[u64](); look(in s[0..3], in s[1..4]); take(inout s[0..m], inout s[m..n]); take(inout s[0..2], inout s[2..4]); 0 }",
    ));
    // Slicing an `in` slice for inout is a mutation of a shared parameter.
    rejected(
        &with_prelude("fn h(in x: Slice[u64]) { take(inout x[0..1], inout x[1..2]) }"),
        "E0307",
        "shared parameter",
    );
    // Sub-slices of an inout slice parameter split the same way.
    accepted(&with_prelude(
        "fn h(inout x: Slice[u64], m: u64) { let n = x.len(); take(inout x[0..m], inout x[m..n]) }",
    ));
}

#[test]
fn slices_are_second_class() {
    rejected(
        "fn main() -> u64 { let mut s: ChunkSeq[u64] = ChunkSeq::new[u64](); let v = s[0..1]; 0 }",
        "E1521",
        "second-class",
    );
    rejected(
        "fn main() -> u64 { let mut s: ChunkSeq[u64] = ChunkSeq::new[u64](); let v = s.as_slice(); 0 }",
        "E1521",
        "second-class",
    );
    rejected(
        "fn f(in x: Slice[u64]) -> Slice[u64] { x }",
        "E1520",
        "result",
    );
    rejected("struct Holder { view: Slice[u64] }", "E1520", "field");
    rejected("fn f(sink x: Slice[u64]) { () }", "E1520", "sink");
    rejected("fn f(x: (Slice[u64], u64)) { () }", "E1520", "parameter");
    rejected(
        "fn main() -> u64 { let mut s: ChunkSeq[Slice[u64]] = ChunkSeq::new[Slice[u64]](); 0 }",
        "E1520",
        "element",
    );
    rejected(
        "fn main() -> u64 { let mut s: ChunkSeq[fn() -> u64] = ChunkSeq::new[fn() -> u64](); 0 }",
        "E1524",
        "functions",
    );
}

#[test]
fn element_modes_are_enforced() {
    // A linear sequence that is not finished leaks its elements.
    rejected(
        &with_prelude(
            "fn main() -> u64 { let mut s: ChunkSeq[Ticket] = ChunkSeq::new[Ticket](); s.push(Ticket { id: 1 }); 0 }",
        ),
        "E0302",
        "must be consumed",
    );
    rejected(
        &with_prelude(
            "fn f(in s: ChunkSeq[Ticket]) -> u64 eff [Fail[RuntimeError]] { let Ticket { id } = s.get(0); id }",
        ),
        "E1522",
        "not copy",
    );
    rejected(
        &with_prelude(
            "fn f(in s: ChunkSeq[Ticket]) -> u64 eff [Fail[RuntimeError]] { let Ticket { id } = s[0]; id }",
        ),
        "E1522",
        "not copy",
    );
    rejected(
        &with_prelude("fn f(inout s: ChunkSeq[Ticket]) { s.clear() }"),
        "E1523",
        "linear",
    );
    rejected(
        &with_prelude("fn f(inout s: ChunkSeq[Ticket]) { s.truncate(0) }"),
        "E1523",
        "linear",
    );
    rejected(
        &with_prelude("fn f(inout s: ChunkSeq[Ticket]) -> bool { s.try_set(0, Ticket { id: 1 }) }"),
        "E1523",
        "linear",
    );
    rejected(
        "fn f(inout s: ChunkSeq[ChunkSeq[u64]], in t: ChunkSeq[ChunkSeq[u64]]) { s.extend(in t.as_slice()) }",
        "E1522",
        "not copy",
    );
    // Updating a shared sequence, and growing a sequence from itself.
    rejected(
        "fn f(in s: ChunkSeq[u64]) { s.push(1) }",
        "E0307",
        "shared parameter",
    );
    rejected(
        "fn f(inout s: ChunkSeq[u64]) { s.extend(in s[0..1]) }",
        "E0301",
        "",
    );
    // Element reads and finish raise Fail[RuntimeError].
    rejected(
        "fn f(in s: ChunkSeq[u64]) -> u64 { s.get(0) }",
        "E0401",
        "Fail",
    );
    rejected(
        "fn f(sink s: ChunkSeq[u64]) { ChunkSeq::finish(s) }",
        "E0401",
        "Fail",
    );
    // An abortable read cannot cross an inout parameter; try_get can.
    rejected(
        "fn f(inout s: ChunkSeq[u64]) -> u64 eff [Fail[RuntimeError]] { s.get(0) }",
        "E0402",
        "inout",
    );
    accepted(
        "fn f(inout s: ChunkSeq[u64]) -> u64 { match s.try_get(0) { Option::Some(v) => v, Option::None => 0 } }",
    );
    // Operations, type arguments and index types.
    rejected(
        "fn f(in s: ChunkSeq[u64]) -> u64 { s.front() }",
        "E1525",
        "unknown method",
    );
    rejected(
        "fn main() -> u64 { let s = ChunkSeq::new(); 0 }",
        "E0208",
        "infer",
    );
    rejected(
        "fn f(in s: ChunkSeq[u64], i: u32) -> u64 eff [Fail[RuntimeError]] { s[i] }",
        "E0200",
        "",
    );
}

#[test]
fn interval_faults_agree_across_backends() {
    let source = "fn total(in x: Slice[u64]) -> u64 { x.len() }\nfn main() -> u64 { let mut s: ChunkSeq[u64] = ChunkSeq::new[u64](); s.push(1); s.push(2); total(in s[1..5]) }";
    let c = accepted(source);
    let e = Runtime::new(&c.world).run("main", vec![]).unwrap_err();
    assert!(e.contains("out of bounds"), "evaluator: {e}");
    let j = native::jit(&c.world, "main").unwrap_err();
    assert!(j.contains("out of bounds"), "JIT: {j}");
    // A later JIT run on the same thread is unaffected.
    let ok = accepted(
        "fn main() -> u64 { let mut s: ChunkSeq[u64] = ChunkSeq::new[u64](); s.push(3); s.len() }",
    );
    assert_eq!(native::jit(&ok.world, "main").unwrap().to_string(), "1");
    if links_executables() {
        let (status, stderr) = executable("interval_fault", source);
        assert_eq!(status, Some(71), "{stderr}");
        assert!(stderr.contains("out of bounds"), "{stderr}");
    }
}

#[test]
fn capacity_overflow_is_a_fault() {
    let source = "fn main() -> u64 { let mut s: ChunkSeq[u64] = ChunkSeq::with_capacity[u64](2305843009213693952); s.push(1); s.len() }";
    let c = accepted(source);
    let e = Runtime::new(&c.world).run("main", vec![]).unwrap_err();
    assert!(e.contains("out of memory"), "evaluator: {e}");
    if links_executables() {
        let (status, stderr) = executable("capacity_overflow", source);
        assert_eq!(status, Some(71), "{stderr}");
        assert!(stderr.contains("out of memory"), "{stderr}");
    }
}

fn lean_compiler() -> Option<String> {
    let compiler = std::env::var("VESPER_LEAN").unwrap_or_else(|_| "lean".into());
    Command::new(&compiler)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|_| compiler)
}

#[test]
fn lean_extraction_models_sequences_as_arrays_and_replays() {
    let source = fixture("lean_model");
    // s[8] is out of bounds after the pop, so the handler answers 1000.
    assert_eq!(differential("lean_model", &source), "1000");
    let c = accepted(&source);
    let mut lean_src = lean::extract(&c.world).unwrap_or_else(|e| panic!("{e}"));
    assert!(lean_src.contains("Array"), "{lean_src}");
    assert!(!lean_src.contains("sorry"), "{lean_src}");
    lean_src.push_str("\n#eval Vesper.«main»\n");
    let Some(compiler) = lean_compiler() else {
        eprintln!("no Lean compiler: replay skipped");
        return;
    };
    let temp = Temp::new();
    let path = temp.0.join("seq.lean");
    fs::write(&path, &lean_src).unwrap();
    let out = Command::new(&compiler).arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "lean failed:\n{}\n{}\n{lean_src}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim().lines().last(),
        Some("1000")
    );

    // In bounds, the same program answers the sum.
    let in_bounds = source.replace("s.get(8)", "s.get(4)");
    let c = accepted(&in_bounds);
    let expected = differential("lean_model_in_bounds", &in_bounds);
    // [0, 50, 4, 6, 8] after the pop of 10: 68 + 10 + 50 + 3 + 8.
    assert_eq!(expected, "139");
    let mut lean_src = lean::extract(&c.world).unwrap_or_else(|e| panic!("{e}"));
    lean_src.push_str("\n#eval Vesper.«main»\n");
    fs::write(&path, &lean_src).unwrap();
    let out = Command::new(&compiler).arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim().lines().last(),
        Some("139")
    );
}

#[test]
fn lean_refuses_inout_slices() {
    let c = accepted(&fixture("disjoint_slices"));
    let e = lean::extract(&c.world).unwrap_err().to_string();
    assert!(e.contains("inout Slice"), "{e}");
}

#[test]
fn wasm_and_ebpf_refuse_sequences_with_typed_errors() {
    let c = accepted(&fixture("growth"));
    let Err(e) = vesper::wasm_backend::compile(&c.world, "main", &Default::default()) else {
        panic!("Wasm refuses ChunkSeq");
    };
    let text = e.to_string();
    assert!(
        text.contains("ChunkSeq") || text.contains("unsupported"),
        "{text}"
    );
    let source = "copy enum XdpAction { Aborted, Drop, Pass, Tx, Redirect }
fn main() -> XdpAction {
    let mut s: ChunkSeq[u64] = ChunkSeq::new[u64]();
    s.push(1);
    XdpAction::Pass
}";
    let c = accepted(source);
    let Err(e) = vesper::ebpf_backend::compile_world(&c.world, None, &Default::default()) else {
        panic!("eBPF refuses ChunkSeq");
    };
    assert!(e.to_string().contains("ChunkSeq"), "{e}");
}

/// Executables link with the MSVC toolchain on Windows hosts.
#[cfg(windows)]
#[test]
fn windows_executables_run_sequences() {
    for name in [
        "growth",
        "disjoint_slices",
        "linear_elements",
        "out_of_bounds",
    ] {
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

#[test]
fn review_findings_are_rejected() {
    let body = |s: &str| format!("fn mutate(inout a: Slice[u64]) {{ () }}\n{s}");
    // An `in` sequence cannot be lent out as an exclusive whole view.
    rejected(
        &body("fn f(in s: ChunkSeq[u64]) -> u64 { mutate(inout s.as_slice()); 0 }"),
        "E0307",
        "cannot mutate shared parameter",
    );
    // A bound that another argument mutates proves nothing.
    rejected(
        "fn bump(inout m: u64) -> u64 { 0 }\nfn take3(inout a: Slice[u64], x: u64, inout b: Slice[u64]) { () }\nfn main() -> u64 { let mut s: ChunkSeq[u64] = ChunkSeq::new[u64](); let m: u64 = 2; take3(inout s[0..m], bump(inout m), inout s[m..4]); 0 }",
        "E0301",
        "may overlap",
    );
    // A slice cannot be passed by sink into a generic parameter.
    rejected(
        "fn id[T](sink x: T) -> T { x }\nfn main() -> u64 { let mut s: ChunkSeq[u64] = ChunkSeq::new[u64](); let v = id(s[0..1]); 0 }",
        "E1521",
        "sink",
    );
    // @fip code cannot allocate a sequence.
    rejected(
        "@fip fn make() -> u64 { let mut s: ChunkSeq[u64] = ChunkSeq::new[u64](); 0 }",
        "E0801",
        "ChunkSeq::new",
    );
    // The JIT cannot hand a sequence back to the host.
    let c = accepted("fn main() -> ChunkSeq[u64] { ChunkSeq::new[u64]() }");
    let e = native::jit(&c.world, "main").unwrap_err();
    assert!(e.contains("cannot return a ChunkSeq"), "{e}");
}
