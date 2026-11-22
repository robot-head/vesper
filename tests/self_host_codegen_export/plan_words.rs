//! The Rust half of `@vesper/codegen-export`'s interface planner (SH4 lane
//! 12, #1286): `plan_with`'s inputs as little-endian `u64` words, which
//! `plan_words` in packages/vesper-codegen-export/src/plan.ves reads, and
//! the planner's answer read back as a [`LibraryInterface`] or the
//! refusals.
//!
//! The planner decides in Vesper; the checker's answers about each type
//! (its class, mode, layout and declaration) come with the input, since
//! they belong to the checker and layout ports.
//!
//! A string is its byte length, then the bytes zero padded to a word; an
//! optional string is 0 for none, else 1 and the string. Input, in order:
//!
//! - library name, the `.vsys` effect prefix, abi (0 none), option_mirrors;
//! - `.vsys` imports: count; library, handler, symbol count and symbols,
//!   optional rust_path;
//! - instances: count; function, alias, spelling;
//! - types: count, then per type (an index into this table) its display,
//!   class (`C_*`), name, flag, mode (0 copy, 1 affine, 2 linear), part
//!   count and parts; the declaration (0, or 1 then generics, tiled,
//!   is_enum, has a `.vsys` handle field, variant count and names); the
//!   layout (0 then size, aggregate, kind (0 scalar, 1 product, 2 enum),
//!   variant count, per variant name and field count, per field name,
//!   type and byte offset; or 1 then the reason);
//! - functions (`world.functions` order): count; name, generated, public,
//!   has generics, dyn (0, or 1 then the parameter and the trait), optional
//!   row tail, effect count, per effect name, labeled, argument count and
//!   types; caller slot, parameter count, per parameter name, pass (0 in, 1
//!   inout, 2 sink) and type; result type.
use std::collections::{BTreeMap, BTreeSet};
use vesper::ast::{Mode, Pass, Type};
use vesper::check::World;
use vesper::layout::{Layout, LayoutField, LayoutKind};
use vesper::native::export::{
    AbiType, Directives, ExportError, ExportParam, ExportedFunction, FieldDecl, ForeignLibrary,
    LibraryInterface, Mirror, RuntimeSymbols, TypeDecl, VariantDecl,
};

pub const C_OTHER: u64 = 0;
pub const C_TUPLE: u64 = 1;
pub const C_SCALAR: u64 = 2;
pub const C_WIDE: u64 = 3;
pub const C_SEQ: u64 = 4;
pub const C_SLICE: u64 = 5;
pub const C_STRING: u64 = 6;
pub const C_STR: u64 = 7;
pub const C_BOX: u64 = 8;
pub const C_NAMED: u64 = 9;
pub const C_EXTERN: u64 = 10;

struct Enc<'a> {
    world: &'a World,
    out: Vec<u64>,
    ids: BTreeMap<String, u64>,
    // Per type, in index order: its words, finished once all its parts have
    // indices.
    rows: Vec<Vec<u64>>,
}

fn string(out: &mut Vec<u64>, s: &str) {
    let b = s.as_bytes();
    out.push(b.len() as u64);
    for c in b.chunks(8) {
        let mut w = [0u8; 8];
        w[..c.len()].copy_from_slice(c);
        out.push(u64::from_le_bytes(w));
    }
}

fn opt(out: &mut Vec<u64>, s: Option<&str>) {
    match s {
        None => out.push(0),
        Some(s) => {
            out.push(1);
            string(out, s);
        }
    }
}

fn is_extern_handle(world: &World, ty: &Type) -> bool {
    matches!(ty, Type::Named(n, args) if args.is_empty()
        && world.adts.get(n).is_some_and(|a| a.variants.iter().any(|v| v.fields.iter()
            .any(|f| f.name == vesper::ffi_lowering::HANDLE_FIELD))))
}

impl Enc<'_> {
    /// The index of `ty`, encoding it (and what it reaches) on first use.
    fn ty(&mut self, ty: &Type) -> u64 {
        // `Debug` tells apart types that display alike.
        let key = format!("{ty:?}");
        if let Some(i) = self.ids.get(&key) {
            return *i;
        }
        let i = self.rows.len() as u64;
        self.ids.insert(key, i);
        self.rows.push(vec![]);
        let world = self.world;
        let (class, name, flag, parts): (u64, &str, u64, Vec<Type>) = match ty {
            _ if is_extern_handle(world, ty) => {
                let Type::Named(n, _) = ty else {
                    unreachable!()
                };
                (C_EXTERN, n, 0, vec![])
            }
            Type::Named(n, a) if a.is_empty() && vesper::numeric::is_wide_name(n) => {
                (C_WIDE, n, 0, vec![])
            }
            Type::Tuple(items) => (C_TUPLE, "", 0, items.clone()),
            Type::Named(n, a)
                if a.is_empty() && vesper::numeric::is_scalar_name(n) && n != "Never" =>
            {
                (C_SCALAR, n, 0, vec![])
            }
            t if vesper::seq::is_seq_or_slice(t) => {
                let elem = vesper::seq::elem(t).expect("sequence element").clone();
                let c = if vesper::seq::is_seq_type(t) {
                    C_SEQ
                } else {
                    C_SLICE
                };
                (c, "", 0, vec![elem])
            }
            Type::Named(n, a) if a.is_empty() && n == vesper::strings::STRING_TYPE => {
                (C_STRING, "", 0, vec![])
            }
            Type::Named(n, a) if a.is_empty() && n == vesper::strings::STR_TYPE => {
                (C_STR, "", 0, vec![])
            }
            t if vesper::fip::is_box_type(world, t) => {
                let inner = vesper::fip::box_inner(world, t).expect("box inner").clone();
                let object = vesper::dyn_trait::is_object(&inner);
                (C_BOX, "", object as u64, vec![inner])
            }
            Type::Named(n, a) => (C_NAMED, n, 0, a.clone()),
            _ => (C_OTHER, "", 0, vec![]),
        };
        let name = name.to_string();
        let ids: Vec<u64> = if class == C_BOX && flag == 1 {
            // A boxed object is opaque: only its display is used.
            vec![]
        } else {
            parts.iter().map(|p| self.ty(p)).collect()
        };
        let mut row = vec![];
        string(&mut row, &ty.to_string());
        row.push(class);
        string(&mut row, &name);
        row.push(flag);
        if class == C_BOX && flag == 1 {
            string(&mut row, &parts[0].to_string());
        }
        let mode = if class == C_OTHER {
            0
        } else {
            match world.mode(ty, &BTreeMap::new()) {
                Mode::Copy => 0,
                Mode::Affine => 1,
                Mode::Linear => 2,
            }
        };
        row.push(mode);
        row.push(ids.len() as u64);
        row.extend(&ids);
        match (class, world.adts.get(&name)) {
            (C_NAMED, Some(adt)) => {
                row.extend([
                    1,
                    adt.generics.len() as u64,
                    adt.tile_width.is_some() as u64,
                    adt.is_enum as u64,
                    adt.variants.len() as u64,
                ]);
                for v in &adt.variants {
                    string(&mut row, &v.name);
                }
            }
            _ => row.push(0),
        }
        if class == C_OTHER {
            row.push(1);
            string(&mut row, "");
        } else {
            match Layout::of(world, ty) {
                Ok(l) => {
                    row.extend([0, l.size as u64, l.aggregate() as u64]);
                    let variants: Vec<(&str, &[LayoutField])> = match &l.kind {
                        LayoutKind::Scalar => vec![],
                        LayoutKind::Product(fs) => vec![("", fs.as_slice())],
                        LayoutKind::Enum(vs) => vs
                            .iter()
                            .map(|v| (v.name.as_str(), v.fields.as_slice()))
                            .collect(),
                    };
                    let kind = match &l.kind {
                        LayoutKind::Scalar => 0,
                        LayoutKind::Product(_) => 1,
                        LayoutKind::Enum(_) => 2,
                    };
                    row.push(kind);
                    row.push(variants.len() as u64);
                    for (vname, fields) in variants {
                        string(&mut row, vname);
                        row.push(fields.len() as u64);
                        for f in fields {
                            string(&mut row, &f.name);
                            let t = self.ty(&f.ty);
                            row.push(t);
                            row.push(f.offset as u64);
                        }
                    }
                }
                Err(reason) => {
                    row.push(1);
                    string(&mut row, &reason);
                }
            }
        }
        self.rows[i as usize] = row;
        i
    }
}

/// The input of `plan_words`: what `plan_with(world, public, library,
/// directives)` reads.
pub fn plan_input(
    world: &World,
    public: &BTreeSet<String>,
    library: &str,
    directives: &Directives,
) -> Vec<u8> {
    let mut e = Enc {
        world,
        out: vec![],
        ids: BTreeMap::new(),
        rows: vec![],
    };
    let mut head = vec![];
    string(&mut head, library);
    string(&mut head, &vesper::ffi_lowering::effect_name(""));
    head.push(directives.abi.map_or(0, u64::from));
    head.push(directives.option_mirrors as u64);
    head.push(directives.vsys.len() as u64);
    for v in &directives.vsys {
        string(&mut head, &v.library);
        string(&mut head, v.handler.name());
        head.push(v.symbols.len() as u64);
        for s in &v.symbols {
            string(&mut head, s);
        }
        opt(&mut head, v.rust_path.as_deref());
    }
    head.push(directives.instances.len() as u64);
    for i in &directives.instances {
        string(&mut head, &i.function);
        string(&mut head, &i.alias());
        string(&mut head, &i.spelling());
    }
    let mut funcs = vec![world.functions.len() as u64];
    for (name, f) in &world.functions {
        string(&mut funcs, name);
        funcs.push(world.elaboration.generated.contains_key(name) as u64);
        funcs.push(public.contains(name) as u64);
        funcs.push(!f.generics.is_empty() as u64);
        match world
            .dyn_objects
            .params
            .get(name)
            .and_then(|p| p.iter().next())
        {
            None => funcs.push(0),
            Some((g, tr)) => {
                funcs.push(1);
                let param = f
                    .params
                    .iter()
                    .find(|p| p.ty.name() == g)
                    .map_or("?", |p| p.name.as_str());
                string(&mut funcs, param);
                string(&mut funcs, &tr.to_string());
            }
        }
        opt(&mut funcs, f.effects.tail.as_deref());
        funcs.push(f.effects.instances.len() as u64);
        for ef in &f.effects.instances {
            string(&mut funcs, &ef.name);
            funcs.push(ef.label.is_some() as u64);
            funcs.push(ef.args.len() as u64);
            for a in &ef.args {
                let t = e.ty(a);
                funcs.push(t);
            }
        }
        funcs.push(vesper::fip::uses_caller_slot(world, f) as u64);
        funcs.push(f.params.len() as u64);
        for p in &f.params {
            string(&mut funcs, &p.name);
            funcs.push(match p.pass {
                Pass::In => 0,
                Pass::Inout => 1,
                Pass::Sink => 2,
            });
            let t = e.ty(&p.ty);
            funcs.push(t);
        }
        let t = e.ty(&f.result);
        funcs.push(t);
    }
    e.out.extend(head);
    e.out.push(e.rows.len() as u64);
    for r in &e.rows {
        e.out.extend(r);
    }
    e.out.extend(funcs);
    let mut bytes = Vec::with_capacity(e.out.len() * 8);
    for x in &e.out {
        bytes.extend(x.to_le_bytes());
    }
    bytes
}

/// Reads the planner's answer (plan.ves `write_plan`).
struct Dec<'a> {
    w: &'a [u64],
    pos: usize,
}

impl Dec<'_> {
    fn word(&mut self) -> u64 {
        let x = self.w.get(self.pos).copied().unwrap_or(0);
        self.pos += 1;
        x
    }
    fn flag(&mut self) -> bool {
        self.word() != 0
    }
    fn n(&mut self) -> u32 {
        self.word() as u32
    }
    fn string(&mut self) -> String {
        let len = self.word() as usize;
        let mut b = Vec::with_capacity(len);
        for _ in 0..len.div_ceil(8) {
            b.extend(self.word().to_le_bytes());
        }
        b.truncate(len);
        String::from_utf8_lossy(&b).into_owned()
    }
    fn opt(&mut self) -> Option<String> {
        self.flag().then(|| self.string())
    }
    fn list<T>(&mut self, mut f: impl FnMut(&mut Self) -> T) -> Vec<T> {
        let n = self.word();
        (0..n).map(|_| f(self)).collect()
    }
    fn ty(&mut self) -> AbiType {
        match self.word() {
            0 => AbiType::Scalar {
                name: self.string(),
            },
            1 => AbiType::Tuple {
                items: self.list(Self::ty),
            },
            2 => AbiType::Named {
                name: self.string(),
                args: self.list(Self::ty),
            },
            3 => AbiType::Seq {
                elem: Box::new(self.ty()),
            },
            4 => AbiType::Slice {
                elem: Box::new(self.ty()),
            },
            5 => AbiType::Box {
                inner: Box::new(self.ty()),
            },
            6 => AbiType::Str { owned: self.flag() },
            7 => AbiType::Object {
                name: self.string(),
            },
            8 => AbiType::Extern {
                name: self.string(),
            },
            k => panic!("plan words: type kind {k}"),
        }
    }
    fn error(&mut self) -> ExportError {
        match self.word() {
            0 => ExportError::LibraryName(self.string()),
            1 => ExportError::NoExports,
            2 => ExportError::Generic {
                function: self.string(),
            },
            3 => ExportError::Effect {
                function: self.string(),
                effect: self.string(),
            },
            4 => ExportError::Type {
                function: self.string(),
                ty: self.string(),
                reason: self.string(),
            },
            5 => ExportError::Instance {
                instance: self.string(),
                reason: self.string(),
            },
            6 => ExportError::Compile(self.string()),
            k => panic!("plan words: error kind {k}"),
        }
    }
    fn interface(&mut self) -> LibraryInterface {
        let abi = self.string();
        let library = self.string();
        let option_mirrors = self.flag();
        let runtime = self.flag().then(|| RuntimeSymbols {
            seq_new: self.string(),
            seq_free: self.string(),
            live_allocations: self.string(),
            box_new: self.opt(),
        });
        let types = self
            .list(|d| {
                let name = d.string();
                let is_enum = d.flag();
                let words = d.n();
                let handle = d.flag();
                let linear = d.flag();
                let drop_symbol = d.opt();
                let owns_sequence = d.flag();
                let mirror = match d.word() {
                    0 => None,
                    1 => Some(Mirror::ReprC),
                    _ => Some(Mirror::Converted { reason: d.string() }),
                };
                let variants = d.list(|d| VariantDecl {
                    name: d.string(),
                    fields: d.list(|d| FieldDecl {
                        name: d.string(),
                        ty: d.ty(),
                        offset: d.n(),
                    }),
                });
                TypeDecl {
                    name,
                    is_enum,
                    words,
                    variants,
                    handle,
                    linear,
                    drop_symbol,
                    owns_sequence,
                    mirror,
                }
            })
            .into_iter()
            .map(|d| (d.name.clone(), d))
            .collect();
        let functions = self.list(|d| {
            let name = d.string();
            let symbol = d.string();
            let params = d.list(|d| ExportParam {
                name: d.string(),
                ty: d.ty(),
                pointer: d.flag(),
                pass: d.opt(),
                in_place: d.flag(),
            });
            let result = d.ty();
            let result_words = d.n();
            let result_pointer = d.flag();
            let errors = d.list(Self::ty);
            let fail = d.word();
            ExportedFunction {
                name,
                symbol,
                params,
                result,
                result_words,
                result_pointer,
                errors,
                fail_record_words: (fail != 0).then(|| fail as u32 - 1),
                instance_of: d.opt(),
                foreign: d.list(Self::string),
                result_in_place: d.flag(),
            }
        });
        let foreign = self.list(|d| ForeignLibrary {
            name: d.string(),
            handler: d.string(),
            symbols: d.list(Self::string),
            rust_path: d.opt(),
        });
        LibraryInterface {
            abi,
            library,
            types,
            functions,
            runtime,
            foreign,
            option_mirrors,
        }
    }
}

/// The planner's answer from its words.
pub fn decode_plan(words: &[u64]) -> Result<LibraryInterface, Vec<ExportError>> {
    let mut d = Dec { w: words, pos: 0 };
    if d.word() == 0 {
        Ok(d.interface())
    } else {
        Err(d.list(Dec::error))
    }
}
