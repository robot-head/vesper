//! `nocapture in argv: CStrList` (SH6 lane 13, #1408, gap G2): one source
//! `Slice[u8]` of NUL-ended strings becomes C's NULL-terminated
//! `char *const argv[]`, in the JIT, the evaluator's host and an executable
//! that spawns a process with `posix_spawnp`.
use std::ffi::{CStr, c_char};
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

/// 1000 per string plus its bytes, and 100000 if the array is not
/// NULL-terminated within 64 entries.
extern "C" fn probe_argv(argv: *const *const c_char) -> u64 {
    let mut out = 0;
    for k in 0..64 {
        // SAFETY: the lowering passes a NULL-terminated array of
        // NUL-terminated strings that lives for the call.
        let p = unsafe { *argv.add(k) };
        if p.is_null() {
            return out;
        }
        out += 1000 + unsafe { CStr::from_ptr(p) }.to_bytes().len() as u64;
    }
    100_000
}

const SOURCE: &str = "fn bytes(which: u64) -> ChunkSeq[u8] {\n    let mut s: ChunkSeq[u8] = ChunkSeq::new[u8]();\n    \
     if which == 1 {\n        s.push(97); s.push(98); s.push(0); s.push(99); s.push(0); s.push(0); s.push(100);\n    }\n    \
     if which == 2 { s.push(120); s.push(0); }\n    s\n}\n\n\
     pub fn run(which: u64) -> u64 eff [Ffi[Probe]] {\n    let s = bytes(which);\n    Probe::probe_argv(in s.as_slice())\n}\n\n\
     pub fn four() -> u64 eff [Ffi[Probe]] { run(1) }\n\
     pub fn none() -> u64 eff [Ffi[Probe]] { run(0) }\n\
     pub fn one() -> u64 eff [Ffi[Probe]] { run(2) }\n";

#[test]
fn a_byte_slice_becomes_a_null_terminated_argv() {
    if !host_jit() {
        return;
    }
    let (t, a) = target();
    let library = format!(
        "sys library Probe (target = \"{t}\", abi = \"{a}\", link = \"static\", name = \"probe\", \
         artifact = \"blake3:0000000000000000000000000000000000000000000000000000000000000000\") {{\n    \
         fn probe_argv(nocapture in argv: CStrList) -> ForeignVal[u64];\n}}\n"
    );
    let mut imports = Imports::new();
    imports.add(&library, Handler::Direct).unwrap();
    let checked = ffi_lowering::check_source(SOURCE, &imports).unwrap_or_else(|e| panic!("{e}"));
    let symbols = [("probe_argv", probe_argv as *const u8)];
    // "ab", "c", "" and "d" (no NUL of its own); none; "x".
    let expected = [("four", "4004"), ("none", "0"), ("one", "1001")];
    for optimize in [true, false] {
        for (entry, want) in expected {
            let v = checked.jit_with_options(entry, &symbols, optimize).unwrap();
            assert_eq!(v.to_string(), want, "{entry} optimize {optimize}");
        }
    }
    let host = ffi_lowering::EvalHost::new(&imports, &symbols).unwrap();
    for (entry, want) in expected {
        let v = checked.evaluate(entry, |call| host.call(call)).unwrap();
        assert_eq!(v.to_string(), want, "{entry} evaluated");
    }
}
