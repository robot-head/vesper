//! Export ABI v4: compiler trees across the boundary (self-hosting SH1
//! lane 6, #1149; manuscript §16.6).
//!
//! `// @export abi 4` keeps every v3 form and adds three:
//!
//! * owned trees: each handle type whose fields are tree forms gets a Rust
//!   `<Type>Tree`, encoded into this library's allocations once
//!   (`<Type>::encode`) and decoded once (`<Type>::decode`);
//! * lent views that also read a `ChunkSeq` of handles in place
//!   (`<Elem>Seq<'a>`), so Rust can walk a whole Vesper tree without a copy;
//! * lent Rust handles: an `in` parameter of a `.vsys` extern type is a Rust
//!   value's address (`unsafe { H::lend(&value) }`) that Vesper hands to accessor
//!   imports the linking crate implements.
//!
//! The end-to-end test builds the library object, generates its binding,
//! links both into a Rust program with `rustc` and runs it: the owned tree
//! path (encode, walk through borrows, decode), the lent views, the lent
//! Rust handle, and the library's live allocation count. It runs where a C
//! compiler and `rustc` are on the path (Cargo CI); elsewhere it checks the
//! interface and the binding text only.
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::native::export::{
    self, AbiType, BindingOptions, LibraryOptions, compile_source, rust_binding,
};

const LIBRARY: &str = r#"// @export abi 4
// @export vsys direct rust=crate::host
// | sys library Host (
// |     target = "@TARGET@", abi = "@ABI@",
// |     link = "static", name = "tree_host",
// |     artifact = "blake3:0000000000000000000000000000000000000000000000000000000000000000"
// | ) {
// |     affine extern type World;
// |     fn world_type_count(in w: World) -> ForeignVal[u64];
// |     fn world_type_words(in w: World, ty: u64) -> ForeignVal[u64];
// | }

pub enum Term {
    Ident(String),
    Nat(u64),
    App(Box[Term], Box[Term]),
    Arrow(Box[Term], Box[Term]),
    Lam {
        name: String,
        body: Box[Term]
    },
    Hole(Option[Box[Term]])
}

pub struct Decl {
    name: String,
    ty: Term,
    arity: u32
}

pub struct Module {
    name: String,
    decls: ChunkSeq[Decl],
    tags: ChunkSeq[u64]
}

fn atom(in t: Term) -> bool {
    match t {
        Term::Ident(_) => true,
        Term::Nat(_) => true,
        Term::Hole(_) => true,
        _ => false,
    }
}

fn put(in t: Term, inout out: String) {
    match t {
        Term::Ident(s) => out.push_str(s),
        Term::Nat(n) => out.push_str("n"),
        Term::App(f, a) => match f {
            Box(g) => match a {
                Box(x) => {
                    put(in g, inout out);
                    out.push_str(" ");
                    if atom(in x) {
                        put(in x, inout out);
                    } else {
                        out.push_str("(");
                        put(in x, inout out);
                        out.push_str(")");
                    }
                },
            },
        },
        Term::Arrow(a, b) => match a {
            Box(x) => match b {
                Box(y) => {
                    out.push_str("(");
                    put(in x, inout out);
                    out.push_str(" -> ");
                    put(in y, inout out);
                    out.push_str(")");
                },
            },
        },
        Term::Lam { name, body } => match body {
            Box(b) => {
                out.push_str("fun ");
                out.push_str(name);
                out.push_str(" => ");
                put(in b, inout out);
            },
        },
        Term::Hole(h) => match h {
            Option::Some(x) => match x {
                Box(y) => {
                    out.push_str("?");
                    put(in y, inout out);
                },
            },
            Option::None => out.push_str("_"),
        },
    }
}

pub fn render(in t: Term) -> String {
    let mut out = String::new();
    put(in t, inout out);
    out
}

pub fn size(in t: Term) -> u64 {
    match t {
        Term::App(f, a) => match f { Box(g) => match a { Box(x) => wrapping_add(1, wrapping_add(size(in g), size(in x))) } },
        Term::Arrow(f, a) => match f { Box(g) => match a { Box(x) => wrapping_add(1, wrapping_add(size(in g), size(in x))) } },
        Term::Lam { name, body } => match body { Box(b) => wrapping_add(1, size(in b)) },
        Term::Hole(h) => match h { Option::Some(x) => match x { Box(y) => wrapping_add(1, size(in y)) }, Option::None => 1 },
        _ => 1,
    }
}

fn chain(n: u64) -> Term {
    if n == 0 {
        Term::Ident("x")
    } else {
        Term::App(Box::new(Term::Ident("f")), Box::new(chain(wrapping_sub(n, 1))))
    }
}

pub fn module(n: u64) -> Module {
    let mut decls: ChunkSeq[Decl] = ChunkSeq::new();
    let mut tags: ChunkSeq[u64] = ChunkSeq::new();
    let mut i: u64 = 0;
    loop {
        if i >= n { break; }
        decls.push(Decl { name: "d", ty: chain(i), arity: 2 });
        tags.push(wrapping_mul(i, 3));
        i = wrapping_add(i, 1);
    }
    Module { name: "M", decls: decls, tags: tags }
}

// A right-leaning tree of `n` applications and arrows, for the measurements.
pub fn deep(n: u64) -> Term {
    if n == 0 {
        Term::Ident("x")
    } else if n % 2 == 0 {
        Term::App(Box::new(Term::Ident("f")), Box::new(deep(wrapping_sub(n, 1))))
    } else {
        Term::Arrow(Box::new(Term::Nat(n)), Box::new(deep(wrapping_sub(n, 1))))
    }
}

pub fn words_of_all(in w: World) -> u64 eff [Ffi[Host]] {
    let n = Host::world_type_count(in w);
    let mut i: u64 = 0;
    let mut total: u64 = 0;
    loop {
        if i >= n { break; }
        total = wrapping_add(total, Host::world_type_words(in w, i));
        i = wrapping_add(i, 1);
    }
    total
}
"#;

const DRIVER: &str = r#"#[path = "binding.rs"]
mod binding;
use binding::*;

// The linking crate's side of the `.vsys` accessors: Vesper reads the lent
// Rust value only through these.
mod host {
    pub struct Types(pub Vec<u64>);
    #[unsafe(no_mangle)]
    pub extern "C" fn world_type_count(w: *const Types) -> u64 {
        let w = unsafe { &*w };
        w.0.len() as u64
    }
    #[unsafe(no_mangle)]
    pub extern "C" fn world_type_words(w: *const Types, ty: u64) -> u64 {
        let w = unsafe { &*w };
        w.0[ty as usize]
    }
}

use TermTree::*;

fn atom(t: &TermTree) -> bool {
    matches!(t, Ident(_) | Nat(_) | Hole(_))
}

/// The printer of `put` in the Vesper library, in Rust.
fn reference(t: &TermTree) -> String {
    fn put(t: &TermTree, out: &mut String) {
        match t {
            Ident(s) => out.push_str(s),
            Nat(_) => out.push('n'),
            App(f, x) => {
                put(f, out);
                out.push(' ');
                if atom(x) {
                    put(x, out);
                } else {
                    out.push('(');
                    put(x, out);
                    out.push(')');
                }
            }
            Arrow(a, b) => {
                out.push('(');
                put(a, out);
                out.push_str(" -> ");
                put(b, out);
                out.push(')');
            }
            Lam { name, body } => {
                out.push_str("fun ");
                out.push_str(name);
                out.push_str(" => ");
                put(body, out);
            }
            Hole(Some(x)) => {
                out.push('?');
                put(x, out);
            }
            Hole(None) => out.push('_'),
        }
    }
    let mut out = String::new();
    put(t, &mut out);
    out
}

fn deep(n: u64) -> TermTree {
    let mut t = Ident("x".into());
    for k in 1..=n {
        t = if k % 2 == 0 {
            App(Box::new(Ident("f".into())), Box::new(t))
        } else {
            Arrow(Box::new(Nat(k)), Box::new(t))
        };
    }
    t
}

/// A hand-written flat encoding of the same tree (prefix-order words over a
/// text pool), the shape of the lean-syntax port's 611-line encoder.
#[derive(Default)]
struct Flat {
    words: Vec<u64>,
    pool: String,
}

impl Flat {
    fn text(&mut self, s: &str) {
        self.words.push(self.pool.len() as u64);
        self.words.push(s.len() as u64);
        self.pool.push_str(s);
    }
    fn term(&mut self, t: &TermTree) {
        match t {
            Ident(s) => {
                self.words.push(0);
                self.text(s);
            }
            Nat(n) => self.words.extend([1, *n]),
            App(a, b) => {
                self.words.push(2);
                self.term(a);
                self.term(b);
            }
            Arrow(a, b) => {
                self.words.push(3);
                self.term(a);
                self.term(b);
            }
            Lam { name, body } => {
                self.words.push(4);
                self.text(name);
                self.term(body);
            }
            Hole(None) => self.words.extend([5, 0]),
            Hole(Some(x)) => {
                self.words.extend([5, 1]);
                self.term(x);
            }
        }
    }
}

fn check() {
    let start = live_allocations();
    let t = Lam {
        name: "y".into(),
        body: Box::new(App(
            Box::new(Ident("g".into())),
            Box::new(Arrow(
                Box::new(Nat(3)),
                Box::new(Hole(Some(Box::new(Hole(None))))),
            )),
        )),
    };
    // Owned tree path: encoded once, walked by Vesper through borrows any
    // number of times, decoded back.
    let v = Term::encode(&t);
    assert_eq!(v.decode(), t, "decode after encode");
    assert_eq!(size(&v), 7);
    assert_eq!(v.decode(), t, "decode after a borrowed walk");
    assert_eq!(render(&v), reference(&t));
    assert_eq!(render(&v), "fun y => g ((n -> ?_))");
    assert_eq!(v.decode(), t, "decode after two borrowed walks");
    drop(v);
    assert_eq!(live_allocations(), start, "an encoded tree is freed by its handle");
    let big = deep(500);
    let v = Term::encode(&big);
    assert_eq!(render(&v), reference(&big));
    assert_eq!(size(&v), 1001);
    assert_eq!(v.decode(), big);
    drop(v);
    // A Vesper-built tree read in place through lent views, sequences of
    // handles included, then decoded once and encoded back.
    let m = module(6);
    let r = m.as_ref();
    assert_eq!(r.name(), "M");
    assert_eq!(r.decls().len(), 6);
    assert_eq!(r.tags().to_vec(), vec![0, 3, 6, 9, 12, 15]);
    let d = r.decls().get(3).expect("in bounds");
    assert_eq!((d.name(), d.arity()), ("d", 2));
    assert!(r.decls().get(6).is_none());
    let mut depth = 0;
    let mut ty = d.ty();
    while let TermView::App(_, x) = ty.view() {
        depth += 1;
        ty = x;
    }
    assert_eq!(depth, 3);
    assert!(matches!(ty.view(), TermView::Ident("x")));
    assert_eq!(
        r.decls().iter().map(|d| render(&Term::encode(&d.ty().decode()))).collect::<Vec<_>>(),
        ["x", "f x", "f (f x)", "f (f (f x))", "f (f (f (f x)))", "f (f (f (f (f x))))"]
    );
    let tree = m.decode();
    assert_eq!(tree.decls.len(), 6);
    assert_eq!(tree.decls[2].arity, 2);
    let back = Module::encode(&tree);
    assert_eq!(back.decode(), tree);
    drop(back);
    drop(m);
    // Vesper-built trees decode like encoded ones.
    let built = binding::deep(40);
    assert_eq!(built.decode(), deep(40));
    drop(built);
    assert_eq!(live_allocations(), start, "every Vesper allocation is freed");
    // Lent Rust handle: Vesper reads the Rust value through `.vsys`
    // accessor imports that this crate implements.
    let types = host::Types(vec![1, 2, 3, 9]);
    assert_eq!({
        // SAFETY: the accessors read a `host::Types`.
        words_of_all(unsafe { World::lend(&types) })
    }, 15);
    assert_eq!({
        let empty = host::Types(vec![]);
        // SAFETY: the accessors read a `host::Types`.
        words_of_all(unsafe { World::lend(&empty) })
    }, 0);
    println!("ok");
}

fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    let n: u64 = args.get(2).and_then(|n| n.parse().ok()).unwrap_or(1_000);
    match args.get(1).map(String::as_str) {
        None | Some("check") => check(),
        // Measurement modes, each over the same tree; the report subtracts
        // `build` (the Rust tree alone) from the others.
        Some("build") => {
            std::hint::black_box(deep(n));
        }
        Some("flat") => {
            let t = deep(n);
            let mut f = Flat::default();
            f.term(&t);
            std::hint::black_box((f.words.len(), f.pool.len()));
        }
        // The modes after `encode` keep the Vesper value alive (forget), so
        // each measures its own step on top of `encode`; `drop` frees it.
        Some("encode") => {
            let t = deep(n);
            std::mem::forget(std::hint::black_box(Term::encode(&t)));
        }
        Some("drop") => {
            let t = deep(n);
            drop(std::hint::black_box(Term::encode(&t)));
        }
        Some("decode") => {
            let t = deep(n);
            let v = Term::encode(&t);
            std::hint::black_box(v.decode());
            std::mem::forget(v);
        }
        Some("walk") => {
            let t = deep(n);
            let v = Term::encode(&t);
            std::hint::black_box(size(&v));
            std::mem::forget(v);
        }
        Some("render") => {
            let t = deep(n);
            let v = Term::encode(&t);
            std::hint::black_box(render(&v));
            std::mem::forget(v);
        }
        Some("reference") => {
            let t = deep(n);
            std::hint::black_box(reference(&t));
        }
        Some(other) => panic!("unknown mode {other}"),
    }
}
"#;

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "vesper-tree-abi-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn options(library: &'static str) -> LibraryOptions<'static> {
    LibraryOptions {
        library,
        target: None,
        optimize: true,
    }
}

fn tree() -> export::Library {
    compile_source("tree.ves", LIBRARY, &options("tree")).unwrap_or_else(|e| panic!("{e}"))
}

fn rustc() -> String {
    std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into())
}

fn can_link() -> bool {
    let ok = |c: &str| {
        Command::new(c)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    };
    cfg!(all(unix, target_arch = "x86_64"))
        && ok("cc")
        && ok(&rustc())
        && Path::new("crates/vesper-port-abi/src/lib.rs").exists()
}

/// Builds the library and the driver into `temp` and returns the program.
fn program(temp: &Temp) -> PathBuf {
    let lib = tree();
    let dir = &temp.0;
    fs::write(dir.join("tree.o"), &lib.object).unwrap();
    fs::write(
        dir.join("binding.rs"),
        rust_binding(&lib.interface, &BindingOptions::default()),
    )
    .unwrap();
    fs::write(dir.join("main.rs"), DRIVER).unwrap();
    let run = |args: &[&std::ffi::OsStr]| {
        let out = Command::new(rustc()).args(args).output().unwrap();
        assert!(
            out.status.success(),
            "rustc {args:?}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    let rlib = dir.join("libvesper_port_abi.rlib");
    run(&[
        "--edition".as_ref(),
        "2024".as_ref(),
        "--crate-type".as_ref(),
        "rlib".as_ref(),
        "--crate-name".as_ref(),
        "vesper_port_abi".as_ref(),
        "-O".as_ref(),
        "crates/vesper-port-abi/src/lib.rs".as_ref(),
        "-o".as_ref(),
        rlib.as_os_str(),
    ]);
    let exe = dir.join("tree_driver");
    let extern_arg = format!("vesper_port_abi={}", rlib.display());
    let link_arg = format!("link-arg={}", dir.join("tree.o").display());
    run(&[
        "--edition".as_ref(),
        "2024".as_ref(),
        "-O".as_ref(),
        dir.join("main.rs").as_os_str(),
        "--extern".as_ref(),
        extern_arg.as_ref(),
        "-L".as_ref(),
        dir.as_os_str(),
        "-C".as_ref(),
        link_arg.as_ref(),
        "-o".as_ref(),
        exe.as_os_str(),
    ]);
    exe
}

#[test]
fn the_interface_names_abi_v4_and_its_forms() {
    let lib = tree();
    let i = &lib.interface;
    assert_eq!(i.abi, export::EXPORT_ABI_V4);
    assert!(i.is_v4() && i.is_v3() && i.is_v2());
    assert_eq!(
        i.runtime.as_ref().and_then(|r| r.box_new.as_deref()),
        Some("vesper_tree_box_new")
    );
    let f = |name: &str| i.functions.iter().find(|f| f.name == name).unwrap();
    assert_eq!(
        f("words_of_all").params[0].ty,
        AbiType::Extern {
            name: "World".into()
        }
    );
    // `in` handles are lent in place, as in v3.
    assert!(f("render").params[0].in_place);
    let text = rust_binding(i, &BindingOptions::default());
    for item in [
        "pub enum TermTree {",
        "Lam { name: String, body: Box<TermTree> },",
        "Hole(Option<Box<TermTree>>),",
        "pub struct ModuleTree {",
        "pub decls: Vec<DeclTree>,",
        "pub tags: Vec<u64>,",
        "pub fn encode(tree: &TermTree) -> Self {",
        "pub fn decode(&self) -> TermTree {",
        "pub struct DeclSeq<'a> {",
        "pub fn decls(&self) -> DeclSeq<'a> {",
        "pub struct World<'a> {",
        "pub fn words_of_all(w: World<'_>) -> u64 {",
        "fn vesper_tree_box_new(size: i64) -> i64;",
    ] {
        assert!(text.contains(item), "missing {item:?} in\n{text}");
    }
}

#[test]
fn v3_libraries_are_unchanged() {
    // The same source under `abi 3` (without the lent Rust handle) has no
    // v4 item and its object is byte-identical.
    let body = LIBRARY
        .replace("// @export abi 4", "// @export abi 3")
        .split("pub fn words_of_all")
        .next()
        .unwrap()
        .to_string();
    let body = body
        .lines()
        .filter(|l| !l.starts_with("// @export vsys") && !l.starts_with("// |"))
        .collect::<Vec<_>>()
        .join("\n");
    let v3 = compile_source("tree.ves", &body, &options("tree")).unwrap();
    assert_eq!(v3.interface.abi, export::EXPORT_ABI_V3);
    assert!(v3.interface.runtime.as_ref().unwrap().box_new.is_none());
    let text = rust_binding(&v3.interface, &BindingOptions::default());
    for item in ["TermTree", "DeclSeq", "box_new", "fn encode"] {
        assert!(!text.contains(item), "v3 binding has {item:?}");
    }
    assert!(
        text.contains("OpaqueSeq"),
        "v3 keeps opaque handle sequences"
    );
    let v4 = compile_source(
        "tree.ves",
        &body.replace("// @export abi 3", "// @export abi 4"),
        &options("tree"),
    )
    .unwrap();
    // v4 adds one exported runtime function, `box_new`, and nothing else.
    assert!(v4.object.len() > v3.object.len());
}

#[test]
fn lent_rust_handles_cross_only_as_in_parameters() {
    let header = LIBRARY
        .lines()
        .take_while(|l| l.starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    let refused =
        |body: &str| match compile_source("t.ves", &format!("{header}\n{body}"), &options("t")) {
            Ok(_) => panic!("accepted:\n{body}"),
            Err(e) => e.to_string(),
        };
    let e =
        refused("pub fn f(inout w: World) -> u64 eff [Ffi[Host]] { Host::world_type_count(in w) }");
    assert!(e.contains("only to an `in` parameter"), "{e}");
    // Nor inside a container, which the binding could not represent.
    let e =
        refused("pub fn f(in p: (World, u64)) -> u64 eff [Ffi[Host]] { match p { (w, n) => n } }");
    assert!(e.contains("only directly as an `in` parameter"), "{e}");
    // Without `abi 4` the handle stays inside Vesper, as before.
    let v3 = match compile_source(
        "t.ves",
        &format!(
            "{}\npub fn f(in w: World) -> u64 eff [Ffi[Host]] {{ Host::world_type_count(in w) }}",
            header.replace("// @export abi 4", "// @export abi 3")
        ),
        &options("t"),
    ) {
        Ok(_) => panic!("v3 accepted a lent Rust handle"),
        Err(e) => e.to_string(),
    };
    assert!(v3.contains("@export abi 4"), "{v3}");
}

#[test]
fn lending_a_rust_value_is_unsafe() {
    // Vesper cannot check the lent value's type against what the accessor
    // imports read, so the caller vouches for it.
    let text = rust_binding(&tree().interface, &BindingOptions::default());
    assert!(
        text.contains("pub unsafe fn lend<T>(value: &'a T) -> Self {"),
        "{text}"
    );
}

#[test]
fn a_handle_holding_a_handle_without_a_tree_has_no_tree() {
    // `B` holds a sequence of strings, which has no tree form, so neither
    // `B` nor `A`, which holds a `B`, gets a tree (both stay opaque
    // handles); `C` is a tree.
    let source = "// @export abi 4
pub struct B {
    values: ChunkSeq[String]
}

pub struct A {
    b: B,
    n: u64
}

pub struct C {
    name: String,
    next: Option[Box[C]]
}

pub fn a_n(in a: A) -> u64 {
    a.n
}

pub fn c_one(in c: C) -> u64 {
    1
}
";
    let lib = compile_source("nest.ves", source, &options("nest")).unwrap();
    let text = rust_binding(&lib.interface, &BindingOptions::default());
    for absent in ["ATree", "BTree", "encode_a", "encode_b"] {
        assert!(!text.contains(absent), "binding has {absent:?}:\n{text}");
    }
    assert!(text.contains("pub struct CTree {"), "{text}");
}

#[test]
fn trees_cross_both_ways_in_a_linked_program() {
    if !can_link() {
        eprintln!("skipped: needs cc and rustc (the interface tests still ran)");
        return;
    }
    let temp = Temp::new();
    let exe = program(&temp);
    let out = Command::new(&exe).arg("check").output().unwrap();
    assert!(
        out.status.success() && String::from_utf8_lossy(&out.stdout).contains("ok"),
        "driver failed\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Instructions (callgrind) of each measurement mode over a tree of
/// 2,001 nodes (1,000 applications and arrows), minus building the Rust
/// tree (and, after `encode`, minus encoding it). Ignored: run with
/// `cargo test --release --test tree_abi -- --ignored --nocapture`.
#[test]
#[ignore]
fn measure() {
    if !can_link() {
        return;
    }
    let temp = Temp::new();
    let exe = program(&temp);
    let ir = |mode: &str| -> u64 {
        let file = temp.0.join(format!("cg.{mode}"));
        let out = Command::new("valgrind")
            .arg("--tool=callgrind")
            .arg(format!("--callgrind-out-file={}", file.display()))
            .arg(&exe)
            .arg(mode)
            .arg("1000")
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stderr);
        text.lines()
            .find_map(|l| l.split("Collected : ").nth(1))
            .and_then(|n| n.trim().parse().ok())
            .unwrap_or_else(|| panic!("no callgrind total:\n{text}"))
    };
    let base = ir("build");
    let encode = ir("encode");
    println!("flat encoder: {} Ir", ir("flat").saturating_sub(base));
    println!("encode: {} Ir", encode.saturating_sub(base));
    for mode in ["drop", "decode", "walk", "render"] {
        println!("{mode}: {} Ir", ir(mode).saturating_sub(encode));
    }
    println!(
        "Rust reference printer: {} Ir",
        ir("reference").saturating_sub(base)
    );
}
