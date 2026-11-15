//! `.vsys` types to source types, and the declarations each import adds.
use super::{EFFECT_PREFIX, FfiError, FfiErrorKind, INVALID_TYPE, Imports, effect_name};
use crate::ast::*;
use std::collections::{BTreeMap, BTreeSet};
use vesper_ffi::sys::{
    Access, FunctionDescriptor, ParameterType, Pointee, PointerAccess, Scalar, ValueType,
};
use vesper_ffi::{FieldKind, WireLayout};

/// Pass-1 placeholder of a slice parameter (not a source identifier).
const OPEN: &str = "$open";
/// Built-in source type names a generated declaration may not take.
const RESERVED: [&str; 8] = [
    "Option",
    "Result",
    "RuntimeError",
    "Unit",
    "Never",
    "String",
    "ArithOverflow",
    "Fail",
];

pub(super) enum ParamKind {
    /// A fixed source type.
    Value(Type),
    /// A `CSlice[element]` whose tuple arity is chosen per call.
    Slice { element: Type, nullable: bool },
}
pub(super) struct ParamShape {
    pub name: String,
    pub pass: Pass,
    pub kind: ParamKind,
}
/// The source signature of one foreign function (slice arities open).
pub(super) struct OpShape {
    pub params: Vec<ParamShape>,
    pub result: Type,
}
impl OpShape {
    pub fn slice_count(&self) -> usize {
        self.params
            .iter()
            .filter(|p| matches!(p.kind, ParamKind::Slice { .. }))
            .count()
    }
}

/// The operation name of `symbol` for one vector of slice lengths. Symbols
/// are `.vsys` identifiers, which never contain `$`.
pub(super) fn op_name(symbol: &str, lengths: &[usize]) -> String {
    if lengths.is_empty() {
        symbol.to_owned()
    } else {
        let lengths: Vec<String> = lengths.iter().map(usize::to_string).collect();
        format!("{symbol}${}", lengths.join("_"))
    }
}

/// Source carrier of a by-value scalar parameter or pointee.
fn parameter_scalar(s: Scalar) -> Result<&'static str, String> {
    match s {
        Scalar::U8 => Ok("u8"),
        Scalar::U32 => Ok("u32"),
        Scalar::U64 | Scalar::Usize => Ok("u64"),
        Scalar::I64 => Ok("i64"),
        Scalar::I8 | Scalar::I16 | Scalar::I32 | Scalar::U16 => Err(format!(
            "{} parameters have no source carrier yet (source integers are u8, u32, u64 and i64)",
            s.name()
        )),
    }
}
/// Source carrier of a scalar result: narrow results widen losslessly.
fn result_scalar(s: Scalar) -> &'static str {
    match s {
        Scalar::U8 => "u8",
        Scalar::U16 | Scalar::U32 => "u32",
        Scalar::U64 | Scalar::Usize => "u64",
        Scalar::I8 | Scalar::I16 | Scalar::I32 | Scalar::I64 => "i64",
    }
}
/// Source carrier of a wire-struct integer field.
fn field_integer(i: &vesper_ffi::Integer) -> Result<&'static str, String> {
    match (i.bits, i.signed) {
        (8, false) => Ok("u8"),
        (32, false) => Ok("u32"),
        (64, false) => Ok("u64"),
        (64, true) => Ok("i64"),
        (bits, signed) => Err(format!(
            "wire field of {}{bits} has no source carrier yet",
            if signed { "i" } else { "u" }
        )),
    }
}

pub(super) struct Generated {
    /// Source struct declarations by name (wire structs of every import).
    structs: BTreeMap<String, Result<Adt, String>>,
    /// Source enum declarations by name.
    enums: BTreeMap<String, Adt>,
    /// Library names, for demangling.
    libraries: Vec<String>,
    /// Per library: symbol -> shape.
    shapes: BTreeMap<String, BTreeMap<String, Result<OpShape, String>>>,
    invalid: bool,
}
fn struct_adt(layout: &WireLayout) -> Result<Adt, String> {
    let mut fields = vec![];
    for f in layout.fields() {
        let ty = match f.kind() {
            FieldKind::Integer(i) => Type::named(field_integer(i)?),
            FieldKind::Bytes => Type::Tuple(vec![Type::named("u8"); f.size()]),
            FieldKind::Padding => continue,
            FieldKind::Struct(inner) => {
                struct_adt(inner)?;
                Type::named(inner.name())
            }
        };
        fields.push(Field {
            name: f.name().to_owned(),
            ty,
            modality: Mode::Linear,
        });
    }
    Ok(Adt {
        name: layout.name().to_owned(),
        mode: Mode::Copy,
        generics: vec![],
        variants: vec![Variant {
            name: layout.name().to_owned(),
            fields,
            named: true,
        }],
        is_enum: false,
        span: 0..0,
    })
}
fn collect_structs(layout: &WireLayout, out: &mut Vec<WireLayout>) {
    for f in layout.fields() {
        if let FieldKind::Struct(inner) = f.kind() {
            collect_structs(inner, out);
        }
    }
    out.push(layout.clone());
}

impl Generated {
    pub fn new(imports: &Imports) -> Result<Self, FfiError> {
        let mut g = Self {
            structs: BTreeMap::new(),
            enums: BTreeMap::new(),
            libraries: imports.libraries.keys().cloned().collect(),
            shapes: BTreeMap::new(),
            invalid: false,
        };
        let duplicate = |name: &str| {
            FfiError::new(
                FfiErrorKind::DuplicateLibrary,
                format!("imported libraries declare '{name}' differently"),
                None,
            )
        };
        for import in imports.libraries.values() {
            let d = &import.descriptor;
            let mut layouts = vec![];
            for w in d.wire_structs() {
                collect_structs(w.layout(), &mut layouts);
            }
            for layout in layouts {
                let adt = struct_adt(&layout);
                match g.structs.get(layout.name()) {
                    Some(existing) if existing != &adt => return Err(duplicate(layout.name())),
                    _ => {
                        g.structs.insert(layout.name().to_owned(), adt);
                    }
                }
            }
            for e in d.enums() {
                let adt = Adt {
                    name: e.name().to_owned(),
                    mode: Mode::Copy,
                    generics: vec![],
                    variants: e
                        .variants()
                        .iter()
                        .map(|(n, _)| Variant {
                            name: n.clone(),
                            fields: vec![],
                            named: false,
                        })
                        .collect(),
                    is_enum: true,
                    span: 0..0,
                };
                match g.enums.get(e.name()) {
                    Some(existing) if existing != &adt => return Err(duplicate(e.name())),
                    _ => {
                        g.enums.insert(e.name().to_owned(), adt);
                    }
                }
            }
        }
        for (name, import) in &imports.libraries {
            let mut shapes = BTreeMap::new();
            for f in import.descriptor.functions() {
                let shape = g.compute_shape(f);
                if let Ok(s) = &shape
                    && s.result.name() == "Result"
                {
                    g.invalid = true;
                }
                shapes.insert(f.symbol().to_owned(), shape);
            }
            g.shapes.insert(name.clone(), shapes);
        }
        Ok(g)
    }
    fn pointee(&self, p: &Pointee) -> Result<Type, String> {
        match p {
            Pointee::Scalar(s) => Ok(Type::named(parameter_scalar(*s)?)),
            Pointee::Struct(w) => match self.structs.get(w.name()) {
                Some(Ok(_)) => Ok(Type::named(w.name())),
                Some(Err(e)) => Err(format!("wire struct {}: {e}", w.name())),
                None => Err(format!("wire struct {} was not generated", w.name())),
            },
        }
    }
    fn compute_shape(&self, f: &FunctionDescriptor) -> Result<OpShape, String> {
        let mut params = vec![];
        for p in f.parameters() {
            let (pass, kind) = match p.ty() {
                ParameterType::Value(v) => (
                    Pass::In,
                    ParamKind::Value(match v {
                        ValueType::Scalar(s) => Type::named(parameter_scalar(*s)?),
                        ValueType::Bool => Type::named("bool"),
                        ValueType::Enum(e) => Type::named(e.name()),
                        ValueType::Unit => return Err("Unit parameter".into()),
                        ValueType::Address { .. } => {
                            return Err(
                                "address parameters have no source carrier (source code has no raw pointers)"
                                    .into(),
                            );
                        }
                    }),
                ),
                ParameterType::Pointer {
                    pointee,
                    access,
                    nullable,
                } => {
                    let t = self.pointee(pointee)?;
                    let t = if *nullable {
                        Type::Named("Option".into(), vec![t])
                    } else {
                        t
                    };
                    (
                        match access {
                            PointerAccess::In => Pass::In,
                            PointerAccess::Out | PointerAccess::Inout => Pass::Inout,
                        },
                        ParamKind::Value(t),
                    )
                }
                ParameterType::Slice {
                    element,
                    access,
                    nullable,
                } => (
                    match access {
                        Access::In => Pass::In,
                        Access::Inout => Pass::Inout,
                    },
                    ParamKind::Slice {
                        element: self.pointee(element)?,
                        nullable: *nullable,
                    },
                ),
            };
            params.push(ParamShape {
                name: p.name().to_owned(),
                pass,
                kind,
            });
        }
        let invalid = || Type::named(INVALID_TYPE);
        let result = match f.result() {
            ValueType::Unit => Type::unit(),
            ValueType::Scalar(s) => Type::named(result_scalar(*s)),
            ValueType::Bool => Type::Named("Result".into(), vec![Type::named("bool"), invalid()]),
            ValueType::Enum(e) => {
                Type::Named("Result".into(), vec![Type::named(e.name()), invalid()])
            }
            ValueType::Address { .. } => {
                return Err(
                    "ForeignVal[Ptr[..]] results have no source carrier (source code has no raw pointers)"
                        .into(),
                );
            }
        };
        Ok(OpShape { params, result })
    }
    pub fn shape(&self, library: &str, f: &FunctionDescriptor) -> Result<&OpShape, String> {
        match self.shapes.get(library).and_then(|s| s.get(f.symbol())) {
            Some(Ok(s)) => Ok(s),
            Some(Err(e)) => Err(e.clone()),
            None => Err("unknown function".into()),
        }
    }
    fn names(&self) -> BTreeSet<String> {
        let mut names: BTreeSet<String> = self
            .structs
            .iter()
            .filter(|(_, a)| a.is_ok())
            .map(|(n, _)| n.clone())
            .collect();
        names.extend(self.enums.keys().cloned());
        if self.invalid {
            names.insert(INVALID_TYPE.into());
        }
        names
    }
    /// Generated names must not collide with source declarations, builtins
    /// or library names.
    pub fn check_collisions(&self, module: &Module) -> Result<(), FfiError> {
        let collision = |name: &str, what: &str| {
            Err(FfiError::new(
                FfiErrorKind::NameCollision,
                format!("'{name}' {what}"),
                None,
            ))
        };
        let generated = self.names();
        for name in &generated {
            if RESERVED.contains(&name.as_str()) {
                return collision(name, "is a builtin and cannot be generated from a .vsys");
            }
            if self.libraries.contains(name) {
                return collision(name, "names both a library and one of its types");
            }
        }
        for item in &module.items {
            let (name, span) = match item {
                Item::Adt(a) => (&a.name, &a.span),
                Item::Effect(e) => (&e.name, &e.span),
                _ => continue,
            };
            if generated.contains(name) || self.libraries.contains(name) {
                let mut e = FfiError::new(
                    FfiErrorKind::NameCollision,
                    format!("source declaration '{name}' collides with an imported .vsys name"),
                    Some(span.clone()),
                );
                e.span = Some(span.clone());
                return Err(e);
            }
            if name == super::CAPABILITY {
                return Err(FfiError::new(
                    FfiErrorKind::NameCollision,
                    "'Ffi' is the foreign capability and cannot be declared in source",
                    Some(span.clone()),
                ));
            }
        }
        Ok(())
    }
    /// Append the generated types and one effect per library. Pass 1
    /// (`instances: None`) declares every representable function with open
    /// slice types; pass 2 declares one operation per used slice arity.
    pub fn append(&self, module: &mut Module, instances: Option<&super::Instances>) {
        for adt in self.structs.values().flatten() {
            module.items.push(Item::Adt(adt.clone()));
        }
        for adt in self.enums.values() {
            module.items.push(Item::Adt(adt.clone()));
        }
        if self.invalid {
            module.items.push(Item::Adt(Adt {
                name: INVALID_TYPE.into(),
                mode: Mode::Copy,
                generics: vec![],
                variants: vec![Variant {
                    name: INVALID_TYPE.into(),
                    fields: vec![Field {
                        name: "raw".into(),
                        ty: Type::named("u64"),
                        modality: Mode::Linear,
                    }],
                    named: true,
                }],
                is_enum: false,
                span: 0..0,
            }));
        }
        for (library, shapes) in &self.shapes {
            let mut ops = vec![];
            for (symbol, shape) in shapes {
                let Ok(shape) = shape else { continue };
                let lengths: Vec<Vec<usize>> = match instances {
                    None => vec![vec![]],
                    Some(i) => match i.get(&(library.clone(), symbol.clone())) {
                        Some(set) => set.iter().cloned().collect(),
                        None => continue,
                    },
                };
                for lengths in lengths {
                    let open = lengths.len() != shape.slice_count();
                    let mut slice = 0;
                    let params = shape
                        .params
                        .iter()
                        .map(|p| Param {
                            name: p.name.clone(),
                            pass: p.pass,
                            projections: vec![],
                            ty: match &p.kind {
                                ParamKind::Value(t) => t.clone(),
                                // Pass 1: a type nothing is compatible with, so the
                                // checker records each argument's own type.
                                ParamKind::Slice { .. } if open => Type::named(OPEN),
                                ParamKind::Slice { element, nullable } => {
                                    let n = lengths[slice];
                                    slice += 1;
                                    let t = Type::Tuple(vec![element.clone(); n]);
                                    if *nullable {
                                        Type::Named("Option".into(), vec![t])
                                    } else {
                                        t
                                    }
                                }
                            },
                            span: 0..0,
                        })
                        .collect();
                    ops.push(Function {
                        name: if open {
                            symbol.clone()
                        } else {
                            op_name(symbol, &lengths)
                        },
                        generics: vec![],
                        params,
                        result: shape.result.clone(),
                        effects: Effects::default(),
                        body: None,
                        span: 0..0,
                    });
                }
            }
            module.items.push(Item::Effect(EffectDecl {
                name: effect_name(library),
                linear: false,
                generics: vec![],
                ops,
                span: 0..0,
            }));
        }
    }
    /// Spell checker messages in source terms: `Ffi$Lib::f$3` as `Lib::f`
    /// and `Ffi$Lib` as `Ffi[Lib]`.
    pub fn demangle(&self, message: &str) -> String {
        let mut out = message.to_owned();
        for library in &self.libraries {
            let effect = format!("{EFFECT_PREFIX}{library}");
            let path = format!("{effect}::");
            while let Some(at) = out.find(&path) {
                let rest = &out[at + path.len()..];
                let symbol_end = rest
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .unwrap_or(rest.len());
                let mut end = symbol_end;
                if rest[symbol_end..].starts_with('$') {
                    end += 1 + rest[symbol_end + 1..]
                        .find(|c: char| !(c.is_ascii_digit() || c == '_'))
                        .unwrap_or(rest.len() - symbol_end - 1);
                }
                let symbol = rest[..symbol_end].to_owned();
                out.replace_range(at..at + path.len() + end, &format!("{library}::{symbol}"));
            }
            out = out.replace(&effect, &format!("Ffi[{library}]"));
        }
        out
    }
}
