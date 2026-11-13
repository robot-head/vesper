//! C header front end over a preprocessor-free subset, parsed with `lang-c`.
//!
//! `lang-c` needs preprocessed input. Rather than run a host preprocessor
//! (which differs across Linux, macOS and Windows CI), this front end accepts
//! a documented subset and rejects every other directive with
//! [`ErrorKind::Preprocessor`]:
//!
//! * comments (removed before parsing);
//! * `#include <stdint.h>`, `<stddef.h>`, `<stdbool.h>` (their types are
//!   built in);
//! * `#pragma once`, and include guards `#ifndef X` / `#define X` / `#endif`
//!   where the `#define` has no replacement list;
//! * `#ifdef __cplusplus` ... `#endif` blocks, which are skipped (the usual
//!   `extern "C" {` wrapper).
//!
//! Supported declarations: function prototypes, `struct` and `enum`
//! definitions, `typedef`s, pointers with const-ness, arrays and the
//! fixed-width integers. `int`/`unsigned`/`short`/`long long` are accepted
//! because they have one width on every supported target; `long`, plain
//! `char`, floating point, unions, bit-fields, variadics, function pointers and
//! globals are refused with a typed reason.

use std::collections::{BTreeMap, BTreeSet};

use lang_c::ast::{
    ArraySize, BinaryOperator, Constant, DeclarationSpecifier, Declarator, DeclaratorKind,
    DerivedDeclarator, Ellipsis, Expression, ExternalDeclaration, IntegerBase, IntegerSize,
    PointerQualifier, SpecifierQualifier, StorageClassSpecifier, StructDeclaration, StructKind,
    StructType, TypeQualifier, TypeSpecifier, UnaryOperator,
};
use lang_c::driver::{Config, Flavor, parse_preprocessed};
use vesper_ffi::{Declaration, Endian, Field, Integer, WireType};

use crate::{
    Access, ErrorKind, LiftedEnum, LiftedField, LiftedFunction, LiftedParam, LiftedStruct, Limits,
    MigrateError, Migration, Mode, Origin, PointerAccess, Refusal, RefusalReason, Scalar, SysParam,
    SysPointee, SysSignature, SysValue,
};

/// Types of `<stdint.h>`, `<stddef.h>` and `<stdbool.h>` that are built in.
const PRELUDE: &[(&str, &str)] = &[
    ("uint8_t", "unsigned char"),
    ("int8_t", "signed char"),
    ("uint16_t", "unsigned short"),
    ("int16_t", "short"),
    ("uint32_t", "unsigned int"),
    ("int32_t", "int"),
    ("uint64_t", "unsigned long long"),
    ("int64_t", "long long"),
    ("size_t", "unsigned long long"),
    ("uintptr_t", "unsigned long long"),
    ("intptr_t", "long long"),
    ("ptrdiff_t", "long long"),
    ("bool", "_Bool"),
];

fn builtin(name: &str) -> Option<CType> {
    Some(match name {
        "uint8_t" => CType::Scalar(Scalar::U8),
        "int8_t" => CType::Scalar(Scalar::I8),
        "uint16_t" => CType::Scalar(Scalar::U16),
        "int16_t" => CType::Scalar(Scalar::I16),
        "uint32_t" => CType::Scalar(Scalar::U32),
        "int32_t" => CType::Scalar(Scalar::I32),
        "uint64_t" => CType::Scalar(Scalar::U64),
        "int64_t" => CType::Scalar(Scalar::I64),
        "size_t" | "uintptr_t" => CType::Scalar(Scalar::Usize),
        "intptr_t" | "ptrdiff_t" => CType::Scalar(Scalar::I64),
        "bool" => CType::CBool,
        _ => return None,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CType {
    Void,
    Scalar(Scalar),
    CBool,
    Struct(String),
    Enum(String),
    Pointer {
        pointee: Box<CType>,
        pointee_const: bool,
    },
    Array {
        elem: Box<CType>,
        len: Option<u64>,
        elem_const: bool,
    },
    Refused(RefusalReason, String),
}

impl CType {
    fn vesper(&self) -> String {
        match self {
            Self::Void => "Unit".into(),
            Self::Scalar(s) => s.name().into(),
            Self::CBool => "bool".into(),
            Self::Struct(n) | Self::Enum(n) => n.clone(),
            Self::Pointer { pointee, .. } => format!("NullablePtr[{}]", pointee.vesper()),
            Self::Array {
                elem, len: Some(n), ..
            } => format!("[{}; {n}]", elem.vesper()),
            Self::Array {
                elem, len: None, ..
            } => format!("[{}]", elem.vesper()),
            Self::Refused(..) => "<refused>".into(),
        }
    }
}

/// Strip comments, keeping newlines so line numbers stay correct.
fn strip_comments(src: &str) -> Result<String, MigrateError> {
    let b = src.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    let mut line = 1;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let start = line;
                i += 2;
                loop {
                    if i + 1 >= b.len() {
                        return Err(MigrateError::new(
                            ErrorKind::Syntax,
                            Some(start),
                            "unterminated block comment",
                        ));
                    }
                    if b[i] == b'*' && b[i + 1] == b'/' {
                        i += 2;
                        out.push(b' ');
                        break;
                    }
                    if b[i] == b'\n' {
                        line += 1;
                        out.push(b'\n');
                    }
                    i += 1;
                }
            }
            q @ (b'"' | b'\'') => {
                out.push(q);
                i += 1;
                while i < b.len() && b[i] != q {
                    if b[i] == b'\n' {
                        return Err(MigrateError::new(
                            ErrorKind::Syntax,
                            Some(line),
                            "unterminated literal",
                        ));
                    }
                    if b[i] == b'\\' && i + 1 < b.len() {
                        out.push(b[i]);
                        i += 1;
                    }
                    out.push(b[i]);
                    i += 1;
                }
                if i < b.len() {
                    out.push(q);
                    i += 1;
                }
            }
            c => {
                if c == b'\n' {
                    line += 1;
                }
                out.push(c);
                i += 1;
            }
        }
    }
    // Only ASCII-delimited bytes were removed, so this stays valid UTF-8.
    String::from_utf8(out)
        .map_err(|_| MigrateError::new(ErrorKind::Syntax, None, "header is not UTF-8"))
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[derive(PartialEq)]
enum Cond {
    Guard,
    Cplusplus,
}

/// Blank every accepted directive (and skipped `__cplusplus` blocks) and
/// reject the rest.
fn filter_directives(src: &str) -> Result<String, MigrateError> {
    let mut out = String::with_capacity(src.len());
    let mut stack: Vec<Cond> = vec![];
    let mut skip_depth = 0usize;
    for (index, raw) in src.split('\n').enumerate() {
        let line_no = index + 1;
        let err = |msg: &str| MigrateError::new(ErrorKind::Preprocessor, Some(line_no), msg);
        if index > 0 {
            out.push('\n');
        }
        let line = raw.trim();
        if line.ends_with('\\') {
            return Err(err("line continuations are outside the supported subset"));
        }
        let Some(directive) = line.strip_prefix('#') else {
            if skip_depth == 0 {
                out.push_str(raw);
            }
            continue;
        };
        let mut words = directive.split_whitespace();
        let name = words.next().unwrap_or("");
        let args: Vec<&str> = words.collect();
        if skip_depth > 0 {
            match name {
                "if" | "ifdef" | "ifndef" => skip_depth += 1,
                "endif" => {
                    skip_depth -= 1;
                    if skip_depth == 0 {
                        stack.pop();
                    }
                }
                _ => {}
            }
            continue;
        }
        match (name, args.as_slice()) {
            ("include", [h]) if matches!(*h, "<stdint.h>" | "<stddef.h>" | "<stdbool.h>") => {}
            ("include", _) => {
                return Err(err(
                    "only <stdint.h>, <stddef.h> and <stdbool.h> may be included; inline other headers",
                ));
            }
            ("pragma", ["once"]) => {}
            ("ifndef", [g]) if is_ident(g) => stack.push(Cond::Guard),
            ("define", [g]) if is_ident(g) && stack.last() == Some(&Cond::Guard) => {}
            ("define", _) => {
                return Err(err(
                    "macros are outside the supported subset (only include guards)",
                ));
            }
            ("ifdef", ["__cplusplus"]) => {
                stack.push(Cond::Cplusplus);
                skip_depth = 1;
            }
            ("endif", []) => {
                if stack.pop().is_none() {
                    return Err(err("#endif without a matching conditional"));
                }
            }
            _ => return Err(err("preprocessor directive outside the supported subset")),
        }
    }
    if !stack.is_empty() {
        return Err(MigrateError::new(
            ErrorKind::Preprocessor,
            None,
            "unterminated conditional",
        ));
    }
    Ok(out)
}

struct Ctx {
    typedefs: BTreeMap<String, CType>,
    structs: Vec<LiftedStruct>,
    struct_names: BTreeSet<String>,
    enums: Vec<LiftedEnum>,
    enum_names: BTreeSet<String>,
    constants: BTreeMap<String, i128>,
    functions: Vec<LiftedFunction>,
    refusals: Vec<Refusal>,
    items: usize,
    limits: Limits,
    anonymous: usize,
}

impl Ctx {
    fn count_item(&mut self) -> Result<(), MigrateError> {
        self.items += 1;
        if self.items > self.limits.items {
            return Err(MigrateError::new(
                ErrorKind::Limit,
                None,
                "item budget exceeded",
            ));
        }
        Ok(())
    }
}

fn int_specifiers(spec: &[&TypeSpecifier]) -> CType {
    let (mut signed, mut unsigned, mut chars, mut shorts, mut ints, mut longs) = (0, 0, 0, 0, 0, 0);
    for s in spec {
        match s {
            TypeSpecifier::Signed => signed += 1,
            TypeSpecifier::Unsigned => unsigned += 1,
            TypeSpecifier::Char => chars += 1,
            TypeSpecifier::Short => shorts += 1,
            TypeSpecifier::Int => ints += 1,
            TypeSpecifier::Long => longs += 1,
            _ => {
                return CType::Refused(
                    RefusalReason::UnsupportedType,
                    "mixed type specifiers".into(),
                );
            }
        }
    }
    let sign_ok = signed + unsigned <= 1 && ints <= 1;
    let s = |sg, us| if unsigned == 1 { us } else { sg };
    match (sign_ok, chars, shorts, longs) {
        (true, 1, 0, 0) if ints == 0 && signed + unsigned == 1 => {
            CType::Scalar(s(Scalar::I8, Scalar::U8))
        }
        (true, 1, 0, 0) if ints == 0 => CType::Refused(
            RefusalReason::PlatformDependentWidth,
            "plain `char` has implementation-defined signedness; use int8_t/uint8_t".into(),
        ),
        (true, 0, 1, 0) => CType::Scalar(s(Scalar::I16, Scalar::U16)),
        (true, 0, 0, 0) => CType::Scalar(s(Scalar::I32, Scalar::U32)),
        (true, 0, 0, 1) => CType::Refused(
            RefusalReason::PlatformDependentWidth,
            "`long` is 32-bit on Windows and 64-bit on LP64 targets; use a fixed-width integer"
                .into(),
        ),
        (true, 0, 0, 2) => CType::Scalar(s(Scalar::I64, Scalar::U64)),
        _ => CType::Refused(
            RefusalReason::UnsupportedType,
            "unsupported integer specifier combination".into(),
        ),
    }
}

/// Evaluate an enumerator or array-size constant expression.
///
/// C evaluates these in `int` (C11 6.7.2.2 requires enumerator values to be
/// representable as `int`). The evaluator models signed `int` arithmetic only:
/// unsigned or long literals, and any intermediate value outside the `int`
/// range (overflow is undefined, an out-of-range literal changes type), make
/// the expression unsupported rather than silently computed in wider math.
fn eval(e: &Expression, consts: &BTreeMap<String, i128>) -> Option<i128> {
    let int = |v: i128| {
        (i128::from(i32::MIN)..=i128::from(i32::MAX))
            .contains(&v)
            .then_some(v)
    };
    match e {
        Expression::Constant(c) => match &c.node {
            Constant::Integer(i) => {
                if i.suffix.unsigned || i.suffix.size != IntegerSize::Int || i.suffix.imaginary {
                    return None;
                }
                let radix = match i.base {
                    IntegerBase::Decimal => 10,
                    IntegerBase::Octal => 8,
                    IntegerBase::Hexadecimal => 16,
                    IntegerBase::Binary => 2,
                };
                let digits: &str = &i.number;
                let v = if digits.is_empty() {
                    0
                } else {
                    i128::from(u64::from_str_radix(digits, radix).ok()?)
                };
                int(v)
            }
            _ => None,
        },
        Expression::Identifier(id) => consts.get(&id.node.name).copied().and_then(int),
        Expression::UnaryOperator(u) => {
            let v = eval(&u.node.operand.node, consts)?;
            match u.node.operator.node {
                UnaryOperator::Minus => int(-v),
                UnaryOperator::Plus => Some(v),
                _ => None,
            }
        }
        Expression::BinaryOperator(b) => {
            let l = eval(&b.node.lhs.node, consts)?;
            let r = eval(&b.node.rhs.node, consts)?;
            let v = match b.node.operator.node {
                BinaryOperator::Plus => l + r,
                BinaryOperator::Minus => l - r,
                BinaryOperator::Multiply => l * r,
                BinaryOperator::ShiftLeft if l >= 0 && (0..31).contains(&r) => l << r,
                BinaryOperator::BitwiseOr => l | r,
                _ => return None,
            };
            int(v)
        }
        _ => None,
    }
}

impl Ctx {
    /// Resolve declaration specifiers to a base type; defines any struct/enum
    /// bodies it contains. Returns the type and whether it is `const`.
    fn base_type<'a>(
        &mut self,
        types: impl IntoIterator<Item = &'a TypeSpecifier>,
        is_const: bool,
        name_hint: Option<&str>,
    ) -> Result<(CType, bool), MigrateError> {
        let types: Vec<&TypeSpecifier> = types.into_iter().collect();
        let ty = match types.as_slice() {
            [] => CType::Refused(RefusalReason::UnsupportedType, "implicit int".into()),
            [TypeSpecifier::Void] => CType::Void,
            [TypeSpecifier::Bool] => CType::CBool,
            [TypeSpecifier::Float | TypeSpecifier::Double]
            | [TypeSpecifier::Long, TypeSpecifier::Double]
            | [TypeSpecifier::TS18661Float(_)] => {
                CType::Refused(RefusalReason::Floating, "floating point carrier".into())
            }
            [TypeSpecifier::TypedefName(id)] => {
                let n = &id.node.name;
                if let Some(t) = builtin(n) {
                    t
                } else if let Some(t) = self.typedefs.get(n) {
                    t.clone()
                } else {
                    CType::Refused(
                        RefusalReason::UnsupportedType,
                        format!("unknown typedef `{n}`"),
                    )
                }
            }
            [TypeSpecifier::Struct(st)] => self.structure(&st.node, name_hint)?,
            [TypeSpecifier::Enum(et)] => {
                let tag = et.node.identifier.as_ref().map(|i| i.node.name.clone());
                let name = match tag.or_else(|| name_hint.map(str::to_owned)) {
                    Some(n) => n,
                    None => {
                        self.anonymous += 1;
                        format!("anon_enum_{}", self.anonymous)
                    }
                };
                if !et.node.enumerators.is_empty() {
                    self.define_enum(&name, &et.node)?;
                }
                CType::Enum(name)
            }
            [TypeSpecifier::Atomic(_)] => CType::Refused(
                RefusalReason::InteriorMutability,
                "_Atomic types are shared mutable state".into(),
            ),
            other
                if other.iter().all(|t| {
                    matches!(
                        t,
                        TypeSpecifier::Signed
                            | TypeSpecifier::Unsigned
                            | TypeSpecifier::Char
                            | TypeSpecifier::Short
                            | TypeSpecifier::Int
                            | TypeSpecifier::Long
                    )
                }) =>
            {
                int_specifiers(other)
            }
            _ => CType::Refused(
                RefusalReason::UnsupportedType,
                "unsupported type specifier".into(),
            ),
        };
        Ok((ty, is_const))
    }

    fn define_enum(&mut self, name: &str, et: &lang_c::ast::EnumType) -> Result<(), MigrateError> {
        self.count_item()?;
        if !self.enum_names.insert(name.to_owned()) {
            return Err(MigrateError::new(
                ErrorKind::Duplicate,
                None,
                format!("enum `{name}` defined twice"),
            ));
        }
        let mut next: i128 = 0;
        let mut variants = vec![];
        let mut bad = None;
        for e in &et.enumerators {
            let v = match &e.node.expression {
                Some(x) => match eval(&x.node, &self.constants) {
                    Some(v) => v,
                    None => {
                        bad = Some(format!(
                            "enumerator `{}` is not an integer constant expression in the supported subset",
                            e.node.identifier.node.name
                        ));
                        break;
                    }
                },
                None => next,
            };
            let ident = e.node.identifier.node.name.clone();
            if self.constants.insert(ident.clone(), v).is_some() {
                return Err(MigrateError::new(
                    ErrorKind::Duplicate,
                    None,
                    format!("enumerator `{ident}` defined twice"),
                ));
            }
            variants.push((ident, v));
            next = v.checked_add(1).unwrap_or(v);
        }
        let item = format!("enum {name}");
        let carrier = Err(match bad {
            Some(d) => Refusal::new(item, RefusalReason::UnsupportedType, d),
            None => Refusal::new(
                item,
                RefusalReason::ImplementationDefinedCarrier,
                "a C enum's ABI carrier is implementation-defined; pass it through a fixed-width typedef",
            ),
        });
        self.enums.push(LiftedEnum {
            name: name.to_owned(),
            variants,
            carrier,
        });
        Ok(())
    }

    fn structure(
        &mut self,
        st: &StructType,
        name_hint: Option<&str>,
    ) -> Result<CType, MigrateError> {
        let tag = st.identifier.as_ref().map(|i| i.node.name.clone());
        let name = match tag.clone().or_else(|| name_hint.map(str::to_owned)) {
            Some(n) => n,
            None => {
                self.anonymous += 1;
                format!("anon_struct_{}", self.anonymous)
            }
        };
        if st.kind.node == StructKind::Union {
            if st.declarations.is_some() {
                self.count_item()?;
                self.refusals.push(Refusal::new(
                    format!("union {name}"),
                    RefusalReason::Union,
                    "unions overlay fields and have no FieldTree or wire layout",
                ));
            }
            return Ok(CType::Refused(
                RefusalReason::Union,
                format!("union `{name}`"),
            ));
        }
        let Some(decls) = &st.declarations else {
            return Ok(CType::Struct(name));
        };
        self.count_item()?;
        if !self.struct_names.insert(name.clone()) {
            return Err(MigrateError::new(
                ErrorKind::Duplicate,
                None,
                format!("struct `{name}` defined twice"),
            ));
        }
        let item = format!("struct {name}");
        let mut fields = vec![];
        let mut wire_fields = vec![];
        let mut refusal: Option<Refusal> = None;
        for d in decls {
            let StructDeclaration::Field(f) = &d.node else {
                continue;
            };
            let mut is_const = false;
            let mut specs = vec![];
            for s in &f.node.specifiers {
                match &s.node {
                    SpecifierQualifier::TypeSpecifier(t) => specs.push(&t.node),
                    SpecifierQualifier::TypeQualifier(q) => {
                        is_const |= q.node == TypeQualifier::Const
                    }
                    SpecifierQualifier::Extension(_) => {}
                }
            }
            let (base, base_const) = self.base_type(specs, is_const, None)?;
            for sd in &f.node.declarators {
                let Some(decl) = &sd.node.declarator else {
                    refusal.get_or_insert_with(|| {
                        Refusal::new(&item, RefusalReason::UnsupportedType, "anonymous member")
                    });
                    continue;
                };
                let (fname, ty) = self.declarator(&decl.node, base.clone(), base_const);
                let fname = fname.unwrap_or_default();
                if sd.node.bit_width.is_some() {
                    refusal.get_or_insert_with(|| {
                        Refusal::new(
                            &item,
                            RefusalReason::Bitfield,
                            format!("bit-field `{fname}` has an implementation-defined layout"),
                        )
                    });
                }
                match wire_type(&ty) {
                    Ok(w) => wire_fields.push(Field::new(fname.clone(), w)),
                    Err((reason, why)) => {
                        refusal.get_or_insert_with(|| {
                            Refusal::new(&item, reason, format!("field `{fname}`: {why}"))
                        });
                    }
                }
                fields.push(LiftedField {
                    name: fname,
                    ty: ty.vesper(),
                });
            }
        }
        let wire = match refusal {
            Some(r) => Err(r),
            None => Ok(Declaration::new(name.clone(), wire_fields)),
        };
        self.structs.push(LiftedStruct {
            name: name.clone(),
            fields,
            wire,
        });
        Ok(CType::Struct(name))
    }

    /// Apply a declarator's derived parts to a base type.
    fn declarator(
        &mut self,
        d: &Declarator,
        base: CType,
        base_const: bool,
    ) -> (Option<String>, CType) {
        let name = match &d.kind.node {
            DeclaratorKind::Identifier(i) => Some(i.node.name.clone()),
            DeclaratorKind::Abstract => None,
            DeclaratorKind::Declarator(inner) => {
                let is_fn = d
                    .derived
                    .iter()
                    .any(|x| matches!(x.node, DerivedDeclarator::Function(_)));
                let n = match &inner.node.kind.node {
                    DeclaratorKind::Identifier(i) => Some(i.node.name.clone()),
                    _ => None,
                };
                let why = if is_fn {
                    (
                        RefusalReason::FunctionPointer,
                        "function pointers and callbacks are unsupported",
                    )
                } else {
                    (
                        RefusalReason::UnsupportedType,
                        "parenthesized declarators are unsupported",
                    )
                };
                return (n, CType::Refused(why.0, why.1.into()));
            }
        };
        let mut ty = base;
        let mut cur_const = base_const;
        for part in &d.derived {
            ty = match &part.node {
                DerivedDeclarator::Pointer(quals) => {
                    let t = CType::Pointer {
                        pointee: Box::new(ty),
                        pointee_const: cur_const,
                    };
                    cur_const = quals.iter().any(|q| matches!(&q.node, PointerQualifier::TypeQualifier(t) if t.node == TypeQualifier::Const));
                    t
                }
                DerivedDeclarator::Array(a) => {
                    let len = match &a.node.size {
                        ArraySize::Unknown => None,
                        ArraySize::VariableExpression(e) | ArraySize::StaticExpression(e) => {
                            match eval(&e.node, &self.constants).and_then(|v| u64::try_from(v).ok())
                            {
                                Some(n) => Some(n),
                                None => {
                                    return (
                                        name,
                                        CType::Refused(
                                            RefusalReason::UnsupportedType,
                                            "array length is not a constant".into(),
                                        ),
                                    );
                                }
                            }
                        }
                        ArraySize::VariableUnknown => {
                            return (
                                name,
                                CType::Refused(
                                    RefusalReason::UnsupportedType,
                                    "variable-length array".into(),
                                ),
                            );
                        }
                    };
                    if matches!(ty, CType::Array { .. }) {
                        return (
                            name,
                            CType::Refused(
                                RefusalReason::UnsupportedType,
                                "multidimensional arrays are unsupported".into(),
                            ),
                        );
                    }
                    CType::Array {
                        elem: Box::new(ty),
                        len,
                        elem_const: cur_const,
                    }
                }
                DerivedDeclarator::Function(_)
                | DerivedDeclarator::KRFunction(_)
                | DerivedDeclarator::Block(_) => {
                    return (
                        name,
                        CType::Refused(
                            RefusalReason::FunctionPointer,
                            "function types in this position are unsupported".into(),
                        ),
                    );
                }
            };
        }
        (name, ty)
    }
}

fn wire_type(t: &CType) -> Result<WireType, (RefusalReason, String)> {
    Ok(match t {
        CType::Scalar(s) => {
            let bits = u16::from(s.bits());
            if s.signed() {
                WireType::Integer(Integer::signed(bits, Endian::Native))
            } else {
                WireType::Integer(Integer::unsigned(bits, Endian::Native))
            }
        }
        CType::CBool => WireType::Bool,
        CType::Struct(n) => WireType::Struct(n.clone()),
        CType::Array {
            elem, len: Some(n), ..
        } if matches!(**elem, CType::Scalar(Scalar::U8 | Scalar::I8)) => WireType::ByteArray(
            usize::try_from(*n)
                .map_err(|_| (RefusalReason::WireLayout, "array too large".to_string()))?,
        ),
        CType::Array { .. } => {
            return Err((
                RefusalReason::WireLayout,
                "only fixed-length byte arrays have wire layouts".into(),
            ));
        }
        CType::Enum(_) => {
            return Err((
                RefusalReason::ImplementationDefinedCarrier,
                "C enum field has an implementation-defined size".into(),
            ));
        }
        CType::Pointer { .. } => {
            return Err((
                RefusalReason::Aggregate,
                "pointer fields have no wire layout".into(),
            ));
        }
        CType::Void => return Err((RefusalReason::UnsupportedType, "void field".into())),
        CType::Refused(r, d) => return Err((*r, d.clone())),
    })
}

/// Map a C parameter list onto `.vsys` parameters and Vesper parameters.
fn lift_function(
    name: &str,
    params: Vec<(String, CType)>,
    decayed: &[bool],
    ret: CType,
) -> Result<LiftedFunction, Refusal> {
    let item = format!("fn {name}");
    let refuse = |reason, detail: String| Refusal::new(&item, reason, detail);
    let mut sys = vec![];
    let mut lifted = vec![];
    let mut assumptions = vec![];
    let safe = crate::synth::safe_idents(params.iter().map(|(n, _)| n.as_str()));
    let mut i = 0;
    while i < params.len() {
        let (_, ty) = &params[i];
        let pname = safe[i].clone();
        match ty {
            CType::Scalar(s) => {
                sys.push((pname.clone(), SysParam::Value(SysValue::Scalar(*s))));
                lifted.push(LiftedParam {
                    name: pname,
                    mode: Mode::Value,
                    footprint: None,
                    ty: s.name().into(),
                });
            }
            CType::Pointer {
                pointee,
                pointee_const,
            } => {
                let next_is_len =
                    matches!(params.get(i + 1), Some((_, CType::Scalar(Scalar::Usize))));
                let single = match &**pointee {
                    CType::Scalar(e) => SysPointee::Scalar(*e),
                    CType::Struct(n) if next_is_len => {
                        return Err(refuse(
                            RefusalReason::Aggregate,
                            format!(
                                "parameter `{pname}` points to struct `{n}` and is followed by a size_t: arrays of structs are not modelled by .vsys"
                            ),
                        ));
                    }
                    CType::Struct(n) => SysPointee::Struct(n.clone()),
                    CType::Refused(r, d) => {
                        return Err(refuse(*r, format!("parameter `{pname}`: {d}")));
                    }
                    _ => {
                        return Err(refuse(
                            RefusalReason::OpaquePointer,
                            format!("parameter `{pname}` has an opaque, void or nested pointee"),
                        ));
                    }
                };
                if let (SysPointee::Scalar(e), true) = (&single, next_is_len) {
                    let e = *e;
                    let access = if *pointee_const {
                        Access::In
                    } else {
                        Access::Inout
                    };
                    let len_name = &params[i + 1].0;
                    assumptions.push(format!(
                        "`{pname}` + `{len_name}` lifted to a nocapture {} slice: a C header cannot show that the callee does not retain the pointer",
                        if *pointee_const { "shared" } else { "unique" }
                    ));
                    sys.push((pname.clone(), SysParam::Slice { element: e, access }));
                    lifted.push(LiftedParam {
                        name: pname,
                        mode: if *pointee_const {
                            Mode::In
                        } else {
                            Mode::Inout
                        },
                        footprint: None,
                        ty: format!("CSlice[{}]", e.name()),
                    });
                    i += 2;
                    continue;
                }
                if decayed.get(i).copied().unwrap_or(false) {
                    return Err(refuse(
                        RefusalReason::ArrayWithoutLength,
                        format!(
                            "array parameter `{pname}` is not followed by a size_t length; its extent is unknown"
                        ),
                    ));
                }
                if matches!(single, SysPointee::Scalar(Scalar::U8 | Scalar::I8)) {
                    return Err(refuse(
                        RefusalReason::ArrayWithoutLength,
                        format!(
                            "byte pointer `{pname}` has no size_t length: a buffer or string of unknown extent, not one element"
                        ),
                    ));
                }
                let access = if *pointee_const {
                    PointerAccess::In
                } else {
                    PointerAccess::Inout
                };
                assumptions.push(format!(
                    "`{pname}` lifted to a non-null nocapture {} pointer to one `{}`: a C header cannot show non-null, single-element or non-retention",
                    access.keyword(),
                    single.name()
                ));
                lifted.push(LiftedParam {
                    name: pname.clone(),
                    mode: if *pointee_const {
                        Mode::In
                    } else {
                        Mode::Inout
                    },
                    footprint: None,
                    ty: single.name().to_owned(),
                });
                sys.push((
                    pname,
                    SysParam::Pointer {
                        pointee: single,
                        access,
                        nullable: false,
                    },
                ));
            }
            CType::Array { .. } => unreachable!("arrays decay before lifting"),
            CType::CBool | CType::Enum(_) => {
                return Err(refuse(
                    RefusalReason::ImplementationDefinedCarrier,
                    format!(
                        "parameter `{pname}` uses a C `_Bool`/enum whose ABI carrier is implementation-defined; use a fixed-width integer"
                    ),
                ));
            }
            CType::Struct(n) => {
                return Err(refuse(
                    RefusalReason::Aggregate,
                    format!("parameter `{pname}` passes struct `{n}` by value"),
                ));
            }
            CType::Void => {
                return Err(refuse(
                    RefusalReason::UnsupportedType,
                    format!("parameter `{pname}` has type void"),
                ));
            }
            CType::Refused(r, d) => return Err(refuse(*r, format!("parameter `{pname}`: {d}"))),
        }
        i += 1;
    }
    if let Some(why) = crate::synth::aliasing_pair(&sys) {
        return Err(refuse(RefusalReason::AliasingPointers, why));
    }
    let (result, sys_result) = match ret {
        CType::Void => ("ForeignVal[Unit]".to_string(), SysValue::Unit),
        CType::Scalar(s) => (format!("ForeignVal[{}]", s.name()), SysValue::Scalar(s)),
        CType::Pointer { pointee, .. } => match *pointee {
            CType::Scalar(e) => (
                format!("ForeignVal[NullablePtr[{}]]", e.name()),
                SysValue::Address {
                    pointee: e,
                    nullable: true,
                },
            ),
            _ => {
                return Err(refuse(
                    RefusalReason::OpaquePointer,
                    "returns a non-scalar pointer".into(),
                ));
            }
        },
        CType::Struct(n) => {
            return Err(refuse(
                RefusalReason::Aggregate,
                format!("returns struct `{n}` by value"),
            ));
        }
        CType::CBool | CType::Enum(_) => {
            return Err(refuse(
                RefusalReason::ImplementationDefinedCarrier,
                "returns a C `_Bool`/enum".into(),
            ));
        }
        CType::Array { .. } => {
            return Err(refuse(
                RefusalReason::UnsupportedType,
                "returns an array".into(),
            ));
        }
        CType::Refused(r, d) => return Err(refuse(r, format!("return type: {d}"))),
    };
    Ok(LiftedFunction {
        name: name.to_owned(),
        params: lifted,
        result,
        sys: Ok(SysSignature {
            params: sys,
            result: sys_result,
        }),
        assumptions,
    })
}

/// Analyse one C header. `header` names it in the result (for `.vsys`
/// `header = "..."`); `source` is its text.
pub fn analyze_header(
    header: &str,
    source: &str,
    limits: Limits,
) -> Result<Migration, MigrateError> {
    if source.len() > limits.source_bytes {
        return Err(MigrateError::new(
            ErrorKind::Limit,
            None,
            "header byte budget exceeded",
        ));
    }
    crate::check_nesting(source, limits.nesting)?;
    let text = filter_directives(&strip_comments(source)?)?;
    for (name, _) in PRELUDE {
        if redefines(&text, name) {
            return Err(MigrateError::new(
                ErrorKind::Duplicate,
                None,
                format!("header redefines built-in type `{name}`"),
            ));
        }
    }
    let mut full = String::with_capacity(text.len() + 512);
    for (name, def) in PRELUDE {
        full.push_str(&format!("typedef {def} {name}; "));
    }
    full.push('\n');
    full.push_str(&text);
    let config = Config {
        flavor: Flavor::StdC11,
        ..Config::default()
    };
    let parsed = parse_preprocessed(&config, full).map_err(|e| {
        let mut expected: Vec<&str> = e.expected.iter().copied().collect();
        expected.sort_unstable();
        MigrateError::new(
            ErrorKind::Syntax,
            Some(e.line.saturating_sub(1).max(1)),
            format!(
                "C syntax error at column {}; expected one of {}",
                e.column,
                expected.join(" ")
            ),
        )
    })?;
    let mut ctx = Ctx {
        typedefs: BTreeMap::new(),
        structs: vec![],
        struct_names: BTreeSet::new(),
        enums: vec![],
        enum_names: BTreeSet::new(),
        constants: BTreeMap::new(),
        functions: vec![],
        refusals: vec![],
        items: 0,
        limits,
        anonymous: 0,
    };
    let mut fn_names = BTreeSet::new();
    for ext in parsed.unit.0.iter().skip(PRELUDE.len()) {
        let (specifiers, declarators): (
            &[lang_c::span::Node<DeclarationSpecifier>],
            Vec<&Declarator>,
        ) = match &ext.node {
            ExternalDeclaration::Declaration(d) => (
                &d.node.specifiers,
                d.node
                    .declarators
                    .iter()
                    .map(|i| &i.node.declarator.node)
                    .collect(),
            ),
            ExternalDeclaration::FunctionDefinition(f) => {
                (&f.node.specifiers, vec![&f.node.declarator.node])
            }
            ExternalDeclaration::StaticAssert(_) => continue,
        };
        let mut storage = None;
        let mut is_const = false;
        let mut types = vec![];
        for s in specifiers {
            match &s.node {
                DeclarationSpecifier::StorageClass(c) => storage = Some(c.node.clone()),
                DeclarationSpecifier::TypeSpecifier(t) => types.push(&t.node),
                DeclarationSpecifier::TypeQualifier(q) => {
                    is_const |= q.node == TypeQualifier::Const
                }
                _ => {}
            }
        }
        // An anonymous struct/enum in a typedef takes the typedef's name.
        let hint = match (&storage, declarators.as_slice()) {
            (Some(StorageClassSpecifier::Typedef), [d]) if d.derived.is_empty() => {
                match &d.kind.node {
                    DeclaratorKind::Identifier(i) => Some(i.node.name.clone()),
                    _ => None,
                }
            }
            _ => None,
        };
        let (base, base_const) = ctx.base_type(types, is_const, hint.as_deref())?;
        for d in declarators {
            if let Some(DerivedDeclarator::KRFunction(_)) = d.derived.last().map(|x| &x.node) {
                let name = match &d.kind.node {
                    DeclaratorKind::Identifier(i) => i.node.name.clone(),
                    _ => "<declarator>".into(),
                };
                ctx.count_item()?;
                ctx.refusals.push(Refusal::new(
                    format!("fn {name}"),
                    RefusalReason::UnsupportedItem,
                    "`()` or K&R parameters declare no prototype in C; write `(void)`",
                ));
                continue;
            }
            let is_fn = matches!(
                d.derived.last().map(|x| &x.node),
                Some(DerivedDeclarator::Function(_))
            );
            if storage == Some(StorageClassSpecifier::Typedef) {
                ctx.count_item()?;
                let (name, ty) = ctx.declarator(d, base.clone(), base_const);
                let Some(name) = name else { continue };
                if is_fn {
                    ctx.typedefs.insert(
                        name,
                        CType::Refused(RefusalReason::FunctionPointer, "function typedef".into()),
                    );
                } else if ctx.typedefs.insert(name.clone(), ty).is_some() {
                    return Err(MigrateError::new(
                        ErrorKind::Duplicate,
                        None,
                        format!("typedef `{name}` defined twice"),
                    ));
                }
                continue;
            }
            if !is_fn {
                let name = match &d.kind.node {
                    DeclaratorKind::Identifier(i) => i.node.name.clone(),
                    _ => "<declarator>".into(),
                };
                ctx.count_item()?;
                ctx.refusals.push(Refusal::new(
                    format!("global {name}"),
                    RefusalReason::UnsupportedItem,
                    "global variables are not part of a Bridge sys-spec",
                ));
                continue;
            }
            ctx.count_item()?;
            // Split off the function declarator; earlier parts apply to the return.
            let Some((last, before)) = d.derived.split_last() else {
                continue;
            };
            let DerivedDeclarator::Function(fd) = &last.node else {
                continue;
            };
            let ret_decl = Declarator {
                kind: d.kind.clone(),
                derived: before.to_vec(),
                extensions: vec![],
            };
            let (name, ret) = ctx.declarator(&ret_decl, base.clone(), base_const);
            let Some(name) = name else { continue };
            if !fn_names.insert(name.clone()) {
                return Err(MigrateError::new(
                    ErrorKind::Duplicate,
                    None,
                    format!("function `{name}` declared twice"),
                ));
            }
            let item = format!("fn {name}");
            if storage == Some(StorageClassSpecifier::Static) {
                ctx.refusals.push(Refusal::new(
                    item,
                    RefusalReason::UnsupportedItem,
                    "static functions have no linkable symbol",
                ));
                continue;
            }
            if fd.node.ellipsis == Ellipsis::Some {
                ctx.refusals.push(Refusal::new(
                    item,
                    RefusalReason::Variadic,
                    "variadic functions have no fixed ABI signature",
                ));
                continue;
            }
            if fd.node.parameters.len() > limits.parameters {
                return Err(MigrateError::new(
                    ErrorKind::Limit,
                    None,
                    format!("function `{name}` exceeds the parameter budget"),
                ));
            }
            let mut params = vec![];
            let mut decayed = vec![];
            let mut void_list = false;
            for (index, p) in fd.node.parameters.iter().enumerate() {
                let mut pc = false;
                let mut pt = vec![];
                for s in &p.node.specifiers {
                    match &s.node {
                        DeclarationSpecifier::TypeSpecifier(t) => pt.push(&t.node),
                        DeclarationSpecifier::TypeQualifier(q) => {
                            pc |= q.node == TypeQualifier::Const
                        }
                        _ => {}
                    }
                }
                let (pbase, pconst) = ctx.base_type(pt, pc, None)?;
                let (pname, mut pty) = match &p.node.declarator {
                    Some(decl) => ctx.declarator(&decl.node, pbase, pconst),
                    None => (None, pbase),
                };
                if fd.node.parameters.len() == 1 && pname.is_none() && pty == CType::Void {
                    void_list = true;
                    break;
                }
                // Array parameters decay to pointers.
                let mut is_array = false;
                if let CType::Array {
                    elem, elem_const, ..
                } = pty
                {
                    is_array = true;
                    pty = CType::Pointer {
                        pointee: elem,
                        pointee_const: elem_const,
                    };
                }
                decayed.push(is_array);
                params.push((pname.unwrap_or_else(|| format!("arg{index}")), pty));
            }
            if !void_list && params.is_empty() {
                ctx.refusals.push(Refusal::new(
                    item,
                    RefusalReason::UnsupportedItem,
                    "`()` declares no prototype in C; write `(void)`",
                ));
                continue;
            }
            let mut seen = BTreeSet::new();
            if let Some((dup, _)) = params.iter().find(|(n, _)| !seen.insert(n.clone())) {
                return Err(MigrateError::new(
                    ErrorKind::Duplicate,
                    None,
                    format!("function `{name}` repeats parameter `{dup}`"),
                ));
            }
            match lift_function(&name, params, &decayed, ret) {
                Ok(f) => ctx.functions.push(f),
                Err(r) => ctx.refusals.push(r),
            }
        }
    }
    // Wire layouts must compile through vesper_ffi; padding and unknown
    // struct references become refusals here.
    crate::synth::check_wire_layouts(&mut ctx.structs);
    let mut m = Migration {
        origin: Origin::CHeader {
            header: header.to_owned(),
        },
        functions: ctx.functions,
        structs: ctx.structs,
        enums: ctx.enums,
        refusals: ctx.refusals,
    };
    // A struct pointee needs a checked wire layout; otherwise the whole C
    // function is refused, as other unliftable C signatures are.
    crate::synth::check_pointer_pointees(&mut m);
    let (keep, refused): (Vec<_>, Vec<_>) = m.functions.into_iter().partition(|f| f.sys.is_ok());
    m.functions = keep;
    m.refusals
        .extend(refused.into_iter().filter_map(|f| f.sys.err()));
    Ok(m)
}

/// Whether `text` contains a typedef whose declared name is `name`.
fn redefines(text: &str, name: &str) -> bool {
    text.split(';').any(|stmt| {
        let s = stmt.trim();
        s.starts_with("typedef")
            && s.rsplit(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .next()
                == Some(name)
    })
}
