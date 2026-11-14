//! Vesper Vector source integration (design §14): fixed-width vector types
//! `Vec[T, N]` and `Mask[T, N]`, the ISA capability `eff [Isa[P]]`, and the
//! reference semantics of every vector operation.
//!
//! * **Types.** `Vec[T, N]` holds `N` lanes of `T` and `Mask[T, N]` holds one
//!   boolean per lane of the same shape. `T` is one of the scalar types the
//!   language has (`u8`, `u32`, `u64`, `i64`, `f64`) and the vector is 128,
//!   256 or 512 bits wide. Both are `copy` and have no source fields.
//! * **Operations** are reserved path intrinsics (`Vec::add`, `Mask::count`,
//!   `Isa::supported`, ...; see [`Intrinsic`]). They take their operands by
//!   value and are total: lane arithmetic wraps, float arithmetic is IEEE,
//!   loads past the end of a string read zero lanes.
//! * **`Isa[P]`** is a stateless, erased capability effect with one profile
//!   argument (`Baseline128`, `Avx2`, `Avx512`, `Sve2`, `Rvv1`). A function
//!   whose body performs a vector operation must carry a profile wide enough
//!   for it (128 bits: any profile; 256 bits: `Avx2` or `Avx512`; 512 bits:
//!   `Avx512`). A call to a function carrying `Isa[Q]` needs a profile `P`
//!   with `Q ⊑ P` in the lattice `Baseline128 ⊑ Avx2 ⊑ Avx512`,
//!   `Baseline128 ⊑ Sve2`, `Baseline128 ⊑ Rvv1`. Plain code discharges the
//!   requirement only through the checked dispatch
//!   `if Isa::supported[P]() { ... } else { ... }`, whose then-branch holds
//!   `Isa[P]`. No handler can discharge `Isa`, and it is never requested from
//!   the caller's row the way an ordinary effect is.
//! * **Dispatch.** `Isa::supported[P]()` is true when the code generator of
//!   this execution targets `P`: the native backend folds it from the
//!   Cranelift ISA flags it compiles with (the host's flags for the JIT, the
//!   target's for object emission), and the reference evaluator asks the
//!   same host ISA builder, so both select the same branch on one host.
//!
//! These are source typing rules checked on the surface HIR. They are not a
//! proof about the emitted machine code.
use crate::ast::*;
use crate::check::World;
use crate::runtime::Value;
use serde::Serialize;
use std::collections::BTreeMap;

#[path = "vector/eval.rs"]
mod eval;
pub use eval::evaluate;

/// The reserved ISA capability effect.
pub const ISA: &str = "Isa";
/// Bits of one native SIMD register chunk: every vector lowers as 1, 2 or 4
/// 128-bit Cranelift vectors.
pub const CHUNK_BITS: u32 = 128;

/// A vector lane type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum Elem {
    U8,
    U32,
    U64,
    I64,
    F64,
}
impl Elem {
    pub const ALL: [Elem; 5] = [Elem::U8, Elem::U32, Elem::U64, Elem::I64, Elem::F64];
    pub fn from_type(t: &Type) -> Option<Self> {
        match t {
            Type::Named(n, a) if a.is_empty() => Self::from_name(n),
            _ => None,
        }
    }
    pub fn from_name(n: &str) -> Option<Self> {
        Some(match n {
            "u8" => Self::U8,
            "u32" => Self::U32,
            "u64" => Self::U64,
            "i64" => Self::I64,
            "f64" => Self::F64,
            _ => return None,
        })
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::U8 => "u8",
            Self::U32 => "u32",
            Self::U64 => "u64",
            Self::I64 => "i64",
            Self::F64 => "f64",
        }
    }
    pub fn bits(self) -> u32 {
        match self {
            Self::U8 => 8,
            Self::U32 => 32,
            _ => 64,
        }
    }
    pub fn is_float(self) -> bool {
        self == Self::F64
    }
    pub fn is_signed(self) -> bool {
        self == Self::I64
    }
    /// All-ones value of one lane, as raw bits.
    pub fn mask(self) -> u64 {
        if self.bits() == 64 {
            u64::MAX
        } else {
            (1u64 << self.bits()) - 1
        }
    }
    pub fn ty(self) -> Type {
        Type::named(self.name())
    }
}

/// `Vec[T, N]` or `Mask[T, N]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum Kind {
    Vec,
    Mask,
}
impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Vec => "Vec",
            Self::Mask => "Mask",
        }
    }
}

/// The lane type and count of a vector or mask.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct Shape {
    pub elem: Elem,
    pub lanes: u16,
}
impl Shape {
    /// A shape of 128, 256 or 512 bits.
    pub fn new(elem: Elem, lanes: u16) -> Option<Self> {
        let bits = elem.bits() * u32::from(lanes);
        matches!(bits, 128 | 256 | 512).then_some(Self { elem, lanes })
    }
    pub fn bits(self) -> u32 {
        self.elem.bits() * u32::from(self.lanes)
    }
    pub fn bytes(self) -> u32 {
        self.bits() / 8
    }
    /// Number of 128-bit chunks.
    pub fn chunks(self) -> usize {
        (self.bits() / CHUNK_BITS) as usize
    }
    pub fn lanes_per_chunk(self) -> u16 {
        (CHUNK_BITS / self.elem.bits()) as u16
    }
    pub fn ty(self, kind: Kind) -> Type {
        Type::Named(
            kind.name().into(),
            vec![self.elem.ty(), Type::Named(self.lanes.to_string(), vec![])],
        )
    }
    pub fn vec_type(self) -> Type {
        self.ty(Kind::Vec)
    }
    pub fn mask_type(self) -> Type {
        self.ty(Kind::Mask)
    }
    /// The narrowest profile whose fixed registers hold this shape.
    pub fn required_profile(self) -> Profile {
        match self.bits() {
            128 => Profile::Baseline128,
            256 => Profile::Avx2,
            _ => Profile::Avx512,
        }
    }
}
impl std::fmt::Display for Shape {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}, {}]", self.elem.name(), self.lanes)
    }
}

/// Whether `t` is spelled as a vector or mask type (`Vec[_, _]` or
/// `Mask[_, _]` whose second argument is a lane count), valid or not.
pub fn is_vector_spelling(t: &Type) -> bool {
    matches!(t, Type::Named(n, args)
        if (n == "Vec" || n == "Mask")
            && args.len() == 2
            && matches!(&args[1], Type::Named(d, a) if a.is_empty() && is_lane_count(d)))
}
fn is_lane_count(d: &str) -> bool {
    !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit())
}
/// The kind and shape of a valid vector or mask type.
pub fn shape_of(t: &Type) -> Option<(Kind, Shape)> {
    let Type::Named(n, args) = t else {
        return None;
    };
    let kind = match n.as_str() {
        "Vec" => Kind::Vec,
        "Mask" => Kind::Mask,
        _ => return None,
    };
    let [elem, Type::Named(count, none)] = args.as_slice() else {
        return None;
    };
    if !none.is_empty() || !is_lane_count(count) {
        return None;
    }
    let lanes = count.parse::<u16>().ok()?;
    Some((kind, Shape::new(Elem::from_type(elem)?, lanes)?))
}
/// Why a vector-spelled type is not a valid shape, if it is not.
pub fn type_error(t: &Type) -> Option<String> {
    if !is_vector_spelling(t) || shape_of(t).is_some() {
        return None;
    }
    let Type::Named(n, args) = t else {
        return None;
    };
    Some(if Elem::from_type(&args[0]).is_none() {
        format!(
            "{n} lanes must be u8, u32, u64, i64 or f64, found {}",
            args[0]
        )
    } else {
        format!("{t} is not a 128-, 256- or 512-bit vector shape")
    })
}

/// The native value layout of a vector or mask: its packed lane bytes as
/// eight-byte words (lane `i` occupies bytes `i * width ..`, little endian).
pub fn layout_words(t: &Type) -> Option<u32> {
    shape_of(t).map(|(_, s)| s.bytes() / 8)
}

/// An ISA profile (§14.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum Profile {
    Baseline128,
    Avx2,
    Avx512,
    Sve2,
    Rvv1,
}
impl Profile {
    pub const ALL: [Profile; 5] = [
        Profile::Baseline128,
        Profile::Avx2,
        Profile::Avx512,
        Profile::Sve2,
        Profile::Rvv1,
    ];
    pub fn from_name(n: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == n)
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Baseline128 => "Baseline128",
            Self::Avx2 => "Avx2",
            Self::Avx512 => "Avx512",
            Self::Sve2 => "Sve2",
            Self::Rvv1 => "Rvv1",
        }
    }
    /// The profile lattice: `self ⊑ other`.
    pub fn le(self, other: Self) -> bool {
        self == other || self == Self::Baseline128 || (self == Self::Avx2 && other == Self::Avx512)
    }
    /// Widest fixed-width vector this profile's code may use. The scalable
    /// profiles grant 128-bit fixed vectors only; their length-agnostic
    /// `VVec` layer is not implemented.
    pub fn fixed_bits(self) -> u32 {
        match self {
            Self::Avx2 => 256,
            Self::Avx512 => 512,
            _ => 128,
        }
    }
    /// The Cranelift ISA flags a code generator must have enabled to target
    /// this profile, and the architecture they belong to. `None` when
    /// Cranelift 0.125 cannot target the profile at all.
    pub fn cranelift_requirements(self) -> Option<(&'static str, &'static [&'static str])> {
        match self {
            // SSE2 and NEON are architectural on x86_64 and AArch64, and
            // Cranelift lowers every operation here with them alone.
            Self::Baseline128 => Some(("any", &[])),
            Self::Avx2 => Some(("x86_64", &["has_avx2", "has_fma"])),
            Self::Avx512 => Some(("x86_64", &["has_avx512f", "has_avx512vl"])),
            // Cranelift 0.125 has no SVE2 or RVV code generation.
            Self::Sve2 | Self::Rvv1 => None,
        }
    }
}

/// Whether `name` is the reserved `Isa` capability effect.
pub fn is_isa_effect(name: &str) -> bool {
    name == ISA
}
/// The profile of an `Isa[P]` row entry.
pub fn profile_of(effect: &Effect) -> Option<Profile> {
    if !is_isa_effect(&effect.name) {
        return None;
    }
    match effect.args.as_slice() {
        [Type::Named(n, a)] if a.is_empty() => Profile::from_name(n),
        _ => None,
    }
}
/// Profiles carried by an effect row.
pub fn row_profiles(effects: &Effects) -> Vec<Profile> {
    effects.instances.iter().filter_map(profile_of).collect()
}
/// Diagnostic for a malformed `Isa` row entry.
pub(crate) fn isa_row_entry(effect: &Effect, span: Span) -> Option<Diagnostic> {
    if profile_of(effect).is_some() {
        return None;
    }
    let found = effect
        .args
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    Some(
        Diagnostic::new(
            "E1404",
            format!("'Isa' takes one ISA profile argument, found [{found}]"),
            span,
        )
        .with_help("use Isa[Baseline128], Isa[Avx2], Isa[Avx512], Isa[Sve2] or Isa[Rvv1]"),
    )
}

/// Whether a code generator for `isa` targets profile `p`.
pub fn isa_enabled(isa: &dyn cranelift_codegen::isa::TargetIsa, p: Profile) -> bool {
    let Some((arch, flags)) = p.cranelift_requirements() else {
        return false;
    };
    let architecture = isa.triple().architecture;
    let arch_ok = match arch {
        "x86_64" => architecture == target_lexicon::Architecture::X86_64,
        _ => matches!(
            architecture,
            target_lexicon::Architecture::X86_64 | target_lexicon::Architecture::Aarch64(_)
        ),
    };
    if !arch_ok {
        return false;
    }
    let enabled = isa.isa_flags();
    flags.iter().all(|flag| {
        enabled
            .iter()
            .any(|v| v.name == *flag && v.as_bool() == Some(true))
    })
}
/// [`isa_enabled`] for the host code generator the JIT uses.
pub fn host_enabled(p: Profile) -> bool {
    use std::sync::OnceLock;
    static HOST: OnceLock<BTreeMap<Profile, bool>> = OnceLock::new();
    HOST.get_or_init(|| {
        let isa = cranelift_native::builder().ok().and_then(|b| {
            b.finish(cranelift_codegen::settings::Flags::new(
                cranelift_codegen::settings::builder(),
            ))
            .ok()
        });
        Profile::ALL
            .into_iter()
            .map(|p| (p, isa.as_deref().is_some_and(|isa| isa_enabled(isa, p))))
            .collect()
    })
    .get(&p)
    .copied()
    .unwrap_or(false)
}

/// Lane-wise binary operations producing a vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Min,
    Max,
    And,
    Or,
    Xor,
    SaturatingAdd,
    SaturatingSub,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum ShiftOp {
    Shl,
    Shr,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum MaskOp {
    And,
    Or,
    Xor,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum ReduceOp {
    Add,
    Xor,
    Or,
    And,
    Min,
    Max,
}

/// A reserved vector intrinsic, before its shape is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum Intrinsic {
    /// `Vec::splat[T, N](x)`.
    Splat,
    /// `Vec::from[T, N](x0, ..., x(N-1))`.
    From,
    /// `Vec::add(a, b)` and the other lane-wise binary operations.
    Binary(BinOp),
    /// `Vec::not(a)` (integer lanes).
    Not,
    /// `Vec::shl(a, k)`, `Vec::shr(a, k)`: `k: u64` modulo the lane width.
    Shift(ShiftOp),
    /// `Vec::eq(a, b)` ...: a `Mask`.
    Compare(CmpOp),
    /// `Vec::select(mask, a, b)`: lanes of `a` where the mask is set.
    Select,
    /// `Mask::and(a, b)`, `Mask::or`, `Mask::xor`.
    MaskBinary(MaskOp),
    /// `Mask::not(m)`.
    MaskNot,
    /// `Mask::count(m) -> u64`: set lanes.
    MaskCount,
    /// `Mask::bitmask(m) -> u64`: lane `i` is bit `i`.
    MaskBitmask,
    /// `Mask::any(m) -> bool`.
    MaskAny,
    /// `Mask::all(m) -> bool`.
    MaskAll,
    /// `Vec::reduce_add(v) -> T` ...: lanes folded in index order.
    Reduce(ReduceOp),
    /// `Vec::extract(v, i) -> T` with a literal lane index.
    Extract,
    /// `Vec::replace(v, i, x)` with a literal lane index.
    Replace,
    /// `Vec::shuffle(v, i0, ..., i(N-1))`: compile-time checked swizzle.
    Shuffle,
    /// `Vec::load_masked[u8, N](s, offset)`: bytes of `s` from `offset`,
    /// zero past its end.
    LoadMasked,
    /// `Mask::in_bounds[u8, N](s, offset)`: lanes `load_masked` reads from `s`.
    InBounds,
    /// `Mask::active[T, N](offset, len)`: lane `i` is set iff
    /// `offset + i < len` (the design's `P::active_mask`).
    Active,
    /// `Isa::supported[P]() -> bool`.
    IsaSupported,
}
impl Intrinsic {
    /// The intrinsic a reserved path names, `Err(())` for an unknown name in
    /// a reserved namespace, `None` outside them.
    pub fn lookup(name: &str) -> Option<Result<Self, ()>> {
        let (space, op) = name.split_once("::")?;
        use Intrinsic as I;
        Some(Ok(match (space, op) {
            ("Vec", "splat") => I::Splat,
            ("Vec", "from") => I::From,
            ("Vec", "add") => I::Binary(BinOp::Add),
            ("Vec", "sub") => I::Binary(BinOp::Sub),
            ("Vec", "mul") => I::Binary(BinOp::Mul),
            ("Vec", "div") => I::Binary(BinOp::Div),
            ("Vec", "min") => I::Binary(BinOp::Min),
            ("Vec", "max") => I::Binary(BinOp::Max),
            ("Vec", "and") => I::Binary(BinOp::And),
            ("Vec", "or") => I::Binary(BinOp::Or),
            ("Vec", "xor") => I::Binary(BinOp::Xor),
            ("Vec", "saturating_add") => I::Binary(BinOp::SaturatingAdd),
            ("Vec", "saturating_sub") => I::Binary(BinOp::SaturatingSub),
            ("Vec", "not") => I::Not,
            ("Vec", "shl") => I::Shift(ShiftOp::Shl),
            ("Vec", "shr") => I::Shift(ShiftOp::Shr),
            ("Vec", "eq") => I::Compare(CmpOp::Eq),
            ("Vec", "ne") => I::Compare(CmpOp::Ne),
            ("Vec", "lt") => I::Compare(CmpOp::Lt),
            ("Vec", "le") => I::Compare(CmpOp::Le),
            ("Vec", "gt") => I::Compare(CmpOp::Gt),
            ("Vec", "ge") => I::Compare(CmpOp::Ge),
            ("Vec", "select") => I::Select,
            ("Vec", "reduce_add") => I::Reduce(ReduceOp::Add),
            ("Vec", "reduce_xor") => I::Reduce(ReduceOp::Xor),
            ("Vec", "reduce_or") => I::Reduce(ReduceOp::Or),
            ("Vec", "reduce_and") => I::Reduce(ReduceOp::And),
            ("Vec", "reduce_min") => I::Reduce(ReduceOp::Min),
            ("Vec", "reduce_max") => I::Reduce(ReduceOp::Max),
            ("Vec", "extract") => I::Extract,
            ("Vec", "replace") => I::Replace,
            ("Vec", "shuffle") => I::Shuffle,
            ("Vec", "load_masked") => I::LoadMasked,
            ("Mask", "and") => I::MaskBinary(MaskOp::And),
            ("Mask", "or") => I::MaskBinary(MaskOp::Or),
            ("Mask", "xor") => I::MaskBinary(MaskOp::Xor),
            ("Mask", "not") => I::MaskNot,
            ("Mask", "count") => I::MaskCount,
            ("Mask", "bitmask") => I::MaskBitmask,
            ("Mask", "any") => I::MaskAny,
            ("Mask", "all") => I::MaskAll,
            ("Mask", "in_bounds") => I::InBounds,
            ("Mask", "active") => I::Active,
            ("Isa", "supported") => I::IsaSupported,
            ("Vec" | "Mask" | "Isa", _) => return Some(Err(())),
            _ => return None,
        }))
    }
    /// Whether the shape comes from the call's `[T, N]` type arguments.
    pub fn takes_shape_arguments(self) -> bool {
        matches!(
            self,
            Self::Splat | Self::From | Self::LoadMasked | Self::InBounds | Self::Active
        )
    }
    /// Argument positions that must be integer literals (lane indices).
    pub fn constant_positions(self, args: usize) -> std::ops::Range<usize> {
        match self {
            Self::Extract | Self::Replace => 1..2,
            Self::Shuffle => 1..args.max(1),
            _ => 0..0,
        }
    }
    /// Whether the result is a mask.
    pub fn mask_result(self) -> bool {
        matches!(
            self,
            Self::Compare(_) | Self::MaskBinary(_) | Self::MaskNot | Self::InBounds | Self::Active
        )
    }
    /// Whether the (first) operand is a mask.
    pub fn mask_operand(self) -> bool {
        matches!(
            self,
            Self::Select
                | Self::MaskBinary(_)
                | Self::MaskNot
                | Self::MaskCount
                | Self::MaskBitmask
                | Self::MaskAny
                | Self::MaskAll
        )
    }
    /// Why this intrinsic is not defined for lanes of `elem`, if it is not.
    pub fn lane_error(self, elem: Elem) -> Option<&'static str> {
        use Intrinsic as I;
        match self {
            I::Binary(BinOp::Mul) if elem == Elem::U8 => {
                Some("Vec::mul is not defined for u8 lanes")
            }
            I::Binary(BinOp::Div) if !elem.is_float() => {
                Some("Vec::div is defined for f64 lanes only")
            }
            I::Binary(BinOp::SaturatingAdd | BinOp::SaturatingSub) if elem != Elem::U8 => {
                Some("saturating lane arithmetic is defined for u8 lanes only")
            }
            I::Binary(BinOp::And | BinOp::Or | BinOp::Xor)
            | I::Not
            | I::Shift(_)
            | I::Reduce(ReduceOp::And | ReduceOp::Or | ReduceOp::Xor)
                if elem.is_float() =>
            {
                Some("bitwise lane operations are not defined for f64 lanes")
            }
            I::LoadMasked | I::InBounds if elem != Elem::U8 => {
                Some("string loads produce u8 lanes")
            }
            _ => None,
        }
    }
}

/// A vector operation of the Place IR (`Op::Vector`): an intrinsic at a
/// concrete shape, with its literal lane indices resolved. Its IR operands
/// are the call's non-literal arguments in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum VectorOp {
    Lanes {
        intrinsic: Intrinsic,
        shape: Shape,
        /// Literal lane indices (`extract`, `replace`, `shuffle`).
        constants: Vec<u16>,
    },
    IsaSupported(Profile),
}
impl VectorOp {
    /// Resolve a checked call: `types` are its explicit type arguments,
    /// `arg_types` the checked types of its arguments and `result` its type.
    pub fn resolve(
        name: &str,
        types: &[Type],
        args: &[&Expr],
        arg_types: &[Type],
        result: &Type,
    ) -> Option<Self> {
        let intrinsic = Intrinsic::lookup(name)?.ok()?;
        if intrinsic == Intrinsic::IsaSupported {
            let [Type::Named(p, a)] = types else {
                return None;
            };
            return a
                .is_empty()
                .then(|| Profile::from_name(p))
                .flatten()
                .map(Self::IsaSupported);
        }
        let shape = shape_of(result)
            .or_else(|| arg_types.first().and_then(shape_of))
            .or_else(|| match types {
                [elem, Type::Named(n, _)] => {
                    Shape::new(Elem::from_type(elem)?, n.parse().ok()?).map(|s| (Kind::Vec, s))
                }
                _ => None,
            })?
            .1;
        let constants = args[intrinsic.constant_positions(args.len())]
            .iter()
            .map(|e| match &e.kind {
                ExprKind::Literal(Literal::Int(n)) => u16::try_from(*n).ok(),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Self::Lanes {
            intrinsic,
            shape,
            constants,
        })
    }
    pub fn shape(&self) -> Option<Shape> {
        match self {
            Self::Lanes { shape, .. } => Some(*shape),
            Self::IsaSupported(_) => None,
        }
    }
}
/// Whether the argument at `index` of a call to `intrinsic` with `count`
/// arguments is an IR operand (not a literal lane index).
pub fn is_operand(name: &str, index: usize, count: usize) -> bool {
    match Intrinsic::lookup(name) {
        Some(Ok(i)) => !i.constant_positions(count).contains(&index),
        _ => true,
    }
}

/// Isa discipline over the surface HIR of a well-typed World (run after
/// checking, like the capability-effect rules).
pub(crate) fn check_world(world: &World) -> Vec<Diagnostic> {
    let mut out = vec![];
    for (name, a) in &world.adts {
        if matches!(name.as_str(), "Vec" | "Mask") && a.generics.len() == 2 {
            out.push(Diagnostic::new(
                "E1401",
                format!("'{name}' with two parameters is reserved for vector types"),
                a.span.clone(),
            ));
        }
    }
    for f in world.functions.values() {
        let Some(body) = &f.body else { continue };
        let available = row_profiles(&f.effects);
        let mut walker = IsaWalker {
            world,
            out: &mut out,
        };
        walker.expr(body, &available);
    }
    out
}
/// Whether a value of `t` holds a vector or mask lane anywhere inline.
fn contains_vector(world: &World, t: &Type, depth: usize) -> bool {
    if depth > 32 {
        return false;
    }
    match t {
        _ if shape_of(t).is_some() => true,
        Type::Tuple(ts) => ts.iter().any(|t| contains_vector(world, t, depth + 1)),
        Type::Named(n, args) => world.adts.get(n).is_some_and(|a| {
            let vars = a
                .generics
                .iter()
                .zip(args)
                .map(|(g, t)| (g.name.clone(), t.clone()))
                .collect();
            a.variants
                .iter()
                .flat_map(|v| &v.fields)
                .any(|f| contains_vector(world, &substitute(&f.ty, &vars), depth + 1))
        }),
        _ => false,
    }
}
struct IsaWalker<'a> {
    world: &'a World,
    out: &'a mut Vec<Diagnostic>,
}
impl IsaWalker<'_> {
    fn ty(&self, e: &Expr) -> Type {
        self.world
            .types
            .get(&(e.span.start, e.span.end))
            .cloned()
            .unwrap_or(Type::Infer)
    }
    fn require(&mut self, needed: Profile, available: &[Profile], what: String, span: Span) {
        if available.iter().any(|p| needed.le(*p)) {
            return;
        }
        self.out.push(
            Diagnostic::new(
                "E1403",
                format!(
                    "{what} requires 'eff [Isa[{}]]'{}",
                    needed.name(),
                    if available.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "; only {} is available here",
                            available
                                .iter()
                                .map(|p| format!("Isa[{}]", p.name()))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    }
                ),
                span,
            )
            .with_help(format!(
                "add Isa[{}] to the function's effect row, or call it from the checked dispatch `if Isa::supported[{}]() {{ ... }}`",
                needed.name(),
                needed.name()
            )),
        );
    }
    fn vector_width(&mut self, shape: Shape, available: &[Profile], name: &str, span: Span) {
        if available.iter().any(|p| p.fixed_bits() >= shape.bits()) {
            return;
        }
        self.require(
            shape.required_profile(),
            available,
            format!("'{name}' on {}-bit vectors", shape.bits()),
            span,
        );
    }
    fn expr(&mut self, e: &Expr, available: &[Profile]) {
        match &e.kind {
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                self.expr(condition, available);
                let mut granted = available.to_vec();
                if let ExprKind::Call { callee, types, .. } = &condition.kind
                    && let ExprKind::Name(n) = &callee.kind
                    && Intrinsic::lookup(n) == Some(Ok(Intrinsic::IsaSupported))
                    && let [Type::Named(p, _)] = types.as_slice()
                    && let Some(p) = Profile::from_name(p)
                {
                    granted.push(p);
                }
                self.expr(then, &granted);
                if let Some(o) = otherwise {
                    self.expr(o, available);
                }
            }
            ExprKind::Call {
                callee,
                types,
                args,
            } => {
                if let ExprKind::Name(n) = &callee.kind {
                    if let Some(Ok(intrinsic)) =
                        Intrinsic::lookup(n).filter(|_| !self.world.functions.contains_key(n))
                    {
                        if intrinsic != Intrinsic::IsaSupported {
                            let arg_types =
                                args.iter().map(|a| self.ty(&a.expr)).collect::<Vec<_>>();
                            let exprs = args.iter().map(|a| &a.expr).collect::<Vec<_>>();
                            if let Some(shape) =
                                VectorOp::resolve(n, types, &exprs, &arg_types, &self.ty(e))
                                    .and_then(|op| op.shape())
                            {
                                self.vector_width(shape, available, n, e.span.clone());
                            }
                        }
                    } else if let Some(f) = self.world.functions.get(n) {
                        for q in row_profiles(&f.effects) {
                            self.require(q, available, format!("calling '{n}'"), e.span.clone());
                        }
                    } else if let Type::Function { effects, .. } = self.ty(callee) {
                        for q in row_profiles(&effects) {
                            self.require(q, available, format!("calling '{n}'"), e.span.clone());
                        }
                    }
                } else {
                    // An inherent method `receiver.method(..)` resolves
                    // through the receiver's type, like the checker does.
                    if let ExprKind::Field(base, method) = &callee.kind {
                        let name = format!("{}::{method}", self.ty(base).name());
                        if let Some(f) = self.world.functions.get(&name) {
                            for q in row_profiles(&f.effects) {
                                self.require(
                                    q,
                                    available,
                                    format!("calling '{name}'"),
                                    e.span.clone(),
                                );
                            }
                        }
                    }
                    if let Type::Function { effects, .. } = self.ty(callee) {
                        for q in row_profiles(&effects) {
                            self.require(
                                q,
                                available,
                                "calling this function value".into(),
                                e.span.clone(),
                            );
                        }
                    }
                    self.expr(callee, available);
                }
                for a in args {
                    self.expr(&a.expr, available);
                }
            }
            ExprKind::Closure { effects, body, .. } => {
                // A closure may run outside this scope: only its own row
                // grants it a profile.
                self.expr(body, &row_profiles(effects));
            }
            ExprKind::Handle {
                body,
                ret,
                ops,
                state,
                ..
            } => {
                if let Some((_, init)) = state {
                    self.expr(init, available);
                }
                self.expr(body, available);
                self.expr(&ret.body, available);
                for c in ops {
                    self.expr(&c.body, available);
                }
            }
            ExprKind::Tuple(es) => {
                for e in es {
                    self.expr(e, available);
                }
            }
            ExprKind::Struct { fields, .. } => {
                for (_, e) in fields {
                    self.expr(e, available);
                }
            }
            ExprKind::Block { statements, tail } => {
                for s in statements {
                    match s {
                        Statement::Let { value, .. } => self.expr(value, available),
                        Statement::Expr(e) => self.expr(e, available),
                    }
                }
                if let Some(t) = tail {
                    self.expr(t, available);
                }
            }
            ExprKind::Field(e, _) | ExprKind::Try(e) | ExprKind::Unary(_, e) => {
                self.expr(e, available)
            }
            ExprKind::Binary(op, a, _)
                if matches!(op.as_str(), "==" | "!=" | "<" | "<=" | ">" | ">=")
                    && contains_vector(self.world, &self.ty(a), 0) =>
            {
                // Aggregate equality would compare lane bits, not values
                // (-0.0 and NaN): lanes are compared with Vec::eq and friends.
                self.out.push(
                    Diagnostic::new(
                        "E1402",
                        format!("'{op}' is not defined on vector and mask values"),
                        e.span.clone(),
                    )
                    .with_help("compare lanes with Vec::eq, Vec::ne, Vec::lt, ... and reduce the mask with Mask::all or Mask::any"),
                );
            }
            ExprKind::Index(a, b) | ExprKind::Binary(_, a, b) | ExprKind::Assign(_, a, b) => {
                self.expr(a, available);
                self.expr(b, available);
            }
            ExprKind::Match { value, arms } => {
                self.expr(value, available);
                for (_, a) in arms {
                    self.expr(a, available);
                }
            }
            ExprKind::Literal(_) | ExprKind::Name(_) => {}
        }
    }
}

/// Lane values of a vector or mask, for programs and tests that inspect
/// evaluator results.
pub fn lanes(value: &Value) -> Option<&[Value]> {
    match value {
        Value::Tuple(v) => Some(v),
        _ => None,
    }
}

/// Decode the packed native bytes of a vector or mask into the evaluator's
/// lane tuple. Mask lanes are all ones or all zeros.
pub fn decode(t: &Type, bytes: &[u8]) -> Option<Value> {
    let (kind, shape) = shape_of(t)?;
    let width = (shape.elem.bits() / 8) as usize;
    let lanes = bytes
        .get(..shape.bytes() as usize)?
        .chunks(width)
        .map(|lane| {
            let mut word = [0u8; 8];
            word[..width].copy_from_slice(lane);
            u64::from_le_bytes(word)
        });
    Some(Value::Tuple(match kind {
        Kind::Mask => lanes.map(|l| Value::Bool(l != 0)).collect(),
        Kind::Vec => lanes
            .map(|l| match shape.elem {
                Elem::F64 => Value::Float(f64::from_bits(l)),
                Elem::I64 => Value::Int(l as i64),
                e => Value::UInt(l, e.bits()),
            })
            .collect(),
    }))
}
