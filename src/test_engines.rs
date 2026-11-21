//! `vesper test --engines all` (Rust-exit wave SH6 lane 19, #1414): a unit
//! test that passed on its usual engine must also pass on every other one.
//!
//! This is the check the compiler's Rust integration tests made through
//! their `differential` helpers (tests/continue.rs and its siblings), so
//! that a test converted to a Vesper source under tests/vesper/ keeps the
//! coverage it had: the evaluator and the JIT, optimized and not, each
//! return `true`; the JIT leaves no sequence or string for teardown, frees
//! no buffer twice and frees every `Box` it allocates; the test compiles to
//! an object for both Linux targets; and when the file has a `main()` and a
//! C driver links executables, the executable's exit status is the low byte
//! of the evaluator's `main()`.

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::{
    ast::Type,
    check::World,
    fip::native_runtime as boxes,
    native,
    runtime::{Runtime, Value},
    seq::native_runtime as seqs,
};

/// The targets every converted test compiles an object for.
pub const OBJECT_TARGETS: [&str; 2] = ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"];

/// Whether `--engines` names the full set. `unit` (the default) is the
/// usual single-engine run.
pub fn parse(value: &str) -> Result<bool, String> {
    match value {
        "all" => Ok(true),
        "unit" => Ok(false),
        _ => Err("--engines takes `all` or `unit`".into()),
    }
}

/// Runs `entry` (a test function returning `bool`) on every engine; `Err`
/// names the first engine that disagrees. `source` is the file's source,
/// used to build the executable when it has a `main()`.
pub fn agree(world: &World, entry: &str, source: &str, fuel: u64) -> Result<(), String> {
    for optimize in [true, false] {
        let mut runtime = Runtime::new(world);
        runtime.optimize = optimize;
        runtime.fuel = fuel;
        let value = runtime
            .run(entry, vec![])
            .map_err(|e| format!("evaluator (optimize {optimize}) failed: {e}"))?;
        expect_true(&value, &format!("evaluator (optimize {optimize})"))?;

        boxes::reset_counts();
        let stray = seqs::stray_frees();
        let value = native::jit_with_options(world, entry, optimize)
            .map_err(|e| format!("JIT (optimize {optimize}) failed: {e}"))?;
        expect_true(&value, &format!("JIT (optimize {optimize})"))?;
        if seqs::teardown_frees() != 0 {
            return Err(format!(
                "JIT (optimize {optimize}) left {} sequence(s) for teardown",
                seqs::teardown_frees()
            ));
        }
        if seqs::stray_frees() != stray {
            return Err(format!("JIT (optimize {optimize}) freed a sequence twice"));
        }
        let (allocated, freed) = (boxes::alloc_count(), boxes::free_count());
        boxes::reset_counts();
        if allocated != freed {
            return Err(format!(
                "JIT (optimize {optimize}) allocated {allocated} box(es) and freed {freed}"
            ));
        }
    }
    let main = world
        .functions
        .get("main")
        .filter(|f| f.params.is_empty() && f.generics.is_empty());
    let entries: &[&str] = if main.is_some() {
        &[entry, "main"]
    } else {
        &[entry]
    };
    for name in entries {
        for target in OBJECT_TARGETS {
            native::object_for_target(world, name, Some(target))
                .map_err(|e| format!("{target} object for `{name}` failed: {e}"))?;
        }
    }
    if let Some(main) = main
        && is_integer(&main.result)
        && links_executables()
    {
        let mut runtime = Runtime::new(world);
        runtime.fuel = fuel;
        let value = runtime
            .run("main", vec![])
            .map_err(|e| format!("evaluator `main()` failed: {e}"))?;
        let low = value
            .to_string()
            .parse::<i128>()
            .map_err(|_| format!("`main()` returned {value}, not an integer"))?
            & 0xff;
        let status = executable_status(source)?;
        if status != Some(low as i32) {
            return Err(format!(
                "the executable exited with {status:?}; the evaluator's `main()` is {value} (low byte {low})"
            ));
        }
    }
    Ok(())
}

fn expect_true(value: &Value, engine: &str) -> Result<(), String> {
    match value {
        Value::Bool(true) => Ok(()),
        other => Err(format!("{engine} returned {other}")),
    }
}

fn is_integer(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Named(name, args) if args.is_empty() && matches!(
            name.as_str(),
            "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64"
        )
    )
}

fn links_executables() -> bool {
    cfg!(unix)
        && Command::new("cc")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
}

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Temp(PathBuf);

impl Temp {
    fn new() -> Result<Self, String> {
        let dir = std::env::temp_dir().join(format!(
            "vesper-engines-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        Ok(Self(dir))
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Builds `source` with `vesper build` and runs the executable.
fn executable_status(source: &str) -> Result<Option<i32>, String> {
    let temp = Temp::new()?;
    let file = temp.path("main.ves");
    std::fs::write(&file, source).map_err(|e| format!("{}: {e}", file.display()))?;
    let exe = temp.path("main");
    let vesper = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let built = Command::new(vesper)
        .arg("build")
        .arg(&file)
        .arg("-o")
        .arg(&exe)
        .output()
        .map_err(|e| format!("vesper build: {e}"))?;
    if !built.status.success() {
        return Err(format!(
            "vesper build failed\n{}{}",
            String::from_utf8_lossy(&built.stdout),
            String::from_utf8_lossy(&built.stderr)
        ));
    }
    run(&exe)
}

fn run(exe: &Path) -> Result<Option<i32>, String> {
    Command::new(exe)
        .output()
        .map(|o| o.status.code())
        .map_err(|e| format!("{}: {e}", exe.display()))
}
