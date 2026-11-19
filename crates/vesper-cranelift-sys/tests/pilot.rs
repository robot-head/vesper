//! The SH0 lane 4 pilot (#1126): Vesper code builds a Cranelift object
//! through `@vesper/cranelift` and the shim, the host's system linker links
//! it, and the program runs. Handle leak and error-path tests too.
mod support;

use std::path::Path;
use std::process::Command;
use support::*;

fn pilot_arith(a: i64, b: i64) -> i64 {
    a.wrapping_add(b).wrapping_mul(3).wrapping_sub(a ^ b)
}
fn pilot_sum(n: i64) -> i64 {
    (0..n).sum()
}
fn pilot_call(x: i64) -> i64 {
    pilot_arith(x, x + 1) + pilot_sum(x) + 100 * ((x & 3) + 1)
}

fn run(cmd: &mut Command) -> std::process::Output {
    let out = cmd
        .output()
        .unwrap_or_else(|e| panic!("cannot run {cmd:?}: {e}"));
    assert!(
        out.status.success(),
        "{cmd:?} failed: {}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}
fn works(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Link `object` with a host program that calls the pilot functions, using
/// the system C compiler where there is one and rustc (which drives the
/// platform linker, link.exe on MSVC) otherwise. Returns the executable.
fn link(temp: &Path, object: &Path) -> Option<std::path::PathBuf> {
    let exe = temp.join(if cfg!(windows) { "pilot.exe" } else { "pilot" });
    if !cfg!(windows) && works("cc") {
        let driver = temp.join("main.c");
        std::fs::write(&driver, fixture("pilot_main.c")).unwrap();
        run(Command::new("cc")
            .arg(&driver)
            .arg(object)
            .arg("-o")
            .arg(&exe));
    } else if works("rustc") {
        let driver = temp.join("main.rs");
        std::fs::write(&driver, fixture("pilot_main.rs")).unwrap();
        run(Command::new("rustc")
            .args(["--edition", "2024", "-O"])
            .arg(&driver)
            .arg("-C")
            .arg(format!("link-arg={}", object.display()))
            .arg("-o")
            .arg(&exe));
    } else {
        return None;
    }
    Some(exe)
}

#[test]
fn vesper_builds_links_and_runs_a_cranelift_object() {
    let Some(target) = host_target() else {
        eprintln!("no .vsys target for this host; pilot evidence unavailable");
        return;
    };
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let before = vesper_cranelift_sys::live_handles();
    let checked = checked_with(&fixture("pilot.ves"), target);
    let id = jit(&checked, "pilot");
    assert_ne!(id, 0, "the pilot failed to build its object");
    assert_eq!(
        vesper_cranelift_sys::live_handles(),
        before,
        "pilot leaked a handle"
    );
    let bytes = vesper_cranelift_sys::take_published(id).expect("published object");
    assert!(bytes.len() > 64, "object of {} bytes", bytes.len());

    let temp = Temp::new("pilot");
    let object = temp.0.join(if cfg!(windows) {
        "pilot.obj"
    } else {
        "pilot.o"
    });
    std::fs::write(&object, &bytes).unwrap();
    let Some(exe) = link(&temp.0, &object) else {
        if std::env::var_os("CI").is_some() {
            panic!("no C compiler or rustc to link the pilot with");
        }
        eprintln!("no linker driver (cc or rustc); link evidence unavailable");
        return;
    };
    let out = run(&mut Command::new(&exe));
    let got: Vec<i64> = String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .map(|w| w.parse().unwrap())
        .collect();
    let want = vec![
        pilot_arith(7, 5),
        pilot_arith(-9, 1 << 40),
        pilot_sum(0),
        pilot_sum(10),
        pilot_sum(1000),
        pilot_call(4),
        pilot_call(7),
    ];
    assert_eq!(got, want);
}

#[test]
fn errors_are_reported_and_handles_never_leak() {
    let Some(target) = host_target() else {
        return;
    };
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let before = vesper_cranelift_sys::live_handles();
    let checked = checked_with(&fixture("pilot.ves"), target);
    // The verifier's message comes back through the module.
    let err = jit(&checked, "invalid_function");
    assert!(err > 3, "invalid function: {err}");
    // An unknown opcode poisons the builder; later calls are refused.
    let err = jit(&checked, "poisoned_builder");
    assert!(err > 0, "poisoned builder: {err}");
    // An invalid value in an argument list is an error, not an abort.
    let err = jit(&checked, "invalid_argument");
    assert!(err > 0, "invalid argument: {err}");
    // A bad triple: no module, a message.
    let err = jit(&checked, "bad_triple");
    assert!(err > 0, "bad triple: {err}");
    // Vector constants, shuffles and atomics compile.
    assert!(jit(&checked, "vectors_and_atomics") > 0);
    // 50 modules, half finished into objects and copied out.
    assert_eq!(jit(&checked, "churn"), 25);
    assert_eq!(
        vesper_cranelift_sys::live_handles(),
        before,
        "every module, builder, data and object is finalized once"
    );
}

#[test]
fn the_checker_refuses_leaked_and_reused_handles() {
    let target = host_target().unwrap_or(("x86_64-unknown-linux-gnu", "sysv64"));
    let package = package_source();
    let mut imports = vesper::ffi_lowering::Imports::new();
    imports
        .add(
            &vsys(&package, target),
            vesper::ffi_lowering::Handler::Direct,
        )
        .unwrap();
    for (name, body, code) in [
        (
            "leaked builder",
            "pub fn f() -> u64 eff [Ffi[Cranelift]] {
                match module_new(in \"\".as_bytes(), in \"\".as_bytes(), in \"\".as_bytes()) {
                    Some(m) => {
                        let mut m = m;
                        let s = sig_new(inout m, 0);
                        let b = builder_new(in m, s);
                        module_free(m);
                        0
                    },
                    None => 1,
                }
            }",
            "E0302",
        ),
        (
            "builder defined twice",
            "pub fn f() -> u64 eff [Ffi[Cranelift]] {
                match module_new(in \"\".as_bytes(), in \"\".as_bytes(), in \"\".as_bytes()) {
                    Some(m) => {
                        let mut m = m;
                        let s = sig_new(inout m, 0);
                        let f = declare_function(inout m, in \"f\".as_bytes(), 4, s);
                        match builder_new(in m, s) {
                            Some(b) => {
                                define_function(inout m, f, b);
                                define_function(inout m, f, b);
                            },
                            None => {},
                        }
                        module_free(m);
                        0
                    },
                    None => 1,
                }
            }",
            "E0303",
        ),
    ] {
        let source = format!("{package}\n{body}");
        let e = match vesper::ffi_lowering::check_source(&source, &imports) {
            Ok(_) => panic!("{name}: accepted"),
            Err(e) => e,
        };
        let codes: Vec<&str> = e.diagnostics.iter().map(|d| d.code.as_str()).collect();
        assert!(
            codes.contains(&code),
            "{name}: want {code}, got {e} {codes:?}"
        );
    }
}
