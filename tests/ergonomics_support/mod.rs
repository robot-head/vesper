//! Shared drivers for the SH1 ergonomics tests (`or_patterns`,
//! `str_literals`, `checked_arith_builtins`): every program runs in the
//! evaluator (optimized and not), the JIT (optimized and not), AArch64 and
//! x86-64 object emission and, where a C driver links executables, an AOT
//! executable, and every backend must agree.
#![allow(dead_code)]
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::{check, native, parser, runtime::Runtime};

static NEXT: AtomicU64 = AtomicU64::new(0);

pub fn checked(source: &str) -> check::Checked {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{source}\n{:?}", p.diagnostics);
    check::check(&p.module)
}

pub fn accepted(source: &str) -> check::Checked {
    let c = checked(source);
    assert!(c.diagnostics.is_empty(), "{source}\n{:#?}", c.diagnostics);
    c
}

/// The diagnostics of `source` carry `code`.
pub fn rejects(source: &str, code: &str) -> Vec<vesper::ast::Diagnostic> {
    let c = checked(source);
    assert!(
        c.diagnostics.iter().any(|d| d.code == code),
        "expected {code} for {source}: {:#?}",
        c.diagnostics
    );
    c.diagnostics
}

fn links_executables() -> bool {
    cfg!(unix)
        && Command::new("cc")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
}

/// The exit status of `source` built as an executable.
fn executable(name: &str, source: &str) -> Option<i32> {
    let dir = std::env::temp_dir().join(format!(
        "vesper-ergonomics-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.ves"));
    fs::write(&file, source).unwrap();
    let exe: PathBuf = dir.join(name);
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
    let status = Command::new(&exe).output().unwrap().status.code();
    let _ = fs::remove_dir_all(&dir);
    status
}

/// Runs `main` (a `u64`) on every backend and returns the agreed value.
pub fn agree(name: &str, source: &str) -> u64 {
    let c = accepted(source);
    let mut values = vec![];
    for optimize in [true, false] {
        let mut runtime = Runtime::new(&c.world);
        runtime.optimize = optimize;
        let v = runtime
            .run("main", vec![])
            .unwrap_or_else(|e| panic!("{name}: evaluator (optimize {optimize}): {e}"))
            .to_string();
        values.push((format!("evaluator optimize={optimize}"), v));
        let v = native::jit_with_options(&c.world, "main", optimize)
            .unwrap_or_else(|e| panic!("{name}: JIT (optimize {optimize}): {e}"))
            .to_string();
        values.push((format!("JIT optimize={optimize}"), v));
    }
    for (what, v) in &values[1..] {
        assert_eq!(v, &values[0].1, "{name}: {what} vs {}", values[0].0);
    }
    let (x86, _) =
        native::object_for_target(&c.world, "main", Some("x86_64-unknown-linux-gnu")).unwrap();
    assert_eq!(&x86[..4], b"\x7fELF", "{name}: x86-64 object");
    let (arm, _) =
        native::object_for_target(&c.world, "main", Some("aarch64-unknown-linux-gnu")).unwrap();
    assert_eq!(
        u16::from_le_bytes([arm[18], arm[19]]),
        183,
        "{name}: AArch64 object"
    );
    let value: u64 = values[0]
        .1
        .parse()
        .unwrap_or_else(|_| panic!("{name}: main is not a u64: {}", values[0].1));
    if links_executables() {
        assert_eq!(
            executable(name, source),
            Some((value & 0xff) as i32),
            "{name}: executable"
        );
    }
    value
}
