//! Drop imports of affine `.vsys` handles (§16.3, #1095 part 4):
//! `affine extern type Probe drop res_release;` releases a `Probe` through
//! the import wherever source code drops one without consuming it, and
//! code that may drop one performs `Ffi[Res]`.
//!
//! Every scenario counts the handles the library opened, closed
//! explicitly and released through the drop import: each handle is
//! closed or released exactly once on every path, in the JIT, in the
//! evaluator and (on Linux x86-64) in an object linked with a C library.
use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    process::{Command, Output},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use vesper::ffi_lowering::{self, FfiErrorKind, Handler, Imports};
use vesper::runtime::Value;

const VSYS: &str = r#"sys library Res (
    target = "@TARGET@", abi = "@ABI@",
    link = "static", name = "vesper_res", artifact = "blake3:@DIGEST@"
) {
    // A borrowed resource: dropping one releases it through res_release.
    affine extern type Probe drop res_release;
    fn res_open(n: u64) -> ForeignVal[Probe];
    fn res_maybe(n: u64) -> ForeignVal[Option[Probe]];
    fn res_read(in p: Probe) -> ForeignVal[u64];
    fn res_close(sink p: Probe) -> ForeignVal[u64];
    fn res_release(sink p: Probe) -> ForeignVal[Unit];
}
"#;

const SOURCE: &str = r#"
affine struct Holder {
    p: Probe,
    n: u64,
}

// `res_open` never returns NULL here.
fn open(n: u64) -> Probe eff [Ffi[Res]] {
    match Res::res_open(n) {
        Ok(p) => p,
        Err(_) => loop {},
    }
}

fn eat(sink p: Probe) -> u64 eff [Ffi[Res]] {
    1
}

// Dropped at the end of its scope.
pub fn scope() -> u64 eff [Ffi[Res]] {
    let p = open(1);
    Res::res_read(in p)
}

// Closed explicitly: nothing to release.
pub fn closed() -> u64 eff [Ffi[Res]] {
    let p = open(2);
    Res::res_close(p)
}

// Closed on one branch, released on the other.
fn branch(n: u64) -> u64 eff [Ffi[Res]] {
    let p = open(n);
    if n > 1 {
        Res::res_close(p)
    } else {
        5
    }
}
pub fn branches() -> u64 eff [Ffi[Res]] {
    wrapping_add(branch(1), branch(3))
}

// One handle per iteration.
fn looped_n(n: u64) -> u64 eff [Ffi[Res]] {
    let mut total: u64 = 0;
    for i in 0..n {
        let p = open(i);
        total = wrapping_add(total, Res::res_read(in p));
    }
    total
}
pub fn looped() -> u64 eff [Ffi[Res]] {
    looped_n(10)
}

// Overwriting a handle releases the old one.
pub fn reassigned() -> u64 eff [Ffi[Res]] {
    let mut p = open(1);
    p = open(2);
    Res::res_read(in p)
}

// A discarded call result.
pub fn discarded() -> u64 eff [Ffi[Res]] {
    open(3);
    0
}

// `Option[Probe]` values release the handle they hold.
pub fn optional() -> u64 eff [Ffi[Res]] {
    let some = Res::res_maybe(1);
    let none = Res::res_maybe(0);
    let read = match Res::res_maybe(4) {
        Some(p) => Res::res_read(in p),
        None => 100,
    };
    read
}

// A field of a struct and a destructured tuple item.
pub fn fields() -> u64 eff [Ffi[Res]] {
    let h = Holder {
        p: open(5),
        n: 1,
    };
    let (q, m) = (open(6), wrapping_add(h.n, 1));
    wrapping_add(h.n, m)
}

// An early return releases what the function still owns.
fn early(n: u64) -> u64 eff [Ffi[Res]] {
    let p = open(n);
    if n == 0 {
        return 7;
    }
    Res::res_read(in p)
}
pub fn early_exit() -> u64 eff [Ffi[Res]] {
    wrapping_add(early(0), early(8))
}

// `let _ =` discards (and releases) a handle.
pub fn wildcard() -> u64 eff [Ffi[Res]] {
    let _ = open(11);
    0
}

// A handle argument evaluated before a branching argument is the call's.
fn take(sink p: Probe, n: u64) -> u64 eff [Ffi[Res]] {
    wrapping_add(Res::res_close(p), n)
}
fn crossing(flag: bool) -> u64 eff [Ffi[Res]] {
    take(open(12), if flag { 2 } else { 3 })
}
pub fn cross_block() -> u64 eff [Ffi[Res]] {
    wrapping_add(crossing(true), crossing(false))
}

// A callee that takes the handle `sink` releases it.
pub fn callee() -> u64 eff [Ffi[Res]] {
    eat(open(9))
}
"#;

fn host_target() -> Option<(&'static str, &'static str)> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some(("x86_64-unknown-linux-gnu", "sysv64"))
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some(("aarch64-unknown-linux-gnu", "aapcs64"))
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some(("aarch64-apple-darwin", "darwin64"))
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some(("x86_64-pc-windows-msvc", "win64"))
    } else {
        None
    }
}
const ZERO: &str = "0000000000000000000000000000000000000000000000000000000000000000";
fn vsys(target: (&str, &str), digest: &str) -> String {
    VSYS.replace("@TARGET@", target.0)
        .replace("@ABI@", target.1)
        .replace("@DIGEST@", digest)
}
fn imports(target: (&str, &str), digest: &str) -> Imports {
    let mut imports = Imports::new();
    imports
        .add(&vsys(target, digest), Handler::Direct)
        .unwrap_or_else(|e| panic!("{e}"));
    imports
}
fn checked(source: &str, target: (&str, &str)) -> ffi_lowering::ForeignChecked {
    ffi_lowering::check_source(source, &imports(target, ZERO)).unwrap_or_else(|e| panic!("{e}"))
}
fn word(v: &Value) -> u64 {
    match v {
        Value::Int(n) => *n as u64,
        Value::UInt(n, _) => *n,
        other => panic!("not a word: {other:?}"),
    }
}

/// Each entry's value and the handles it releases through the drop import
/// (every other handle it opens is closed explicitly).
fn expected() -> BTreeMap<&'static str, (u64, u64)> {
    // res_read(p) is the n the handle was opened with, plus 100.
    BTreeMap::from([
        ("scope", (101, 1)),
        ("closed", (2, 0)),
        // branch(1) releases, branch(3) closes (res_close returns n).
        ("branches", (5 + 3, 1)),
        ("looped", ((0..10).map(|i| i + 100).sum(), 10)),
        ("reassigned", (102, 2)),
        ("discarded", (0, 1)),
        // some and the matched handle are released; none holds nothing.
        ("optional", (104, 2)),
        ("fields", (3, 2)),
        ("early_exit", (7 + 108, 2)),
        ("callee", (1, 1)),
        ("wildcard", (0, 1)),
        // take closes both: 12 + 2 and 12 + 3.
        ("cross_block", (29, 0)),
    ])
}

// ---------------------------------------------------------------------------
// The library, in Rust for the JIT.

static OPENED: AtomicU64 = AtomicU64::new(0);
static CLOSED: AtomicU64 = AtomicU64::new(0);
static RELEASED: AtomicU64 = AtomicU64::new(0);
static JIT: Mutex<()> = Mutex::new(());

extern "C" fn res_open(n: u64) -> *mut u64 {
    OPENED.fetch_add(1, Ordering::SeqCst);
    Box::into_raw(Box::new(n))
}
extern "C" fn res_maybe(n: u64) -> *mut u64 {
    if n == 0 {
        std::ptr::null_mut()
    } else {
        res_open(n)
    }
}
extern "C" fn res_read(p: *const u64) -> u64 {
    // SAFETY: `p` came from res_open and is neither closed nor released.
    unsafe { *p + 100 }
}
extern "C" fn res_close(p: *mut u64) -> u64 {
    CLOSED.fetch_add(1, Ordering::SeqCst);
    // SAFETY: the sink import consumes the handle exactly once.
    *unsafe { Box::from_raw(p) }
}
extern "C" fn res_release(p: *mut u64) {
    RELEASED.fetch_add(1, Ordering::SeqCst);
    // SAFETY: the drop import consumes the handle exactly once.
    drop(unsafe { Box::from_raw(p) });
}
fn symbols() -> Vec<(&'static str, *const u8)> {
    vec![
        ("res_open", res_open as *const u8),
        ("res_maybe", res_maybe as *const u8),
        ("res_read", res_read as *const u8),
        ("res_close", res_close as *const u8),
        ("res_release", res_release as *const u8),
    ]
}

#[test]
fn jit_releases_every_dropped_handle_once() {
    let Some(target) = host_target() else {
        eprintln!("no .vsys target for this host; JIT evidence unavailable");
        return;
    };
    let _lock = JIT.lock().unwrap();
    let checked = checked(SOURCE, target);
    let symbols = symbols();
    for (entry, (value, released)) in expected() {
        let opened = OPENED.load(Ordering::SeqCst);
        let closed = CLOSED.load(Ordering::SeqCst);
        let was = RELEASED.load(Ordering::SeqCst);
        let got = checked
            .jit(entry, &symbols)
            .unwrap_or_else(|e| panic!("{entry}: {e}"));
        assert_eq!(word(&got), value, "{entry}");
        let opened = OPENED.load(Ordering::SeqCst) - opened;
        let closed = CLOSED.load(Ordering::SeqCst) - closed;
        let rel = RELEASED.load(Ordering::SeqCst) - was;
        assert_eq!(rel, released, "{entry}: released");
        assert_eq!(opened, closed + rel, "{entry}: every handle ends once");
    }
}

// ---------------------------------------------------------------------------
// The evaluator, with a Rust stand-in.

#[derive(Default)]
struct EvalHost {
    live: Vec<Option<u64>>,
    opened: u64,
    closed: u64,
    released: u64,
}
impl EvalHost {
    fn call(&mut self, call: &mut ffi_lowering::EvalCall<'_, '_>) -> Result<Value, String> {
        let args = call.arguments().to_vec();
        let handle = |v: &Value| -> Result<usize, String> {
            let w = ffi_lowering::handle_word(v).ok_or("not a handle")?;
            Ok(w as usize - 1)
        };
        let open = |host: &mut Self, n: u64| {
            host.opened += 1;
            host.live.push(Some(n));
            ffi_lowering::handle_value("Probe", host.live.len() as u64)
        };
        Ok(match call.symbol.as_str() {
            // `ForeignVal[Probe]`: a NULL-free handle is `Ok`.
            "res_open" => Value::Data {
                ty: "Result".into(),
                constructor: "Ok".into(),
                fields: BTreeMap::from([("0".to_string(), open(self, word(&args[0])))]),
                resource_identity: None,
            },
            "res_maybe" => {
                let n = word(&args[0]);
                if n == 0 {
                    Value::Data {
                        ty: "Option".into(),
                        constructor: "None".into(),
                        fields: BTreeMap::new(),
                        resource_identity: None,
                    }
                } else {
                    Value::Data {
                        ty: "Option".into(),
                        constructor: "Some".into(),
                        fields: BTreeMap::from([("0".to_string(), open(self, n))]),
                        resource_identity: None,
                    }
                }
            }
            "res_read" => {
                let n = self.live[handle(&args[0])?].ok_or("use after release")?;
                Value::UInt(n + 100, 64)
            }
            "res_close" => {
                let n = self.live[handle(&args[0])?].take().ok_or("double close")?;
                self.closed += 1;
                Value::UInt(n, 64)
            }
            "res_release" => {
                self.live[handle(&args[0])?]
                    .take()
                    .ok_or("double release")?;
                self.released += 1;
                Value::Tuple(vec![])
            }
            other => return Err(format!("unknown symbol {other}")),
        })
    }
}

#[test]
fn evaluator_releases_every_dropped_handle_once() {
    let checked = checked(SOURCE, ("x86_64-unknown-linux-gnu", "sysv64"));
    for (entry, (value, released)) in expected() {
        let mut host = EvalHost::default();
        let got = checked
            .evaluate(entry, |call| host.call(call))
            .unwrap_or_else(|e| panic!("{entry}: {e}"));
        assert_eq!(word(&got), value, "{entry}");
        assert_eq!(host.released, released, "{entry}: released");
        assert_eq!(host.opened, host.closed + host.released, "{entry}");
        assert!(host.live.iter().all(Option::is_none), "{entry}");
    }
}

#[test]
fn objects_for_both_architectures() {
    for (triple, abi) in [
        ("x86_64-unknown-linux-gnu", "sysv64"),
        ("aarch64-unknown-linux-gnu", "aapcs64"),
    ] {
        let checked = checked(SOURCE, (triple, abi));
        for entry in expected().keys() {
            let object = checked
                .object(entry, Some(triple))
                .unwrap_or_else(|e| panic!("{triple} {entry}: {e}"));
            assert!(!object.bytes.is_empty());
            assert_eq!(object.link.len(), 1, "{triple} {entry}");
        }
    }
}

// ---------------------------------------------------------------------------
// Linked with a C library and run.

const LIBRARY_C: &str = r#"#include <stdint.h>
#include <stdlib.h>
uint64_t res_opened, res_closed, res_released;
uint64_t *res_open(uint64_t n) { uint64_t *p = malloc(8); *p = n; res_opened++; return p; }
uint64_t *res_maybe(uint64_t n) { return n == 0 ? NULL : res_open(n); }
uint64_t res_read(const uint64_t *p) { return *p + 100; }
uint64_t res_close(uint64_t *p) { uint64_t n = *p; free(p); res_closed++; return n; }
void res_release(uint64_t *p) { free(p); res_released++; }
"#;
const HOST_C: &str = r#"#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
extern uint64_t res_opened, res_closed, res_released;
uint64_t ENTRY(void);
int main(void) {
    uint64_t v = ENTRY();
    printf("%" PRIu64 " %" PRIu64 " %" PRIu64 " %" PRIu64 "\n", v, res_opened, res_closed, res_released);
    return 0;
}
"#;
fn run(command: &mut Command) -> Output {
    let output = command.output().expect("run native tool");
    assert!(
        output.status.success(),
        "command {command:?}\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
fn linked_objects_release_every_dropped_handle_once() {
    if !(cfg!(target_os = "linux") && cfg!(target_arch = "x86_64")) {
        eprintln!("object fixtures need a Linux x86-64 host; object evidence unavailable");
        return;
    }
    for tool in ["cc", "ar"] {
        if Command::new(tool).arg("--version").output().is_err() {
            eprintln!("object fixtures unavailable: missing {tool}");
            return;
        }
    }
    let dir: PathBuf =
        std::env::temp_dir().join(format!("vesper-ffi-affine-drops-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("res.c"), LIBRARY_C).unwrap();
    fs::write(dir.join("host.c"), HOST_C).unwrap();
    run(Command::new("cc")
        .args(["-std=c11", "-Wall", "-Werror", "-O2", "-c"])
        .arg(dir.join("res.c"))
        .arg("-o")
        .arg(dir.join("res.o")));
    let archive = dir.join("libvesper_res.a");
    run(Command::new("ar")
        .arg("crs")
        .arg(&archive)
        .arg(dir.join("res.o")));
    let digest = blake3::hash(&fs::read(&archive).unwrap()).to_hex();
    let checked = ffi_lowering::check_source(
        SOURCE,
        &imports(("x86_64-unknown-linux-gnu", "sysv64"), digest.as_str()),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    for (entry, (value, released)) in expected() {
        let object = checked
            .object(entry, Some("x86_64-unknown-linux-gnu"))
            .unwrap_or_else(|e| panic!("{entry}: {e}"));
        let path = dir.join(format!("{entry}.o"));
        fs::write(&path, &object.bytes).unwrap();
        let exe = dir.join(entry);
        run(Command::new("cc")
            .arg(format!("-DENTRY={entry}"))
            .arg(dir.join("host.c"))
            .arg(&path)
            .arg(&archive)
            .arg("-o")
            .arg(&exe));
        let out = run(&mut Command::new(&exe));
        let stdout = String::from_utf8_lossy(&out.stdout);
        let fields: Vec<u64> = stdout
            .split_whitespace()
            .map(|f| f.parse().unwrap())
            .collect();
        let [v, opened, closed, rel] = fields[..] else {
            panic!("{entry}: unexpected output {stdout}");
        };
        assert_eq!(v, value, "{entry}");
        assert_eq!(rel, released, "{entry}: released");
        assert_eq!(opened, closed + rel, "{entry}");
    }
    let _ = fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Diagnostics.

fn refused(source: &str) -> ffi_lowering::FfiError {
    match ffi_lowering::check_source(
        source,
        &imports(("x86_64-unknown-linux-gnu", "sysv64"), ZERO),
    ) {
        Ok(_) => panic!("accepted:\n{source}"),
        Err(e) => e,
    }
}
fn codes(e: &ffi_lowering::FfiError) -> Vec<&str> {
    e.diagnostics.iter().map(|d| d.code.as_str()).collect()
}

#[test]
fn dropping_a_handle_needs_its_library_in_the_row() {
    let e = refused("fn f(sink p: Probe) -> u64 { 1 }");
    assert_eq!(e.kind, FfiErrorKind::Check, "{e}");
    assert!(codes(&e).contains(&"E0401"), "{e}");
    assert!(e.to_string().contains("Ffi[Res]"), "{e}");
    // Lending the handle drops nothing, so no row is needed.
    ffi_lowering::check_source(
        "fn f(in p: Probe) -> u64 { 1 }",
        &imports(("x86_64-unknown-linux-gnu", "sysv64"), ZERO),
    )
    .unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn other_aggregates_holding_a_handle_are_not_dropped_whole() {
    // A discarded `ForeignVal[Probe]` is a `Result[Probe, ForeignInvalid]`
    // temporary: only `Probe` and `Option[Probe]` are released implicitly.
    let e = refused("fn f() -> u64 eff [Ffi[Res]] { Res::res_open(3); 0 }");
    assert_eq!(e.kind, FfiErrorKind::Check, "{e}");
    assert!(
        e.message
            .contains("only the handle itself or an `Option` of it"),
        "{e}"
    );
}

#[test]
fn a_live_handle_may_not_cross_an_abortable_effect() {
    let e = refused(
        "copy struct Bad { n: u64 }\n\
         fn risky(n: u64) -> u64 eff [Fail[Bad]] { if n > 3 { Fail::raise(Bad { n: n }) } else { n } }\n\
         fn f(sink p: Probe) -> u64 eff [Ffi[Res], Fail[Bad]] {\n\
             let n = risky(4);\n\
             wrapping_add(Res::res_read(in p), n)\n\
         }",
    );
    assert!(codes(&e).contains(&"E0402"), "{e}");
    // Closed before the checkpoint, it is fine.
    ffi_lowering::check_source(
        "copy struct Bad { n: u64 }\n\
         fn risky(n: u64) -> u64 eff [Fail[Bad]] { if n > 3 { Fail::raise(Bad { n: n }) } else { n } }\n\
         fn f(sink p: Probe) -> u64 eff [Ffi[Res], Fail[Bad]] {\n\
             let m = Res::res_close(p);\n\
             wrapping_add(risky(4), m)\n\
         }",
        &imports(("x86_64-unknown-linux-gnu", "sysv64"), ZERO),
    )
    .unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn a_handle_consumed_inside_a_handled_body_that_may_abort_is_refused() {
    // The body may abort before closing `p`; the handle expression would
    // then leave `p` neither closed nor released.
    let e = refused(
        "copy struct Bad { n: u64 }\n\
         fn risky(n: u64) -> u64 eff [Fail[Bad]] { if n > 3 { Fail::raise(Bad { n: n }) } else { n } }\n\
         fn f(sink p: Probe) -> u64 eff [Ffi[Res]] {\n\
             handle Fail[Bad] in { let n = risky(4); Res::res_close(p) } with { return v => v, raise(e, k) => 0, }\n\
         }",
    );
    assert!(codes(&e).contains(&"E0402"), "{e}");
}

#[test]
fn a_struct_whose_handle_was_closed_may_cross_an_abortable_effect() {
    ffi_lowering::check_source(
        "affine struct Holder { p: Probe, n: u64 }\n\
         copy struct Bad { n: u64 }\n\
         fn risky(n: u64) -> u64 eff [Fail[Bad]] { if n > 3 { Fail::raise(Bad { n: n }) } else { n } }\n\
         fn f(sink h: Holder) -> u64 eff [Ffi[Res], Fail[Bad]] {\n\
             let c = Res::res_close(h.p);\n\
             wrapping_add(risky(c), h.n)\n\
         }",
        &imports(("x86_64-unknown-linux-gnu", "sysv64"), ZERO),
    )
    .unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn handles_that_no_drop_point_can_release_are_refused() {
    // `_` inside a destructuring pattern.
    let e = refused(
        "affine struct Holder { p: Probe, n: u64 }\n\
         fn f(sink h: Holder) -> u64 eff [Ffi[Res]] { let Holder { p: _, n: n } = h; n }",
    );
    assert!(e.message.contains("bind it to a name"), "{e}");
    // A handle moved into a closure.
    let e = refused(
        "fn f(sink p: Probe) -> u64 eff [Ffi[Res]] {\n\
             let c = || eff [Ffi[Res]] { Res::res_close(p) };\n\
             1\n\
         }",
    );
    assert!(e.message.contains("into a closure"), "{e}");
}

#[test]
fn drop_import_declarations_are_validated() {
    let target = ("x86_64-unknown-linux-gnu", "sysv64");
    let bad = |from: &str, to: &str| {
        let text = vsys(target, ZERO).replace(from, to);
        Imports::new()
            .add(&text, Handler::Direct)
            .expect_err(&format!("accepted {to}"))
            .to_string()
    };
    let e = bad(
        "affine extern type Probe drop res_release;",
        "linear extern type Probe drop res_release;",
    );
    assert!(
        e.contains("only an affine extern type declares a drop import"),
        "{e}"
    );
    let e = bad(
        "fn res_release(sink p: Probe) -> ForeignVal[Unit];",
        "fn res_release(in p: Probe) -> ForeignVal[Unit];",
    );
    assert!(e.contains("must be declared as"), "{e}");
    let e = bad(
        "fn res_release(sink p: Probe) -> ForeignVal[Unit];",
        "fn res_release(sink p: Probe) -> ForeignVal[u64];",
    );
    assert!(e.contains("must be declared as"), "{e}");
    let e = bad("drop res_release;", "drop res_missing;");
    assert!(e.contains("must be declared as"), "{e}");
    // Without a drop import an affine handle still drops silently.
    let plain = vsys(target, ZERO).replace(" drop res_release", "");
    let mut imports = Imports::new();
    imports.add(&plain, Handler::Direct).unwrap();
    ffi_lowering::check_source("fn f(sink p: Probe) -> u64 { 1 }", &imports)
        .unwrap_or_else(|e| panic!("{e}"));
}
