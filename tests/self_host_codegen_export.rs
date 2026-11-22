//! Self-hosting SH4 lane 12 (#1286): `@vesper/codegen-export` against the
//! Rust export ABI in src/native_export.rs and src/native_export/**.
//!
//! Part 1 ports the Rust binding generators (`rust_binding`, ABI v1-v4).
//! The corpus is every Vesper library a port crate or the hybrid build
//! links (the `Port::new` calls of crates/*/build.rs, listed in [`PORTS`]),
//! compiled with the `vesper` executable exactly as the port harness does
//! (`build --lib`, with `--no-opt`: the binding does not depend on the
//! optimizer), plus every interface again under other binding options.
//!
//! - `rust_bindings_match_committed`: the Rust generator on each
//!   interface reproduces the crate's committed binding, which checks
//!   that [`PORTS`] builds each library as its build script does.
//! - `vesper_bindings_match_rust`: the Vesper generator's text equals the
//!   Rust generator's, byte for byte, for every interface and every
//!   options variant.
//!
//! `CODEGEN_EXPORT_PORTS` limits the corpus to a comma-separated list of
//! libraries (the default is all of them).
#[path = "common/check_harness.rs"]
mod check_harness;
#[path = "self_host_codegen_export/plan_words.rs"]
mod plan_words;
#[path = "self_host_codegen_export/ports.rs"]
mod ports;
#[path = "self_host_codegen_export/words.rs"]
mod words;

use check_harness::*;
use ports::*;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use vesper::native::export::{BindingOptions, LibraryInterface, rust_binding};

fn package() -> &'static Package {
    static P: OnceLock<Package> = OnceLock::new();
    P.get_or_init(|| Package::load("packages/vesper-codegen-export"))
}

/// One compiled library.
struct Built {
    crate_dir: PathBuf,
    library: &'static str,
    rust_types: Option<&'static str>,
    binding: &'static str,
    interface: LibraryInterface,
}

fn selected(library: &str) -> bool {
    match std::env::var("CODEGEN_EXPORT_PORTS") {
        Ok(list) if !list.is_empty() => list.split(',').any(|l| l.trim() == library),
        _ => true,
    }
}

fn build(port: &Port, scratch: &Path) -> Result<Built, String> {
    let (krate, library, _, rust_types, binding) = *port;
    let dir = repo().join("crates").join(krate);
    let input = port_input(port, &repo(), scratch)?;
    let json = scratch.join(format!("{library}.json"));
    let out = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .args(["build", "--no-opt", "--lib", "--lib-name", library])
        .arg(&input)
        .arg("-o")
        .arg(scratch.join(format!("{library}.o")))
        .arg("--interface")
        .arg(&json)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "{library}: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let text = std::fs::read_to_string(&json).map_err(|e| e.to_string())?;
    let interface = serde_json::from_str(&text).map_err(|e| format!("{library}: {e}"))?;
    Ok(Built {
        crate_dir: dir,
        library,
        rust_types,
        binding,
        interface,
    })
}

/// Every selected library, compiled once per test binary (four at a time).
fn corpus() -> &'static [Built] {
    static C: OnceLock<Vec<Built>> = OnceLock::new();
    C.get_or_init(|| {
        let scratch =
            std::env::temp_dir().join(format!("vesper-codegen-export-{}", std::process::id()));
        std::fs::create_dir_all(&scratch).expect("scratch directory");
        let ports: Vec<&Port> = PORTS.iter().filter(|p| selected(p.1)).collect();
        let next = std::sync::atomic::AtomicUsize::new(0);
        let results = std::sync::Mutex::new(vec![]);
        std::thread::scope(|s| {
            for _ in 0..4 {
                s.spawn(|| {
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(p) = ports.get(i) else { break };
                        let r = build(p, &scratch);
                        results.lock().unwrap().push((i, r));
                    }
                });
            }
        });
        let _ = std::fs::remove_dir_all(&scratch);
        let mut results = results.into_inner().unwrap();
        results.sort_by_key(|(i, _)| *i);
        let mut out = vec![];
        for (_, r) in results {
            match r {
                Ok(b) => out.push(b),
                Err(e) => panic!("build failed: {e}"),
            }
        }
        out
    })
}

fn options(b: &Built) -> BindingOptions {
    BindingOptions {
        types_path: b.rust_types.map(str::to_string),
    }
}

/// The options a library is checked under: its own, plus no types path
/// and a made-up one (so every library exercises both generator paths).
fn variants(b: &Built) -> Vec<BindingOptions> {
    let mut v = vec![options(b)];
    for alt in [None, Some("crate::alt::types")] {
        let alt = alt.map(str::to_string);
        if alt != v[0].types_path {
            v.push(BindingOptions { types_path: alt });
        }
    }
    v
}

#[test]
fn rust_bindings_match_committed() {
    let mut stale = vec![];
    for b in corpus() {
        let committed = std::fs::read_to_string(b.crate_dir.join(b.binding))
            .unwrap_or_default()
            .replace("\r\n", "\n");
        if rust_binding(&b.interface, &options(b)) != committed {
            stale.push(b.library);
        }
    }
    eprintln!(
        "committed bindings: {} of {} reproduced",
        corpus().len() - stale.len(),
        corpus().len()
    );
    assert!(stale.is_empty(), "not reproduced: {stale:?}");
}

#[test]
fn vesper_bindings_match_rust() {
    let entry = package().entry("binding_text", true);
    let (mut bad, mut cases, mut bytes) = (0, 0, 0usize);
    let mut abis = std::collections::BTreeMap::<String, usize>::new();
    for b in corpus() {
        *abis.entry(b.interface.abi.clone()).or_default() += 1;
        for o in variants(b) {
            cases += 1;
            let rust = rust_binding(&b.interface, &o);
            bytes += rust.len();
            let vesper = entry.text(&words::binding_input(&b.interface, &o));
            if let Some(d) = first_difference(&rust, &vesper) {
                bad += 1;
                if bad <= 5 {
                    eprintln!(
                        "binding: {} (types_path {:?}) differs at {d}",
                        b.library, o.types_path
                    );
                }
            }
        }
    }
    eprintln!(
        "bindings: {bad} of {cases} differ ({} libraries, {bytes} bytes, {abis:?})",
        corpus().len()
    );
    assert_eq!(bad, 0);
}

// ------------------------------------------------------------- directives

/// The input of `directives_dump`: the source, the `@TARGET@` and `@ABI@`
/// values and `vsys_target`'s error (empty on success), each a byte length
/// word and the bytes padded to a word.
fn directives_input(source: &str, target: Option<&str>) -> Vec<u8> {
    let (triple, abi, error) = match vesper::packages::vsys_target(target) {
        Ok((t, a)) => (t.to_string(), a.to_string(), String::new()),
        Err(e) => (String::new(), String::new(), e),
    };
    let mut out = vec![];
    for s in [source, &triple, &abi, &error] {
        out.extend((s.len() as u64).to_le_bytes());
        out.extend(s.as_bytes());
        while !out.len().is_multiple_of(8) {
            out.push(0);
        }
    }
    out
}

/// `Directives::parse` as the dump `parse_directives` writes. The `.vsys`
/// descriptor parse stays in Rust, so a `vsys` import shows its handler,
/// Rust path and filled-in text, not its library name and symbols.
fn directives_dump(source: &str, target: Option<&str>) -> String {
    use std::fmt::Write as _;
    let d = match vesper::native::export::Directives::parse(source, target) {
        Ok(d) => d,
        Err(e) => return format!("error {e}\n"),
    };
    let mut o = format!(
        "abi {}\noptions {}\n",
        d.abi.map_or("none".to_string(), |a| a.to_string()),
        d.option_mirrors as u8
    );
    for i in &d.instances {
        let _ = writeln!(
            o,
            "instance {} {} {} {}",
            i.name.as_deref().unwrap_or("-"),
            i.function,
            i.spelling(),
            i.alias()
        );
    }
    for l in &d.links {
        let _ = writeln!(
            o,
            "link {} {} {}",
            l.library,
            l.handler.map_or("-", |h| h.name()),
            l.rust_path.as_deref().unwrap_or("-")
        );
    }
    for v in &d.vsys {
        let _ = writeln!(
            o,
            "vsys {} {} {}",
            v.handler.name(),
            v.rust_path.as_deref().unwrap_or("-"),
            v.source.len()
        );
        o.push_str(&v.source);
    }
    o
}

/// Every `.ves` source under crates/, packages/, examples/ and tests/ that
/// names an `@export` directive.
fn directive_sources() -> Vec<(String, String)> {
    let mut out = vec![];
    let mut todo: Vec<PathBuf> = ["crates", "packages", "examples", "tests"]
        .iter()
        .map(|d| repo().join(d))
        .collect();
    while let Some(dir) = todo.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                if p.file_name().is_some_and(|n| n != "target") {
                    todo.push(p);
                }
            } else if p.extension().is_some_and(|x| x == "ves")
                && let Ok(text) = std::fs::read_to_string(&p)
                && text.contains("// @export")
            {
                out.push((p.display().to_string(), text));
            }
        }
    }
    out.sort();
    out
}

/// Directive lines that exercise every branch, refusals included. The
/// `.vsys` bodies are valid descriptors (their parse is Rust's).
const DIRECTIVE_CASES: &[&str] = &[
    "// @export abi 3\n",
    "// @export abi 4\n",
    "// @export abi 3 options\n",
    "// @export abi 3 options options\n",
    "// @export abi 5\n",
    "// @export abi\n",
    "// @export abi 3\n// @export abi 4\n",
    "// @export abi 3 fast\n",
    "// @export abilities\n",
    "  \t// @export abi 3   \r\n",
    "// @export instance largest_u64 = largest[u64]\n",
    "// @export instance largest[i32]\n",
    "// @export instance pair[(u8, u16), Option[u32]]\n",
    "// @export instance unit[()]\n",
    "// @export instance one[(u8)]\n",
    "// @export instance one[(u8,)]\n",
    "// @export instance nested[Result[Option[u8], (i64, bool)]]\n",
    "// @export instance trailing[u8,]\n",
    "// @export instance lit[Words[3]]\n",
    "// @export instance Bad = f[u8]\n",
    "// @export instance = f[u8]\n",
    "// @export instance f(u8)\n",
    "// @export instance f[u8\n",
    "// @export instance [u8]\n",
    "// @export instance f g[u8]\n",
    "// @export instance f[]\n",
    "// @export instance f[u8 u16]\n",
    "// @export instance f[,]\n",
    "// @export instance mixed_Case[MyType, my_type]\n",
    "// @export instance lower_args[my_type[u8]]\n",
    "// @export vsys Clock direct rust=crate::shim\n",
    "// @export vsys Clock isolated(mpk)\n",
    "// @export vsys Clock\n",
    "// @export vsys Clock direct direct\n",
    "// @export vsys Clock rust=\n",
    "// @export vsys Clock rust=a rust=b\n",
    "// @export vsys Clock\n// @export vsys Clock direct\n",
    "// @export vsys Clock\n// @export vsys Timer rust=t\n",
    "// @export vsys\n",
    "// @export vsys indirect\n",
    "// @export vsys direct fast\n",
    "// @export vsys direct rust=\n",
    "// @export frobnicate\n",
    "// @export\n",
    "fn f() -> Unit {\n    ()\n}\n// not // @export abi 3\n",
    "// @export vsys direct rust=crate::shim rust=crate::other\n// | sys library Clock (target = \"@TARGET@\", abi = \"@ABI@\", link = \"static\",\n// |     name = \"vesper_port_pilot\", artifact = \"blake3:0000000000000000000000000000000000000000000000000000000000000000\") {\n// |     fn pilot_clock_ticks() -> ForeignVal[u64];\n// | }\nfn after() -> Unit {\n    ()\n}\n",
    "// @export vsys isolated(mpk)\n  // |sys library Clock (target = \"@TARGET@\", abi = \"@ABI@\", link = \"static\",\n// |  name = \"c\", artifact = \"blake3:0000000000000000000000000000000000000000000000000000000000000000\") {\n// |  fn c_ticks() -> ForeignVal[u64];\n// | }\n",
];

#[test]
fn vesper_directives_match_rust() {
    let entry = package().entry("directives_dump", true);
    let mut cases: Vec<(String, String, Option<&str>)> = directive_sources()
        .into_iter()
        .map(|(p, t)| (p, t, None))
        .collect();
    for (i, c) in DIRECTIVE_CASES.iter().enumerate() {
        cases.push((format!("case {i}"), c.to_string(), None));
    }
    // The vsys cases again for the other hosted targets and a bad one.
    let vsys: Vec<_> = DIRECTIVE_CASES
        .iter()
        .filter(|c| c.contains("// |"))
        .collect();
    for target in [
        "aarch64-apple-darwin",
        "x86_64-pc-windows-msvc",
        "riscv64gc-unknown-linux-gnu",
        "not-a-target",
    ] {
        for (i, c) in vsys.iter().enumerate() {
            cases.push((format!("vsys {i} on {target}"), c.to_string(), Some(target)));
        }
    }
    let (mut bad, mut errors) = (0, 0);
    for (name, source, target) in &cases {
        let rust = directives_dump(source, *target);
        errors += rust.starts_with("error ") as usize;
        let vesper = entry.text(&directives_input(source, *target));
        if let Some(d) = first_difference(&rust, &vesper) {
            bad += 1;
            if bad <= 8 {
                eprintln!("directives: {name} differs at {d}\nrust:   {rust}vesper: {vesper}");
            }
        }
    }
    eprintln!(
        "directives: {bad} of {} sources differ ({errors} refused)",
        cases.len()
    );
    assert!(cases.len() > 60, "only {} cases", cases.len());
    assert_eq!(bad, 0);
}

// ----------------------------------------------------------------- timing

/// ABBA over the whole corpus (every library under its own options): the
/// Rust generator, against the Vesper generator alone (inputs encoded
/// beforehand) and with the encoding and the text unpacking included.
#[test]
#[ignore = "timing; CHECK_HARNESS_SPEED=1"]
fn binding_cost() {
    if !speed() {
        return;
    }
    let entry = package().entry("binding_text", true);
    let cases: Vec<(&LibraryInterface, BindingOptions)> = corpus()
        .iter()
        .map(|b| (&b.interface, options(b)))
        .collect();
    let inputs: Vec<Vec<u8>> = cases
        .iter()
        .map(|(i, o)| words::binding_input(i, o))
        .collect();
    let encode = time(|| {
        for (i, o) in &cases {
            std::hint::black_box(words::binding_input(i, o));
        }
    });
    eprintln!("encode: {encode:?} for {} libraries", cases.len());
    abba(
        "binding generator",
        &mut || {
            time(|| {
                for (i, o) in &cases {
                    std::hint::black_box(rust_binding(i, o));
                }
            })
        },
        &mut || {
            time(|| {
                for input in &inputs {
                    std::hint::black_box(entry.words(input));
                }
            })
        },
    );
    abba(
        "binding generator with encode and unpack",
        &mut || {
            time(|| {
                for (i, o) in &cases {
                    std::hint::black_box(rust_binding(i, o));
                }
            })
        },
        &mut || {
            time(|| {
                for (i, o) in &cases {
                    std::hint::black_box(entry.text(&words::binding_input(i, o)));
                }
            })
        },
    );
}

// ---------------------------------------------------------------- planner

/// What `plan_with` saw during one compile: its inputs as words, and the
/// Rust planner's answer.
type Seen = (
    Vec<u8>,
    Result<LibraryInterface, Vec<vesper::native::export::ExportError>>,
);

thread_local! {
    static SEEN: std::cell::RefCell<Vec<Seen>> = const { std::cell::RefCell::new(vec![]) };
}

/// The `plan` hook of [`planned`]: records the inputs and the Rust answer,
/// then stops the compile (the object is not needed).
fn record_plan(
    world: &vesper::check::World,
    public: &std::collections::BTreeSet<String>,
    library: &str,
    directives: &vesper::native::export::Directives,
) -> Option<Result<LibraryInterface, Vec<vesper::native::export::ExportError>>> {
    let input = plan_words::plan_input(world, public, library, directives);
    let rust = vesper::native::export::plan_native(world, public, library, directives);
    SEEN.with(|s| s.borrow_mut().push((input, rust)));
    Some(Err(vec![]))
}

/// The plans `vesper build --lib` makes compiling `source` as `library`.
fn planned(source: &str, library: &str) -> Vec<Seen> {
    let options = vesper::native::export::LibraryOptions {
        library,
        target: None,
        optimize: false,
    };
    let hooks = vesper::codegen_swap::CodegenHooks {
        plan: Some(record_plan),
        ..Default::default()
    };
    SEEN.with(|s| s.borrow_mut().clear());
    let r = vesper::codegen_swap::with_hooks(hooks, || {
        vesper::native::export::compile_source_with("plan.ves", source, &options, &[])
    });
    let seen = SEEN.with(|s| std::mem::take(&mut *s.borrow_mut()));
    if seen.is_empty()
        && let Err(e) = r
    {
        eprintln!("plan: {library} was never planned: {e}");
    }
    seen
}

/// Sources the planner refuses (or plans in an unusual way), one rule each.
const PLAN_CASES: &[(&str, &str)] = &[
    ("Bad-Name", "pub fn f() -> i64 { 1 }\n"),
    ("empty", "fn hidden() -> i64 { 1 }\n"),
    (
        "generic",
        "pub fn id[T](in x: T) -> i64 { 1 }\npub fn one() -> i64 { 1 }\n",
    ),
    (
        "effect",
        "effect Log { fn log(x: i64) -> Unit; }\npub fn f() -> Unit eff [Log] { Log::log(1) }\n",
    ),
    (
        "fail",
        "enum E { A, B(i64) }\npub fn f(x: i64) -> i64 eff [Fail[E]] { x }\n",
    ),
    (
        "failseq",
        "pub fn f(x: i64) -> i64 eff [Fail[ChunkSeq[i64]]] { x }\n",
    ),
    (
        "seqs",
        "pub fn total(in xs: Slice[i64]) -> i64 { let mut t: i64 = 0; for i in 0..xs.len() { t = wrapping_add(t, match xs.try_get(i) { Option::Some(v) => v, Option::None => 0 }); } t }\npub fn make(n: u64) -> ChunkSeq[i64] { ChunkSeq::with_capacity[i64](n) }\n",
    ),
    (
        "strings",
        "pub fn len(in s: Str) -> u64 { s.len() }\npub fn grow(inout s: String) -> Unit { s.push_str(\"x\") }\n",
    ),
    ("strview", "pub fn f() -> Str { \"x\" }\n"),
    (
        "handles",
        "struct Bag { items: ChunkSeq[i64], name: String }\npub fn new_bag() -> Bag { Bag { items: ChunkSeq::new[i64](), name: String::new() } }\npub fn count(in b: Bag) -> u64 { b.items.len() }\npub fn eat(sink b: Bag) -> Unit { () }\n",
    ),
    (
        "linear",
        "linear struct Token { id: i64 }\npub fn mint() -> Token { Token { id: 1 } }\npub fn burn(sink t: Token) -> i64 { let Token { id } = t; id }\n",
    ),
    (
        "copies",
        "struct P { x: i64, y: f64 }\nenum Shape { Dot, Line(P, P) }\nstruct Empty {}\npub fn mid(a: P, b: P) -> P { P { x: a.x, y: b.y } }\npub fn shape(s: Shape) -> i64 { 0 }\npub fn empty(e: Empty) -> i64 { 0 }\npub fn opt(x: Option[i64]) -> Result[i64, bool] { Result::Ok(1) }\npub fn pair(t: (i64, bool)) -> (i64, bool) { t }\n",
    ),
    (
        "v3",
        "// @export abi 3 options\ncopy struct P { x: i64, y: f64 }\nenum Shape { Dot, Line(P, P) }\npub fn mid(inout a: P, in b: P) -> P { P { x: a.x, y: b.y } }\npub fn first(in ps: Slice[P]) -> Option[P] { Option::None }\npub fn opt(in x: Option[i64]) -> Result[i64, bool] { Result::Ok(1) }\npub fn shape(s: Shape) -> i64 { 0 }\n",
    ),
    (
        "v4",
        "// @export abi 4\nstruct Bag { items: ChunkSeq[i64] }\npub fn new_bag() -> Bag { Bag { items: ChunkSeq::new[i64]() } }\npub fn count(in b: Bag) -> u64 { b.items.len() }\n",
    ),
    (
        "wide",
        "pub fn add(a: i128, b: u128) -> i128 { a }\nstruct W { lo: i128, hi: u64 }\npub fn w(x: W) -> u64 { x.hi }\n",
    ),
    (
        "unit",
        "pub fn nothing() -> Unit { () }\npub fn flag(b: bool) -> bool { b }\n",
    ),
    ("never", "pub fn boom() -> Never { loop { } }\n"),
    (
        "generic_type",
        "struct G[T] { v: T }\npub fn g(x: G[i64]) -> i64 { x.v }\n",
    ),
    (
        "boxes",
        "struct Node { v: i64, next: Option[Box[Node]] }\npub fn leaf(v: i64) -> Node { Node { v: v, next: Option::None } }\npub fn val(in n: Node) -> i64 { n.v }\npub fn boxed(v: i64) -> Box[i64] { Box::new(v) }\n",
    ),
    (
        "seq_of_handles",
        "struct H { s: String }\npub fn hs() -> ChunkSeq[H] { ChunkSeq::new[H]() }\n",
    ),
    (
        "instances",
        "// @export instance first[i64]\n// @export instance first_bool = first[bool]\n// @export instance first_again = first[i64]\npub fn first[T](in xs: Slice[T]) -> u64 { xs.len() }\n",
    ),
    (
        "copy_structs",
        "copy struct P { x: i64, y: f64 }\ncopy struct Q { p: P, n: u8 }\npub fn q(a: P) -> Q { Q { p: a, n: 1 } }\npub fn ps(in xs: Slice[P]) -> u64 { xs.len() }\n",
    ),
    (
        "v3_plain",
        "// @export abi 3\ncopy struct P { x: i64 }\ncopy struct E0 {}\nenum K { A, B }\npub fn p(inout a: P, e: E0, k: K) -> Option[P] { Option::None }\n",
    ),
    (
        "tuple_handle",
        "struct H { s: String }\npub fn two() -> (H, Option[H]) { (H { s: String::new() }, Option::None) }\npub fn bad() -> (H, Box[i64]) { (H { s: String::new() }, Box::new(1)) }\n",
    ),
];

#[test]
fn vesper_plans_match_rust() {
    let entry = package().entry("export_plan", true);
    let scratch = std::env::temp_dir().join(format!("vesper-plan-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).expect("scratch directory");
    let mut sources: Vec<(String, String)> = PLAN_CASES
        .iter()
        .map(|(l, s)| (l.to_string(), s.to_string()))
        .collect();
    for port in PORTS.iter().filter(|p| selected(p.1)) {
        let input = port_input(port, &repo(), &scratch).unwrap();
        // A package port goes through the CLI's package assembly, which the
        // hybrid's tests/codegen_export.rs covers.
        if input.is_dir() {
            continue;
        }
        let text = std::fs::read_to_string(&input).unwrap();
        sources.push((port.1.to_string(), text));
    }
    let _ = std::fs::remove_dir_all(&scratch);
    let (mut plans, mut bad, mut refused, mut none) = (0, 0, 0, vec![]);
    for (library, source) in &sources {
        let seen = planned(source, library);
        if seen.is_empty() {
            none.push(library.clone());
        }
        for (input, rust) in seen {
            plans += 1;
            refused += rust.is_err() as usize;
            let vesper = plan_words::decode_plan(&entry.words(&input));
            if std::env::var_os("PLAN_SHOW").is_some() {
                eprintln!("plan: {library}: {rust:?}");
            }
            if vesper != rust {
                bad += 1;
                if bad <= 5 {
                    eprintln!("plan: {library} differs\n rust:   {rust:?}\n vesper: {vesper:?}");
                }
            }
        }
    }
    eprintln!(
        "plans: {bad} of {plans} differ ({refused} refused; {} sources, {} never planned: {none:?})",
        sources.len(),
        none.len()
    );
    assert_eq!(bad, 0);
}

// ------------------------------------------------------------ interface file

/// `interface_json` in Vesper (json.ves) against serde_json, for every
/// library of the corpus and every interface the planner cases plan.
#[test]
fn vesper_interface_json_matches_rust() {
    let entry = package().entry("interface_text", true);
    let mut interfaces: Vec<(String, LibraryInterface)> = corpus()
        .iter()
        .map(|b| (b.library.to_string(), b.interface.clone()))
        .collect();
    for (library, source) in PLAN_CASES {
        for (_, plan) in planned(source, library) {
            if let Ok(i) = plan {
                interfaces.push((library.to_string(), i));
            }
        }
    }
    let (mut bad, mut bytes) = (0, 0);
    for (library, i) in &interfaces {
        let rust = vesper::native::export::interface_json(i).unwrap();
        bytes += rust.len();
        let vesper = entry.text(&words::binding_input(i, &BindingOptions::default()));
        if rust != vesper {
            bad += 1;
            if bad <= 5 {
                let d = first_difference(&rust, &vesper)
                    .unwrap_or_else(|| "in the line endings".into());
                eprintln!("interface: {library} differs {d}");
            }
        }
    }
    eprintln!(
        "interface files: {bad} of {} differ ({bytes} bytes)",
        interfaces.len()
    );
    assert_eq!(bad, 0);
}
