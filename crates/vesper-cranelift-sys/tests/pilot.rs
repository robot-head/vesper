//! The SH0 lane 4 pilot (#1126): Vesper code builds a Cranelift object
//! through `@vesper/cranelift` and the shim, the host's system linker links
//! it, and the program runs. Error paths too. Stage 2 builds the fixture
//! and the Cranelift component runs it (tests/support/stage2.rs).
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
    let Some(stage2) = stage2::stage2() else {
        return;
    };
    let id = stage2.answer(&fixture("pilot.ves"), "pilot");
    assert_ne!(id, 0, "the pilot failed to build its object");
    let bytes = stage2.published(id).expect("published object");
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
fn errors_are_reported() {
    let Some(stage2) = stage2::stage2() else {
        return;
    };
    let pilot = fixture("pilot.ves");
    let answer = |entry| stage2.answer(&pilot, entry);
    // The verifier's message comes back through the module.
    let err = answer("invalid_function");
    assert!(err > 3, "invalid function: {err}");
    // An unknown opcode poisons the builder; later calls are refused.
    let err = answer("poisoned_builder");
    assert!(err > 0, "poisoned builder: {err}");
    // An invalid value in an argument list is an error, not an abort.
    let err = answer("invalid_argument");
    assert!(err > 0, "invalid argument: {err}");
    // A bad triple: no module, a message.
    let err = answer("bad_triple");
    assert!(err > 0, "bad triple: {err}");
    // Vector constants, shuffles and atomics compile.
    assert!(answer("vectors_and_atomics") > 0);
    // 50 modules, half finished into objects and copied out.
    assert_eq!(answer("churn"), 25);
}
