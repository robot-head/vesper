//! Module constants `const NAME: T = e;` (manuscript §2.2, P3b gaps lane 6,
//! #1091): scalars, copy structs, tuples and array tables, read the same in
//! the evaluator, the JIT and objects, with constant tables in read-only
//! data natively.
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
            "vesper-const-items-{}-{}",
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

fn rejects(source: &str, code: &str) {
    let c = checked(source);
    assert!(
        c.diagnostics.iter().any(|d| d.code == code),
        "expected {code} for {source}: {:?}",
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

#[test]
fn scalar_struct_and_table_constants() {
    differential(
        "constants",
        "copy struct Range { lo: u64, hi: u64 }
const LIMIT: u64 = 3;
const NEG: i64 = -5;
const ON: bool = true;
const SPAN: Range = Range { lo: 10, hi: 20 };
const PAIR: (u64, i64) = (7, -2);
const TABLE: [u64; 5] = [1, 10, 100, 1000, 10000];
const RANGES: [Range; 2] = [Range { lo: 1, hi: 2 }, SPAN];
fn look(i: u64) -> u64 { if i < 5 { TABLE[i] } else { 0 } }
fn main() -> u64 {
    let a = wrapping_add(LIMIT, look(4));
    let b = if ON { SPAN.hi } else { 0 };
    let (p0, p1) = PAIR;
    let c = wrapping_add(p0, wrapping_u64(wrapping_add(NEG, wrapping_sub(10, p1))));
    let d = wrapping_add(RANGES[1].lo, RANGES[0].hi);
    wrapping_add(wrapping_add(a, b), wrapping_add(c, d))
}",
        3 + 10000 + 20 + (7 + 7) + (10 + 2),
    );
}

#[test]
fn a_table_copied_into_a_mutable_local_is_not_shared() {
    differential(
        "copied",
        "const TABLE: [u64; 4] = [1, 2, 3, 4];
fn main() -> u64 {
    let mut t = TABLE;
    t[0] = 77;
    let u = TABLE;
    wrapping_add(wrapping_add(t[0], TABLE[0]), u[0])
}",
        77 + 1 + 1,
    );
}

#[test]
fn a_large_table_is_read_in_place_everywhere() {
    let values = (0..256u64).map(|i| (i * 37) % 251).collect::<Vec<_>>();
    let table = values
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    let source = format!(
        "const TABLE: [u64; 256] = [{table}];
fn sum() -> u64 {{
    let mut s: u64 = 0;
    for i in 0..256 {{
        s = wrapping_add(s, TABLE[i]);
    }}
    s
}}
fn closure() -> u64 {{
    let f = |i: u64| -> u64 {{ if i < 256 {{ TABLE[i] }} else {{ 0 }} }};
    f(255)
}}
fn main() -> u64 {{ wrapping_add(sum(), closure()) }}"
    );
    differential(
        "large",
        &source,
        values.iter().sum::<u64>() + values[255],
    );
    // Natively the reads are loads from read-only data, not a rebuilt table.
    let c = accepted(&source);
    let object = native::object(&c.world, "main").unwrap().0;
    let needle = values[1..9]
        .iter()
        .flat_map(|v| v.to_ne_bytes())
        .collect::<Vec<_>>();
    assert!(
        object.windows(needle.len()).any(|w| w == needle.as_slice()),
        "the table is in the object's data"
    );
}

#[test]
fn constants_refer_to_constants() {
    differential(
        "chained",
        "const A: [u64; 2] = [5, 6];
const B: [[u64; 2]; 2] = [A, [7, 8]];
const C: u64 = 4;
fn main() -> u64 {
    let i: u64 = 1;
    wrapping_add(B[i][0], wrapping_add(B[0][i], C))
}",
        7 + 6 + 4,
    );
}

#[test]
fn initializers_must_be_constant_and_names_unique() {
    rejects(
        "fn f() -> u64 { 1 } const A: u64 = f(); fn main() -> u64 { A }",
        vesper::arrays::NOT_CONSTANT,
    );
    rejects(
        "const Some: u64 = 1; fn main() -> u64 { 0 }",
        vesper::arrays::CONST_NAME,
    );
    rejects("const A: u64 = true; fn main() -> u64 { A }", "E0200");
    // A constant is a value, not a place.
    rejects(
        "const A: [u64; 2] = [1, 2]; fn main() -> u64 { A[0] = 3; 0 }",
        "E0300",
    );
}
