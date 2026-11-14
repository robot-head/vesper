//! Recognition of the eBPF source surface: the entry's action enum, the
//! `Packet` and `Bpf` effects, map effects and their specification
//! functions, and the set of functions reachable from the entry.
use super::{EbpfBackendError, rules, unsupported};
use crate::{
    ast::{Literal, Type},
    check::World,
    ir::{self, Op, ValueId},
};
use std::collections::{BTreeMap, BTreeSet};
use vesper_ebpf::{MapDef, MapKind, ProgramType, helpers};

/// The entry's result enum, which fixes the program type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionKind {
    /// `XdpAction`: `BPF_PROG_TYPE_XDP`.
    Xdp,
    /// `TcAction`: `BPF_PROG_TYPE_SCHED_CLS`.
    Tc,
}

impl ActionKind {
    pub fn prog_type(self) -> ProgramType {
        match self {
            Self::Xdp => ProgramType::Xdp,
            Self::Tc => ProgramType::SchedCls,
        }
    }

    pub fn enum_name(self) -> &'static str {
        match self {
            Self::Xdp => "XdpAction",
            Self::Tc => "TcAction",
        }
    }

    /// Variants in kernel return-code order.
    pub fn variants(self) -> &'static [&'static str] {
        match self {
            Self::Xdp => &["Aborted", "Drop", "Pass", "Tx", "Redirect"],
            Self::Tc => &[
                "Ok",
                "Reclassify",
                "Shot",
                "Pipe",
                "Stolen",
                "Queued",
                "Repeat",
                "Redirect",
            ],
        }
    }

    /// Offsets of `data` and `data_end` in the context (`struct xdp_md`,
    /// `struct __sk_buff`).
    pub(crate) fn packet_fields(self) -> (i16, i16) {
        match self {
            Self::Xdp => (0, 4),
            Self::Tc => (76, 80),
        }
    }
}

/// A source map effect and the map it became.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapBinding {
    /// The source effect name.
    pub effect: String,
    /// Index into `Program::maps`.
    pub index: usize,
}

/// Scalar value kinds of the subset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scalar {
    U8,
    U32,
    U64,
    I64,
    Bool,
}

impl Scalar {
    pub(crate) fn of(t: &Type) -> Option<Scalar> {
        match crate::numeric::default_type(t) {
            Type::Named(n, a) if a.is_empty() => Some(match n.as_str() {
                "u8" => Scalar::U8,
                "u32" => Scalar::U32,
                "u64" => Scalar::U64,
                "i64" => Scalar::I64,
                "bool" => Scalar::Bool,
                _ => return None,
            }),
            _ => None,
        }
    }

    /// Bytes a map key or value of this kind occupies.
    pub(crate) fn map_bytes(self) -> Option<u32> {
        match self {
            Scalar::U8 => Some(1),
            Scalar::U32 => Some(4),
            Scalar::U64 => Some(8),
            Scalar::I64 | Scalar::Bool => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PacketOp {
    U8,
    U16Be,
    U32Be,
}

impl PacketOp {
    pub(crate) fn bytes(self) -> i32 {
        match self {
            PacketOp::U8 => 1,
            PacketOp::U16Be => 2,
            PacketOp::U32Be => 4,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MapOp {
    Lookup,
    Update,
    Delete,
    Add,
}

/// How one `Effect::op` call is lowered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EffectOp {
    Packet(PacketOp),
    Helper(i32),
    Map {
        map: usize,
        op: MapOp,
        key: Scalar,
        value: Scalar,
        kind: MapKind,
    },
}

pub(crate) struct Surface {
    pub action: ActionKind,
    pub maps: Vec<MapBinding>,
    pub map_defs: Vec<MapDef>,
    /// `"Effect::op"` -> lowering.
    pub ops: BTreeMap<String, EffectOp>,
    /// Functions reachable from the entry, entry first.
    pub functions: Vec<String>,
}

fn named<'a>(t: &'a Type, name: &str) -> Option<&'a [Type]> {
    match t {
        Type::Named(n, a) if n == name => Some(a),
        _ => None,
    }
}

fn is_unit(t: &Type) -> bool {
    matches!(t, Type::Tuple(a) if a.is_empty()) || named(t, "Unit").is_some_and(<[_]>::is_empty)
}

fn option_of(t: &Type) -> Option<&Type> {
    match named(t, "Option") {
        Some([inner]) => Some(inner),
        _ => None,
    }
}

/// `BlockedSources` -> `blocked_sources`.
pub(crate) fn snake(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

const PACKET: &str = "Packet";
const HELPERS: &str = "Bpf";

impl Surface {
    pub(crate) fn analyze(
        world: &World,
        program: &ir::Program,
        entry: &str,
    ) -> Result<Surface, EbpfBackendError> {
        let f = world.functions.get(entry).ok_or_else(|| {
            unsupported(
                rules::ENTRY,
                entry,
                None,
                format!("entry function `{entry}` not found"),
            )
        })?;
        if !f.params.is_empty() {
            return Err(unsupported(
                rules::ENTRY,
                entry,
                Some(&f.span),
                "an eBPF entry takes no parameters; packet bytes are read through the `Packet` effect",
            ));
        }
        let action = match f.result.name() {
            "XdpAction" => ActionKind::Xdp,
            "TcAction" => ActionKind::Tc,
            other => {
                return Err(unsupported(
                    rules::ENTRY,
                    entry,
                    Some(&f.span),
                    format!(
                        "an eBPF entry returns `XdpAction` (XDP) or `TcAction` (tc), not `{}`",
                        if other.is_empty() {
                            f.result.to_string()
                        } else {
                            other.to_owned()
                        }
                    ),
                ));
            }
        };
        let adt = &world.adts[action.enum_name()];
        let names: Vec<&str> = adt.variants.iter().map(|v| v.name.as_str()).collect();
        if names != action.variants() || adt.variants.iter().any(|v| !v.fields.is_empty()) {
            return Err(unsupported(
                rules::ACTION,
                entry,
                Some(&adt.span),
                format!(
                    "`{}` must declare exactly the field-less variants {} in this order (the tag is the kernel return code)",
                    action.enum_name(),
                    action.variants().join(", ")
                ),
            ));
        }

        let mut s = Surface {
            action,
            maps: Vec::new(),
            map_defs: Vec::new(),
            ops: BTreeMap::new(),
            functions: Vec::new(),
        };
        // Reachable functions, entry first (depth-first order).
        let mut seen = BTreeSet::new();
        let mut stack = vec![entry.to_owned()];
        while let Some(name) = stack.pop() {
            if !seen.insert(name.clone()) {
                continue;
            }
            let func = &world.functions[&name];
            if !func.generics.is_empty() {
                return Err(unsupported(
                    rules::GENERIC,
                    &name,
                    Some(&func.span),
                    "generic functions are not monomorphised for the eBPF target",
                ));
            }
            for e in &func.effects.instances {
                s.effect(world, program, &name, &e.name)?;
            }
            if func.effects.tail.is_some() {
                return Err(unsupported(
                    rules::EFFECT,
                    &name,
                    Some(&func.span),
                    "open effect rows are not supported on the eBPF target",
                ));
            }
            s.functions.push(name.clone());
            let ir = program.functions.get(&name).ok_or_else(|| {
                unsupported(
                    rules::OPERATION,
                    &name,
                    Some(&func.span),
                    "function has no body",
                )
            })?;
            let mut callees = Vec::new();
            for b in &ir.blocks {
                for i in &b.instructions {
                    if let Op::Call { callee, .. } = &i.op {
                        if program.functions.contains_key(callee) {
                            callees.push(callee.clone());
                        } else if let Some((effect, _)) = callee.split_once("::") {
                            if world.effects.contains_key(effect) {
                                s.effect(world, program, &name, effect)?;
                                if !s.ops.contains_key(callee) {
                                    return Err(unsupported(
                                        rules::EFFECT,
                                        &name,
                                        Some(&i.span),
                                        format!("`{callee}` has no eBPF lowering"),
                                    ));
                                }
                            } else {
                                return Err(unsupported(
                                    rules::OPERATION,
                                    &name,
                                    Some(&i.span),
                                    format!("call of `{callee}` has no eBPF lowering"),
                                ));
                            }
                        } else {
                            return Err(unsupported(
                                rules::OPERATION,
                                &name,
                                Some(&i.span),
                                format!("call of `{callee}` has no eBPF lowering"),
                            ));
                        }
                    }
                }
            }
            for c in callees.into_iter().rev() {
                stack.push(c);
            }
        }
        Ok(s)
    }

    /// Recognise and register effect `name` (idempotent).
    fn effect(
        &mut self,
        world: &World,
        program: &ir::Program,
        function: &str,
        name: &str,
    ) -> Result<(), EbpfBackendError> {
        if self
            .ops
            .keys()
            .any(|k| k.split_once("::").is_some_and(|(e, _)| e == name))
            || self.maps.iter().any(|m| m.effect == name)
        {
            return Ok(());
        }
        if name == "Fail" {
            return Err(unsupported(
                rules::FALLIBLE,
                function,
                None,
                "`Fail` cannot reach the kernel: use wrapping_add/wrapping_sub/wrapping_mul or prove the operation total",
            ));
        }
        let Some(decl) = world.effects.get(name) else {
            return Err(unsupported(
                rules::EFFECT,
                function,
                None,
                format!("effect `{name}` is not declared"),
            ));
        };
        if !decl.generics.is_empty() {
            return Err(unsupported(
                rules::EFFECT,
                function,
                Some(&decl.span),
                format!("effect `{name}` is generic"),
            ));
        }
        let bad = |op: &str, want: &str| {
            unsupported(
                rules::EFFECT,
                function,
                Some(&decl.span),
                format!("`{name}::{op}` must have the signature `{want}`"),
            )
        };
        let params = |f: &crate::ast::Function| -> Vec<Option<Scalar>> {
            f.params.iter().map(|p| Scalar::of(&p.ty)).collect()
        };
        match name {
            PACKET => {
                for op in &decl.ops {
                    let (packet, result, want) = match op.name.as_str() {
                        "load_u8" => (PacketOp::U8, "u8", "fn load_u8(offset: u32) -> Option[u8]"),
                        "load_u16be" => (
                            PacketOp::U16Be,
                            "u32",
                            "fn load_u16be(offset: u32) -> Option[u32]",
                        ),
                        "load_u32be" => (
                            PacketOp::U32Be,
                            "u32",
                            "fn load_u32be(offset: u32) -> Option[u32]",
                        ),
                        other => {
                            return Err(unsupported(
                                rules::EFFECT,
                                function,
                                Some(&op.span),
                                format!(
                                    "`Packet::{other}` is not a supported packet view (load_u8, load_u16be, load_u32be)"
                                ),
                            ));
                        }
                    };
                    if params(op) != [Some(Scalar::U32)]
                        || option_of(&op.result).map(Type::name) != Some(result)
                    {
                        return Err(bad(&op.name, want));
                    }
                    self.ops
                        .insert(format!("{name}::{}", op.name), EffectOp::Packet(packet));
                }
            }
            HELPERS => {
                for op in &decl.ops {
                    let (id, result) = match op.name.as_str() {
                        "ktime_get_ns" => (helpers::KTIME_GET_NS, "u64"),
                        "smp_processor_id" => (helpers::GET_SMP_PROCESSOR_ID, "u32"),
                        "prandom_u32" => (helpers::GET_PRANDOM_U32, "u32"),
                        other => {
                            return Err(unsupported(
                                rules::EFFECT,
                                function,
                                Some(&op.span),
                                format!(
                                    "`Bpf::{other}` is not a supported helper (ktime_get_ns, smp_processor_id, prandom_u32)"
                                ),
                            ));
                        }
                    };
                    if !op.params.is_empty() || op.result.name() != result {
                        return Err(bad(&op.name, &format!("fn {}() -> {result}", op.name)));
                    }
                    if !helpers::allowed(self.action.prog_type(), id) {
                        return Err(unsupported(
                            rules::EFFECT,
                            function,
                            Some(&op.span),
                            format!("helper `{}` is not allowed in this program type", op.name),
                        ));
                    }
                    self.ops
                        .insert(format!("{name}::{}", op.name), EffectOp::Helper(id));
                }
            }
            _ => self.map_effect(world, program, function, decl)?,
        }
        Ok(())
    }

    fn map_effect(
        &mut self,
        _world: &World,
        program: &ir::Program,
        function: &str,
        decl: &crate::ast::EffectDecl,
    ) -> Result<(), EbpfBackendError> {
        let name = decl.name.as_str();
        let err = |message: String| unsupported(rules::MAP, function, Some(&decl.span), message);
        let mut key: Option<Scalar> = None;
        let mut value: Option<Scalar> = None;
        let unify = |slot: &mut Option<Scalar>, t: &Type, what: &str, op: &str| {
            let Some(s) = Scalar::of(t).filter(|s| s.map_bytes().is_some()) else {
                return Err(err(format!(
                    "`{name}::{op}`: map {what} type must be u8, u32 or u64, found {t}"
                )));
            };
            match slot {
                Some(prev) if *prev != s => Err(err(format!(
                    "`{name}::{op}`: map {what} type disagrees with another operation"
                ))),
                _ => {
                    *slot = Some(s);
                    Ok(())
                }
            }
        };
        let mut ops = Vec::new();
        for op in &decl.ops {
            let p: Vec<&Type> = op.params.iter().map(|p| &p.ty).collect();
            let m = match (op.name.as_str(), p.as_slice()) {
                ("lookup", [k]) => {
                    unify(&mut key, k, "key", &op.name)?;
                    let Some(v) = option_of(&op.result) else {
                        return Err(err(format!("`{name}::lookup` must return Option[V]")));
                    };
                    unify(&mut value, v, "value", &op.name)?;
                    MapOp::Lookup
                }
                ("update", [k, v]) | ("add", [k, v]) => {
                    unify(&mut key, k, "key", &op.name)?;
                    unify(&mut value, v, "value", &op.name)?;
                    if !is_unit(&op.result) {
                        return Err(err(format!("`{name}::{}` must return Unit", op.name)));
                    }
                    if op.name == "add" {
                        MapOp::Add
                    } else {
                        MapOp::Update
                    }
                }
                ("delete", [k]) => {
                    unify(&mut key, k, "key", &op.name)?;
                    if !is_unit(&op.result) {
                        return Err(err(format!("`{name}::delete` must return Unit")));
                    }
                    MapOp::Delete
                }
                _ => {
                    return Err(unsupported(
                        rules::EFFECT,
                        function,
                        Some(&op.span),
                        format!(
                            "effect `{name}` is not Packet, Bpf or a map: `{}` is not lookup(key), update(key, value), delete(key) or add(key, delta)",
                            op.name
                        ),
                    ));
                }
            };
            ops.push((op.name.clone(), m));
        }
        let (Some(key), Some(value)) = (key, value) else {
            return Err(err(format!(
                "map effect `{name}` needs operations that fix both its key and value types"
            )));
        };
        let spec_name = format!("{}_map", snake(name));
        let (kind, max_entries) = map_spec(program, &spec_name).map_err(|m| {
            err(format!(
                "map effect `{name}` needs `fn {spec_name}() -> BpfMapSpec {{ BpfMapSpec {{ kind: BpfMapKind::..., max_entries: N }} }}`: {m}"
            ))
        })?;
        let map_name = snake(name);
        let mut def = MapDef::new(
            &map_name,
            kind,
            key.map_bytes().unwrap_or(8),
            value.map_bytes().unwrap_or(8),
            max_entries,
        );
        if kind.is_array() && key != Scalar::U32 {
            return Err(err(format!("array map `{name}` must have u32 keys")));
        }
        if ops.iter().any(|(_, o)| *o == MapOp::Add) && value == Scalar::U8 {
            return Err(err(format!(
                "`{name}::add` needs a u32 or u64 value (atomic add has no byte form)"
            )));
        }
        if ops.iter().any(|(_, o)| *o == MapOp::Delete) && kind.is_array() {
            return Err(err(format!("array map `{name}` has no delete")));
        }
        def.flags = 0;
        def.validate()
            .map_err(|m| err(format!("map `{map_name}`: {m}")))?;
        let index = self.map_defs.len();
        self.map_defs.push(def);
        self.maps.push(MapBinding {
            effect: name.to_owned(),
            index,
        });
        for (op, m) in ops {
            self.ops.insert(
                format!("{name}::{op}"),
                EffectOp::Map {
                    map: index,
                    op: m,
                    key,
                    value,
                    kind,
                },
            );
        }
        Ok(())
    }
}

/// Evaluate a constant `BpfMapSpec { kind: BpfMapKind::K, max_entries: N }`
/// specification function.
fn map_spec(program: &ir::Program, name: &str) -> Result<(MapKind, u32), String> {
    let f = program
        .functions
        .get(name)
        .ok_or_else(|| "function not found".to_owned())?;
    if !f.params.is_empty() || f.result.name() != "BpfMapSpec" {
        return Err("it must take no parameters and return BpfMapSpec".into());
    }
    let defs: BTreeMap<ValueId, &Op> = f
        .blocks
        .iter()
        .flat_map(|b| b.instructions.iter().map(|i| (i.dest, &i.op)))
        .collect();
    let [block] = f.blocks.as_slice() else {
        return Err("its body must be one constant struct literal".into());
    };
    let ir::Terminator::Return(v) = &block.terminator else {
        return Err("its body must be one constant struct literal".into());
    };
    let Some(Op::Data(n, fields)) = defs.get(v) else {
        return Err("its body must be one constant struct literal".into());
    };
    if n != "BpfMapSpec" {
        return Err(format!("it returns `{n}`"));
    }
    let field = |f: &str| {
        fields
            .iter()
            .find(|(n, _)| n == f)
            .and_then(|(_, v)| defs.get(v).copied())
    };
    let kind = match field("kind") {
        Some(Op::Data(k, f)) if f.is_empty() => match k.strip_prefix("BpfMapKind::") {
            Some("Hash") => MapKind::Hash,
            Some("Array") => MapKind::Array,
            Some("PerCpuHash") => MapKind::PerCpuHash,
            Some("PerCpuArray") => MapKind::PerCpuArray,
            Some("LruHash") => MapKind::LruHash,
            _ => return Err(format!("unsupported map kind `{k}`")),
        },
        _ => return Err("`kind` must be a BpfMapKind constant".into()),
    };
    let max_entries = match field("max_entries") {
        Some(Op::Constant(Literal::Int(n))) => {
            u32::try_from(*n).map_err(|_| format!("max_entries {n} is out of range"))?
        }
        _ => return Err("`max_entries` must be an integer literal".into()),
    };
    Ok((kind, max_entries))
}
