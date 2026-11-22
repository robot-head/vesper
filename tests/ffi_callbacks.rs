//! Calling C function pointers through `.vsys` callback types (§16.3, SH4
//! lane 13, #1287): a loader declared twice with `#[link_name]` hands out
//! typed pointers, and source calls them with `Lib::Name(f, args)` in the
//! JIT (optimizer on and off) and the evaluator's host.
use vesper::ffi_lowering::{self, FfiErrorKind, Handler, Imports};

extern "C" fn add(a: u64, b: u64) -> u64 {
    a.wrapping_add(b)
}
extern "C" fn scale(x: u64, by: u32) -> u64 {
    x.wrapping_mul(u64::from(by))
}
/// `lookup(name)`: the address of `add` or `scale`, or NULL.
extern "C" fn lookup(name: *const std::ffi::c_char) -> usize {
    // SAFETY: the Vesper call passes a NUL-terminated copy of its bytes.
    let name = unsafe { std::ffi::CStr::from_ptr(name) };
    match name.to_bytes() {
        b"add" => add as *const () as usize,
        b"scale" => scale as *const () as usize,
        _ => 0,
    }
}
/// `apply(f, x)`: C calling a pointer Vesper handed back.
extern "C" fn apply(f: extern "C" fn(u64, u64) -> u64, x: u64) -> u64 {
    f(x, x)
}

fn target() -> (&'static str, &'static str) {
    if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        ("aarch64-unknown-linux-gnu", "aapcs64")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        ("aarch64-apple-darwin", "darwin64")
    } else if cfg!(windows) {
        ("x86_64-pc-windows-msvc", "win64")
    } else {
        ("x86_64-unknown-linux-gnu", "sysv64")
    }
}

fn host_jit() -> bool {
    cfg!(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "aarch64"),
    ))
}

fn library() -> String {
    let (t, a) = target();
    format!(
        "sys library Loader (target = \"{t}\", abi = \"{a}\", link = \"dynamic\", name = \"loader\", \
         artifact = \"blake3:{}\") {{
    callback Add(a: u64, b: u64) -> ForeignVal[u64];
    callback Scale(x: u64, by: u32) -> ForeignVal[u64];
    #[link_name = \"lookup\"]
    fn lookup_add(nocapture in name: CStr) -> ForeignVal[Option[Add]];
    #[link_name = \"lookup\"]
    fn lookup_scale(nocapture in name: CStr) -> ForeignVal[NonNull[Scale]];
    fn apply(in f: Add, x: u64) -> ForeignVal[u64];
}}\n",
        "0".repeat(64)
    )
}

const SOURCE: &str = "\
fn get_add() -> Option[Add] eff [Ffi[Loader]] {
    match Loader::lookup_add(in \"add\".as_bytes()) {
        Result::Ok(f) => f,
        Result::Err(_) => Option::None,
    }
}
pub fn main() -> u64 eff [Ffi[Loader]] {
    let missing = match Loader::lookup_add(in \"nothing\".as_bytes()) {
        Result::Ok(Option::None) => 1000,
        _ => 0,
    };
    match get_add() {
        Option::Some(f) => {
            let scale = match Loader::lookup_scale(in \"scale\".as_bytes()) {
                Result::Ok(s) => s,
                Result::Err(_) => return 1,
            };
            let sum = Loader::Add(f, 40, 2);
            let scaled = Loader::Scale(scale, sum, 10);
            let again = Loader::apply(f, 7);
            wrapping_add(wrapping_add(scaled, wrapping_mul(again, 100000)), missing)
        }
        Option::None => 2,
    }
}
";

const EXPECTED: &str = "1401420";

#[test]
fn source_calls_c_function_pointers() {
    if !host_jit() {
        return;
    }
    let mut imports = Imports::new();
    imports.add(&library(), Handler::Direct).unwrap();
    let checked = ffi_lowering::check_source(SOURCE, &imports).unwrap_or_else(|e| panic!("{e:#?}"));
    let symbols = [
        ("lookup", lookup as *const u8),
        ("apply", apply as *const u8),
    ];
    for optimize in [true, false] {
        let v = checked
            .jit_with_options("main", &symbols, optimize)
            .unwrap();
        assert_eq!(v.to_string(), EXPECTED, "optimize {optimize}");
    }
    let host = ffi_lowering::EvalHost::new(&imports, &symbols).unwrap();
    let v = checked.evaluate("main", |call| host.call(call)).unwrap();
    assert_eq!(v.to_string(), EXPECTED);
}

/// The object imports the loader once, under its C name, and no symbol for
/// the callback types.
#[test]
fn objects_import_the_link_name() {
    let mut imports = Imports::new();
    imports.add(&library(), Handler::Direct).unwrap();
    let checked = ffi_lowering::check_source(SOURCE, &imports).unwrap();
    let (triple, _) = target();
    let object = checked.object("main", Some(triple)).unwrap();
    let has = |s: &[u8]| object.bytes.windows(s.len()).any(|w| w == s);
    assert!(has(b"lookup"));
    assert!(has(b"apply"));
    assert!(!has(b"lookup_add") && !has(b"lookup_scale"));
}

/// Callback handles are opaque: source cannot build or compare one, and an
/// isolated library cannot call through a pointer yet.
#[test]
fn callback_handles_are_opaque_and_isolation_refuses_them() {
    let mut imports = Imports::new();
    imports.add(&library(), Handler::Direct).unwrap();
    for source in [
        "pub fn main() -> u64 eff [Ffi[Loader]] { let f = Add { x: 1 }; Loader::Add(f, 1, 2) }",
        "pub fn main(f: Add, g: Add) -> bool { f == g }",
    ] {
        assert!(
            ffi_lowering::check_source(source, &imports).is_err(),
            "{source}"
        );
    }
    let mut isolated = Imports::new();
    isolated.add(&library(), Handler::IsolatedMpk).unwrap();
    let e = match ffi_lowering::check_source(SOURCE, &isolated) {
        Ok(_) => panic!("isolated callback accepted"),
        Err(e) => e,
    };
    assert_eq!(e.kind, FfiErrorKind::IsolationUnavailable, "{e}");
    assert!(e.message.contains("E0835"), "{e}");
}

unsafe extern "C" {
    fn dlopen(path: *const std::ffi::c_char, flags: i32) -> *mut std::ffi::c_void;
    fn dlsym(handle: *mut std::ffi::c_void, name: *const std::ffi::c_char)
    -> *mut std::ffi::c_void;
}

/// An optional library (graphics gap 7) needs no new link mode: a package
/// declares the loader it already has (`dlopen`/`dlsym` in libc) and one
/// typed `#[link_name = "dlsym"]` lookup per entry point, so nothing about
/// the library is resolved at link time, and a missing library or symbol
/// is a `None` the program handles.
#[test]
fn optional_libraries_load_through_dlopen() {
    if !host_jit() || cfg!(windows) {
        return;
    }
    let (t, a) = target();
    let libc_name = if cfg!(target_os = "macos") {
        "/usr/lib/libSystem.B.dylib"
    } else {
        "libc.so.6"
    };
    let vsys = format!(
        "sys library Dl (target = \"{t}\", abi = \"{a}\", link = \"dynamic\", name = \"c\", \
         artifact = \"blake3:{}\") {{
    affine extern type Library;
    callback Abs(x: i64) -> ForeignVal[i64];
    fn dlopen(nocapture in path: CStr, flags: i32) -> ForeignVal[Option[Library]];
    #[link_name = \"dlsym\"]
    fn load_abs(in library: Library, nocapture in name: CStr) -> ForeignVal[Option[Abs]];
}}\n",
        "0".repeat(64)
    );
    let source = format!(
        "\
fn abs_from(path: Slice[u8]) -> i64 eff [Ffi[Dl]] {{
    // RTLD_NOW = 2.
    match Dl::dlopen(in path, 2) {{
        Result::Ok(Option::Some(library)) => match Dl::load_abs(in library, in \"labs\".as_bytes()) {{
            Result::Ok(Option::Some(f)) => Dl::Abs(f, -41),
            _ => -2,
        }},
        _ => -1,
    }}
}}
pub fn main() -> i64 eff [Ffi[Dl]] {{
    let found = abs_from(\"{libc_name}\".as_bytes());
    let missing = abs_from(\"libvesper-no-such-library.so\".as_bytes());
    wrapping_add(wrapping_mul(found, 10), missing)
}}
"
    );
    let mut imports = Imports::new();
    imports.add(&vsys, Handler::Direct).unwrap();
    let checked =
        ffi_lowering::check_source(&source, &imports).unwrap_or_else(|e| panic!("{e:#?}"));
    let symbols = [
        ("dlopen", dlopen as *const u8),
        ("dlsym", dlsym as *const u8),
    ];
    for optimize in [true, false] {
        let v = checked
            .jit_with_options("main", &symbols, optimize)
            .unwrap();
        assert_eq!(v.to_string(), "409", "optimize {optimize}");
    }
    let host = ffi_lowering::EvalHost::new(&imports, &symbols).unwrap();
    let v = checked.evaluate("main", |call| host.call(call)).unwrap();
    assert_eq!(v.to_string(), "409");
}

// ---------------------------------------------------------------- C calls Vesper

/// `fold(f, n)`: `f(f(..f(0, 1).., n-1), n)`, C calling a Vesper function
/// it was lent (`nocapture`).
extern "C" fn fold(f: extern "C" fn(u64, u64) -> u64, n: u64) -> u64 {
    (1..=n).fold(0, |acc, i| f(acc, i))
}
static HANDLER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static BUMPS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// `on_signal(h)`: keeps `h` (`retain`), as `signal(2)` does.
extern "C" fn on_signal(h: extern "C" fn(i32)) -> i32 {
    HANDLER.store(h as *const () as usize, std::sync::atomic::Ordering::SeqCst);
    0
}
/// `raise_signal(sig)`: calls the kept handler later, from another thread.
extern "C" fn raise_signal(sig: i32) -> i32 {
    let h = HANDLER.load(std::sync::atomic::Ordering::SeqCst);
    if h == 0 {
        return -1;
    }
    // SAFETY: `on_signal` stored a pointer of this type.
    let h: extern "C" fn(i32) = unsafe { std::mem::transmute(h) };
    std::thread::spawn(move || h(sig)).join().unwrap();
    0
}
extern "C" fn bump(by: u64) {
    BUMPS.fetch_add(by, std::sync::atomic::Ordering::SeqCst);
}

fn host_library() -> String {
    let (t, a) = target();
    format!(
        "sys library Host (target = \"{t}\", abi = \"{a}\", link = \"dynamic\", name = \"host\", \
         artifact = \"blake3:{}\") {{
    callback Step(acc: u64, i: u64) -> ForeignVal[u64];
    callback Handler(sig: i32) -> ForeignVal[Unit];
    fn fold(nocapture f: Step, n: u64) -> ForeignVal[u64];
    fn on_signal(retain h: Handler) -> ForeignVal[i32];
    fn raise_signal(sig: i32) -> ForeignVal[i32];
    fn bump(by: u64) -> ForeignVal[Unit];
}}\n",
        "0".repeat(64)
    )
}

const HOST_SOURCE: &str = "\
fn square_sum(acc: u64, i: u64) -> u64 { wrapping_add(acc, wrapping_mul(i, i)) }
fn handler(sig: i32) eff [Ffi[Host]] { Host::bump(wrapping_u64(sig)); }
pub fn main() -> u64 eff [Ffi[Host]] {
    let sum = Host::fold(square_sum, 10);
    let again = Host::fold(square_sum, 3);
    if Host::on_signal(handler) != 0 { return 1; }
    if Host::raise_signal(7) != 0 { return 2; }
    if Host::raise_signal(5) != 0 { return 3; }
    wrapping_add(sum, wrapping_mul(again, 1000))
}
";

#[test]
fn c_calls_vesper_functions_it_was_handed() {
    if !host_jit() {
        return;
    }
    let mut imports = Imports::new();
    imports.add(&host_library(), Handler::Direct).unwrap();
    let checked =
        ffi_lowering::check_source(HOST_SOURCE, &imports).unwrap_or_else(|e| panic!("{e:#?}"));
    let symbols = [
        ("fold", fold as *const u8),
        ("on_signal", on_signal as *const u8),
        ("raise_signal", raise_signal as *const u8),
        ("bump", bump as *const u8),
    ];
    for optimize in [true, false] {
        BUMPS.store(0, std::sync::atomic::Ordering::SeqCst);
        let v = checked
            .jit_with_options("main", &symbols, optimize)
            .unwrap_or_else(|e| panic!("{e}"));
        // 1 + 4 + ... + 100 = 385; 1 + 4 + 9 = 14.
        assert_eq!(v.to_string(), "14385", "optimize {optimize}");
        assert_eq!(BUMPS.load(std::sync::atomic::Ordering::SeqCst), 12);
    }
    // Objects carry the trampolines too.
    let object = checked.object("main", Some(target().0)).unwrap();
    assert!(object.bytes.windows(4).any(|w| w == b"fold"));
    // The evaluator cannot hand C a pointer back into itself.
    let host = ffi_lowering::EvalHost::new(&imports, &symbols).unwrap();
    let e = checked
        .evaluate("main", |call| host.call(call))
        .unwrap_err();
    assert!(format!("{e:?}").contains("compiled code"), "{e:?}");
}

#[test]
fn callbacks_are_named_functions_with_foreign_rows() {
    let mut imports = Imports::new();
    imports.add(&host_library(), Handler::Direct).unwrap();
    let kind = |source: &str| match ffi_lowering::check_source(source, &imports) {
        Ok(_) => panic!("accepted: {source}"),
        Err(e) => e.kind,
    };
    // A closure is not a named function.
    assert_eq!(
        kind("pub fn main() -> u64 eff [Ffi[Host]] { Host::fold(|a, i| a, 3) }"),
        FfiErrorKind::CallbackArgument
    );
    // A generic function is not either.
    assert_eq!(
        kind(
            "fn id[T](a: u64, i: u64) -> u64 { a } pub fn main() -> u64 eff [Ffi[Host]] { Host::fold(id, 3) }"
        ),
        FfiErrorKind::CallbackArgument
    );
    // A local binding that shares a top-level function's name is a value,
    // not that function: it could hold anything (here a thread-affine
    // function), so it is refused.
    assert_eq!(
        kind(
            "fn h(sig: i32) { } fn bad(sig: i32) { let b = Box::new(sig); } pub fn main() -> i32 eff [Ffi[Host]] { let h = bad; Host::on_signal(h) }"
        ),
        FfiErrorKind::CallbackArgument
    );
    assert_eq!(
        kind(
            "fn h(sig: i32) { } fn run(h: fn(i32)) -> i32 eff [Ffi[Host]] { Host::on_signal(h) } pub fn main() -> i32 eff [Ffi[Host]] { run(h) }"
        ),
        FfiErrorKind::CallbackArgument
    );
    // The signature must match the callback type.
    assert_eq!(
        kind("fn f(a: u64) -> u64 { a } pub fn main() -> u64 eff [Ffi[Host]] { Host::fold(f, 3) }"),
        FfiErrorKind::Check
    );
    // A retained function may run on another thread, so it cannot hold
    // thread-affine runtime values.
    let e = match ffi_lowering::check_source(
        "fn h(sig: i32) { let b = Box::new(sig); } pub fn main() -> i32 eff [Ffi[Host]] { Host::on_signal(h) }",
        &imports,
    ) {
        Ok(_) => panic!("a thread-affine retained function was accepted"),
        Err(e) => e,
    };
    assert_eq!(e.kind, FfiErrorKind::CallbackArgument, "{e}");
    assert!(e.message.contains("E0832"), "{e}");
    // A callback cannot fail through C, or use effects that need a handler.
    assert_eq!(
        kind(
            "fn f(a: u64, i: u64) -> u64 eff [Fail[u64]] { a } pub fn main() -> u64 eff [Ffi[Host]] { Host::fold(f, 3) }"
        ),
        FfiErrorKind::Check
    );
}

/// An export library's binding keeps the Rust shim's symbols linked by
/// their C names: two `#[link_name = "lookup"]` aliases retain `lookup`
/// once, and a callback type's call retains nothing.
#[test]
fn export_bindings_retain_link_names() {
    let source = "// @export vsys direct rust=crate::loader\n\
        // | sys library Loader (target = \"@TARGET@\", abi = \"@ABI@\", link = \"static\", name = \"loader\",\n\
        // |     artifact = \"blake3:0000000000000000000000000000000000000000000000000000000000000000\") {\n\
        // |     callback Add(a: u64, b: u64) -> ForeignVal[u64];\n\
        // |     #[link_name = \"lookup\"]\n\
        // |     fn lookup_add(nocapture in name: CStr) -> ForeignVal[Option[Add]];\n\
        // |     #[link_name = \"lookup\"]\n\
        // |     fn lookup_scale(nocapture in name: CStr) -> ForeignVal[Option[Add]];\n\
        // |     fn apply(in f: Add, x: u64) -> ForeignVal[u64];\n\
        // | }\n\
        pub fn go() -> u64 { 1 }\n";
    let d = vesper::native::export::Directives::parse(source, Some(target().0)).unwrap();
    assert_eq!(d.vsys.len(), 1);
    assert_eq!(d.vsys[0].symbols, ["apply", "lookup"]);
}
