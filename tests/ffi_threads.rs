//! Threads through `.vsys` (graphics gap 8): `pthread_create` takes a
//! `retain` Vesper function and runs it on a new thread, and
//! `pthread_join` hands back its result. Nothing here is new compiler
//! support: it is the callback types of §16.3 applied to libpthread.
use vesper::ffi_lowering::{self, Handler, Imports};

unsafe extern "C" {
    fn pthread_create(
        thread: *mut u64,
        attr: *const u8,
        start: extern "C" fn(usize) -> usize,
        arg: usize,
    ) -> i32;
    fn pthread_join(thread: u64, retval: *mut usize) -> i32;
}

fn target() -> (&'static str, &'static str) {
    if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        ("aarch64-unknown-linux-gnu", "aapcs64")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        ("aarch64-apple-darwin", "darwin64")
    } else {
        ("x86_64-unknown-linux-gnu", "sysv64")
    }
}

fn library() -> String {
    let (t, a) = target();
    format!(
        "sys library Pthread (target = \"{t}\", abi = \"{a}\", link = \"dynamic\", name = \"pthread\", \
         artifact = \"blake3:{}\") {{
    callback Start(arg: usize) -> ForeignVal[usize];
    fn pthread_create(nocapture inout thread: u64, nocapture in attr: Option[u64], retain start: Start, arg: usize) -> ForeignVal[i32];
    fn pthread_join(thread: u64, nocapture inout retval: usize) -> ForeignVal[i32];
}}\n",
        "0".repeat(64)
    )
}

const SOURCE: &str = "\
fn work(n: usize) -> usize {
    let mut sum: usize = 0;
    for i in 0..n {
        sum = wrapping_add(sum, wrapping_mul(i, i));
    }
    sum
}
fn spawn(n: usize) -> u64 eff [Ffi[Pthread]] {
    let mut thread: u64 = 0;
    let none: Option[u64] = Option::None;
    if Pthread::pthread_create(inout thread, in none, work, n) != 0 { return 0; }
    thread
}
fn join(thread: u64) -> usize eff [Ffi[Pthread]] {
    let mut result: usize = 0;
    if Pthread::pthread_join(thread, inout result) != 0 { return 0; }
    result
}
pub fn main() -> u64 eff [Ffi[Pthread]] {
    let a = spawn(10);
    let b = spawn(100);
    let c = spawn(1000);
    let sum = wrapping_add(wrapping_add(join(a), join(b)), join(c));
    widen_u64(sum)
}
";

#[test]
fn vesper_functions_run_on_new_threads() {
    if !cfg!(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "aarch64"),
    )) {
        return;
    }
    let mut imports = Imports::new();
    imports.add(&library(), Handler::Direct).unwrap();
    let checked = ffi_lowering::check_source(SOURCE, &imports).unwrap_or_else(|e| panic!("{e:#?}"));
    let symbols = [
        ("pthread_create", pthread_create as *const u8),
        ("pthread_join", pthread_join as *const u8),
    ];
    let squares = |n: u64| (0..n).map(|i| i * i).sum::<u64>();
    let want = (squares(10) + squares(100) + squares(1000)).to_string();
    for optimize in [true, false] {
        let v = checked
            .jit_with_options("main", &symbols, optimize)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(v.to_string(), want, "optimize {optimize}");
    }
    let object = checked.object("main", Some(target().0)).unwrap();
    assert!(object.bytes.windows(14).any(|w| w == b"pthread_create"));
}

/// A fault the function raises on the C thread reaches the JIT entry's
/// result instead of vanishing with that thread.
#[test]
fn faults_on_c_threads_reach_the_entry() {
    if !cfg!(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "aarch64"),
    )) {
        return;
    }
    let source = SOURCE.replace(
        "fn work(n: usize) -> usize {",
        "fn total(in s: Str) -> u64 { s.len() }\nfn work(n: usize) -> usize {\n    let s = \"a\u{e9}b\";\n    if n == 100 { let _ = total(s[1..2]); }",
    );
    let mut imports = Imports::new();
    imports.add(&library(), Handler::Direct).unwrap();
    let checked =
        ffi_lowering::check_source(&source, &imports).unwrap_or_else(|e| panic!("{e:#?}"));
    let symbols = [
        ("pthread_create", pthread_create as *const u8),
        ("pthread_join", pthread_join as *const u8),
    ];
    let e = checked
        .jit_with_options("main", &symbols, true)
        .expect_err("the fault on the C thread was lost");
    assert!(e.to_string().contains("char boundaries"), "{e}");
}
