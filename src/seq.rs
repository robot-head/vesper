//! Growable `ChunkSeq[T]` sequences and borrowed `Slice[T]` views (§13,
//! port wave P0 lane 3, feature F1).
//!
//! `ChunkSeq[T]` owns a contiguous, growable buffer of `T`. Its mode is the
//! mode of `T` joined with `affine`: a sequence of `linear` elements is itself
//! `linear` and is consumed by `ChunkSeq::finish`, which raises
//! `Fail[RuntimeError]` (`IndexOutOfBounds`) instead of leaking elements that
//! are still in the sequence.
//!
//! `Slice[T]` is a second-class view `[start, start + len)` of a sequence. A
//! slice exists only as an `in` or `inout` argument: `f(in s[a..b])`,
//! `f(inout s.as_slice_mut())`, or a sub-slice of a slice parameter. It is
//! never bound, returned, stored or sunk, so it cannot outlive the loan of its
//! sequence, and the sequence cannot grow while the slice is live. Two
//! interval arguments of one place are disjoint by construction when one
//! interval ends with the same name or literal the other starts with
//! (`inout s[0..mid], inout s[mid..n]`), or when both bounds are known
//! integers. Every interval is bounds checked at run time.
//!
//! Every backend shares one representation. The evaluator holds the elements
//! behind a shared cell. Native code uses a one-word handle to a three-word
//! header `{len, cap, data}`; elements are packed at [`elem_bytes`] and the
//! header is grown by `vesper_seq_grow` (a Rust function in the JIT, a
//! generated function in executables, see `src/seq/aot_runtime.rs`). A slice
//! is a three-word value `{handle, start, len}`. The Lean model is `Array T`.
//!
//! Element indexing (`get`, `set`, `s[i]`) and `finish` raise
//! `Fail[RuntimeError]` `IndexOutOfBounds` on every backend. An abortable
//! effect may cross a live `inout` sequence or slice (keep-writes, #935): the
//! caller keeps every write made before the failure. The total `try_get` (an
//! `Option[T]`) and `try_set` (a `bool`) avoid the failure altogether.
//! The block operations `copy_from` and `fill` and the fixed-width byte
//! accesses (`read_u32_be`, `write_u16_le`, ...) check their whole interval
//! once ([`SeqOp::CopyFrom`], [`SeqOp::Fill`], [`SeqOp::Wide`]); native code
//! lowers them in `src/seq/native_bytes.rs`. The byte scans `sum_be16`, `eq`
//! and `find_byte` ([`SeqOp::SumBe16`], [`SeqOp::BytesEq`],
//! [`SeqOp::FindByte`]) are total; optimized native code runs them 16 bytes
//! at a time (`src/seq/native_bytes.rs`). An interval `s[a..b]` is evaluated while its sibling loans are
//! live, so an interval outside `0 <= a <= b <= len` is a run-time fault
//! like a `TiledSeq` bounds fault: the evaluator and the JIT report an error
//! and an executable exits with [`FAULT_STATUS`].
//!
//! Allocation is not yet tracked by an `Alloc` effect: neither `Box` nor
//! `TiledSeq` requests one today, and no runnable entry can discharge it.
use crate::{ast::*, check::World};

#[path = "seq/native_runtime.rs"]
pub mod native_runtime;

/// The growable sequence type `ChunkSeq[T]`.
pub const CHUNK_SEQ_TYPE: &str = "ChunkSeq";
/// The borrowed view type `Slice[T]`.
pub const SLICE_TYPE: &str = "Slice";
/// Exit status of an executable whose sequence allocation fails.
pub const FAULT_STATUS: i64 = 71;
/// Header words of a native sequence: length, capacity, data pointer.
pub const HEADER_BYTES: i64 = 24;

/// A sequence or slice operation in Place IR (`Op::Seq`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum SeqOp {
    /// `ChunkSeq::new[T]()`.
    New,
    /// `ChunkSeq::with_capacity[T](cap)`.
    WithCapacity,
    /// `s.len()`.
    Len,
    /// `s.push(v)`.
    Push,
    /// `s.pop()`: `Option[T]`.
    Pop,
    /// `s.get(i)` or `s[i]`.
    Get,
    /// `s.set(i, v)`.
    Set,
    /// `s.try_get(i)` on a sequence or slice: `Option[T]`, total.
    TryGet,
    /// `s.try_set(i, v)` on a sequence or slice: `false` when out of bounds.
    TrySet,
    /// `s.clear()`.
    Clear,
    /// `s.truncate(n)`.
    Truncate,
    /// `s.reserve(n)`: room for `n` more elements. A capacity hint only:
    /// total, and no other operation observes it.
    Reserve,
    /// `s.extend(in slice)`.
    Extend,
    /// `ChunkSeq::finish(s)`: consume an empty sequence.
    Finish,
    /// `s.as_slice()` / `s.as_slice_mut()`: the whole sequence.
    AsSlice,
    /// `s[a..b]` on a sequence or a slice.
    SubSlice,
    /// `x.len()` on a slice.
    SliceLen,
    /// `x.get(i)` or `x[i]` on a slice.
    SliceGet,
    /// `x.set(i, v)` on a slice.
    SliceSet,
    /// A `String` or `Str` primitive (§13): a string is a `ChunkSeq[u8]`
    /// that holds valid UTF-8, so its operations share `Op::Seq`.
    Str(crate::strings::StrOp),
    /// An element access of a sequence or slice whose index a dominating
    /// [`SeqOp::BoundsGuard`] proved in bounds. Only the native bounds-check
    /// hoisting pass ([`crate::bounds_hoist`]) creates it, in the fast copy
    /// of a counted loop; its operands are those of the access it replaces.
    Hoisted(Access),
    /// `[lo, hi, s1, c1, .., sn, cn]`: whether the range `lo..hi` (`lo..=hi`
    /// when `inclusive`) is empty, or `c_k + i` is in bounds of `s_k` for
    /// every index `i` of the range and every `k` (`c_k + hi` fits the
    /// length without overflow). A `bool`; total. Created by
    /// [`crate::bounds_hoist`].
    BoundsGuard { inclusive: bool },
    /// `s.copy_from(at, in src)` (`try_copy_from` when `total`): copies every
    /// element of `src` to `s[at..at + src.len()]` with one bounds check.
    /// The checked form raises `IndexOutOfBounds` and the total one yields
    /// `false` when the interval does not fit; neither writes anything then.
    CopyFrom { total: bool },
    /// `s.fill(v)`: every element of `s` becomes `v`. Total.
    Fill,
    /// A fixed-width integer read or write of a byte sequence or slice.
    Wide(Wide),
    /// `s.sum_be16()` on a byte sequence or slice: the sum, wrapping at
    /// 2^64, of its big-endian 16-bit words `s[2k] << 8 | s[2k + 1]`; an odd
    /// last byte counts as `s[n - 1] << 8`. Folding it gives the RFC 1071
    /// one's-complement sum. Total.
    SumBe16,
    /// `s.eq(in other)` on a byte sequence or slice: whether `other` has the
    /// same length and the same bytes. Total.
    BytesEq,
    /// `s.find_byte(b)` on a byte sequence or slice: the index of the first
    /// byte equal to `b`, as an `Option[u64]`. Total.
    FindByte,
}

/// `read_u16_le`, `try_write_u64_be` and the other fixed-width accesses of a
/// `ChunkSeq[u8]` or `Slice[u8]`: one bounds check for all `bytes` bytes at
/// `at`, then one unaligned load or store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Wide {
    /// 2, 4 or 8.
    pub bytes: u8,
    /// Big-endian (`_be`) rather than little-endian (`_le`).
    pub big: bool,
    /// `write_*(at, v)` rather than `read_*(at)`.
    pub write: bool,
    /// The total `try_` form: `Option[uN]` for a read, `bool` for a write.
    pub total: bool,
}

const WIDE_NAMES: [&str; 24] = [
    "read_u16_le",
    "read_u16_be",
    "read_u32_le",
    "read_u32_be",
    "read_u64_le",
    "read_u64_be",
    "write_u16_le",
    "write_u16_be",
    "write_u32_le",
    "write_u32_be",
    "write_u64_le",
    "write_u64_be",
    "try_read_u16_le",
    "try_read_u16_be",
    "try_read_u32_le",
    "try_read_u32_be",
    "try_read_u64_le",
    "try_read_u64_be",
    "try_write_u16_le",
    "try_write_u16_be",
    "try_write_u32_le",
    "try_write_u32_be",
    "try_write_u64_le",
    "try_write_u64_be",
];

impl Wide {
    fn index(self) -> usize {
        let width = match self.bytes {
            2 => 0,
            4 => 1,
            _ => 2,
        };
        usize::from(self.total) * 12
            + usize::from(self.write) * 6
            + width * 2
            + usize::from(self.big)
    }
    pub fn name(self) -> &'static str {
        WIDE_NAMES[self.index()]
    }
    /// The access a method name denotes, such as `try_read_u32_be`.
    pub fn parse(method: &str) -> Option<Self> {
        (0..24).find_map(|k| {
            let wide = Self {
                bytes: [2, 4, 8][(k / 2) % 3],
                big: k % 2 == 1,
                write: (k / 6) % 2 == 1,
                total: k >= 12,
            };
            (WIDE_NAMES[wide.index()] == method).then_some(wide)
        })
    }
    /// The integer type read or written: `u16`, `u32` or `u64`.
    pub fn int_type(self) -> &'static str {
        match self.bytes {
            2 => "u16",
            4 => "u32",
            _ => "u64",
        }
    }
}

/// The element access a [`SeqOp::Hoisted`] performs without its check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Access {
    /// `get` or `s[i]` on a sequence or slice: the element.
    Get,
    /// `set` on a sequence or slice: `Unit`.
    Set,
    /// `try_get`: always `Some(element)`.
    TryGet,
    /// `try_set`: stores and yields `true`.
    TrySet,
}

impl Access {
    /// The checked operation this access stands for, on a receiver that is
    /// a slice (`slice`) or a sequence.
    pub fn checked(self, slice: bool) -> SeqOp {
        match (self, slice) {
            (Self::Get, false) => SeqOp::Get,
            (Self::Get, true) => SeqOp::SliceGet,
            (Self::Set, false) => SeqOp::Set,
            (Self::Set, true) => SeqOp::SliceSet,
            (Self::TryGet, _) => SeqOp::TryGet,
            (Self::TrySet, _) => SeqOp::TrySet,
        }
    }
}

impl SeqOp {
    /// Whether the operation is a byte scan: `sum_be16`, `eq` or
    /// `find_byte`, which need a `u8` element type.
    pub fn byte_scan(self) -> bool {
        matches!(self, Self::SumBe16 | Self::BytesEq | Self::FindByte)
    }
    /// Whether the operation may raise `Fail[RuntimeError]`.
    pub fn fallible(self) -> bool {
        if let Self::Str(op) = self {
            return op.fallible();
        }
        matches!(
            self,
            Self::Get
                | Self::Set
                | Self::Finish
                | Self::SliceGet
                | Self::SliceSet
                | Self::CopyFrom { total: false }
                | Self::Wide(Wide { total: false, .. })
        )
    }
    /// Whether the operation mutates its receiver (argument 0) in place.
    pub fn mutates(self) -> bool {
        if let Self::Str(op) = self {
            return op.mutates();
        }
        matches!(
            self,
            Self::Push
                | Self::Pop
                | Self::Set
                | Self::TrySet
                | Self::Clear
                | Self::Truncate
                | Self::Reserve
                | Self::Extend
                | Self::CopyFrom { .. }
                | Self::Fill
                | Self::Wide(Wide { write: true, .. })
        )
    }
    /// How the operation borrows its receiver.
    pub fn receiver_pass(self) -> Pass {
        if self.mutates() || self == Self::SliceSet {
            Pass::Inout
        } else {
            Pass::In
        }
    }
    /// Whether argument 0 is the receiver sequence or slice.
    pub fn has_receiver(self) -> bool {
        !matches!(
            self,
            Self::New | Self::WithCapacity | Self::BoundsGuard { .. }
        )
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::WithCapacity => "with_capacity",
            Self::Len | Self::SliceLen => "len",
            Self::Push => "push",
            Self::Pop => "pop",
            Self::Get | Self::SliceGet => "get",
            Self::Set | Self::SliceSet => "set",
            Self::TryGet => "try_get",
            Self::TrySet => "try_set",
            Self::Clear => "clear",
            Self::Truncate => "truncate",
            Self::Reserve => "reserve",
            Self::Extend => "extend",
            Self::Finish => "finish",
            Self::AsSlice => "as_slice",
            Self::SubSlice => "slice",
            Self::Str(op) => op.name(),
            Self::Hoisted(access) => access.checked(false).name(),
            Self::BoundsGuard { .. } => "bounds_guard",
            Self::CopyFrom { total: false } => "copy_from",
            Self::CopyFrom { total: true } => "try_copy_from",
            Self::Fill => "fill",
            Self::Wide(wide) => wide.name(),
            Self::SumBe16 => "sum_be16",
            Self::BytesEq => "eq",
            Self::FindByte => "find_byte",
        }
    }
}

pub fn is_seq_type(ty: &Type) -> bool {
    matches!(ty, Type::Named(n, a) if n == CHUNK_SEQ_TYPE && a.len() == 1)
}

pub fn is_slice_type(ty: &Type) -> bool {
    matches!(ty, Type::Named(n, a) if n == SLICE_TYPE && a.len() == 1)
}

/// `ChunkSeq[T]` or `Slice[T]`.
pub fn is_seq_or_slice(ty: &Type) -> bool {
    is_seq_type(ty) || is_slice_type(ty)
}

/// The element type of a `ChunkSeq[T]` or `Slice[T]`.
pub fn elem(ty: &Type) -> Option<&Type> {
    match ty {
        Type::Named(n, args) if (n == CHUNK_SEQ_TYPE || n == SLICE_TYPE) && args.len() == 1 => {
            Some(&args[0])
        }
        _ => None,
    }
}

pub fn slice_of(elem: Type) -> Type {
    Type::Named(SLICE_TYPE.into(), vec![elem])
}

pub fn seq_of(elem: Type) -> Type {
    Type::Named(CHUNK_SEQ_TYPE.into(), vec![elem])
}

/// Whether `ty` mentions `Slice` anywhere.
pub fn mentions_slice(ty: &Type) -> bool {
    match ty {
        Type::Named(n, args) => n == SLICE_TYPE || args.iter().any(mentions_slice),
        Type::Tuple(ts) => ts.iter().any(mentions_slice),
        Type::Function { params, result, .. } => {
            params.iter().any(|(_, t)| mentions_slice(t)) || mentions_slice(result)
        }
        _ => false,
    }
}

/// Packed bytes per element in native buffers: one byte for `u8` and
/// `bool`, four for `u32`, and the eight-byte-aligned value layout for every
/// other type. The same widths are the export ABI of a byte buffer.
pub fn elem_bytes(world: &World, ty: &Type) -> Result<i64, String> {
    let ty = crate::numeric::default_type(ty);
    Ok(match ty.name() {
        "u8" | "bool" if matches!(&ty, Type::Named(_, a) if a.is_empty()) => 1,
        "u32" if matches!(&ty, Type::Named(_, a) if a.is_empty()) => 4,
        _ => i64::from(crate::layout::Layout::of(world, &ty)?.size),
    })
}

/// A slice-interval expression `base[lo..hi]`.
pub fn interval(e: &Expr) -> Option<(&Expr, &Expr, &Expr)> {
    match &e.kind {
        ExprKind::Index(base, index) => match &index.kind {
            ExprKind::Binary(op, lo, hi) if op == ".." => Some((base, lo, hi)),
            _ => None,
        },
        _ => None,
    }
}

/// A whole-sequence view `base.as_slice()` or `base.as_slice_mut()`.
pub fn whole_view(e: &Expr) -> Option<(&Expr, bool)> {
    match &e.kind {
        ExprKind::Call { callee, args, .. } if args.is_empty() => match &callee.kind {
            ExprKind::Field(base, m) if m == "as_slice" => Some((base, false)),
            ExprKind::Field(base, m) if m == "as_slice_mut" => Some((base, true)),
            _ => None,
        },
        _ => None,
    }
}

/// The synthetic place path segment that names an interval loan of a
/// sequence or slice place, for example `[0..mid]`. Distinct intervals are
/// sibling places; [`crate::check`] proves them disjoint before a call.
pub fn interval_segment(lo: &Expr, hi: &Expr) -> String {
    format!("[{}..{}]", render_bound(lo), render_bound(hi))
}

/// The path segment of a whole-sequence view.
pub const WHOLE_SEGMENT: &str = "[..]";

pub fn is_segment(s: &str) -> bool {
    s.starts_with('[')
}

fn render_bound(e: &Expr) -> String {
    match &e.kind {
        ExprKind::Literal(Literal::Int(n)) => n.to_string(),
        ExprKind::Name(n) => n.clone(),
        ExprKind::Field(b, f) => format!("{}.{f}", render_bound(b)),
        _ => format!("#{}:{}", e.span.start, e.span.end),
    }
}

/// A stable interval bound: a literal, a name or a field path. Two equal
/// stable bounds denote the same value within one argument list.
pub fn stable_bound(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Literal(Literal::Int(_)) | ExprKind::Name(_) => true,
        ExprKind::Field(b, _) => stable_bound(b),
        _ => false,
    }
}

pub fn same_bound(a: &Expr, b: &Expr) -> bool {
    stable_bound(a) && stable_bound(b) && render_bound(a) == render_bound(b)
}

/// Resolves a sequence call to its operation. `receiver_ty` is the type of a
/// method receiver; static calls are `ChunkSeq::new`, `ChunkSeq::with_capacity`
/// and `ChunkSeq::finish`.
/// Whether a value of `ty` can hold a sequence handle (through type
/// arguments or data fields). The JIT cannot hand such a value back to the
/// host, because its sequences are freed when the entry returns.
pub fn holds_sequence(world: &World, ty: &Type) -> bool {
    fn visit(world: &World, ty: &Type, seen: &mut std::collections::BTreeSet<String>) -> bool {
        if is_seq_or_slice(ty) {
            return true;
        }
        match ty {
            Type::Tuple(ts) => ts.iter().any(|t| visit(world, t, seen)),
            Type::Named(n, args) => {
                if args.iter().any(|a| visit(world, a, seen)) {
                    return true;
                }
                let Some(a) = world.adts.get(n) else {
                    return false;
                };
                if !seen.insert(n.clone()) {
                    return false;
                }
                let found = a
                    .variants
                    .iter()
                    .any(|v| v.fields.iter().any(|f| visit(world, &f.ty, seen)));
                seen.remove(n);
                found
            }
            _ => false,
        }
    }
    visit(world, ty, &mut Default::default())
}

/// The growing sequence operation a call performs, if any: `@fip` code
/// cannot allocate, so the FIP verifier rejects these.
pub fn fip_allocation(world: &World, callee: &Expr) -> Option<&'static str> {
    let op = match &callee.kind {
        ExprKind::Name(n) => resolve(n, None)?,
        ExprKind::Field(base, method) => resolve(
            method,
            Some(world.types.get(&(base.span.start, base.span.end))?),
        )?,
        _ => return None,
    };
    matches!(
        op,
        SeqOp::New
            | SeqOp::WithCapacity
            | SeqOp::Push
            | SeqOp::Extend
            | SeqOp::Str(
                crate::strings::StrOp::PushStr
                    | crate::strings::StrOp::PushChar
                    | crate::strings::StrOp::Reserve
            )
    )
    .then(|| op.name())
}

/// The block operations of both `ChunkSeq[T]` and `Slice[T]`.
fn shared_method(method: &str) -> Option<SeqOp> {
    Some(match method {
        "copy_from" => SeqOp::CopyFrom { total: false },
        "try_copy_from" => SeqOp::CopyFrom { total: true },
        "fill" => SeqOp::Fill,
        "sum_be16" => SeqOp::SumBe16,
        "eq" => SeqOp::BytesEq,
        "find_byte" => SeqOp::FindByte,
        _ => SeqOp::Wide(Wide::parse(method)?),
    })
}

pub fn resolve(callee_name: &str, receiver_ty: Option<&Type>) -> Option<SeqOp> {
    if let Some(t) = receiver_ty {
        let method = callee_name.rsplit("::").next().unwrap_or(callee_name);
        if crate::strings::is_text(t) {
            return crate::strings::resolve(method, t).map(SeqOp::Str);
        }
        if is_seq_type(t) {
            return Some(match method {
                "len" => SeqOp::Len,
                "push" => SeqOp::Push,
                "pop" => SeqOp::Pop,
                "get" => SeqOp::Get,
                "set" => SeqOp::Set,
                "try_get" => SeqOp::TryGet,
                "try_set" => SeqOp::TrySet,
                "clear" => SeqOp::Clear,
                "truncate" => SeqOp::Truncate,
                "reserve" => SeqOp::Reserve,
                "extend" | "extend_from_slice" => SeqOp::Extend,
                "as_slice" | "as_slice_mut" => SeqOp::AsSlice,
                _ => return shared_method(method),
            });
        }
        if is_slice_type(t) {
            return Some(match method {
                "len" => SeqOp::SliceLen,
                "get" => SeqOp::SliceGet,
                "set" => SeqOp::SliceSet,
                "try_get" => SeqOp::TryGet,
                "try_set" => SeqOp::TrySet,
                _ => return shared_method(method),
            });
        }
        return None;
    }
    match callee_name.strip_prefix("ChunkSeq::")? {
        "new" => Some(SeqOp::New),
        "with_capacity" => Some(SeqOp::WithCapacity),
        "finish" => Some(SeqOp::Finish),
        _ => None,
    }
}

/// Where `Slice[T]` may appear: only as the whole type of an `in` or `inout`
/// parameter. Element types of a sequence must be storable values.
pub(crate) fn check_world(world: &World) -> Vec<Diagnostic> {
    let mut diagnostics = vec![];
    let second_class = |what: &str, span: &Span| {
        Diagnostic::new(
            "E1520",
            format!("Slice[T] is a second-class borrow and cannot appear in {what}"),
            span.clone(),
        )
        .with_help("take the slice as an 'in' or 'inout' parameter and pass s[a..b] or s.as_slice() at the call")
    };
    for f in world.functions.values() {
        for p in &f.params {
            let top = is_slice_type(&p.ty) && matches!(p.pass, Pass::In | Pass::Inout);
            let nested = if top {
                elem(&p.ty).is_some_and(mentions_slice)
            } else {
                mentions_slice(&p.ty)
            };
            if nested || (is_slice_type(&p.ty) && !top) {
                diagnostics.push(second_class(
                    &format!("the {:?} parameter '{}'", p.pass, p.name).to_lowercase(),
                    &p.span,
                ));
            }
        }
        if mentions_slice(&f.result) {
            diagnostics.push(second_class(
                &format!("the result of '{}'", f.name),
                &f.span,
            ));
        }
    }
    for adt in world.adts.values() {
        for field in adt.variants.iter().flat_map(|v| &v.fields) {
            if mentions_slice(&field.ty) {
                diagnostics.push(second_class(
                    &format!("field '{}.{}'", adt.name, field.name),
                    &adt.span,
                ));
            }
        }
    }
    diagnostics
}
