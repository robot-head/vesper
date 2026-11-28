//! Stage 2 bootstrap switch slice 3b (#2660, part of #2659): the direct
//! callee words of each body request from `@vesper/native-walk`
//! (packages/vesper-codegen-walk/src/calls.ves) against the Rust walk's
//! `Codegen::plain_call`.
//!
//! A recording `body` hook notes, for every function the walk would hand
//! to Vesper, the callee words of its request (one per `Op::Call` of the
//! body, in instruction order: a function id with `WORDS_CALL`,
//! `FAIL_CALL` or `STATUS_CALL`, or `NO_CALLEE`), then declines, so the
//! Rust walk emits the object. On the first request of each lowered
//! program it also asks the package for the words of every instance, from
//! the calls input of that program (`calls_input_flags_wide`, as
//! @vesper/native-lib reads it) with the compile's task. Each recorded
//! function's words must equal the package's words for its instance.
//!
//! The compiles are libraries of plain functions (`vesper build --lib`, the
//! fixtures of self_host_native_lib.rs and a few more callers), the test
//! sources below and the corpus worlds with a `main` (the native-walk body
//! corpus), with optimize on and off. `NATIVE_WALK_CALLS_STEP=n` compares
//! every n-th corpus world (default 4).
#[path = "../../../tests/common/check_harness.rs"]
mod check_harness;
#[path = "../../../tests/common/check_words.rs"]
mod check_words;
#[path = "../../../tests/common/codegen_calls_sources.rs"]
mod codegen_calls_sources;
#[path = "../../../tests/common/codegen_calls_words.rs"]
mod codegen_calls_words;
#[path = "../../../tests/common/ir_words.rs"]
mod ir_words;
#[path = "common/native_lib_fixtures.rs"]
mod native_lib_fixtures;

use std::cell::RefCell;
use std::sync::OnceLock;

use vesper::codegen_swap::{self, CodegenHooks, Lent};
use vesper::ir::Program;
use vesper::native::export::{LibraryOptions, compile_source};
use vesper_cranelift_sys::cranelift_module::Module;

/// The package's answer for a task the Rust compile refuses before the
/// walk (`REFUSED` in calls.ves).
const REFUSED: u64 = u64::MAX;
/// The calls task flags (packages/vesper-codegen-calls/src/data.ves):
/// optimize, a library's first root and its other roots.
const OPTIMIZE_TASK: u64 = 1;
const LIBRARY_TASK: u64 = 4;
const MORE_ROOTS_TASK: u64 = 8;
/// The bits above a callee word's function id: its call flags.
const CALL_FLAGS: u64 = codegen_swap::body::FAIL_CALL
    | codegen_swap::body::STATUS_CALL
    | codegen_swap::body::WORDS_CALL
    | codegen_swap::body::SRET_CALL;
/// The target the library objects are built for.
const TRIPLE: &str = "x86_64-unknown-linux-gnu";
/// Every n-th corpus world by default.
const DEFAULT_STEP: usize = 4;
/// The entry of the corpus worlds and test sources.
const ENTRY: &str = "main";
/// A compile thread's stack: the CLI's deep stack.
const STACK_BYTES: usize = 256 << 20;

fn vesper() -> &'static check_harness::Entry {
    static V: OnceLock<check_harness::Entry> = OnceLock::new();
    V.get_or_init(|| {
        codegen_swap::with_hooks(CodegenHooks::default(), || {
            check_harness::Package::load("packages/vesper-codegen-walk")
                .entry("native_walk_call_words", true)
        })
    })
}

/// What one compile compared.
#[derive(Default, Debug)]
struct Tally {
    /// Functions whose request was compared, and their calls.
    functions: u64,
    calls: u64,
    /// Of the calls, those the walk makes directly, and of those the
    /// calls with each flag.
    direct: u64,
    fail: u64,
    status: u64,
    words: u64,
    /// Lowered programs the calls input cannot encode (left unverified).
    unencodable: u64,
    diffs: Vec<String>,
}

impl Tally {
    fn add(&mut self, o: Tally) {
        self.functions += o.functions;
        self.calls += o.calls;
        self.direct += o.direct;
        self.fail += o.fail;
        self.status += o.status;
        self.words += o.words;
        self.unencodable += o.unencodable;
        self.diffs.extend(o.diffs);
    }

    fn summary(&self) -> String {
        format!(
            "{} functions, {} calls ({} direct: {} fail, {} status, {} words), \
             {} programs not encodable, {} diffs",
            self.functions,
            self.calls,
            self.direct,
            self.fail,
            self.status,
            self.words,
            self.unencodable,
            self.diffs.len()
        )
    }
}

/// The recording state of one compile.
#[derive(Default)]
struct Record {
    name: String,
    /// The calls tasks of the compile: an entry's, or a library's (set by
    /// the `export_plans` hook from its roots).
    tasks: Vec<(String, u64)>,
    /// The lowered program the package answered for (its address) and
    /// its answer: each instance's call words, by instance index.
    answered: Option<(usize, Option<Vec<Vec<u64>>>)>,
    tally: Tally,
}

thread_local! {
    static RECORD: RefCell<Record> = RefCell::new(Record::default());
}

/// The package's answer for `program`: per instance (by index) its call
/// words; `None` when the calls input cannot encode the program.
fn answer(
    world: &vesper::check::World,
    program: &Program,
    tasks: &[(String, u64)],
) -> Option<Vec<Vec<u64>>> {
    let input = codegen_calls_words::calls_input_flags_wide(world, program, tasks)?;
    let words = vesper().words(&input);
    let (&count, mut rest) = words.split_first().expect("an answer per task");
    if count == REFUSED {
        return Some(vec![]);
    }
    let mut out = vec![];
    for i in 0..count {
        let [index, n, tail @ ..] = rest else {
            panic!("an instance row cut short: {rest:?}");
        };
        assert_eq!(*index, i, "instance rows out of order");
        let n = *n as usize;
        out.push(tail[..n].to_vec());
        rest = &tail[n..];
    }
    assert!(rest.is_empty(), "an answer's trailing words: {rest:?}");
    Some(out)
}

/// `native::local_symbol`: a local instance's symbol.
fn local_symbol(i: usize, key: &str) -> String {
    let name: String = key
        .chars()
        .take(96)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("vesper_function_{i}_{name}")
}

/// The function id `key` was declared under: its own symbol (an entry),
/// its export symbol in library `library` (`native_export`'s
/// `vesper_<library>_<name>`) or its local symbol.
fn function_id(module: &dyn Module, library: &str, key: &str) -> Option<usize> {
    let export = format!("vesper_{library}_{key}");
    module
        .declarations()
        .get_functions()
        .find(|(id, decl)| {
            let i = id.as_u32() as usize;
            let name = decl.linkage_name(*id);
            name == key || name == export || name == local_symbol(i, key)
        })
        .map(|(id, _)| id.as_u32() as usize)
}

/// The recording `body` hook: compares the request's callee words with
/// the package's and declines.
fn record(lent: &mut Lent<'_, '_>) -> Result<bool, String> {
    if lent.op != codegen_swap::body::FUNCTION {
        return Ok(false);
    }
    let Some((callees, _, _)) = codegen_swap::body::layout(lent.args) else {
        return Err("a body request without its callees".into());
    };
    let key = lent.site.function;
    RECORD.with(|cell| {
        let mut r = cell.borrow_mut();
        let id = function_id(&*lent.module, &r.name, key);
        let at = std::ptr::from_ref(lent.program) as usize;
        if r.answered.as_ref().is_none_or(|(p, _)| *p != at) {
            let a = answer(lent.world, lent.program, &r.tasks);
            if a.is_none() {
                r.tally.unencodable += 1;
            }
            r.answered = Some((at, a));
        }
        let Some((_, Some(rows))) = &r.answered else {
            return;
        };
        let theirs = id.and_then(|i| rows.get(i)).cloned();
        let name = r.name.clone();
        let t = &mut r.tally;
        t.functions += 1;
        t.calls += callees.len() as u64;
        for &c in callees {
            if c != codegen_swap::body::NO_CALLEE {
                t.direct += 1;
                t.fail += u64::from(c & codegen_swap::body::FAIL_CALL != 0);
                t.status += u64::from(c & codegen_swap::body::STATUS_CALL != 0);
                t.words += u64::from(c & codegen_swap::body::WORDS_CALL != 0);
                debug_assert!(c & !CALL_FLAGS < u64::from(u32::MAX));
            }
        }
        if theirs.as_deref() != Some(callees) {
            t.diffs.push(format!(
                "{name} {key} (id {id:?}): Rust {callees:x?}, Vesper {theirs:x?}"
            ));
        }
    });
    Ok(false)
}

/// The recording `export_plans` hook: notes the library's tasks and
/// declines, so the Rust plans are made.
fn library_tasks(
    _world: &vesper::check::World,
    _program: &Program,
    roots: &[&str],
    optimize: bool,
) -> Option<vesper::native::EntryPlans> {
    let tasks = roots
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let place = if i == 0 {
                LIBRARY_TASK
            } else {
                MORE_ROOTS_TASK
            };
            (r.to_string(), u64::from(optimize) | place)
        })
        .collect();
    RECORD.with(|cell| cell.borrow_mut().tasks = tasks);
    None
}

fn hooks() -> CodegenHooks {
    CodegenHooks {
        body: Some(record),
        export_plans: Some(library_tasks),
        ..CodegenHooks::default()
    }
}

/// Runs `f` with a fresh record named `name` over `tasks`; its tally.
fn recorded(name: &str, tasks: Vec<(String, u64)>, f: impl FnOnce()) -> Tally {
    RECORD.with(|cell| {
        *cell.borrow_mut() = Record {
            name: name.into(),
            tasks,
            ..Record::default()
        }
    });
    codegen_swap::with_hooks(hooks(), f);
    RECORD.with(|cell| std::mem::take(&mut cell.borrow_mut().tally))
}

/// Compiles `main` of `world` to an object, optimize on and off.
fn entry(name: &str, world: &vesper::check::World) -> Tally {
    let mut total = Tally::default();
    for optimize in [true, false] {
        let tasks = vec![(ENTRY.to_string(), u64::from(optimize) * OPTIMIZE_TASK)];
        total.add(recorded(name, tasks, || {
            let _ = vesper::native::object_with_options(world, ENTRY, None, optimize);
        }));
    }
    total
}

/// Builds `source` as library `library`, optimize on and off.
fn library(library: &str, source: &str) -> Tally {
    let mut total = Tally::default();
    for optimize in [true, false] {
        let options = LibraryOptions {
            library,
            target: Some(TRIPLE),
            optimize,
        };
        total.add(recorded(library, vec![], || {
            if let Err(e) = compile_source(library, source, &options) {
                panic!("library {library} does not build: {e:?}");
            }
        }));
    }
    total
}

/// Runs `f` on a thread with the CLI's deep stack (the deepest corpus
/// worlds overflow a test thread's 2 MiB in debug builds).
fn big_stack<R: Send>(f: impl FnOnce() -> R + Send) -> R {
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(STACK_BYTES)
            .spawn_scoped(s, f)
            .expect("spawn a compile thread")
            .join()
            .unwrap_or_else(|e| std::panic::resume_unwind(e))
    })
}

/// Libraries beyond the native-lib fixtures: callers of `fail_only`
/// functions with and without a register status, small enum results, and
/// calls the walk leaves to itself (generic, `inout` and struct-result
/// callees). Recursive callees, so the inliner keeps the calls.
const EXTRA_LIBRARIES: &[(&str, &str)] = &[
    (
        "recursive_fails",
        "fn down(n: u64, acc: u64) -> u64 eff [Fail[ArithOverflow]] {
    if n == 0 { acc } else { down(n - 1, acc * 3) }
}
fn safe_down(n: u64, acc: u64) -> u64 {
    if n == 0 { acc } else { safe_down(wrapping_sub(n, 1), wrapping_add(acc, n)) }
}
pub fn run(n: u64) -> u64 eff [Fail[ArithOverflow]] { down(n, 1) + safe_down(n, 0) }
pub fn plain(n: u64) -> u64 { safe_down(n, 1) }",
    ),
    (
        "enums",
        "pub enum Step { Done(u64), More(u64) }
fn next(n: u64) -> Step { if n < 2 { Step::Done(n) } else { Step::More(n / 2) } }
fn count(n: u64, k: u64) -> u64 {
    match next(n) { Step::Done(x) => wrapping_add(k, x), Step::More(m) => count(m, wrapping_add(k, 1)) }
}
pub fn steps(n: u64) -> u64 { count(n, 0) }",
    ),
    (
        "refused",
        "pub struct Pair { a: u64, b: u64 }
fn pair(x: u64) -> Pair { if x == 0 { Pair { a: 0, b: 0 } } else { let p = pair(wrapping_sub(x, 1)); Pair { a: p.b, b: wrapping_add(p.a, x) } } }
fn bump(inout x: u64, n: u64) { if n > 0 { x = wrapping_add(x, 1); bump(inout x, wrapping_sub(n, 1)); } }
fn ident[T: mod copy](x: T, n: u64) -> T { if n == 0 { x } else { ident(x, wrapping_sub(n, 1)) } }
pub fn use_all(x: u64) -> u64 {
    let p = pair(x);
    let mut y: u64 = x;
    bump(inout y, 3);
    wrapping_add(wrapping_add(p.a, y), ident(x, 2))
}",
    ),
];

/// The native-walk body differential, whose test sources call plain,
/// `fail_only`, struct-result and small-enum functions.
const BODY_TESTS: &[&str] =
    &["crates/vesper-codegen-swap-test/tests/self_host_native_walk_body.rs"];

fn step() -> usize {
    std::env::var("NATIVE_WALK_CALLS_STEP")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_STEP)
}

/// 0 differences between the Rust walk's callee words and the package's,
/// over the libraries, the test sources and the corpus.
#[test]
fn vesper_call_words_match_plain_call() {
    big_stack(|| {
        let mut total = Tally::default();
        for (name, source) in native_lib_fixtures::LIBRARIES.iter().chain(EXTRA_LIBRARIES) {
            total.add(library(name, source));
        }
        let sources = codegen_calls_sources::worlds_in(BODY_TESTS)
            .into_iter()
            .chain(codegen_calls_sources::source_worlds());
        for (name, world) in sources.filter(|(_, w)| w.functions.contains_key(ENTRY)) {
            total.add(entry(&name, &world));
        }
        let library_and_sources = total.direct;
        for case in check_harness::corpus()
            .iter()
            .filter(|c| c.world.functions.contains_key(ENTRY))
            .step_by(step())
        {
            total.add(entry(&case.name, &case.world));
        }
        eprintln!("native walk call words: {}", total.summary());
        assert!(
            library_and_sources > 0,
            "the libraries and sources made no direct call"
        );
        assert!(
            total.fail > 0 && total.status > 0,
            "no fail or status call was compared"
        );
        assert!(
            total.diffs.is_empty(),
            "{} differences:\n{}",
            total.diffs.len(),
            total.diffs.join("\n")
        );
    });
}
