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
