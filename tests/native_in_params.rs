//! `in` parameters read in place (port speed lane 3, #1023).
//!
//! On the native optimize path an `in` parameter of a function that calls
//! nothing, whose word is a handle (a sequence or a string), stays in its
//! incoming argument register instead of being copied to a zeroed home slot
//! at entry: the checker forbids every write to an `in` parameter, so the
//! word read at entry is the word at every later read. The shape tests
//! check the emitted Cranelift function: no incoming argument is stored,
//! and warp-wheel's `sum3` has no stack slot. The differential tests check
//! that the evaluator, both JIT configurations, x86-64 and AArch64 objects
//! and an executable agree, including parameters that are lent to calls or
//! read after the caller changed its own sequence.
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::{check, native, parser, runtime::Runtime};

static NEXT: AtomicU64 = AtomicU64::new(0);

fn accepted(source: &str) -> check::Checked {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:#?}", c.diagnostics);
    c
}

/// For each emitted function named `function` (optimizer on or off): its
/// explicit stack slots, and how many stores write one of its incoming
/// arguments to memory (a parameter's home slot).
fn shape(source: &str, function: &str, optimize: bool) -> Vec<(usize, usize)> {
    use cranelift_codegen::ir::{InstructionData, Opcode};
    let c = accepted(source);
    let mut out = vec![];
    native::object_with_observer(
        &c.world,
        "main",
        None,
        optimize,
        Some(&mut |f: native::EmittedFunction| {
            if f.source != function {
                return;
            }
            let func = &f.function;
            let entry = func.layout.entry_block().unwrap();
            let params: Vec<_> = func.dfg.block_params(entry).to_vec();
            let mut spills = 0;
            for block in func.layout.blocks() {
                for inst in func.layout.block_insts(block) {
                    if let InstructionData::Store {
                        opcode: Opcode::Store,
                        args,
                        ..
                    } = func.dfg.insts[inst]
                        && params.contains(&func.dfg.resolve_aliases(args[0]))
                    {
                        spills += 1;
                    }
                }
            }
            out.push((func.sized_stack_slots.len(), spills));
        }),
    )
    .unwrap();
    assert!(!out.is_empty(), "{function} was not emitted");
    out
}

fn links_executables() -> bool {
    cfg!(unix)
        && Command::new("cc")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
}

/// Runs `main` in the evaluator, both JIT configurations, x86-64 and
/// AArch64 objects and (where a C driver links) an executable.
fn differential(name: &str, source: &str) -> u64 {
    let c = accepted(source);
    let evaluated = Runtime::new(&c.world)
        .run("main", vec![])
        .unwrap_or_else(|e| panic!("{name}: evaluator failed: {e}"))
        .to_string();
    for optimize in [true, false] {
        let jit = native::jit_with_options(&c.world, "main", optimize)
            .unwrap_or_else(|e| panic!("{name}: JIT (optimize {optimize}) failed: {e}"))
            .to_string();
        assert_eq!(jit, evaluated, "{name}: JIT (optimize {optimize})");
    }
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        let (obj, _) = native::object_with_options(&c.world, "main", Some(target), true)
            .unwrap_or_else(|e| panic!("{name}: {target}: {e}"));
        assert_eq!(&obj[..4], b"\x7fELF");
    }
    let expected: u64 = evaluated.parse().unwrap();
    if links_executables() {
        let dir = std::env::temp_dir().join(format!(
            "vesper-in-params-test-{}-{}",
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
            "{name}: build failed: {}",
            String::from_utf8_lossy(&built.stderr)
        );
        let run = Command::new(&exe).output().unwrap();
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(run.status.code(), Some((expected & 0xff) as i32), "{name}");
    }
    expected
}

/// warp-wheel's `sum3` (port parity gap 3).
const SUM3: &str = r#"
fn sum3(in s: ChunkSeq[u64], i: u64) -> u64 {
    let a = match s.try_get(i) {
        Option::Some(v) => v,
        Option::None => 0,
    };
    let b = match s.try_get(wrapping_add(i, 1)) {
        Option::Some(v) => v,
        Option::None => 0,
    };
    wrapping_add(a, b)
}

fn main() -> u64 {
    let mut s = ChunkSeq::new[u64]();
    let n: u64 = 10;
    for i in 0..n {
        s.push(wrapping_mul(i, 3));
    }
    let mut total: u64 = 0;
    for i in 0..n {
        total = wrapping_add(total, sum3(s, i));
    }
    total
}
"#;

#[test]
fn sum3_reads_its_parameter_without_a_slot() {
    assert_eq!(shape(SUM3, "sum3", true), vec![(0, 0)]);
    // The unoptimized path keeps its home slots.
    assert!(
        shape(SUM3, "sum3", false)
            .iter()
            .all(|&(_, spills)| spills > 0)
    );
    // 3 * (sum over i of i + (i + 1)), with the last read out of range.
    assert_eq!(differential("sum3", SUM3), 270);
}

#[test]
fn in_strings_and_sequences_lent_on_to_calls() {
    let source = r#"
fn count(in s: ChunkSeq[u64], k: u64) -> u64 {
    wrapping_add(s.len(), k)
}

fn twice(in s: ChunkSeq[u64], in t: String, depth: u64) -> u64 {
    if depth > 0 {
        return twice(s, t, wrapping_sub(depth, 1));
    }
    let mut total: u64 = 0;
    let n = s.len();
    for i in 0..n {
        total = wrapping_add(total, count(s, t.len()));
        let v = match s.try_get(i) {
            Option::Some(v) => v,
            Option::None => 0,
        };
        total = wrapping_add(total, v);
    }
    total
}

fn main() -> u64 {
    let mut s = ChunkSeq::new[u64]();
    let n: u64 = 6;
    for i in 0..n {
        s.push(wrapping_add(i, 1));
    }
    let t = "hello";
    twice(s, t, 1)
}
"#;
    // `twice` calls, so the lowering gives its parameters home slots
    // (#1023); the CLIF clean-up then promotes those slots, which nothing
    // else addresses, to SSA values (#1426). The leaf `count` reads `s` in
    // place.
    assert!(
        shape(source, "twice", true)
            .iter()
            .all(|&(_, spills)| spills == 0)
    );
    assert!(
        shape(source, "count", true)
            .iter()
            .all(|&(_, spills)| spills == 0)
    );
    assert_eq!(differential("lent", source), 6 * (6 + 5) + 21);
}

#[test]
fn an_in_parameter_read_after_the_caller_mutates_its_own_copy() {
    // The caller pushes between calls: each call sees the length at its
    // own entry.
    let source = r#"
fn last(in s: ChunkSeq[u64], depth: u64) -> u64 {
    if depth > 0 {
        return last(s, wrapping_sub(depth, 1));
    }
    let n = s.len();
    if n == 0 {
        return 0;
    }
    match s.try_get(wrapping_sub(n, 1)) {
        Option::Some(v) => wrapping_add(v, n),
        Option::None => 0,
    }
}

fn main() -> u64 {
    let mut s = ChunkSeq::new[u64]();
    let mut total: u64 = 0;
    let n: u64 = 5;
    for i in 0..n {
        total = wrapping_add(total, last(s, 1));
        s.push(wrapping_mul(i, 10));
    }
    wrapping_add(total, last(s, 1))
}
"#;
    // last after k pushes: 10 * (k - 1) + k, for k = 1..5.
    assert_eq!(differential("mutated", source), 1 + 12 + 23 + 34 + 45);
}
