//! `-> ForeignVal[Option[CStr]]` (SH6 lane 13, #1408, gap G1): a C string
//! the library returns is copied into a fresh `ChunkSeq[u8]` (`NULL` is
//! `None`), in the JIT, the evaluator's host and an executable that reads
//! an environment variable with `getenv`.
use std::ffi::c_char;
use vesper::ffi_lowering::{self, Handler, Imports};

fn target() -> (&'static str, &'static str) {
    if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        ("aarch64-unknown-linux-gnu", "aapcs64")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        ("aarch64-apple-darwin", "darwin64")
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
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
        all(target_os = "windows", target_arch = "x86_64"),
    ))
}

/// `NULL`, "hello" or "": strings the library keeps.
extern "C" fn probe_str(which: u64) -> *const c_char {
    match which {
        1 => c"hello".as_ptr(),
        2 => c"".as_ptr(),
        _ => std::ptr::null(),
    }
}

const SOURCE: &str = "fn sum(in s: Slice[u8]) -> u64 {\n    let mut i: u64 = 0;\n    let mut t: u64 = 0;\n    loop {\n        \
     if i >= s.len() { break; }\n        match s.try_get(i) { Option::Some(c) => { t = wrapping_add(t, wrapping_u64(c)); }, Option::None => {} }\n        \
     i = wrapping_add(i, 1);\n    }\n    t\n}\n\n\
     pub fn run(which: u64) -> u64 eff [Ffi[Probe]] {\n    match Probe::probe_str(which) {\n        \
     Option::Some(s) => wrapping_add(1000000, wrapping_add(wrapping_mul(s.len(), 1000), sum(in s.as_slice()))),\n        \
     Option::None => 7,\n    }\n}\n\n\
     pub fn hello() -> u64 eff [Ffi[Probe]] { run(1) }\n\
     pub fn empty() -> u64 eff [Ffi[Probe]] { run(2) }\n\
     pub fn null() -> u64 eff [Ffi[Probe]] { run(0) }\n";

#[test]
fn a_returned_c_string_is_copied() {
    if !host_jit() {
        return;
    }
    let (t, a) = target();
    let library = format!(
        "sys library Probe (target = \"{t}\", abi = \"{a}\", link = \"static\", name = \"probe\", \
         artifact = \"blake3:0000000000000000000000000000000000000000000000000000000000000000\") {{\n    \
         fn probe_str(which: u64) -> ForeignVal[Option[CStr]];\n}}\n"
    );
    let mut imports = Imports::new();
    imports.add(&library, Handler::Direct).unwrap();
    let checked = ffi_lowering::check_source(SOURCE, &imports).unwrap_or_else(|e| panic!("{e}"));
    let symbols = [("probe_str", probe_str as *const u8)];
    // "hello": 5 bytes summing to 532; "": none; NULL: `None`.
    let expected = [("hello", "1005532"), ("empty", "1000000"), ("null", "7")];
    let host = ffi_lowering::EvalHost::new(&imports, &symbols).unwrap();
    for (entry, want) in expected {
        let v = checked.evaluate(entry, |call| host.call(call)).unwrap();
        assert_eq!(v.to_string(), want, "{entry} evaluated");
    }
    for optimize in [false, true] {
        for (entry, want) in expected {
            let v = checked.jit_with_options(entry, &symbols, optimize).unwrap();
            assert_eq!(v.to_string(), want, "{entry} optimize {optimize}");
        }
    }
}

extern "C" fn probe_wstr(which: u64) -> *const u16 {
    static HELLO: [u16; 6] = [104, 101, 108, 108, 111, 0];
    static EMPTY: [u16; 1] = [0];
    match which {
        1 => HELLO.as_ptr(),
        2 => EMPTY.as_ptr(),
        _ => std::ptr::null(),
    }
}

extern "C" fn check_wstr(ptr: *const u16) -> u64 {
    let mut i = 0usize;
    let mut sum = 0u64;
    loop {
        // SAFETY: `ptr` is NUL-terminated by `CWStr` lowering.
        let u = unsafe { *ptr.add(i) };
        if u == 0 {
            break;
        }
        sum = sum.wrapping_add(u64::from(u));
        i += 1;
    }
    (i as u64) * 1000 + sum
}

#[test]
fn a_wide_c_string_parameter_and_result_round_trip() {
    if !host_jit() {
        return;
    }
    let (t, a) = target();
    let library = format!(
        "sys library WProbe (target = \"{t}\", abi = \"{a}\", link = \"static\", name = \"wprobe\", \
         artifact = \"blake3:0000000000000000000000000000000000000000000000000000000000000000\") {{\n    \
         fn probe_wstr(which: u64) -> ForeignVal[Option[CWStr]];\n    \
         fn check_wstr(nocapture in s: CWStr) -> ForeignVal[u64];\n}}\n"
    );
    let mut imports = Imports::new();
    imports.add(&library, Handler::Direct).unwrap();
    let source = "
fn sum16(in s: Slice[u16]) -> u64 {
    let mut i: u64 = 0;
    let mut t: u64 = 0;
    loop {
        if i >= s.len() { break; }
        match s.try_get(i) { Option::Some(c) => { t = wrapping_add(t, widen_u64(c)); }, Option::None => {} }
        i = wrapping_add(i, 1);
    }
    t
}

pub fn ret_hello() -> u64 eff [Ffi[WProbe]] {
    match WProbe::probe_wstr(1) {
        Option::Some(s) => wrapping_add(wrapping_mul(s.len(), 1000), sum16(in s.as_slice())),
        Option::None => 7,
    }
}

pub fn ret_empty() -> u64 eff [Ffi[WProbe]] {
    match WProbe::probe_wstr(2) {
        Option::Some(s) => wrapping_add(1000000, s.len()),
        Option::None => 7,
    }
}

pub fn ret_null() -> u64 eff [Ffi[WProbe]] {
    match WProbe::probe_wstr(0) {
        Option::Some(_) => 99,
        Option::None => 7,
    }
}

pub fn pass_ok() -> u64 eff [Ffi[WProbe]] {
    let mut s = ChunkSeq::new[u16]();
    s.push(104);
    s.push(105);
    match WProbe::check_wstr(in s.as_slice()) {
        Result::Ok(n) => n,
        Result::Err(e) => wrapping_add(500, e.raw),
    }
}

pub fn pass_nul() -> u64 eff [Ffi[WProbe]] {
    let mut s = ChunkSeq::new[u16]();
    s.push(104);
    s.push(0);
    s.push(105);
    match WProbe::check_wstr(in s.as_slice()) {
        Result::Ok(_) => 99,
        Result::Err(e) => wrapping_add(500, e.raw),
    }
}
";
    let checked = ffi_lowering::check_source(source, &imports).unwrap_or_else(|e| panic!("{e}"));
    let symbols = [
        ("probe_wstr", probe_wstr as *const u8),
        ("check_wstr", check_wstr as *const u8),
    ];
    let expected = [
        ("ret_empty", "1000000"),
        ("ret_null", "7"),
        ("pass_ok", "2209"),
        ("pass_nul", "501"),
        ("ret_hello", "5532"),
    ];
    let host = ffi_lowering::EvalHost::new(&imports, &symbols).unwrap();
    for (entry, want) in expected {
        let v = checked.evaluate(entry, |call| host.call(call)).unwrap();
        assert_eq!(v.to_string(), want, "{entry} evaluated");
    }
    for optimize in [false, true] {
        for (entry, want) in expected {
            let v = checked.jit_with_options(entry, &symbols, optimize).unwrap();
            assert_eq!(v.to_string(), want, "{entry} optimize {optimize}");
        }
    }
}
