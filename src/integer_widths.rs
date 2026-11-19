//! Integer widths (`vesper.arith.checked/1`): the byte-order prelude.
//!
//! The integer kinds, operators, conversions and byte-order intrinsics are
//! specified in [`crate::numeric`]. The checker rules live in
//! `src/integer_widths/check.rs`. Each byte-order intrinsic
//! (`to_le_bytes_u32`, `from_be_bytes_i16`, ...) is an ordinary source
//! function over shifts, `|` and conversions, added to the module when the
//! module mentions it and does not define a function of that name, so every
//! backend runs it without dedicated support.
//!
//! `mul_wide(a: u64, b: u64) -> (u64, u64)` is the full 128-bit product of
//! two `u64` values as `(low, high)` words, like Rust's `u64::widening_mul`.
//! It is a reserved prelude function of the same kind: its source body
//! builds the product from 32-bit halves, so every backend agrees, and the
//! Cranelift backend lowers a call to it to one `imul` and one `umulhi`.
//!
//! The bit builtins (port gaps lane 5, #988) are reserved prelude functions
//! of the same kind, one per integer kind `k`: `leading_zeros_k(x: k) -> u32`,
//! `trailing_zeros_k(x: k) -> u32` and `count_ones_k(x: k) -> u32` count over
//! the kind's two's-complement bit pattern and are defined at zero (the
//! width); `wrapping_shl_k(x: k, n: u32) -> k` and `wrapping_shr_k(x: k, n:
//! u32) -> k` are total shifts by `n` modulo the width (`>>` arithmetic on
//! signed kinds), like Rust's `wrapping_shl`/`wrapping_shr`. Their source
//! bodies use only total operators (SWAR population counts, rotations and
//! constant shifts), so every backend runs them unchanged; the Cranelift
//! backend lowers each call to `clz`, `ctz`, `popcnt`, `ishl`, `ushr` or
//! `sshr` in the kind's own width ([`lower_bit_builtin_calls`]).
//! `wrapping_neg_k(x: k) -> k` (P3b gaps lane 7, #1092) is `0 - x` modulo
//! the width, like Rust's `wrapping_neg`, lowered to one `ineg`.
use crate::ast::*;
use crate::numeric::IntegerKind;
use std::collections::BTreeSet;

fn item_end(item: &Item) -> usize {
    match item {
        Item::Adt(a) => a.span.end,
        Item::Function(f) => f.span.end,
        Item::Effect(e) => e.span.end,
        Item::Impl { methods, .. } => methods.iter().map(|m| m.span.end).max().unwrap_or(0),
        Item::Trait(t) => t.span.end,
        Item::Conformance(c) => c.span.end,
    }
}

/// The byte-order functions `module` uses, as a module whose spans all lie
/// after `after` and after every span of `module`.
pub(crate) fn prelude(module: &Module, after: usize) -> Module {
    let mut used = BTreeSet::new();
    let mut defined = BTreeSet::new();
    let mut end = after;
    for item in &module.items {
        end = end.max(item_end(item));
        let functions: Vec<&Function> = match item {
            Item::Function(f) => {
                defined.insert(f.name.clone());
                vec![f]
            }
            Item::Impl { methods, .. } => methods.iter().collect(),
            Item::Trait(t) => t.methods.iter().collect(),
            Item::Conformance(c) => c.methods.iter().collect(),
            Item::Adt(_) | Item::Effect(_) => vec![],
        };
        for f in functions {
            if let Some(body) = &f.body {
                crate::capability_effects::used_names(body, &mut used);
            }
        }
    }
    let source: String = used
        .iter()
        .filter(|n| !defined.contains(*n))
        .filter_map(|n| {
            crate::numeric::byte_order_source(n)
                .or_else(|| wide_source(n))
                .or_else(|| bit_source(n))
        })
        .map(|f| f + "\n")
        .collect();
    if source.is_empty() {
        return Module::default();
    }
    let padded = format!("{}{source}", " ".repeat(end + 1));
    let parsed = crate::parser::parse(&padded);
    debug_assert!(
        parsed.diagnostics.is_empty(),
        "byte-order prelude: {:?}",
        parsed.diagnostics
    );
    parsed.module
}

/// The largest span end in `module`.
pub(crate) fn module_end(module: &Module) -> usize {
    module.items.iter().map(item_end).max().unwrap_or(0)
}

/// The reserved 64x64 -> 128-bit multiply: `mul_wide(a, b)` is `(low, high)`.
pub const MUL_WIDE: &str = "mul_wide";

/// The portable source of [`MUL_WIDE`]: schoolbook multiplication of 32-bit
/// halves, with every partial sum below 2^64.
fn wide_source(name: &str) -> Option<String> {
    (name == MUL_WIDE).then(|| {
        "fn mul_wide(a: u64, b: u64) -> (u64, u64) { \
         let m: u64 = 4294967295; \
         let a0 = a & m; let a1 = a >> 32; let b0 = b & m; let b1 = b >> 32; \
         let p00 = wrapping_mul(a0, b0); let p01 = wrapping_mul(a0, b1); \
         let p10 = wrapping_mul(a1, b0); let p11 = wrapping_mul(a1, b1); \
         let mid = wrapping_add(wrapping_add(p00 >> 32, p01 & m), p10 & m); \
         let hi = wrapping_add(wrapping_add(wrapping_add(p11, p01 >> 32), p10 >> 32), mid >> 32); \
         (wrapping_mul(a, b), hi) }"
            .to_string()
    })
}

/// A bit builtin ([module docs](self)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitOp {
    /// `leading_zeros_k(x) -> u32`.
    LeadingZeros,
    /// `trailing_zeros_k(x) -> u32`.
    TrailingZeros,
    /// `count_ones_k(x) -> u32`.
    CountOnes,
    /// `wrapping_shl_k(x, n: u32) -> k`.
    WrappingShl,
    /// `wrapping_shr_k(x, n: u32) -> k`.
    WrappingShr,
    /// `wrapping_neg_k(x) -> k`: `0 - x` modulo 2^bits.
    WrappingNeg,
}

/// The bit builtin families, by name prefix (the suffix is the kind).
pub const BIT_BUILTINS: [(&str, BitOp); 6] = [
    ("leading_zeros_", BitOp::LeadingZeros),
    ("trailing_zeros_", BitOp::TrailingZeros),
    ("count_ones_", BitOp::CountOnes),
    ("wrapping_shl_", BitOp::WrappingShl),
    ("wrapping_shr_", BitOp::WrappingShr),
    ("wrapping_neg_", BitOp::WrappingNeg),
];

/// The bit builtin spelled `name`, with its integer kind.
pub fn bit_builtin(name: &str) -> Option<(BitOp, IntegerKind)> {
    BIT_BUILTINS.iter().find_map(|(prefix, op)| {
        let kind = IntegerKind::from_name(name.strip_prefix(prefix)?)?;
        Some((*op, kind))
    })
}

impl BitOp {
    /// Operand count: one for the counts, two (value and amount) for shifts.
    pub fn arity(self) -> usize {
        match self {
            Self::WrappingShl | Self::WrappingShr => 2,
            _ => 1,
        }
    }
    /// The reference result on `kind` operand `a` (in range) and, for the
    /// shifts, amount `n` (a `u32`).
    pub fn eval(self, kind: IntegerKind, a: i128, n: u32) -> i128 {
        let bits = kind.bits();
        let p = kind.pattern(a);
        match self {
            Self::LeadingZeros => i128::from(p.leading_zeros() - (64 - bits)),
            Self::TrailingZeros => i128::from(p.trailing_zeros().min(bits)),
            Self::CountOnes => i128::from(p.count_ones()),
            Self::WrappingShl => kind.wrap(i128::from(p << (n % bits))),
            Self::WrappingShr if kind.unsigned() => i128::from(p >> (n % bits)),
            Self::WrappingShr => a >> (n % bits),
            Self::WrappingNeg => kind.wrap(-a),
        }
    }
}

/// The portable source of the bit builtin `name` (see [`BitOp`]).
fn bit_source(name: &str) -> Option<String> {
    let (op, kind) = bit_builtin(name)?;
    let k = kind.name();
    let bits = kind.bits();
    let u = if kind.signed() {
        kind.flip_sign()
    } else {
        kind
    };
    let un = u.name();
    // The operand's bit pattern in the unsigned kind of the same width.
    let pattern = if kind.signed() {
        format!("wrapping_{un}(x)")
    } else {
        "x".to_string()
    };
    // A SWAR population count of the `u64` value `v0`.
    let popcount = "let v1 = wrapping_sub(v0, (v0 >> 1) & 0x5555555555555555); \
         let v2 = wrapping_add(v1 & 0x3333333333333333, (v1 >> 2) & 0x3333333333333333); \
         let v3 = wrapping_add(v2, v2 >> 4) & 0x0f0f0f0f0f0f0f0f; \
         let c: u32 = wrapping_u32(wrapping_mul(v3, 0x0101010101010101) >> 56);";
    Some(match op {
        BitOp::CountOnes => format!(
            "fn {name}(x: {k}) -> u32 {{ let v0: u64 = widen_u64({pattern}); {popcount} c }}"
        ),
        BitOp::TrailingZeros => format!(
            "fn {name}(x: {k}) -> u32 {{ let p: {un} = {pattern}; \
             let v0: u64 = widen_u64(~p & wrapping_sub(p, 1)); {popcount} c }}"
        ),
        BitOp::LeadingZeros => {
            let mut smear = format!("let mut p: {un} = {pattern}; ");
            let mut s = 1;
            while s < bits {
                smear += &format!("p = p | (p >> {s}); ");
                s *= 2;
            }
            format!(
                "fn {name}(x: {k}) -> u32 {{ {smear}let v0: u64 = widen_u64(p); {popcount} \
                 let width: u32 = {bits}; wrapping_sub(width, c) }}"
            )
        }
        // 0 - x, modulo 2^bits.
        BitOp::WrappingNeg => {
            format!("fn {name}(x: {k}) -> {k} {{ let zero: {k} = 0; wrapping_sub(zero, x) }}")
        }
        // x * 2^(n mod bits), modulo 2^bits.
        BitOp::WrappingShl => format!(
            "fn {name}(x: {k}, n: u32) -> {k} {{ let one: {k} = 1; wrapping_mul(x, rotate_left(one, n)) }}"
        ),
        BitOp::WrappingShr => {
            // The low `bits - s` ones keep a logical shift's bits.
            let logical = format!(
                "let p: {un} = {pattern}; let one: {un} = 1; \
                 let keep: {un} = wrapping_sub(rotate_right(one, s), one); \
                 let r: {un} = rotate_right(p, s) & keep;"
            );
            let result = if kind.signed() {
                format!("if x < 0 {{ wrapping_{k}(r | ~keep) }} else {{ wrapping_{k}(r) }}")
            } else {
                "r".to_string()
            };
            format!(
                "fn {name}(x: {k}, n: u32) -> {k} {{ let s: u32 = n & {}; \
                 if s == 0 {{ x }} else {{ {logical} {result} }} }}",
                bits - 1
            )
        }
    })
}

/// Native lowering of the bit builtins: every Place IR call of a reserved
/// bit builtin becomes the one-operand `Op::Unary` or two-operand
/// `Op::Binary` of its name, which the Cranelift backend lowers to one
/// instruction. Run right after `lower_world`, before the optimizer and the
/// inliner, so the portable prelude body is never inlined in its place.
pub(crate) fn lower_bit_builtin_calls(
    world: &crate::check::World,
    program: &mut crate::ir::Program,
) {
    use crate::ir::Op;
    for f in program.functions.values_mut() {
        let mut operands = std::collections::BTreeSet::new();
        for inst in f.blocks.iter_mut().flat_map(|b| &mut b.instructions) {
            let Op::Call { callee, args, .. } = &inst.op else {
                continue;
            };
            let Some((op, _)) = bit_builtin(callee) else {
                continue;
            };
            if !world.functions.contains_key(callee)
                || args.len() != op.arity()
                || args.iter().any(|a| a.pass != Pass::In)
            {
                continue;
            }
            inst.op = match args.as_slice() {
                [a] => Op::Unary(callee.clone(), a.value),
                [a, n] => Op::Binary(callee.clone(), a.value, n.value),
                _ => continue,
            };
            operands.extend(crate::ir_inline::operands(&inst.op));
        }
        // A call argument is an `in` read whose loan the call releases; an
        // operator consumes its operands and releases nothing, so the shared
        // loan of `let mut x; x = wrapping_shl_u32(x, 16)` would outlive the
        // operation and overlap the write of `x` (E0410). The operands are
        // integers (copy), so read them by value as every other operator does.
        for inst in f.blocks.iter_mut().flat_map(|b| &mut b.instructions) {
            if let Op::Read { pass, .. } = &mut inst.op
                && *pass == Pass::In
                && operands.contains(&inst.dest)
            {
                *pass = Pass::Sink;
            }
        }
    }
}
