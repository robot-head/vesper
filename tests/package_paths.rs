//! P3b gaps lane 8 (#1093): qualified paths (`self::m::f`, `super::m::T`,
//! `@scope/name::m::f`) in expressions, types and patterns, `use super::`,
//! `pub use` re-exports, separate type and value namespaces for imports,
//! and the parser fixes the P3 ports reported (open slice intervals,
//! `state`/`linear`/`with` as names). Packages are written to a temporary
//! directory, so the self-hosted parser's corpus never sees them.
use std::{
    path::{Path, PathBuf},
    process::Command,
};
use vesper::{check, native, packages, runtime::Runtime};

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vesper"))
}

/// A fresh package directory holding `files` (path, text).
fn package(tag: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("vesper-package-paths-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for (path, text) in files {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    dir
}

fn manifest(name: &str, deps: &str) -> String {
    format!(
        "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2026\"\ncapability_ceiling = []\n{deps}"
    )
}

fn world(dir: &Path) -> check::Checked {
    let assembled = packages::load(dir).unwrap();
    let localized: Vec<_> = assembled
        .diagnostics
        .iter()
        .map(|d| {
            (
                d.file.map(|f| assembled.sources.files[f].display.clone()),
                d.diagnostic.message.clone(),
            )
        })
        .collect();
    assert!(
        assembled.ok(),
        "{localized:?} {:?}",
        assembled.manifest_errors
    );
    let checked = check::check(&assembled.module);
    let localized: Vec<_> = checked
        .diagnostics
        .iter()
        .map(|d| assembled.sources.localize(d))
        .collect();
    assert!(checked.diagnostics.is_empty(), "{localized:?}");
    checked
}

/// The evaluator, the JIT with and without optimization, and (outside
/// Windows) a linked executable all return `expected` from `main`.
fn agree(dir: &Path, expected: u64) {
    let checked = world(dir);
    let interpreted = Runtime::new(&checked.world)
        .run("main", vec![])
        .unwrap()
        .to_string();
    assert_eq!(interpreted, expected.to_string());
    let jit = native::jit(&checked.world, "main").unwrap().to_string();
    assert_eq!(jit, interpreted, "JIT without optimization");
    let optimized = native::jit_with_options(&checked.world, "main", true)
        .unwrap()
        .to_string();
    assert_eq!(optimized, interpreted, "optimized JIT");
    for command in ["run", "jit"] {
        let o = cli().arg(command).arg(dir).output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), interpreted);
    }
    if cfg!(not(target_os = "windows")) {
        let exe = dir.join("main.exe");
        let o = cli()
            .arg("build")
            .arg(dir)
            .arg("-o")
            .arg(&exe)
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let status = Command::new(&exe).status().unwrap();
        assert_eq!(status.code(), Some((expected % 256) as i32), "executable");
    }
}

/// `vesper check --json`: (code, file, message) of every diagnostic.
fn rejected(dir: &Path) -> Vec<(String, String, String)> {
    let o = cli().arg("check").arg("--json").arg(dir).output().unwrap();
    assert!(
        !o.status.success(),
        "{} unexpectedly checked",
        dir.display()
    );
    let stdout = String::from_utf8_lossy(&o.stdout);
    let value: serde_json::Value =
        serde_json::from_str(stdout.lines().next().unwrap_or("[]")).unwrap();
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            (
                d["code"].as_str().unwrap().to_owned(),
                d["file"].as_str().unwrap_or("").to_owned(),
                d["message"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn assert_rejects(found: &[(String, String, String)], code: &str, file: &str, message: &str) {
    assert!(
        found
            .iter()
            .any(|(c, f, m)| c == code && f.ends_with(file) && m.contains(message)),
        "expected {code} in {file} containing {message:?}, got {found:?}"
    );
}

const SHAPE: &str = "pub copy struct Point { x: u64, y: u64 }
impl Point {
    pub fn new(v: u64) -> Point { Point { x: v, y: v } }
}
pub copy enum Tag { Small(u64), Big }
pub fn classify(n: u64) -> Tag { if n < 10 { Tag::Small(n) } else { Tag::Big } }
";

/// Every path form of the manuscript's "Qualified paths" paragraph, in one
/// package with a dependency: `self::` values, struct literals, types,
/// associated functions and patterns; `super::`; `@scope/name::` through a
/// dependency's `pub use`; `use .. as`; and the root's own `pub use`.
#[test]
fn qualified_paths_agree_in_the_evaluator_jit_and_executable() {
    let dep = "[dependencies]\n\"@demo/dep\" = { version = \"^0.1\", path = \"dep\" }\n";
    let dir = package(
        "every-form",
        &[
            ("Loom.toml", &manifest("@demo/paths", dep)),
            ("dep/Loom.toml", &manifest("@demo/dep", "")),
            (
                "dep/src/lib.ves",
                "pub use self::inner::twice;\npub fn base() -> u64 { 7 }\n",
            ),
            (
                "dep/src/inner.ves",
                "pub fn twice(x: u64) -> u64 { wrapping_mul(x, 2) }\n",
            ),
            (
                "src/lib.ves",
                "use self::codec::reader::{u64 as read_u64};
pub use self::codec::enc::run as encode;
pub fn main() -> u64 {
    let p = self::codec::shape::Point { x: 1, y: 2 };
    let q: self::codec::shape::Point = self::codec::shape::Point::new(3);
    let t = match self::codec::shape::classify(5) {
        self::codec::shape::Tag::Small(n) => n,
        self::codec::shape::Tag::Big => 0,
    };
    let a = wrapping_add(self::codec::enc::run(p.x), self::codec::dec::run(q.y));
    let b = wrapping_add(@demo/dep::base(), @demo/dep::twice(t));
    let c: u64 = read_u64(1);
    wrapping_add(wrapping_add(a, b), wrapping_add(c, encode(1)))
}
",
            ),
            (
                "src/codec/enc.ves",
                "pub fn run(x: u64) -> u64 { wrapping_mul(x, 10) }\n",
            ),
            (
                "src/codec/dec.ves",
                "use super::enc::{run as encode};
pub fn run(x: u64) -> u64 { wrapping_add(super::enc::run(x), wrapping_sub(encode(x), encode(x))) }
",
            ),
            (
                "src/codec/reader.ves",
                "pub fn u64(x: u64) -> u64 { wrapping_add(x, 1000) }\n",
            ),
            ("src/codec/shape.ves", SHAPE),
        ],
    );
    // a = 10 + 30, b = 7 + 10, c = 1001, encode(1) = 10.
    agree(&dir, 1068);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The Prism H3 shape: two submodules with entry points of one name, called
/// by path and imported under `as`, instead of `enc_*`/`dec_*` names.
#[test]
fn submodule_functions_of_one_name_are_reached_by_path() {
    let dir = package(
        "same-name",
        &[
            ("Loom.toml", &manifest("@demo/samename", "")),
            (
                "src/lib.ves",
                "use self::prefix::{decode_int};
use self::encoder::encode as enc;
pub fn main() -> u64 {
    let a = self::prefix::decode_int(3);
    let b = self::decoder::decode(4);
    wrapping_add(wrapping_add(a, b), wrapping_add(decode_int(1), enc(5)))
}
",
            ),
            (
                "src/prefix.ves",
                "pub fn decode_int(x: u64) -> u64 { wrapping_mul(x, 2) }\n",
            ),
            (
                "src/decoder.ves",
                "pub fn decode(x: u64) -> u64 { self::prefix::decode_int(x) }\n",
            ),
            (
                "src/encoder.ves",
                "pub fn encode(x: u64) -> u64 { wrapping_add(x, 100) }\n",
            ),
        ],
    );
    agree(&dir, 6 + 8 + 2 + 105);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The Aegis repro: `use self::reader::{u64}` binds the function in the
/// value namespace only, so `u64` in types stays the scalar type.
#[test]
fn an_imported_function_never_shadows_a_scalar_type() {
    let dir = package(
        "shadow",
        &[
            ("Loom.toml", &manifest("@demo/shadow", "")),
            (
                "src/lib.ves",
                "use self::reader::{u64, Rd};
pub fn main() -> u64 {
    let mut s = ChunkSeq::new[u64]();
    s.push(u64(5));
    let r = Rd { at: 2 };
    let x: u64 = u64(r.at);
    wrapping_add(x, s.len())
}
",
            ),
            (
                "src/reader.ves",
                "pub copy struct Rd { at: u64 }
pub fn u64(x: u64) -> u64 { wrapping_add(x, 1) }
",
            ),
        ],
    );
    agree(&dir, 4);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn path_and_reexport_errors_name_the_module_and_file() {
    let dir = package(
        "errors",
        &[
            ("Loom.toml", &manifest("@demo/bad", "")),
            (
                "src/lib.ves",
                "pub use self::m::f;
pub use self::m::f as g;
pub fn main() -> u64 { wrapping_add(self::m::hidden(1), self::nope::x(1)) }
",
            ),
            (
                "src/m.ves",
                "pub fn f(x: u64) -> u64 { x }
fn hidden(x: u64) -> u64 { x }
pub fn up() -> u64 { wrapping_add(main(), super::super::m::f(1)) }
pub fn t(x: self::m::Missing) -> u64 { 0 }
",
            ),
        ],
    );
    let found = rejected(&dir);
    assert_rejects(
        &found,
        packages::AMBIGUOUS,
        "src/lib.ves",
        "re-exported by the root module as both 'f' and 'g'",
    );
    assert_rejects(
        &found,
        packages::PRIVATE_ITEM,
        "src/lib.ves",
        "'hidden' is private to module self::m",
    );
    assert_rejects(
        &found,
        packages::UNKNOWN_IMPORT,
        "src/lib.ves",
        "no module self::nope",
    );
    // `f` is re-exported by the root, so it keeps a plain name there; a
    // submodule still needs an import or a path to reach root names.
    assert_rejects(
        &found,
        packages::UNKNOWN_IMPORT,
        "src/m.ves",
        "'main' is not declared or imported in this module",
    );
    assert_rejects(
        &found,
        packages::UNKNOWN_IMPORT,
        "src/m.ves",
        "`super` goes above the package root",
    );
    assert_rejects(
        &found,
        packages::UNKNOWN_IMPORT,
        "src/m.ves",
        "module self::m has no item 'Missing'",
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_qualified_path_in_a_single_file_needs_a_package() {
    let dir = package(
        "single",
        &[(
            "single.ves",
            "fn f() -> u64 { 1 }\nfn main() -> u64 { self::f() }\n",
        )],
    );
    let o = cli()
        .arg("check")
        .arg("--json")
        .arg(dir.join("single.ves"))
        .output()
        .unwrap();
    assert!(!o.status.success());
    let stdout = String::from_utf8_lossy(&o.stdout);
    assert!(
        stdout.contains(packages::USE_OUTSIDE_PACKAGE) && stdout.contains("self::f"),
        "{stdout}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `state`, `linear` and `with` are names wherever a name is expected
/// (Bench gap 5, Pact gap 6, Chorus gap 5); open slice intervals (Flux
/// gap 1); `Result::Ok(())` as a pattern (Prism H3 gap 4).
#[test]
fn contextual_keywords_open_intervals_and_unit_patterns() {
    let dir = package(
        "parser",
        &[
            ("Loom.toml", &manifest("@demo/parser", "")),
            (
                "src/lib.ves",
                "struct S { state: u64, with: u64 }
struct W { q: ChunkSeq[i64] }
fn f(state: u64, linear: u64) -> u64 { let with = wrapping_add(state, linear); with }
fn g(in s: S) -> u64 { s.state }
fn n(in s: Slice[i64]) -> u64 { s.len() }
fn m(in s: Str) -> u64 { s.len() }
fn unit(x: u64) -> Result[(), u64] { if x > 1 { Result::Ok(()) } else { Result::Err(x) } }
pub fn main() -> u64 {
    let linear: u64 = 3;
    let s = S { state: 4, with: linear };
    let S { state, with } = s;
    let mut q = ChunkSeq::new[i64]();
    q.push(1);
    q.push(2);
    q.push(3);
    let w = W { q: q };
    let t = \"hello\";
    let slices = wrapping_add(
        wrapping_add(n(in w.q[..]), n(in w.q[1..])),
        wrapping_add(n(in w.q[..2]), m(in t[..])),
    );
    let ok = match unit(3) {
        Result::Ok(()) => 1,
        Result::Err(e) => e,
    };
    wrapping_add(
        wrapping_add(f(state, with), g(in S { state: 1, with: 2 })),
        wrapping_add(slices, ok),
    )
}
",
            ),
        ],
    );
    // f = 7, g = 1, slices = 3 + 2 + 2 + 5, ok = 1.
    agree(&dir, 7 + 1 + 12 + 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_reserved_keyword_as_a_name_is_named_in_the_diagnostic() {
    let parsed = vesper::parser::parse("fn f(match: u64) -> u64 { 1 }\n");
    assert!(
        parsed
            .diagnostics
            .iter()
            .any(|d| d.message.contains("the keyword `match`")),
        "{:?}",
        parsed.diagnostics
    );
    let parsed = vesper::parser::parse("fn f(in s: Slice[u8]) -> u64 { g(in h()[1..]) }\n");
    assert!(
        parsed
            .diagnostics
            .iter()
            .any(|d| d.message.contains("needs a named sequence")),
        "{:?}",
        parsed.diagnostics
    );
}

/// Paths in effect rows and generic arguments, contextual names as path
/// segments and import names, and a root item exported under an alias.
#[test]
fn paths_in_rows_and_type_arguments_and_contextual_item_names() {
    let dir = package(
        "rows",
        &[
            ("Loom.toml", &manifest("@demo/rows", "")),
            (
                "src/lib.ves",
                "use self::m::{state as st};
fn internal() -> u64 { 5 }
pub use self::internal as api;
fn tick(n: i64) -> i64 eff [c: self::m::Counter] { wrapping_add(c.next(), n) }
pub fn main() -> u64 {
    let k = handle c: self::m::Counter state counter = 0 in {
        tick(1)
    } with {
        return (value, counter) => wrapping_add(value, counter),
        next(counter, resume) => resume(counter, wrapping_add(counter, 1)),
    };
    let v = self::m::sub::wrap();
    wrapping_add(wrapping_add(self::m::state(), st()), wrapping_add(api(), v.len()))
}
",
            ),
            (
                "src/m.ves",
                "pub linear effect Counter {
    fn next() -> i64;
}
pub copy struct T { x: u64 }
pub fn state() -> u64 { 10 }
",
            ),
            (
                "src/m/sub.ves",
                "pub fn wrap() -> ChunkSeq[super::T] {
    let mut s = ChunkSeq::new[super::T]();
    s.push(super::T { x: 1 });
    s
}
",
            ),
        ],
    );
    agree(&dir, 10 + 10 + 5 + 1);
    // `pub use self::internal as api;` exports `api`, not `internal`.
    let assembled = packages::load(&dir).unwrap();
    assert!(assembled.module.public_items.iter().any(|n| n == "api"));
    assert!(
        !assembled
            .module
            .public_items
            .iter()
            .any(|n| n == "internal")
    );
    let _ = std::fs::remove_dir_all(&dir);
}
