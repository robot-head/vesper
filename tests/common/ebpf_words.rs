//! eBPF backend as words, both directions (SH4 lane 16, #1290).
//!
//! Used by `tests/self_host_ebpf_backend.rs` and `vesper-hybrid`'s
//! `hybrid_ebpf_backend` bridge (`ebpf_backend_bytes`).
#![allow(dead_code)]

use super::check_words::{mode_word, pass_word};
use super::ir_words;
use vesper::ast::{Effects, Type};
use vesper::check::World;
use vesper::ebpf_backend::{ActionKind, Compiled, EbpfBackendError, MapBinding, Options, rules};
use vesper_ebpf::{LineInfo, MapDef, MapKind, Program, ProgramType, RawInsn};

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
