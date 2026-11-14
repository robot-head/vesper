//! The component boundary: a WIT world for the entry, canonical-ABI export
//! wrappers in the core module, and `wit-component` encoding.
//!
//! Entry parameters are scalars. Results may be scalars, tuples, structs
//! (WIT `record`) and enums (WIT `variant`, one per instantiation, such as
//! `option-u8`); recursive and generic-opaque types are rejected.
use super::{
    Unsupported, WasmError, WasmWorld,
    layout::{self, Scalar, Shape},
    lower::Lowerer,
};
use crate::ast::*;
use std::collections::{BTreeMap, BTreeSet};
use wasm_encoder::{BlockType, Function, Instruction as I, MemArg, ValType};

pub(crate) const PACKAGE: &str = "vesper:program@0.1.0";
pub const ENTRY_INTERFACE: &str = "vesper:program/entry@0.1.0";
pub const WASI_RUN: &str = "wasi:cli/run@0.2.0";
const WASI_CLI_WIT: &str =
    "package wasi:cli@0.2.0;\n\ninterface run {\n  run: func() -> result;\n}\n";

/// A WIT identifier (kebab-case, `%`-escaped) for a Vesper name.
pub(crate) fn ident(name: &str) -> String {
    let mut words: Vec<String> = vec![];
    let mut current = String::new();
    let mut prev_lower = false;
    for c in name.chars() {
        if c == '_' || c == '-' || c == ':' || !c.is_ascii_alphanumeric() {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            prev_lower = false;
            continue;
        }
        if c.is_ascii_uppercase() && prev_lower && !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
        prev_lower = c.is_ascii_lowercase() || c.is_ascii_digit();
        current.push(c.to_ascii_lowercase());
    }
    if !current.is_empty() {
        words.push(current);
    }
    let mut merged: Vec<String> = vec![];
    for w in words {
        if w.starts_with(|c: char| c.is_ascii_digit()) {
            match merged.last_mut() {
                Some(last) => last.push_str(&w),
                None => merged.push(format!("n{w}")),
            }
        } else {
            merged.push(w);
        }
    }
    if merged.is_empty() {
        merged.push("unnamed".into());
    }
    merged.join("-")
}

fn mangle(ty: &Type) -> String {
    match ty {
        Type::Named(n, args) => {
            let mut s = ident(n);
            for a in args {
                s.push('-');
                s.push_str(&mangle(a));
            }
            s
        }
        Type::Tuple(ts) => {
            let mut s = "tuple".to_string();
            for t in ts {
                s.push('-');
                s.push_str(&mangle(t));
            }
            s
        }
        _ => "value".into(),
    }
}

/// WIT type expressions and the named definitions they need.
#[derive(Default)]
pub(crate) struct Wit {
    pub defs: Vec<String>,
    names: BTreeMap<String, String>,
    active: BTreeSet<String>,
}

fn export_err(detail: impl Into<String>) -> WasmError {
    WasmError::unsupported(Unsupported::ExportType, "", detail)
}

impl Wit {
    pub(crate) fn ty(
        &mut self,
        world: &crate::check::World,
        ty: &Type,
    ) -> Result<String, WasmError> {
        let ty = crate::numeric::default_type(ty);
        let key = ty.to_string();
        if let Some(n) = self.names.get(&key) {
            return Ok(format!("%{n}"));
        }
        if !self.active.insert(key.clone()) || self.active.len() > 32 {
            return Err(export_err(format!(
                "recursive or too deeply nested type {ty}"
            )));
        }
        let out = match layout::shape(world, &ty)? {
            Shape::Scalar(s) => match s {
                Scalar::I64 => "s64".into(),
                Scalar::U64 => "u64".into(),
                Scalar::U32 => "u32".into(),
                Scalar::U8 => "u8".into(),
                Scalar::Bool => "bool".into(),
                Scalar::F64 => "f64".into(),
                Scalar::Unit | Scalar::Never => {
                    return Err(export_err(format!("{ty} inside an exported aggregate")));
                }
            },
            Shape::Tuple(ts) => {
                let parts = ts
                    .iter()
                    .map(|t| self.ty(world, t))
                    .collect::<Result<Vec<_>, _>>()?;
                format!("tuple<{}>", parts.join(", "))
            }
            Shape::Struct { fields, .. } => {
                if fields.is_empty() {
                    return Err(export_err(format!("empty struct {ty}")));
                }
                let name = mangle(&ty);
                let body = self.fields(world, &fields)?;
                self.defs.push(format!("record %{name} {{ {body} }}"));
                self.names.insert(key.clone(), name.clone());
                format!("%{name}")
            }
            Shape::Enum { variants, .. } => {
                let name = mangle(&ty);
                let mut cases = vec![];
                for v in &variants {
                    let case = ident(&v.name);
                    if v.fields.is_empty() {
                        cases.push(format!("%{case}"));
                    } else if v.named {
                        let rec = format!("{name}-{case}");
                        let body = self.fields(world, &v.fields)?;
                        self.defs.push(format!("record %{rec} {{ {body} }}"));
                        cases.push(format!("%{case}(%{rec})"));
                    } else if v.fields.len() == 1 {
                        cases.push(format!("%{case}({})", self.ty(world, &v.fields[0].1)?));
                    } else {
                        let parts = v
                            .fields
                            .iter()
                            .map(|(_, t)| self.ty(world, t))
                            .collect::<Result<Vec<_>, _>>()?;
                        cases.push(format!("%{case}(tuple<{}>)", parts.join(", ")));
                    }
                }
                self.defs
                    .push(format!("variant %{name} {{ {} }}", cases.join(", ")));
                self.names.insert(key.clone(), name.clone());
                format!("%{name}")
            }
            Shape::Opaque | Shape::Function => {
                return Err(export_err(format!("{ty} has no WIT type")));
            }
        };
        self.active.remove(&key);
        Ok(out)
    }
    fn fields(
        &mut self,
        world: &crate::check::World,
        fields: &[(String, Type)],
    ) -> Result<String, WasmError> {
        let mut seen = BTreeSet::new();
        let mut parts = vec![];
        for (n, t) in fields {
            let f = ident(n);
            if !seen.insert(f.clone()) {
                return Err(export_err(format!("field names collide as '{f}'")));
            }
            parts.push(format!("%{f}: {}", self.ty(world, t)?));
        }
        Ok(parts.join(", "))
    }
}

// ---- canonical ABI -------------------------------------------------------

fn align_to(x: u64, a: u64) -> u64 {
    x.div_ceil(a) * a
}

/// Canonical ABI (size, alignment) of the WIT image of `ty`.
fn size_align(world: &crate::check::World, ty: &Type) -> Result<(u64, u64), WasmError> {
    Ok(match layout::shape(world, ty)? {
        Shape::Scalar(Scalar::Bool | Scalar::U8) => (1, 1),
        Shape::Scalar(Scalar::U32) => (4, 4),
        Shape::Scalar(Scalar::I64 | Scalar::U64 | Scalar::F64) => (8, 8),
        Shape::Tuple(ts) => record(world, &ts)?.0,
        Shape::Struct { fields, .. } => {
            record(
                world,
                &fields.into_iter().map(|(_, t)| t).collect::<Vec<_>>(),
            )?
            .0
        }
        Shape::Enum { variants, .. } => {
            let (disc, _) = discriminant(variants.len());
            let mut size = 0;
            let mut align = disc;
            for v in &variants {
                let ((s, a), _) = record(
                    world,
                    &v.fields.iter().map(|(_, t)| t.clone()).collect::<Vec<_>>(),
                )?;
                size = size.max(s);
                align = align.max(a);
            }
            let payload = align_to(disc, align);
            (align_to(payload + size, align), align)
        }
        _ => return Err(export_err(format!("{ty} has no canonical layout"))),
    })
}
/// ((size, align), field offsets) of a record of `ts`.
fn record(world: &crate::check::World, ts: &[Type]) -> Result<((u64, u64), Vec<u64>), WasmError> {
    let mut offset = 0;
    let mut align = 1;
    let mut offsets = vec![];
    for t in ts {
        let (s, a) = size_align(world, t)?;
        offset = align_to(offset, a);
        offsets.push(offset);
        offset += s;
        align = align.max(a);
    }
    Ok(((align_to(offset, align), align), offsets))
}
fn discriminant(cases: usize) -> (u64, u32) {
    if cases <= 256 {
        (1, 0)
    } else if cases <= 65536 {
        (2, 1)
    } else {
        (4, 2)
    }
}
fn flat_count(world: &crate::check::World, ty: &Type) -> Result<usize, WasmError> {
    Ok(match layout::shape(world, ty)? {
        Shape::Scalar(_) => 1,
        Shape::Tuple(ts) => ts
            .iter()
            .map(|t| flat_count(world, t))
            .sum::<Result<_, _>>()?,
        Shape::Struct { fields, .. } => fields
            .iter()
            .map(|(_, t)| flat_count(world, t))
            .sum::<Result<_, _>>()?,
        Shape::Enum { variants, .. } => {
            let mut m = 0;
            for v in &variants {
                m = m.max(
                    v.fields
                        .iter()
                        .map(|(_, t)| flat_count(world, t))
                        .sum::<Result<usize, _>>()?,
                );
            }
            1 + m
        }
        _ => return Err(export_err(format!("{ty} has no flat lowering"))),
    })
}

fn m(offset: u64, align: u32) -> MemArg {
    MemArg {
        offset,
        align,
        memory_index: 0,
    }
}

/// Emit stores of uniform value `v` (local) of type `ty` into the canonical
/// layout at `base + offset` (`base` is an i64 local).
#[allow(clippy::too_many_arguments)]
fn store(
    l: &Lowerer<'_>,
    code: &mut Vec<I<'static>>,
    locals: &mut Vec<ValType>,
    nparams: u32,
    base: u32,
    offset: u64,
    v: u32,
    ty: &Type,
) -> Result<(), WasmError> {
    let world = l.world;
    let temp = |locals: &mut Vec<ValType>| {
        locals.push(ValType::I64);
        nparams + locals.len() as u32 - 1
    };
    let field = |code: &mut Vec<I<'static>>, from: u32, slot: u64, to: u32| {
        code.extend([
            I::LocalGet(from),
            I::I32WrapI64,
            I::I64Load(m(slot, 3)),
            I::LocalSet(to),
        ]);
    };
    match layout::shape(world, ty)? {
        Shape::Scalar(s) => {
            code.extend([I::LocalGet(base), I::I32WrapI64, I::LocalGet(v)]);
            code.push(match s {
                Scalar::Bool | Scalar::U8 => I::I64Store8(m(offset, 0)),
                Scalar::U32 => I::I64Store32(m(offset, 2)),
                _ => I::I64Store(m(offset, 3)),
            });
        }
        Shape::Tuple(ts) => {
            let (_, offs) = record(world, &ts)?;
            for (i, t) in ts.iter().enumerate() {
                let x = temp(locals);
                field(code, v, 8 * i as u64, x);
                store(l, code, locals, nparams, base, offset + offs[i], x, t)?;
            }
        }
        Shape::Struct { fields, .. } => {
            let ts: Vec<Type> = fields.iter().map(|(_, t)| t.clone()).collect();
            let (_, offs) = record(world, &ts)?;
            for (i, t) in ts.iter().enumerate() {
                let x = temp(locals);
                field(code, v, 8 * i as u64, x);
                store(l, code, locals, nparams, base, offset + offs[i], x, t)?;
            }
        }
        Shape::Enum { variants, .. } => {
            let (_, align) = size_align(world, ty)?;
            let (disc, disc_align) = discriminant(variants.len());
            let payload = align_to(disc, align);
            let tag = temp(locals);
            field(code, v, 0, tag);
            code.extend([I::LocalGet(base), I::I32WrapI64, I::LocalGet(tag)]);
            code.push(match disc {
                1 => I::I64Store8(m(offset, disc_align)),
                2 => I::I64Store16(m(offset, disc_align)),
                _ => I::I64Store32(m(offset, disc_align)),
            });
            for (ci, vshape) in variants.iter().enumerate() {
                if vshape.fields.is_empty() {
                    continue;
                }
                let ts: Vec<Type> = vshape.fields.iter().map(|(_, t)| t.clone()).collect();
                let (_, offs) = record(world, &ts)?;
                code.extend([
                    I::LocalGet(tag),
                    I::I64Const(ci as i64),
                    I::I64Eq,
                    I::If(BlockType::Empty),
                ]);
                for (i, t) in ts.iter().enumerate() {
                    let x = temp(locals);
                    field(code, v, layout::variant_offset(true, i), x);
                    store(
                        l,
                        code,
                        locals,
                        nparams,
                        base,
                        offset + payload + offs[i],
                        x,
                        t,
                    )?;
                }
                code.push(I::End);
            }
        }
        _ => return Err(export_err(format!("{ty} cannot be exported"))),
    }
    Ok(())
}

/// The load of the only flat value of a single-flat type stored at `base`.
fn single_leaf(world: &crate::check::World, ty: &Type) -> Result<(I<'static>, ValType), WasmError> {
    Ok(match layout::shape(world, ty)? {
        Shape::Scalar(Scalar::Bool | Scalar::U8) => (I::I32Load8U(m(0, 0)), ValType::I32),
        Shape::Scalar(Scalar::U32) => (I::I32Load(m(0, 2)), ValType::I32),
        Shape::Scalar(Scalar::F64) => (I::F64Load(m(0, 3)), ValType::F64),
        Shape::Scalar(_) => (I::I64Load(m(0, 3)), ValType::I64),
        Shape::Tuple(ts) if ts.len() == 1 => single_leaf(world, &ts[0])?,
        Shape::Struct { fields, .. } if fields.len() == 1 => single_leaf(world, &fields[0].1)?,
        Shape::Enum { variants, .. } => match discriminant(variants.len()).0 {
            1 => (I::I32Load8U(m(0, 0)), ValType::I32),
            2 => (I::I32Load16U(m(0, 1)), ValType::I32),
            _ => (I::I32Load(m(0, 2)), ValType::I32),
        },
        _ => return Err(WasmError::Invalid(format!("{ty} is not single-flat"))),
    })
}

fn core_scalar(s: Scalar) -> Option<ValType> {
    match s {
        Scalar::I64 | Scalar::U64 => Some(ValType::I64),
        Scalar::U32 | Scalar::U8 | Scalar::Bool => Some(ValType::I32),
        Scalar::F64 => Some(ValType::F64),
        Scalar::Unit | Scalar::Never => None,
    }
}

pub(crate) struct Boundary {
    pub wit: String,
    pub export: String,
}

/// Add the export wrappers for `entry` and return the WIT source.
pub(crate) fn exports(
    l: &mut Lowerer<'_>,
    entry: &str,
    world: WasmWorld,
) -> Result<Boundary, WasmError> {
    let f = l
        .world
        .functions
        .get(entry)
        .ok_or_else(|| WasmError::Entry(format!("unknown entry function '{entry}'")))?;
    if !f.effects.instances.is_empty() || f.effects.tail.is_some() {
        return Err(WasmError::Entry(
            "the entry must handle all of its effects".into(),
        ));
    }
    if !f.generics.is_empty() {
        return Err(WasmError::Entry("the entry must not be generic".into()));
    }
    let mut wit = Wit::default();
    let mut params = vec![];
    let mut core_params = vec![];
    let mut scalars = vec![];
    for p in &f.params {
        if p.pass == Pass::Inout {
            return Err(WasmError::Entry("entry parameters cannot be inout".into()));
        }
        let s = layout::shape(l.world, &p.ty)?
            .scalar()
            .filter(|s| core_scalar(*s).is_some())
            .ok_or_else(|| {
                WasmError::Entry(format!("entry parameter '{}' is not a scalar", p.name))
            })?;
        params.push(format!("%{}: {}", ident(&p.name), wit.ty(l.world, &p.ty)?));
        core_params.push(core_scalar(s).expect("scalar"));
        scalars.push(s);
    }
    let result = crate::numeric::default_type(&f.result);
    let shape = layout::shape(l.world, &result)?;
    let unit = matches!(shape, Shape::Scalar(Scalar::Unit | Scalar::Never));
    let wit_result = if unit {
        None
    } else {
        Some(wit.ty(l.world, &result)?)
    };
    let flat = if unit {
        0
    } else {
        flat_count(l.world, &result)?
    };
    let core_result = match (&shape, flat) {
        (_, 0) => vec![],
        (Shape::Scalar(s), _) => vec![core_scalar(*s).expect("scalar")],
        (_, 1) => vec![single_leaf(l.world, &result)?.1],
        _ => vec![ValType::I32],
    };
    if world == WasmWorld::NexusPlugin
        && (scalars != [Scalar::U32, Scalar::U64] || !matches!(shape, Shape::Scalar(Scalar::I64)))
    {
        return Err(WasmError::Entry(
            "a Nexus plugin entry must have type fn(u32, u64) -> i64".into(),
        ));
    }
    // Wrapper: canonical parameters -> uniform values -> entry -> result.
    let nparams = core_params.len() as u32;
    let mut locals = vec![];
    let mut code = vec![];
    for (i, s) in scalars.iter().enumerate() {
        code.push(I::LocalGet(i as u32));
        match core_scalar(*s) {
            Some(ValType::I32) => code.push(I::I64ExtendI32U),
            Some(ValType::F64) => code.push(I::I64ReinterpretF64),
            _ => {}
        }
    }
    let r = nparams;
    locals.push(ValType::I64);
    code.extend([
        I::Call(l.funcs[entry]),
        I::LocalSet(r),
        I::GlobalGet(l.abort),
        I::I64Const(0),
        I::I64Ne,
        I::If(BlockType::Empty),
        I::Unreachable,
        I::End,
    ]);
    match (&shape, flat) {
        (_, 0) => {}
        (Shape::Scalar(s), _) => {
            code.push(I::LocalGet(r));
            match core_scalar(*s) {
                Some(ValType::I32) => code.push(I::I32WrapI64),
                Some(ValType::F64) => code.push(I::F64ReinterpretI64),
                _ => {}
            }
        }
        _ => {
            let (size, _) = size_align(l.world, &result)?;
            let area = nparams + locals.len() as u32;
            locals.push(ValType::I64);
            code.extend([
                I::I64Const(size as i64),
                I::Call(l.alloc),
                I::LocalSet(area),
            ]);
            store(l, &mut code, &mut locals, nparams, area, 0, r, &result)?;
            code.extend([I::LocalGet(area), I::I32WrapI64]);
            if flat == 1 {
                code.push(single_leaf(l.world, &result)?.0);
            }
        }
    }
    code.push(I::End);
    let export = match world {
        WasmWorld::Command => format!("{ENTRY_INTERFACE}#main"),
        WasmWorld::NexusPlugin => "run".to_owned(),
    };
    let wrapper =
        l.b.declare(&format!("$export:{entry}"), &core_params, &core_result);
    let mut func = Function::new(locals.into_iter().map(|t| (1, t)));
    for ins in &code {
        func.instruction(ins);
    }
    l.b.define(wrapper, func);
    l.b.export_func(&export, wrapper);
    let ret = wit_result.map(|t| format!(" -> {t}")).unwrap_or_default();
    let defs: String = wit.defs.iter().map(|d| format!("  {d}\n")).collect();
    let source = match world {
        WasmWorld::Command => {
            let run = if f.params.is_empty() {
                // wasi:cli/run: run the entry and report success.
                let run = l.b.declare("$wasi:run", &[], &[ValType::I32]);
                let mut func = Function::new([]);
                for ins in [
                    I::Call(wrapper),
                    if core_result.is_empty() {
                        I::Nop
                    } else {
                        I::Drop
                    },
                    I::I32Const(0),
                    I::End,
                ] {
                    func.instruction(&ins);
                }
                l.b.define(run, func);
                l.b.export_func(&format!("{WASI_RUN}#run"), run);
                format!("  export {WASI_RUN};\n")
            } else {
                String::new()
            };
            format!(
                "package {PACKAGE};\n\ninterface entry {{\n{defs}  main: func({}){ret};\n}}\n\nworld program {{\n{run}  export entry;\n}}\n",
                params.join(", ")
            )
        }
        WasmWorld::NexusPlugin => format!(
            "package {PACKAGE};\n\nworld plugin {{\n{defs}  export run: func({}){ret};\n}}\n",
            params.join(", ")
        ),
    };
    Ok(Boundary {
        wit: source,
        export,
    })
}

fn err<E: std::fmt::Display>(stage: &str, e: E) -> WasmError {
    WasmError::Encode(format!("{stage}: {e:#}"))
}

/// Embed the WIT world and encode the component.
pub(crate) fn encode(mut core: Vec<u8>, wit: &str, world: WasmWorld) -> Result<Vec<u8>, WasmError> {
    let mut resolve = wit_parser::Resolve::default();
    resolve
        .push_source("wasi-cli.wit", WASI_CLI_WIT)
        .map_err(|e| err("WASI WIT", e))?;
    let pkg = resolve
        .push_source("vesper-program.wit", wit)
        .map_err(|e| err("program WIT", e))?;
    let name = match world {
        WasmWorld::Command => "program",
        WasmWorld::NexusPlugin => "plugin",
    };
    let id = resolve
        .select_world(&[pkg], Some(name))
        .map_err(|e| err("world", e))?;
    wit_component::embed_component_metadata(
        &mut core,
        &resolve,
        id,
        wit_component::StringEncoding::UTF8,
        false,
    )
    .map_err(|e| err("metadata", e))?;
    wit_component::ComponentEncoder::default()
        .validate(true)
        .module(&core)
        .map_err(|e| err("module", e))?
        .encode()
        .map_err(|e| err("component", e))
}
