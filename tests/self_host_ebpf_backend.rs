//! Self-hosting SH4 lane 16 (#1290): `@vesper/ebpf-backend` against the
//! Rust eBPF target (`vesper::ebpf_backend::compile_world`).
//!
//! The input is the @vesper/place-ir input (check-data's names, types and
//! world, then the Place IR program of `ir::lower_world`) followed by the
//! task section [`ebpf_section`] (options, source text, ADT spans,
//! `world.functions` and `world.effects`). The Vesper entry
//! `ebpf_backend_words` returns the assembled program or the typed error;
//! [`decode`] rebuilds `Result<Compiled, EbpfBackendError>`, running the
//! Rust verifier model on the decoded program, so the `Debug` comparison
//! covers the instructions, maps, relocations, subprograms, line info,
//! frames, loops, map bindings, the verifier report and the verifier error.
//! Accepted programs also compare their ELF objects byte for byte.
//!
//! Cases: the three eBPF fixtures with every function as the entry, the
//! out-of-subset programs of tests/ebpf_backend.rs, truncated and
//! literal-swapped fixture sources, generated subset programs, and every
//! corpus world with `main` as the entry.
#[path = "common/check_harness.rs"]
mod check_harness;
#[path = "common/check_words.rs"]
mod check_words;
#[path = "common/ir_words.rs"]
mod ir_words;

use check_harness::*;
use std::sync::OnceLock;
use vesper::ast::{Effects, Type};
use vesper::check::World;
use vesper::ebpf_backend::{
    ActionKind, Compiled, EbpfBackendError, MapBinding, Options, compile_world, rules,
};
use vesper_ebpf::{LineInfo, MapDef, MapKind, Program, ProgramType, RawInsn};

fn package() -> &'static Package {
    static P: OnceLock<Package> = OnceLock::new();
    P.get_or_init(|| Package::load("packages/vesper-ebpf-backend"))
}

// ------------------------------------------------------------ the section

fn put(out: &mut Vec<u8>, x: u64) {
    out.extend(x.to_le_bytes());
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    put(out, s.len() as u64);
    out.extend(s.as_bytes());
    while !out.len().is_multiple_of(8) {
        out.push(0);
    }
}

fn put_row(out: &mut Vec<u8>, e: &Effects) {
    put(out, e.instances.len() as u64);
    for i in &e.instances {
        put(out, i.label.is_some() as u64);
        if let Some(l) = &i.label {
            put_str(out, l);
        }
        put_str(out, &i.name);
        put(out, i.args.len() as u64);
        for a in &i.args {
            put_ty(out, a);
        }
    }
    put(out, e.tail.is_some() as u64);
    if let Some(t) = &e.tail {
        put_str(out, t);
    }
}

/// An inline type tree (`read_ty` in extra.ves).
fn put_ty(out: &mut Vec<u8>, t: &Type) {
    match t {
        Type::IntLiteral(n) => {
            put(out, 0);
            put(out, (*n < 0) as u64);
            put(out, n.unsigned_abs() as u64);
        }
        Type::Named(n, a) => {
            put(out, 1);
            put_str(out, n);
            put(out, a.len() as u64);
            for t in a {
                put_ty(out, t);
            }
        }
        Type::Tuple(a) => {
            put(out, 2);
            put(out, a.len() as u64);
            for t in a {
                put_ty(out, t);
            }
        }
        Type::Function {
            params,
            result,
            effects,
            mode,
        } => {
            put(out, 3);
            put(out, params.len() as u64);
            for (p, t) in params {
                put(out, pass_word(*p));
                put_ty(out, t);
            }
            put_ty(out, result);
            put_row(out, effects);
            put(out, mode_word(*mode));
        }
        Type::Row(e) => {
            put(out, 4);
            put_row(out, e);
        }
        Type::Infer => put(out, 5),
    }
}

/// The task section (`read_extra` in extra.ves).
pub fn ebpf_section(w: &World, source: Option<&str>, options: &Options) -> Vec<u8> {
    let mut out = vec![];
    put_str(&mut out, &options.entry);
    put_str(&mut out, &options.file);
    put(&mut out, source.is_some() as u64);
    if let Some(s) = source {
        put_str(&mut out, s);
    }
    put(&mut out, w.adts.len() as u64);
    for a in w.adts.values() {
        put(&mut out, a.span.start as u64);
        put(&mut out, a.span.end as u64);
    }
    put(&mut out, w.functions.len() as u64);
    for (name, f) in &w.functions {
        put_str(&mut out, name);
        put(&mut out, f.span.start as u64);
        put(&mut out, f.span.end as u64);
        put(&mut out, f.generics.len() as u64);
        put(&mut out, f.params.len() as u64);
        for p in &f.params {
            put_str(&mut out, &p.name);
            put(&mut out, pass_word(p.pass));
            put(&mut out, p.span.start as u64);
            put(&mut out, p.span.end as u64);
            put_ty(&mut out, &p.ty);
        }
        put_ty(&mut out, &f.result);
        put(&mut out, f.effects.instances.len() as u64);
        for e in &f.effects.instances {
            put_str(&mut out, &e.name);
        }
        put(&mut out, f.effects.tail.is_some() as u64);
    }
    put(&mut out, w.effects.len() as u64);
    for (name, d) in &w.effects {
        put_str(&mut out, name);
        put(&mut out, d.generics.len() as u64);
        put(&mut out, d.span.start as u64);
        put(&mut out, d.span.end as u64);
        put(&mut out, d.ops.len() as u64);
        for o in &d.ops {
            put_str(&mut out, &o.name);
            put(&mut out, o.span.start as u64);
            put(&mut out, o.span.end as u64);
            put(&mut out, o.params.len() as u64);
            for p in &o.params {
                put_ty(&mut out, &p.ty);
            }
            put_ty(&mut out, &o.result);
        }
    }
    out
}

/// Whether the section's inline literals fit (sign and 64-bit magnitude).
fn section_fits(w: &World) -> bool {
    w.functions.values().all(|f| {
        ir_words::literal_fits(&f.result) && f.params.iter().all(|p| ir_words::literal_fits(&p.ty))
    }) && w.effects.values().all(|d| {
        d.ops.iter().all(|o| {
            ir_words::literal_fits(&o.result)
                && o.params.iter().all(|p| ir_words::literal_fits(&p.ty))
        })
    })
}

/// The whole input, or `None` when the world or program is not encodable.
pub fn input(w: &World, source: Option<&str>, options: &Options) -> Option<Vec<u8>> {
    if !section_fits(w) {
        return None;
    }
    let ir = vesper::ir::lower_world(w);
    let (mut bytes, _) = ir_words::program_input(w, &ir)?;
    bytes.extend(ebpf_section(w, source, options));
    Some(bytes)
}

// ------------------------------------------------------------- the result

struct Words<'a> {
    w: &'a [u64],
    at: usize,
}

impl Words<'_> {
    fn next(&mut self) -> u64 {
        let x = self.w[self.at];
        self.at += 1;
        x
    }

    fn usize(&mut self) -> usize {
        self.next() as usize
    }

    fn u32(&mut self) -> u32 {
        self.next() as u32
    }

    fn str(&mut self) -> String {
        let n = self.usize();
        let mut b = Vec::with_capacity(n + 8);
        for _ in 0..n.div_ceil(8) {
            b.extend(self.next().to_le_bytes());
        }
        b.truncate(n);
        String::from_utf8(b).expect("utf-8")
    }
}

const RULES: [&str; 11] = [
    rules::ENTRY,
    rules::ACTION,
    rules::TYPE,
    rules::EFFECT,
    rules::MAP,
    rules::RECURSION,
    rules::OPERATION,
    rules::FALLIBLE,
    rules::CALL,
    rules::STACK,
    rules::GENERIC,
];

const MAP_KINDS: [MapKind; 5] = [
    MapKind::Hash,
    MapKind::Array,
    MapKind::PerCpuHash,
    MapKind::PerCpuArray,
    MapKind::LruHash,
];

/// The Vesper result as the Rust one: the verifier model runs on the
/// decoded program, as `compile_world` runs it on its own.
pub fn decode(words: &[u64], options: &Options) -> Result<Compiled, EbpfBackendError> {
    let mut r = Words { w: words, at: 0 };
    match r.next() {
        0 => {}
        1 => {
            let rule = RULES[r.usize()];
            let function = r.str();
            let has = r.next() != 0;
            let (s0, s1) = (r.usize(), r.usize());
            let message = r.str();
            return Err(EbpfBackendError::Unsupported {
                rule,
                function,
                span: has.then_some(s0..s1),
                message,
            });
        }
        _ => return Err(EbpfBackendError::Asm(r.str())),
    }
    let action = if r.next() == 0 {
        ActionKind::Xdp
    } else {
        ActionKind::Tc
    };
    let name = r.str();
    let insns = (0..r.usize())
        .map(|_| {
            let x = r.next();
            RawInsn {
                code: x as u8,
                dst: (x >> 8) as u8 & 0xf,
                src: (x >> 12) as u8 & 0xf,
                off: (x >> 16) as u16 as i16,
                imm: (x >> 32) as u32 as i32,
            }
        })
        .collect();
    let maps = (0..r.usize())
        .map(|_| MapDef {
            name: r.str(),
            kind: MAP_KINDS[r.usize()],
            key_size: r.u32(),
            value_size: r.u32(),
            max_entries: r.u32(),
            flags: r.u32(),
        })
        .collect();
    let map_relocs = (0..r.usize()).map(|_| (r.usize(), r.usize())).collect();
    let functions = (0..r.usize()).map(|_| (r.str(), r.usize())).collect();
    let lines = (0..r.usize())
        .map(|_| {
            let slot = r.usize();
            let text = r.str();
            LineInfo {
                slot,
                file: options.file.clone(),
                text,
                line: r.u32(),
                col: r.u32(),
            }
        })
        .collect();
    let frames = (0..r.usize()).map(|_| (r.str(), r.u32())).collect();
    let loops = (0..r.usize()).map(|_| r.str()).collect();
    let bindings = (0..r.usize())
        .map(|_| MapBinding {
            effect: r.str(),
            index: r.usize(),
        })
        .collect();
    assert_eq!(r.at, words.len(), "trailing words");
    let program = Program {
        name,
        prog_type: match action {
            ActionKind::Xdp => ProgramType::Xdp,
            ActionKind::Tc => ProgramType::SchedCls,
        },
        insns,
        maps,
        map_relocs,
        functions,
        license: "Dual MIT/GPL".to_owned(),
        attach: String::new(),
        kfuncs: vec![],
        core_relos: vec![],
        lines,
    };
    let report =
        vesper_ebpf::verify(&program, &options.policy).map_err(EbpfBackendError::Verifier)?;
    Ok(Compiled {
        program,
        report,
        maps: bindings,
        action,
        frames,
        loops,
    })
}

// ------------------------------------------------------------------ cases

pub struct EbpfCase {
    pub name: String,
    pub world: World,
    pub source: Option<String>,
    pub options: Options,
}

fn checked(src: &str) -> Option<World> {
    let parsed = vesper::parser::parse(src);
    if !parsed.diagnostics.is_empty() {
        return None;
    }
    let c = vesper::check::check(&parsed.module);
    c.diagnostics.is_empty().then_some(c.world)
}

fn fixture(name: &str) -> String {
    std::fs::read_to_string(repo().join(format!("tests/fixtures/ebpf_backend/{name}.ves"))).unwrap()
}

const PRELUDE: &str = "copy enum XdpAction { Aborted, Drop, Pass, Tx, Redirect }
copy enum BpfMapKind { Hash, Array, PerCpuHash, PerCpuArray, LruHash }
copy struct BpfMapSpec { kind: BpfMapKind, max_entries: u32 }
effect Packet { fn load_u8(offset: u32) -> Option[u8]; }
";

/// The out-of-subset bodies of tests/ebpf_backend.rs, and more.
const BODIES: &[&str] = &[
    "fn main() -> u64 { 0 }",
    "fn main(x: u32) -> XdpAction { XdpAction::Pass }",
    "fn other() -> XdpAction { XdpAction::Pass }\nfn helper() -> u64 { 1 }",
    "fn depth(n: u32) -> u32 { if n == 0 { 0 } else { wrapping_add(depth(wrapping_sub(n, 1)), 1) } }
     fn main() -> XdpAction { if depth(3) == 3 { XdpAction::Pass } else { XdpAction::Drop } }",
    "fn even(n: u32) -> bool { if n == 0 { true } else { odd(wrapping_sub(n, 1)) } }
     fn odd(n: u32) -> bool { if n == 0 { false } else { even(wrapping_sub(n, 1)) } }
     fn main() -> XdpAction { if even(4) { XdpAction::Pass } else { XdpAction::Drop } }",
    "fn bump(x: u32) -> u32 eff [Fail[ArithOverflow]] { x + 1 }
     fn main() -> XdpAction eff [Fail[ArithOverflow]] { if bump(1) == 2 { XdpAction::Pass } else { XdpAction::Drop } }",
    "fn adder(sink k: u32) -> fn @copy (u32) -> u32 { | v | wrapping_add(v, k) }
     fn main() -> XdpAction { let f = adder(1); if f(1) == 2 { XdpAction::Pass } else { XdpAction::Drop } }",
    "effect Tick { fn tick() -> u32; }
     fn main() -> XdpAction {
         let n = handle Tick in { Tick::tick() } with { return value => value, tick(resume) => resume(1), };
         if n == 1 { XdpAction::Pass } else { XdpAction::Drop }
     }",
    "copy struct Pair { a: u32, b: u32 }
     fn sum(p: Pair) -> u32 { wrapping_add(p.a, p.b) }
     fn main() -> XdpAction { let p = Pair { a: 1, b: 2 }; if sum(p) == 3 { XdpAction::Pass } else { XdpAction::Drop } }",
    "effect Clock { fn tick() -> u64; fn reset(a: u32, b: u32, c: u32) -> Unit; }
     fn main() -> XdpAction eff [Clock] { if Clock::tick() == 0 { XdpAction::Pass } else { XdpAction::Drop } }",
    "effect Seen { fn lookup(key: u32) -> Option[u64]; }
     fn main() -> XdpAction eff [Seen] { match Seen::lookup(1) { Some(_) => XdpAction::Drop, None => XdpAction::Pass } }",
    "effect Hits { fn add(key: u32, delta: u8) -> Unit; }
     fn hits_map() -> BpfMapSpec { BpfMapSpec { kind: BpfMapKind::Array, max_entries: 4 } }
     fn main() -> XdpAction eff [Hits] { Hits::add(0, 1); XdpAction::Pass }",
    "fn half(x: i64) -> i64 { x / 2 }
     fn main() -> XdpAction { if half(4) == 2 { XdpAction::Pass } else { XdpAction::Drop } }",
    "fn five(a: u32, b: u32, c: u32, d: u32, e: u32) -> u32 { wrapping_add(a, wrapping_add(b, wrapping_add(c, wrapping_add(d, e)))) }
     fn main() -> XdpAction { if five(1, 2, 3, 4, 5) == 15 { XdpAction::Pass } else { XdpAction::Drop } }",
    "fn has(x: Option[u32]) -> bool { match x { Some(_) => true, None => false } }
     fn main() -> XdpAction { if has(Some(1)) { XdpAction::Pass } else { XdpAction::Drop } }",
    "fn id[T](sink x: T) -> T { x }
     fn main() -> XdpAction { if id(1) == 1 { XdpAction::Pass } else { XdpAction::Drop } }",
    "fn spin(i: u32) -> u32 { if i == 4294967295 { i } else { spin(wrapping_add(i, 0)) } }
     fn main() -> XdpAction { if spin(0) == 0 { XdpAction::Pass } else { XdpAction::Drop } }",
    "fn count(i: u32, acc: u64) -> u64 { if i >= 8 { acc } else { count(wrapping_add(i, 1), wrapping_add(acc, 3)) } }
     fn main() -> XdpAction { if count(0, 0) == 24 { XdpAction::Pass } else { XdpAction::Drop } }",
    "fn main() -> XdpAction eff [Packet] {
         if Packet::load_u8(0) == Packet::load_u8(1) { XdpAction::Pass } else { XdpAction::Drop }
     }",
    "fn main() -> XdpAction eff [Packet] {
         if Packet::load_u8(0) != Packet::load_u8(1) { XdpAction::Pass } else { XdpAction::Drop }
     }",
    "effect Bpf { fn ktime_get_ns() -> u64; fn smp_processor_id() -> u32; fn prandom_u32() -> u32; }
     fn main() -> XdpAction eff [Bpf] {
         let t = Bpf::ktime_get_ns();
         let c = Bpf::smp_processor_id();
         if (t & 1) == 0 && c < 4 && Bpf::prandom_u32() > 7 { XdpAction::Pass } else { XdpAction::Drop }
     }",
    "effect Bpf { fn ktime_get_ns() -> u32; }
     fn main() -> XdpAction eff [Bpf] { if Bpf::ktime_get_ns() == 0 { XdpAction::Pass } else { XdpAction::Drop } }",
    "effect Counts { fn lookup(key: u32) -> Option[u64]; fn update(key: u32, value: u64); fn delete(key: u32); fn add(key: u32, delta: u64); }
     fn counts_map() -> BpfMapSpec { BpfMapSpec { kind: BpfMapKind::Hash, max_entries: 64 } }
     fn main() -> XdpAction eff [Counts] {
         Counts::add(1, 2);
         Counts::update(2, 9);
         Counts::delete(3);
         match Counts::lookup(1) { Some(n) => if n > 5 { XdpAction::Drop } else { XdpAction::Pass }, None => XdpAction::Pass }
     }",
    "effect Per { fn add(key: u32, delta: u64); fn lookup(key: u32) -> Option[u64]; }
     fn per_map() -> BpfMapSpec { BpfMapSpec { kind: BpfMapKind::PerCpuArray, max_entries: 8 } }
     effect Lru { fn update(key: u64, value: u8); }
     fn lru_map() -> BpfMapSpec { BpfMapSpec { kind: BpfMapKind::LruHash, max_entries: 128 } }
     fn main() -> XdpAction eff [Per, Lru] { Per::add(0, 1); Lru::update(7, 1); XdpAction::Pass }",
    "effect Big { fn lookup(key: u32) -> Option[u64]; }
     fn big_map() -> BpfMapSpec { BpfMapSpec { kind: BpfMapKind::Array, max_entries: 0 } }
     fn main() -> XdpAction eff [Big] { match Big::lookup(0) { Some(_) => XdpAction::Drop, None => XdpAction::Pass } }",
    "effect Lookups { fn lookup(key: u32) -> Option[u64]; }
     fn lookups_map() -> BpfMapSpec { BpfMapSpec { kind: BpfMapKind::PerCpuHash, max_entries: 16 } }
     fn main() -> XdpAction eff [Lookups] { match Lookups::lookup(0) { Some(_) => XdpAction::Drop, None => XdpAction::Pass } }",
    "copy enum Color { Red, Green, Blue }
     fn pick(x: u32) -> Color { if x == 0 { Color::Red } else if x == 1 { Color::Green } else { Color::Blue } }
     fn main() -> XdpAction { match pick(2) { Color::Red => XdpAction::Drop, Color::Green => XdpAction::Tx, Color::Blue => XdpAction::Pass } }",
    "fn f(a: u8, b: i64, c: bool) -> i64 { if c { wrapping_add(b, 1) } else { wrapping_sub(b, 1) } }
     fn main() -> XdpAction { if f(1, -3, true) < 0 { XdpAction::Pass } else { XdpAction::Drop } }",
    "fn main() -> XdpAction eff [Packet] {
         let a = match Packet::load_u8(12) { Some(x) => x, None => 0 };
         let b = a ^ 3;
         let c = (b | 4) & 255;
         if c == 7 || !(a > 9) { XdpAction::Pass } else { XdpAction::Aborted }
     }",
    "fn shift(x: u64, n: u64) -> u64 { wrapping_shl(x, n) }
     fn main() -> XdpAction { if shift(1, 3) == 8 { XdpAction::Pass } else { XdpAction::Drop } }",
];

fn add(out: &mut Vec<EbpfCase>, name: String, src: &str, entry: &str) {
    if let Some(world) = checked(src) {
        out.push(EbpfCase {
            name,
            world,
            source: Some(src.to_owned()),
            options: Options {
                entry: entry.to_owned(),
                file: "case.ves".to_owned(),
                ..Options::default()
            },
        });
    }
}

/// Small subset programs from a seeded generator: arithmetic, comparisons,
/// branches, packet loads, calls and tail loops.
fn generated(seed: u64) -> String {
    let mut s = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    let mut next = move |n: u64| {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s % n
    };
    fn expr(next: &mut dyn FnMut(u64) -> u64, vars: &[&str], depth: u32) -> String {
        if depth == 0 || next(3) == 0 {
            return if next(2) == 0 && !vars.is_empty() {
                vars[next(vars.len() as u64) as usize].to_owned()
            } else {
                format!(
                    "{}",
                    [0u64, 1, 7, 255, 4096, 65535, 4294967295][next(7) as usize]
                )
            };
        }
        let a = expr(next, vars, depth - 1);
        let b = expr(next, vars, depth - 1);
        match next(8) {
            0 => format!("wrapping_add({a}, {b})"),
            1 => format!("wrapping_sub({a}, {b})"),
            2 => format!("wrapping_mul({a}, {b})"),
            3 => format!("({a} / 7)"),
            4 => format!("({a} % 13)"),
            5 => format!("(if {a} > {b} && {b} != 3 || {a} == 0 {{ {b} }} else {{ {a} }})"),
            6 => format!("(if {a} < {b} {{ {a} }} else {{ {b} }})"),
            _ => format!("(if {a} == {b} {{ 1 }} else {{ 0 }})"),
        }
    }
    let mut src = String::from(PRELUDE);
    src.push_str(
        "effect Hits { fn add(key: u32, delta: u64); fn lookup(key: u32) -> Option[u64]; }\n",
    );
    src.push_str(
        "fn hits_map() -> BpfMapSpec { BpfMapSpec { kind: BpfMapKind::Array, max_entries: 4 } }\n",
    );
    let ty = ["u32", "u64"][next(2) as usize];
    src.push_str(&format!(
        "fn helper(x: {ty}, y: {ty}) -> {ty} {{ let z = {}; {} }}\n",
        expr(&mut next, &["x", "y"], 3),
        expr(&mut next, &["x", "y", "z"], 3)
    ));
    if next(2) == 0 {
        src.push_str(&format!(
            "fn walk(i: {ty}, acc: {ty}) -> {ty} {{ if i >= {} {{ acc }} else {{ walk(wrapping_add(i, 1), {}) }} }}\n",
            1 + next(6),
            expr(&mut next, &["acc", "i"], 2)
        ));
    } else {
        src.push_str(&format!(
            "fn walk(i: {ty}, acc: {ty}) -> {ty} {{ wrapping_add(i, acc) }}\n"
        ));
    }
    let off = next(64);
    src.push_str(&format!(
        "fn main() -> XdpAction eff [Packet, Hits] {{
             let b = match Packet::load_u8({off}) {{ Some(v) => v, None => 0 }};
             let x: {ty} = {};
             let y = helper(x, {});
             let n = walk(0, y);
             Hits::add({}, 1);
             if n {} {} {{ XdpAction::{} }} else {{ XdpAction::{} }}
         }}\n",
        expr(&mut next, &[], 2),
        expr(&mut next, &["x"], 2),
        next(4),
        ["<", ">", "==", "!=", "<=", ">="][next(6) as usize],
        expr(&mut next, &["x", "y"], 2),
        ["Pass", "Drop", "Tx"][next(3) as usize],
        ["Pass", "Aborted", "Redirect"][next(3) as usize],
    ));
    src
}

/// Every case, built once.
pub fn cases() -> &'static [EbpfCase] {
    static C: OnceLock<Vec<EbpfCase>> = OnceLock::new();
    C.get_or_init(|| {
        let mut out = vec![];
        for name in ["firewall", "counter", "tc_guard"] {
            let src = fixture(name);
            let Some(w) = checked(&src) else {
                panic!("{name} does not check");
            };
            for f in w.functions.keys() {
                add(&mut out, format!("{name}:{f}"), &src, f);
            }
            for cut in (0..src.len()).step_by(29) {
                if src.is_char_boundary(cut) {
                    add(&mut out, format!("{name}[..{cut}]"), &src[..cut], "main");
                }
            }
            for (from, to) in [
                ("2048", "18446744073709551615"),
                ("23", "0"),
                ("1024", "0"),
                ("10", "4294967295"),
                ("XdpAction::Pass", "XdpAction::Tx"),
                ("Hash", "LruHash"),
            ] {
                add(
                    &mut out,
                    format!("{name}/{from}->{to}"),
                    &src.replace(from, to),
                    "main",
                );
            }
        }
        for (k, body) in BODIES.iter().enumerate() {
            add(&mut out, format!("body {k}"), &format!("{PRELUDE}{body}"), "main");
        }
        add(
            &mut out,
            "wrong action order".into(),
            "copy enum XdpAction { Drop, Aborted, Pass, Tx, Redirect }
             fn main() -> XdpAction { XdpAction::Pass }",
            "main",
        );
        let mut big = String::from(PRELUDE);
        big.push_str("fn big(x: u64) -> u64 {\n");
        for k in 0..70 {
            big.push_str(&format!("    let v{k} = wrapping_add(x, {k});\n"));
        }
        big.push_str("    let s0 = v0;\n");
        for k in 1..70 {
            big.push_str(&format!("    let s{k} = wrapping_add(s{}, v{k});\n", k - 1));
        }
        big.push_str("    s69\n}\nfn main() -> XdpAction { if big(1) == 0 { XdpAction::Drop } else { XdpAction::Pass } }");
        add(&mut out, "big frame".into(), &big, "main");
        for seed in 0..generated_count() {
            add(&mut out, format!("generated {seed}"), &generated(seed), "main");
        }
        out
    })
}

/// Whether the differential tests compile the entry optimized
/// (`EBPF_BACKEND_OPT=1`). Off by default: `ir_opt::optimize` runs one
/// egglog program per block, which takes about 7.5 minutes for this
/// package in a debug build (10 s unoptimized); see the lane report.
fn optimize() -> bool {
    std::env::var("EBPF_BACKEND_OPT").is_ok_and(|v| v == "1")
}

fn generated_count() -> u64 {
    std::env::var("EBPF_BACKEND_GENERATED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(200)
}

// ------------------------------------------------------------------ tests

/// The Rust result, or `None` when the Rust backend panics on the case.
fn rust(
    w: &World,
    source: Option<&str>,
    options: &Options,
) -> Option<Result<Compiled, EbpfBackendError>> {
    let quiet = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let r = std::panic::catch_unwind(|| compile_world(w, source, options)).ok();
    std::panic::set_hook(quiet);
    r
}

fn text(r: &Result<Compiled, EbpfBackendError>) -> String {
    match r {
        Ok(c) => format!("{c:#?}\nobject {:?}", c.object()),
        Err(e) => format!("{e:#?}\n{e}"),
    }
}

/// Runs both sides over `cases`, returning (differing, compared, accepted,
/// skipped).
fn compare<'a>(
    label: &str,
    cases: impl Iterator<Item = (String, &'a World, Option<&'a str>, &'a Options)>,
) -> (usize, usize, usize, usize) {
    let t = std::time::Instant::now();
    let p = package();
    eprintln!("{label}: package checked in {:?}", t.elapsed());
    let t = std::time::Instant::now();
    let entry = p.entry("ebpf_backend_words", optimize());
    eprintln!("{label}: entry compiled in {:?}", t.elapsed());
    let t = std::time::Instant::now();
    let (mut bad, mut n, mut ok, mut skipped) = (0, 0, 0, 0);
    for (name, w, source, options) in cases {
        let Some(r) = rust(w, source, options) else {
            skipped += 1;
            continue;
        };
        let Some(input) = input(w, source, options) else {
            skipped += 1;
            continue;
        };
        let v = decode(&entry.words(&input), options);
        n += 1;
        ok += r.is_ok() as usize;
        if std::env::var_os("EBPF_BACKEND_VERBOSE").is_some() {
            match &r {
                Ok(_) => eprintln!("{name}: ok"),
                Err(e) => eprintln!("{name}: {e}"),
            }
        }
        if let Some(d) = first_difference(&text(&r), &text(&v)) {
            bad += 1;
            if bad <= 8 {
                eprintln!("{label}: {name} differs at {d}");
            }
        }
    }
    eprintln!(
        "{label}: {bad} of {n} differ ({ok} accepted by Rust), {skipped} skipped, in {:?}",
        t.elapsed()
    );
    (bad, n, ok, skipped)
}

#[test]
fn programs_and_errors_match_rust() {
    let (bad, n, ok, _) = compare(
        "cases",
        cases()
            .iter()
            .map(|c| (c.name.clone(), &c.world, c.source.as_deref(), &c.options)),
    );
    assert!(n > 100, "only {n} cases");
    assert!(ok > 50, "only {ok} accepted");
    assert_eq!(bad, 0);
}

#[test]
fn corpus_worlds_match_rust() {
    let options = Options::default();
    let (bad, n, _, _) = compare(
        "corpus",
        corpus()
            .iter()
            .map(|c| (c.name.clone(), &c.world, None, &options)),
    );
    assert!(n > 50, "only {n} worlds");
    assert_eq!(bad, 0);
}

/// The backend's time against Rust's: the Rust `Surface::analyze` +
/// `lower` + verify (with `ir::lower_world`, which it calls), and the
/// Vesper entry on the encoded input plus decoding (which verifies).
#[test]
#[ignore = "timing; CHECK_HARNESS_SPEED=1"]
fn ebpf_backend_speed() {
    if !speed() {
        return;
    }
    let entry = package().entry("ebpf_backend_words", true);
    let accepted: Vec<&EbpfCase> = cases()
        .iter()
        .filter(|c| matches!(rust(&c.world, c.source.as_deref(), &c.options), Some(Ok(_))))
        .collect();
    let inputs: Vec<Vec<u8>> = accepted
        .iter()
        .map(|c| input(&c.world, c.source.as_deref(), &c.options).unwrap())
        .collect();
    let reps = 20;
    abba(
        &format!(
            "{} accepted programs x {reps}, encoded input",
            accepted.len()
        ),
        &mut || {
            time(|| {
                for _ in 0..reps {
                    for c in &accepted {
                        std::hint::black_box(
                            compile_world(&c.world, c.source.as_deref(), &c.options).is_ok(),
                        );
                    }
                }
            })
        },
        &mut || {
            time(|| {
                for _ in 0..reps {
                    for (c, i) in accepted.iter().zip(&inputs) {
                        std::hint::black_box(decode(&entry.words(i), &c.options).is_ok());
                    }
                }
            })
        },
    );
    abba(
        &format!(
            "{} accepted programs x {reps}, with encoding",
            accepted.len()
        ),
        &mut || {
            time(|| {
                for _ in 0..reps {
                    for c in &accepted {
                        std::hint::black_box(
                            compile_world(&c.world, c.source.as_deref(), &c.options).is_ok(),
                        );
                    }
                }
            })
        },
        &mut || {
            time(|| {
                for _ in 0..reps {
                    for c in &accepted {
                        let i = input(&c.world, c.source.as_deref(), &c.options).unwrap();
                        std::hint::black_box(decode(&entry.words(&i), &c.options).is_ok());
                    }
                }
            })
        },
    );
    // The parts: `compile_world` is `ir::lower_world` + the backend +
    // the verifier model, so the Rust backend alone is the total minus the
    // other two; the Vesper backend alone is the entry on encoded input.
    let programs: Vec<Program> = accepted
        .iter()
        .map(|c| {
            compile_world(&c.world, c.source.as_deref(), &c.options)
                .unwrap()
                .program
        })
        .collect();
    let best = |f: &mut dyn FnMut()| (0..6).map(|_| time(&mut *f)).min().unwrap();
    let total = best(&mut || {
        for c in &accepted {
            std::hint::black_box(compile_world(&c.world, c.source.as_deref(), &c.options).is_ok());
        }
    });
    let lower = best(&mut || {
        for c in &accepted {
            std::hint::black_box(vesper::ir::lower_world(&c.world));
        }
    });
    let verify = best(&mut || {
        for (c, p) in accepted.iter().zip(&programs) {
            std::hint::black_box(vesper_ebpf::verify(p, &c.options.policy).is_ok());
        }
    });
    let encode = best(&mut || {
        for c in &accepted {
            std::hint::black_box(input(&c.world, c.source.as_deref(), &c.options));
        }
    });
    let outputs: Vec<Vec<u64>> = inputs.iter().map(|i| entry.words(i)).collect();
    let vesper = best(&mut || {
        for i in &inputs {
            std::hint::black_box(entry.words(i));
        }
    });
    let decode = best(&mut || {
        for (c, o) in accepted.iter().zip(&outputs) {
            std::hint::black_box(decode(o, &c.options).is_ok());
        }
    });
    let rust_backend = total.saturating_sub(lower).saturating_sub(verify);
    eprintln!(
        "parts over {} programs: compile_world {total:?} = lower_world {lower:?} + backend \
         {rust_backend:?} + verify {verify:?}; Vesper backend {vesper:?} ({:.2}x of the Rust \
         backend), encode {encode:?} (with lower_world), decode + verify {decode:?}",
        accepted.len(),
        vesper.as_secs_f64() / rust_backend.as_secs_f64()
    );
}
