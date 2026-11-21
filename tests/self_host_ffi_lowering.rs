//! Self-hosting SH4 lane 13 (#1287): the `.vsys` type mapping and call
//! plans of src/ffi_lowering.rs and src/ffi_lowering/types.rs against their
//! Vesper port, `packages/vesper-ffi-lowering`.
//!
//! One case is a set of imported libraries plus probes. The Vesper side gets
//! each library's canonical descriptor bytes
//! (`LibraryDescriptor::canonical_bytes`, the bytes its identity hashes)
//! and handler tier, decodes them, and writes one text: the generated
//! declarations (`types::Generated::new`), every function's source shape,
//! the declarations `append` adds in pass 1 (open slices) and pass 2 (one
//! operation per slice-length vector), then the probes: `check_collisions`
//! against module items, `demangle` of messages, `is_handle`, `op_plan`
//! with and without a target, `target_support`, `slice_length` of argument
//! types, `op_name`, and each function's ABI signature
//! (`FunctionDescriptor::abi`). The Rust side writes the same text from the
//! Rust code through `vesper::ffi_lowering::plan_probe`; the two must be
//! equal byte for byte.
//!
//! The corpus is every `.vsys` file and `// @export vsys` directive of the
//! repository, the `.vsys` sources of the FFI test suites (each target for
//! a `@TARGET@` placeholder), pairs of them, and hand-written libraries and
//! mutants that make every error of the type mapping fire.
//!
//! # Input words
//!
//! Numbers are little-endian `u64` words; a string is its byte length then
//! its bytes (unpadded). A type is `0, name, n, n types` (named) or
//! `1, n, n types` (tuple).
//!
//! ```text
//! libraries:  n, n x (handler (0 direct, 1 isolated(mpk)), canonical bytes)  -- sorted by name
//! instances:  n, n x (library, symbol, k, k x (m, m lengths))  -- u64::MAX is a lent Slice
//! collisions: n, n x (k, k x (name, start, end))
//! demangle:   n, n x message
//! handles:    n, n x name
//! plans:      n, n x (target or "", effect, op)
//! targets:    n, n x triple
//! slices:     n, n x (has type, [type], element type, nullable, in place, start, end)
//! op names:   n, n x (symbol, m, m lengths)
//! ```
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use vesper::ast::*;
use vesper::ffi_lowering::plan_probe::{self, Kind};
use vesper::ffi_lowering::{FfiError, FfiErrorKind, Handler, Imports};
use vesper_ffi::sys::{self, AbiType, LibraryDescriptor, ParameterType, ValueType};

#[path = "common/check_harness.rs"]
mod check_harness;
use check_harness::*;

const PACKAGE: &str = "packages/vesper-ffi-lowering";

/// The targets a `@TARGET@` placeholder takes, with their `abi`.
const TARGETS: [(&str, &str); 4] = [
    ("x86_64-unknown-linux-gnu", "sysv64"),
    ("aarch64-unknown-linux-gnu", "aapcs64"),
    ("aarch64-apple-darwin", "darwin64"),
    ("x86_64-pc-windows-msvc", "win64"),
];

/// Object targets the target and plan probes ask about (the `.vsys`
/// targets and three others).
const PROBE_TARGETS: [&str; 7] = [
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "riscv64gc-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-pc-windows-msvc",
];

const ZERO_DIGEST: &str = "0000000000000000000000000000000000000000000000000000000000000000";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

// ---------------------------------------------------------------- one case

/// One differential case: libraries with their tiers, and the slice
/// probes (argument type, element, nullable, in place).
#[derive(Clone)]
struct Case {
    label: String,
    libraries: Vec<(LibraryDescriptor, Handler)>,
}

impl Case {
    fn imports(&self) -> Result<Imports, FfiError> {
        let mut imports = Imports::new();
        for (d, h) in &self.libraries {
            imports.add_descriptor(d.clone(), *h)?;
        }
        Ok(imports)
    }

    /// The libraries in import (name) order.
    fn sorted(&self) -> Vec<(LibraryDescriptor, Handler)> {
        let mut v = self.libraries.clone();
        v.sort_by(|a, b| a.0.name().cmp(b.0.name()));
        v
    }
}

/// Everything the probes ask, derived from the Rust shapes of a case (the
/// inputs only; the answers come from each side).
struct Probes {
    instances: BTreeMap<(String, String), BTreeSet<Vec<usize>>>,
    collisions: Vec<Vec<(String, usize, usize)>>,
    demangle: Vec<String>,
    handles: Vec<String>,
    plans: Vec<(Option<String>, String, String)>,
    targets: Vec<String>,
    slices: Vec<(Option<Type>, Type, bool, bool, usize, usize)>,
    op_names: Vec<(String, Vec<usize>)>,
}

fn probes(case: &Case, imports: &Imports) -> Probes {
    let libs = case.sorted();
    let mut instances: BTreeMap<(String, String), BTreeSet<Vec<usize>>> = BTreeMap::new();
    let mut names: BTreeSet<String> = BTreeSet::new();
    let mut demangle = vec![];
    let mut plans = vec![];
    let mut slices = vec![];
    let mut op_names = vec![];
    let shapes = plan_probe::generated(imports).map(|g| g.shapes(imports));
    if let Ok(shapes) = &shapes {
        for (k, (library, symbol, shape)) in shapes.iter().enumerate() {
            let Ok((params, _)) = shape else { continue };
            let mut set = BTreeSet::new();
            let slice_params: Vec<&Kind> = params
                .iter()
                .map(|p| &p.2)
                .filter(|k| matches!(k, Kind::Slice { .. }))
                .collect();
            let n = slice_params.len();
            // Every function some of the time, so pass 2 also leaves some
            // functions out.
            if k % 3 != 2 {
                set.insert(vec![1; n]);
                set.insert(vec![0; n]);
                set.insert((0..n).map(|i| i + 2).collect());
                let lent: Vec<usize> = slice_params
                    .iter()
                    .map(|k| match k {
                        Kind::Slice {
                            in_place: true,
                            nullable: false,
                            ..
                        } => plan_probe::SOURCE_SLICE,
                        _ => 3,
                    })
                    .collect();
                set.insert(lent);
                if n > 0 {
                    // A vector of the wrong arity declares the open op.
                    set.insert(vec![4; n + 1]);
                }
            }
            if !set.is_empty() {
                instances.insert((library.clone(), symbol.clone()), set.clone());
            }
            for lengths in set.iter().take(2) {
                op_names.push((symbol.clone(), lengths.clone()));
            }
            for (p, (_, _, kind)) in params.iter().enumerate() {
                if let Kind::Slice {
                    element,
                    nullable,
                    in_place,
                } = kind
                {
                    for (j, ty) in slice_argument_types(element).into_iter().enumerate() {
                        if (k + p + j) % 2 == 0 {
                            slices.push((ty, element.clone(), *nullable, *in_place, k, k + j));
                        }
                    }
                }
            }
            demangle.push(format!(
                "error: call of Ffi${library}::{symbol}$3_s with eff [Ffi${library}]"
            ));
        }
    }
    for (d, _) in &libs {
        let library = d.name();
        names.insert(library.to_owned());
        for e in d.enums() {
            names.insert(e.name().to_owned());
        }
        for w in d.wire_structs() {
            names.insert(w.name().to_owned());
        }
        for t in d.extern_types() {
            names.insert(t.name().to_owned());
        }
        demangle.push(format!("Ffi${library}::"));
        demangle.push(format!("Ffi${library}::f$"));
        demangle.push(format!(
            "x Ffi$Ffi${library}::b$1_2y Ffi${library}Ffi${library}"
        ));
        demangle.push(format!("Ffi${library}::\u{e9}t$12 and Ffi${library}x"));
        let effect = format!("Ffi${library}");
        let mut targets: Vec<Option<String>> = vec![None];
        targets.extend(PROBE_TARGETS.iter().map(|t| Some((*t).to_owned())));
        for (i, target) in targets.iter().enumerate() {
            for (j, f) in d.functions().iter().enumerate() {
                if (i + j) % 3 == 0 {
                    plans.push((target.clone(), effect.clone(), f.symbol().to_owned()));
                    plans.push((
                        target.clone(),
                        effect.clone(),
                        format!("{}$2_s", f.symbol()),
                    ));
                }
            }
            plans.push((target.clone(), effect.clone(), "missing$1".into()));
        }
    }
    plans.push((None, "Ffi$Nobody".into(), "f".into()));
    plans.push((None, "Nobody".into(), "f".into()));
    demangle.push("nothing foreign".into());
    names.insert("ForeignInvalid".into());
    names.insert("Plain".into());
    names.insert("Ffi".into());
    let names: Vec<String> = names.into_iter().collect();
    let mut collisions = vec![vec![]];
    for (i, n) in names.iter().enumerate() {
        collisions.push(vec![
            ("Plain".to_owned(), 0, 5),
            (n.clone(), 10 + i, 20 + i),
        ]);
    }
    collisions.push(vec![("Ffi".into(), 3, 6)]);
    slices.push((
        Some(Type::Tuple(vec![Type::named("u8"); 5000])),
        Type::named("u8"),
        false,
        false,
        1,
        2,
    ));
    Probes {
        instances,
        collisions,
        demangle,
        handles: names,
        plans,
        targets: PROBE_TARGETS.iter().map(|t| (*t).to_owned()).collect(),
        slices,
        op_names,
    }
}

/// Argument types a slice of `element` might be given in pass 1.
fn slice_argument_types(element: &Type) -> Vec<Option<Type>> {
    let tuple = |n: usize| Type::Tuple(vec![element.clone(); n]);
    let option = |t: Type| Type::Named("Option".into(), vec![t]);
    let slice = |t: Type| Type::Named("Slice".into(), vec![t]);
    vec![
        None,
        Some(tuple(3)),
        Some(tuple(0)),
        Some(Type::named("Unit")),
        Some(option(tuple(2))),
        Some(option(Type::named("Unit"))),
        Some(slice(element.clone())),
        Some(slice(Type::named("u16"))),
        Some(Type::named("i64")),
        Some(option(Type::named("bool"))),
        Some(Type::Named(
            "Result".into(),
            vec![tuple(1), Type::named("u8")],
        )),
    ]
}

// ---------------------------------------------------------------- input

struct Words(Vec<u8>);

impl Words {
    fn word(&mut self, x: u64) {
        self.0.extend_from_slice(&x.to_le_bytes());
    }
    fn text(&mut self, s: &[u8]) {
        self.word(s.len() as u64);
        self.0.extend_from_slice(s);
    }
    fn ty(&mut self, t: &Type) {
        match t {
            Type::Named(n, args) => {
                self.word(0);
                self.text(n.as_bytes());
                self.word(args.len() as u64);
                for a in args {
                    self.ty(a);
                }
            }
            Type::Tuple(args) => {
                self.word(1);
                self.word(args.len() as u64);
                for a in args {
                    self.ty(a);
                }
            }
            other => panic!("probe type {other:?}"),
        }
    }
}

fn input(case: &Case, p: &Probes) -> Vec<u8> {
    let mut w = Words(vec![]);
    let libs = case.sorted();
    w.word(libs.len() as u64);
    for (d, h) in &libs {
        w.word(match h {
            Handler::Direct => 0,
            Handler::IsolatedMpk => 1,
        });
        w.text(d.canonical_bytes());
    }
    w.word(p.instances.len() as u64);
    for ((library, symbol), set) in &p.instances {
        w.text(library.as_bytes());
        w.text(symbol.as_bytes());
        w.word(set.len() as u64);
        for v in set {
            w.word(v.len() as u64);
            for n in v {
                w.word(*n as u64);
            }
        }
    }
    w.word(p.collisions.len() as u64);
    for items in &p.collisions {
        w.word(items.len() as u64);
        for (n, a, b) in items {
            w.text(n.as_bytes());
            w.word(*a as u64);
            w.word(*b as u64);
        }
    }
    w.word(p.demangle.len() as u64);
    for m in &p.demangle {
        w.text(m.as_bytes());
    }
    w.word(p.handles.len() as u64);
    for n in &p.handles {
        w.text(n.as_bytes());
    }
    w.word(p.plans.len() as u64);
    for (t, e, o) in &p.plans {
        w.text(t.as_deref().unwrap_or("").as_bytes());
        w.text(e.as_bytes());
        w.text(o.as_bytes());
    }
    w.word(p.targets.len() as u64);
    for t in &p.targets {
        w.text(t.as_bytes());
    }
    w.word(p.slices.len() as u64);
    for (ty, element, nullable, in_place, a, b) in &p.slices {
        match ty {
            Some(t) => {
                w.word(1);
                w.ty(t);
            }
            None => w.word(0),
        }
        w.ty(element);
        w.word(u64::from(*nullable));
        w.word(u64::from(*in_place));
        w.word(*a as u64);
        w.word(*b as u64);
    }
    w.word(p.op_names.len() as u64);
    for (s, v) in &p.op_names {
        w.text(s.as_bytes());
        w.word(v.len() as u64);
        for n in v {
            w.word(*n as u64);
        }
    }
    w.0
}

// ---------------------------------------------------------------- Rust text

fn q(s: &str) -> String {
    let mut out = String::from("\"");
    for &b in s.as_bytes() {
        if !(0x20..=0x7e).contains(&b) || b == b'"' || b == b'\\' {
            write!(out, "\\x{b:02x}").unwrap();
        } else {
            out.push(b as char);
        }
    }
    out.push('"');
    out
}

fn line(o: &mut String, d: usize, text: &str) {
    for _ in 0..d {
        o.push(' ');
    }
    o.push_str(text);
    o.push('\n');
}

fn pass(p: Pass) -> &'static str {
    match p {
        Pass::In => "in",
        Pass::Inout => "inout",
        Pass::Sink => "sink",
    }
}

fn mode(m: Mode) -> &'static str {
    match m {
        Mode::Copy => "copy",
        Mode::Affine => "affine",
        Mode::Linear => "linear",
    }
}

fn sp(s: &Span) -> String {
    format!(" @{}..{}", s.start, s.end)
}

/// The AST dump of a type (tests/self_host_ast.rs's format).
fn dump_ty(o: &mut String, t: &Type, d: usize) {
    match t {
        Type::Named(n, args) => {
            line(o, d, &format!("T {}", q(n)));
            for a in args {
                dump_ty(o, a, d + 1);
            }
        }
        Type::Tuple(args) => {
            line(o, d, "T tuple");
            for a in args {
                dump_ty(o, a, d + 1);
            }
        }
        other => panic!("generated type {other:?}"),
    }
}

/// The AST dump of a generated item (only the forms `append` makes; any
/// other field value fails the test, so a Rust change shows).
fn dump_item(o: &mut String, item: &Item, d: usize) {
    match item {
        Item::Adt(a) => {
            assert!(a.generics.is_empty() && a.tile_width.is_none());
            line(
                o,
                d,
                &format!(
                    "adt {} mode={} enum={} tile=-{}",
                    q(&a.name),
                    mode(a.mode),
                    u8::from(a.is_enum),
                    sp(&a.span)
                ),
            );
            for v in &a.variants {
                line(
                    o,
                    d + 1,
                    &format!("variant {} named={}", q(&v.name), u8::from(v.named)),
                );
                for f in &v.fields {
                    assert!(f.modality.is_none() && f.temperature.is_none());
                    line(o, d + 2, &format!("field {} modality=- temp=-", q(&f.name)));
                    dump_ty(o, &f.ty, d + 3);
                }
            }
        }
        Item::Effect(e) => {
            assert!(e.generics.is_empty());
            line(
                o,
                d,
                &format!(
                    "effect_decl {} linear={}{}",
                    q(&e.name),
                    u8::from(e.linear),
                    sp(&e.span)
                ),
            );
            for f in &e.ops {
                assert!(f.generics.is_empty() && f.body.is_none());
                assert!(f.effects.instances.is_empty() && f.effects.tail.is_none());
                line(
                    o,
                    d + 1,
                    &format!("fn {} fip={}{}", q(&f.name), u8::from(f.fip), sp(&f.span)),
                );
                for p in &f.params {
                    assert!(p.projections.is_empty());
                    line(
                        o,
                        d + 2,
                        &format!("param {} pass={}{}", q(&p.name), pass(p.pass), sp(&p.span)),
                    );
                    dump_ty(o, &p.ty, d + 3);
                }
                line(o, d + 2, "result");
                dump_ty(o, &f.result, d + 3);
                line(o, d + 2, "effects tail=-");
            }
        }
        other => panic!("generated item {other:?}"),
    }
}

fn kind(k: FfiErrorKind) -> String {
    format!("{k:?}")
}

fn err(e: &FfiError) -> String {
    let span = e
        .span
        .as_ref()
        .map_or_else(|| " -".to_owned(), |s| format!(" @{}..{}", s.start, s.end));
    format!("{}{span} {}", kind(e.kind), q(&e.message))
}

fn handler(h: Handler) -> &'static str {
    match h {
        Handler::Direct => "direct",
        Handler::IsolatedMpk => "isolated(mpk)",
    }
}

fn abi_type(t: AbiType) -> String {
    match t {
        AbiType::Pointer => "ptr".into(),
        AbiType::Integer(s) => s.name().into(),
    }
}

fn rust_text(case: &Case, p: &Probes) -> String {
    let imports = case.imports().expect("corpus libraries import");
    let mut o = String::new();
    let generated = plan_probe::generated(&imports);
    match &generated {
        Err(e) => line(&mut o, 0, &format!("G err {}", err(e))),
        Ok(g) => {
            line(&mut o, 0, "G ok");
            for (library, symbol, shape) in g.shapes(&imports) {
                match shape {
                    Err(e) => line(
                        &mut o,
                        0,
                        &format!("shape {} {} err {}", q(&library), q(&symbol), q(&e)),
                    ),
                    Ok((params, result)) => {
                        let slices = params
                            .iter()
                            .filter(|p| matches!(p.2, Kind::Slice { .. }))
                            .count();
                        line(
                            &mut o,
                            0,
                            &format!("shape {} {} ok slices={slices}", q(&library), q(&symbol)),
                        );
                        for (name, ps, k) in &params {
                            match k {
                                Kind::Value(t) => {
                                    line(
                                        &mut o,
                                        1,
                                        &format!("param {} pass={} value", q(name), pass(*ps)),
                                    );
                                    dump_ty(&mut o, t, 2);
                                }
                                Kind::Slice {
                                    element,
                                    nullable,
                                    in_place,
                                } => {
                                    line(
                                        &mut o,
                                        1,
                                        &format!(
                                            "param {} pass={} slice nullable={} in_place={}",
                                            q(name),
                                            pass(*ps),
                                            u8::from(*nullable),
                                            u8::from(*in_place)
                                        ),
                                    );
                                    dump_ty(&mut o, element, 2);
                                }
                            }
                        }
                        line(&mut o, 1, "result");
                        dump_ty(&mut o, &result, 2);
                    }
                }
            }
            line(&mut o, 0, "pass1");
            for item in g.declarations(None) {
                dump_item(&mut o, &item, 1);
            }
            line(&mut o, 0, "pass2");
            for item in g.declarations(Some(&p.instances)) {
                dump_item(&mut o, &item, 1);
            }
        }
    }
    if let Ok(g) = &generated {
        for items in &p.collisions {
            let module = Module {
                items: items
                    .iter()
                    .enumerate()
                    .map(|(i, (name, a, b))| {
                        if i % 2 == 0 {
                            Item::Adt(Adt {
                                name: name.clone(),
                                mode: Mode::Copy,
                                generics: vec![],
                                variants: vec![],
                                is_enum: false,
                                tile_width: None,
                                span: *a..*b,
                            })
                        } else {
                            Item::Effect(EffectDecl {
                                name: name.clone(),
                                linear: false,
                                generics: vec![],
                                ops: vec![],
                                span: *a..*b,
                            })
                        }
                    })
                    .collect(),
                ..Module::default()
            };
            match g.check_collisions(&module) {
                Ok(()) => line(&mut o, 0, "collide ok"),
                Err(e) => line(&mut o, 0, &format!("collide err {}", err(&e))),
            }
        }
        for m in &p.demangle {
            line(
                &mut o,
                0,
                &format!("demangle {} {}", q(m), q(&g.demangle(m))),
            );
        }
        for n in &p.handles {
            line(
                &mut o,
                0,
                &format!("handle {} {}", q(n), u8::from(g.is_handle(n))),
            );
        }
    }
    for (t, e, op) in &p.plans {
        let answer = match plan_probe::op_plan(&imports, t.as_deref(), e, op) {
            Ok((l, h, s)) => format!("ok {} {} {}", q(&l), handler(h), q(&s)),
            Err(m) => format!("err {}", q(&m)),
        };
        let t = t.as_deref().map_or_else(|| "-".to_owned(), q);
        line(&mut o, 0, &format!("plan {t} {} {} {answer}", q(e), q(op)));
    }
    for t in &p.targets {
        line(&mut o, 0, &format!("target {}", q(t)));
        for (library, r) in plan_probe::target_support(&imports, t) {
            match r {
                Ok(()) => line(&mut o, 1, &format!("{} ok", q(&library))),
                Err(e) => line(&mut o, 1, &format!("{} err {}", q(&library), err(&e))),
            }
        }
    }
    for (ty, element, nullable, in_place, a, b) in &p.slices {
        let answer =
            match plan_probe::slice_length(ty.as_ref(), element, *nullable, *in_place, &(*a..*b)) {
                Ok(n) if n == plan_probe::SOURCE_SLICE => "ok s".to_owned(),
                Ok(n) => format!("ok {n}"),
                Err(e) => format!("err {}", err(&e)),
            };
        line(&mut o, 0, &format!("slice {answer}"));
    }
    for (s, v) in &p.op_names {
        line(
            &mut o,
            0,
            &format!("opname {} {}", q(s), q(&plan_probe::op_name(s, v))),
        );
    }
    for (d, _) in case.sorted() {
        for f in d.functions() {
            let unit = f
                .parameters()
                .iter()
                .any(|p| matches!(p.ty(), ParameterType::Value(ValueType::Unit)));
            if unit {
                line(
                    &mut o,
                    0,
                    &format!("abi {} {} none", q(d.name()), q(f.symbol())),
                );
                continue;
            }
            let abi = f.abi();
            let params: Vec<String> = abi.parameters.iter().map(|t| abi_type(*t)).collect();
            line(
                &mut o,
                0,
                &format!(
                    "abi {} {} conv={} params={} result={}",
                    q(d.name()),
                    q(f.symbol()),
                    abi.convention.name(),
                    params.join(","),
                    abi.result.map_or_else(|| "-".to_owned(), abi_type)
                ),
            );
        }
    }
    o
}

// ---------------------------------------------------------------- Vesper

fn text_entry() -> &'static Entry {
    static F: OnceLock<Entry> = OnceLock::new();
    F.get_or_init(|| compile("ffi_plan_text", speed()))
}

fn count_entry() -> &'static Entry {
    static F: OnceLock<Entry> = OnceLock::new();
    F.get_or_init(|| compile("ffi_plan_count", true))
}

fn compile(entry: &str, optimize: bool) -> Entry {
    let t = std::time::Instant::now();
    let e = Package::load(PACKAGE).entry(entry, optimize);
    eprintln!(
        "compiled {entry} (optimize {optimize}) in {:?}",
        t.elapsed()
    );
    e
}

fn vesper_text(input: &[u8]) -> String {
    text_entry().text(input)
}

// ---------------------------------------------------------------- corpus

/// A `.vsys` source with its placeholders filled for `target`.
fn fill(source: &str, target: (&str, &str)) -> String {
    source
        .replace("@DIGEST@", ZERO_DIGEST)
        .replace("@TARGET@", target.0)
        .replace("@ABI@", target.1)
}

/// Every parseable library a source yields (once per target when it has a
/// `@TARGET@` placeholder).
fn libraries_of(source: &str, out: &mut Vec<LibraryDescriptor>) {
    let targets: &[(&str, &str)] = if source.contains("@TARGET@") {
        &TARGETS
    } else {
        &TARGETS[..1]
    };
    for t in targets {
        if let Ok(d) = sys::parse(&fill(source, *t), sys::Limits::default()) {
            out.push(d);
        }
    }
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>, ext: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if path.is_dir() {
            if !matches!(name.to_str(), Some("target" | ".git" | "node_modules"))
                && !name.to_string_lossy().starts_with("bazel-")
            {
                walk(&path, out, ext);
            }
        } else if path.extension().is_some_and(|e| e == ext) {
            out.push(path);
        }
    }
}

/// The `// @export vsys` directive bodies (`// | ` lines) of a `.ves` file.
fn directive_sources(text: &str) -> Vec<String> {
    let mut out = vec![];
    let mut current: Option<String> = None;
    for l in text.lines() {
        if l.starts_with("// @export vsys") {
            current = Some(String::new());
            continue;
        }
        if let Some(c) = &mut current {
            if let Some(rest) = l.strip_prefix("// |") {
                c.push_str(rest.strip_prefix(' ').unwrap_or(rest));
                c.push('\n');
                continue;
            }
            out.push(std::mem::take(c));
            current = None;
        }
    }
    if let Some(c) = current {
        out.push(c);
    }
    out
}

/// The Rust test files whose string literals hold `.vsys` sources.
const VSYS_TESTS: [&str; 15] = [
    "tests/borrowed_box_match.rs",
    "tests/ffi_affine_drops.rs",
    "tests/ffi_handles.rs",
    "tests/ffi_lowering.rs",
    "tests/package_submodule_exports.rs",
    "tests/test_runner_cli.rs",
    "tests/tree_abi.rs",
    "tests/vsys_packages.rs",
    "tests/dyn_effects.rs",
    "crates/vesper-ffi/tests/sys.rs",
    "crates/vesper-ffi/tests/sys_c_abi.rs",
    "crates/vesper-ffi/tests/sys_pointers.rs",
    "crates/vesper-ffi/tests/sys_slices.rs",
    "crates/vesper-ffi/tests/sys_vsys_rest_c.rs",
    "src/native_export/directives.rs",
];

/// `sys library` sources embedded in a Rust file: from the keyword to the
/// brace that closes the library body, with string escapes and format
/// braces undone (both spellings are tried).
fn embedded_sources(text: &str) -> Vec<String> {
    let mut out = vec![];
    let mut from = 0;
    while let Some(at) = text[from..].find("sys library") {
        let start = from + at;
        from = start + 1;
        let bytes = text.as_bytes();
        let mut i = start;
        let mut depth = 0i32;
        let mut opened = false;
        let mut paren = 0i32;
        while i < bytes.len() {
            match bytes[i] {
                b'(' => paren += 1,
                b')' => paren -= 1,
                b'{' if paren == 0 => {
                    depth += 1;
                    opened = true;
                }
                b'}' if paren == 0 => {
                    depth -= 1;
                }
                _ => {}
            }
            i += 1;
            if opened && depth <= 0 {
                break;
            }
        }
        let raw = &text[start..i.min(text.len())];
        let unescaped = raw
            .replace("\\n", "\n")
            .replace("\\\"", "\"")
            .replace("\\\n", "\n");
        out.push(unescaped.clone());
        out.push(unescaped.replace("{{", "{").replace("}}", "}"));
    }
    out
}

/// Hand-written libraries for the paths the repository's do not reach:
/// address, unit and handle parameters, pointer results, nested and
/// byte-array structs, every slice and pointer form, `errno`, `CStr`,
/// reserved and duplicate names.
const HANDWRITTEN: [&str; 6] = [
    r#"sys library Shapes (
    target = "x86_64-unknown-linux-gnu", abi = "sysv64",
    link = "dynamic", name = "shapes", artifact = "blake3:@DIGEST@",
    header = "shapes.h", pkg_config = "shapes ^1.2.3"
) {
    enum Mode: u8 { Off = 0, On = 3 }
    enum Wide: u64 { A = 1, B = 18446744073709551615 }
    wire struct Inner { a: u16le, b: i16be, pad: ZeroPad[4] }
    wire struct Outer { tag: ByteArray[4], inner: Inner, n: i32, z: i8, y: u8, w: u16, v: i64be }
    wire struct Flat { a: u64, b: i64, c: u64 }
    linear extern type Conn;
    affine extern type View drop view_free;
    fn view_free(sink v: View) -> ForeignVal[Unit];
    fn get_addr() -> ForeignVal[Ptr[u32]];
    fn mode(m: Mode) -> ForeignVal[Mode];
    fn wide() -> ForeignVal[Wide];
    fn ok() -> ForeignVal[bool] errno;
    fn sz(n: usize) -> ForeignVal[usize] errno;
    fn name(nocapture in s: CStr) -> ForeignVal[u32];
    fn names(nocapture in a: CStr, nocapture in b: CStr) -> ForeignVal[Mode] errno;
    fn outer(nocapture in o: Outer, nocapture out p: Outer, nocapture inout q: Option[Outer]) -> ForeignVal[u64];
    fn flats(nocapture in a: CSlice[Flat], nocapture inout b: Option[CSlice[Flat]], nocapture in c: CSlice[u16]) -> ForeignVal[Unit];
    fn ints(nocapture in a: CSlice[u32], nocapture inout b: CSlice[i64], nocapture in c: CSlice[usize], nocapture in d: CSlice[u64], nocapture in e: Option[CSlice[u8]]) -> ForeignVal[i16];
    fn sints(nocapture in a: CSlice[i8], nocapture in b: CSlice[i32], nocapture in c: CSlice[Inner]) -> ForeignVal[u16];
    fn ptrs(nocapture in a: i8, nocapture inout b: Option[i64], nocapture out c: usize) -> ForeignVal[i8];
    fn conn_new() -> ForeignVal[Conn];
    fn conn_maybe() -> ForeignVal[Option[Conn]];
    fn conn_must() -> ForeignVal[NonNull[Conn]];
    fn conn_use(in c: Conn, inout d: Conn, nocapture in data: CSlice[u8]) -> ForeignVal[u32] errno;
    fn conn_free(sink c: Conn) -> ForeignVal[Unit];
    fn view_get() -> ForeignVal[Option[View]];
}"#,
    r#"sys library Reserved (
    target = "aarch64-apple-darwin", abi = "darwin64",
    link = "static", name = "reserved", artifact = "blake3:@DIGEST@"
) {
    wire struct Result { a: u32 }
    fn f(nocapture in r: Result) -> ForeignVal[u32];
}"#,
    r#"sys library Clash (
    target = "x86_64-pc-windows-msvc", abi = "win64",
    link = "static", name = "clash", artifact = "blake3:@DIGEST@"
) {
    enum Clash: u32 { A = 1 }
    fn f() -> ForeignVal[Clash];
}"#,
    r#"sys library Twin (
    target = "x86_64-unknown-linux-gnu", abi = "sysv64",
    link = "static", name = "twin", artifact = "blake3:@DIGEST@"
) {
    enum Mode: u8 { Off = 0, On = 3 }
    wire struct Flat { a: u64, b: i64, c: u64 }
    affine extern type Peer;
    fn twin(nocapture in f: Flat, m: Mode) -> ForeignVal[Option[Peer]];
}"#,
    r#"sys library Skew (
    target = "aarch64-unknown-linux-gnu", abi = "aapcs64",
    link = "static", name = "skew", artifact = "blake3:@DIGEST@"
) {
    enum Mode: u8 { Off = 0, Half = 1, On = 3 }
    wire struct Flat { a: u64, b: i64 }
    linear extern type Conn;
    fn skew(nocapture in f: Flat) -> ForeignVal[Mode];
    fn skew_free(sink c: Conn) -> ForeignVal[Unit];
}"#,
    r#"sys library Invalid (
    target = "x86_64-unknown-linux-gnu", abi = "sysv64",
    link = "static", name = "invalid", artifact = "blake3:@DIGEST@"
) {
    wire struct ForeignInvalid { raw: u64 }
    fn f(nocapture in x: ForeignInvalid) -> ForeignVal[bool];
}"#,
];

fn corpus() -> &'static [Case] {
    static CORPUS: OnceLock<Vec<Case>> = OnceLock::new();
    CORPUS.get_or_init(|| {
        let mut sources: Vec<(String, String)> = vec![];
        let mut files = vec![];
        for dir in ["tests/fixtures", "packages", "crates"] {
            walk(&root().join(dir), &mut files, "vsys");
        }
        files.sort();
        for f in &files {
            sources.push((
                f.strip_prefix(root()).unwrap_or(f).display().to_string(),
                std::fs::read_to_string(f).unwrap(),
            ));
        }
        let mut ves = vec![];
        for dir in ["packages", "crates"] {
            walk(&root().join(dir), &mut ves, "ves");
        }
        ves.sort();
        for f in &ves {
            let text = std::fs::read_to_string(f).unwrap();
            for s in directive_sources(&text) {
                sources.push((f.strip_prefix(root()).unwrap_or(f).display().to_string(), s));
            }
        }
        for t in VSYS_TESTS {
            let Ok(text) = std::fs::read_to_string(root().join(t)) else {
                continue;
            };
            for s in embedded_sources(&text) {
                sources.push((t.to_owned(), s));
            }
        }
        for (i, s) in HANDWRITTEN.iter().enumerate() {
            if let Err(e) = sys::parse(&fill(s, TARGETS[0]), sys::Limits::default()) {
                panic!("handwritten library {i} does not parse: {e}");
            }
            sources.push((format!("handwritten {i}"), (*s).to_owned()));
        }
        // Distinct libraries, by canonical bytes.
        let mut seen = BTreeSet::new();
        let mut libraries: Vec<(String, LibraryDescriptor)> = vec![];
        for (label, s) in &sources {
            let mut found = vec![];
            libraries_of(s, &mut found);
            for d in found {
                if seen.insert(d.canonical_bytes().to_vec()) {
                    libraries.push((label.clone(), d));
                }
            }
        }
        let outside = libraries
            .iter()
            .filter(|(l, _)| l.starts_with("crates/"))
            .count();
        eprintln!(
            "{} distinct libraries ({outside} from crates/)",
            libraries.len()
        );
        // Bazel declares only the root package's files (crates/* are their
        // own packages); Cargo reads the whole repository.
        let floor = if std::env::var_os("TEST_SRCDIR").is_some() {
            20
        } else {
            40
        };
        assert!(
            libraries.len() >= floor,
            "only {} distinct libraries",
            libraries.len()
        );
        let mut cases = vec![];
        for (i, (label, d)) in libraries.iter().enumerate() {
            let h = if i % 4 == 1 {
                Handler::IsolatedMpk
            } else {
                Handler::Direct
            };
            cases.push(Case {
                label: format!("{label}: {}", d.name()),
                libraries: vec![(d.clone(), h)],
            });
        }
        // Pairs: each library with its successor of another name, and every
        // handwritten library with every other one.
        for w in libraries.windows(2) {
            if w[0].1.name() != w[1].1.name() {
                cases.push(Case {
                    label: format!("pair {} + {}", w[0].1.name(), w[1].1.name()),
                    libraries: vec![
                        (w[0].1.clone(), Handler::Direct),
                        (w[1].1.clone(), Handler::IsolatedMpk),
                    ],
                });
            }
        }
        let hand: Vec<&(String, LibraryDescriptor)> = libraries
            .iter()
            .filter(|(l, _)| l.starts_with("handwritten"))
            .collect();
        for a in &hand {
            for b in &hand {
                if a.1.name() < b.1.name() {
                    cases.push(Case {
                        label: format!("pair {} + {}", a.1.name(), b.1.name()),
                        libraries: vec![
                            (a.1.clone(), Handler::Direct),
                            (b.1.clone(), Handler::Direct),
                        ],
                    });
                }
            }
        }
        cases
    })
}

/// Compares one case; returns the first difference, if any.
fn compare(case: &Case) -> Option<String> {
    let imports = case.imports().expect("corpus libraries import");
    let p = probes(case, &imports);
    let r = rust_text(case, &p);
    let v = vesper_text(&input(case, &p));
    if r == v {
        return None;
    }
    Some(first_difference(&r, &v).unwrap_or_else(|| "texts differ in trailing bytes".into()))
}

#[test]
fn type_mapping_and_plans_match_rust() {
    let cases = corpus();
    let mut bad = 0;
    let mut lines = 0;
    let mut generated_errors = BTreeSet::new();
    let mut probe_errors = BTreeSet::new();
    for case in cases {
        let imports = case.imports().unwrap();
        let p = probes(case, &imports);
        let r = rust_text(case, &p);
        lines += r.lines().count();
        for l in r.lines() {
            if let Some(rest) = l.strip_prefix("G err ") {
                generated_errors.insert(rest.split(' ').next().unwrap_or("").to_owned());
            }
            for key in [
                " err NameCollision",
                " err TargetMismatch",
                " err IsolationUnavailable",
                " err SliceArgument",
            ] {
                if l.contains(key) {
                    probe_errors.insert(key.trim().to_owned());
                }
            }
            if l.starts_with("shape ") && l.contains(" err ") {
                probe_errors.insert("shape err".to_owned());
            }
        }
        if let Some(d) = compare(case) {
            bad += 1;
            if bad <= 5 {
                eprintln!("{}: {d}", case.label);
            }
        }
    }
    eprintln!(
        "{} cases, {lines} lines; {bad} differ; generation errors {generated_errors:?}; probe errors {probe_errors:?}",
        cases.len()
    );
    assert_eq!(bad, 0, "{bad} of {} cases differ", cases.len());
    // The corpus reaches every error the port writes.
    assert!(
        generated_errors.contains("DuplicateLibrary"),
        "{generated_errors:?}"
    );
    for key in [
        "err NameCollision",
        "err TargetMismatch",
        "err IsolationUnavailable",
        "err SliceArgument",
        "shape err",
    ] {
        assert!(
            probe_errors.contains(key),
            "{key} never fires: {probe_errors:?}"
        );
    }
}

/// ABBA timing of the whole case (decode, generate, probes and text) in
/// Vesper against the Rust probe calls, both building their text.
#[test]
#[ignore = "timing; run with CHECK_HARNESS_SPEED=1 and --ignored"]
fn type_mapping_speed() {
    let cases = corpus();
    let prepared: Vec<(Case, Vec<u8>, Probes)> = cases
        .iter()
        .map(|c| {
            let imports = c.imports().unwrap();
            let p = probes(c, &imports);
            (c.clone(), input(c, &p), p)
        })
        .collect();
    let entry = count_entry();
    let (r, v) = abba(
        "ffi type mapping",
        &mut || {
            time(|| {
                for (c, _, p) in &prepared {
                    std::hint::black_box(rust_text(c, p));
                }
            })
        },
        &mut || {
            time(|| {
                for (_, i, _) in &prepared {
                    std::hint::black_box(entry.word(i));
                }
            })
        },
    );
    eprintln!(
        "rust {r:?} vesper {v:?} ({:.2}x)",
        v.as_secs_f64() / r.as_secs_f64()
    );
}
