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
//!
//! The checked builtins (SH1 lane 5, #1148) are reserved prelude functions
//! of the same kind: `checked_add_k`, `checked_sub_k` and `checked_mul_k`
//! take two `k` operands and return `Option[k]`, `None` exactly where `+`,
//! `-` or `*` would raise `Fail[ArithOverflow]`, like Rust's `checked_add`.
//! They are total: the source bodies use wrapping operators, comparisons,
//! widening and `mul_wide` only ([`checked_builtin`]).
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
    // The 64-bit `checked_mul_k` bodies call `mul_wide`.
    if used
        .iter()
        .any(|n| checked_builtin(n).is_some_and(|(op, k)| op == CheckedOp::Mul && k.bits() == 64))
    {
        used.insert(MUL_WIDE.to_string());
    }
    let source: String = used
        .iter()
        .filter(|n| !defined.contains(*n))
        .filter_map(|n| {
            crate::numeric::byte_order_source(n)
                .or_else(|| wide_source(n))
                .or_else(|| bit_source(n))
                .or_else(|| checked_source(n))
        })
        .map(|f| f + "\n")
        .collect();
    if source.is_empty() {
        return Module::default();
    }
    let parsed = parse_at(&source, end + 1);
    debug_assert!(
        parsed.diagnostics.is_empty(),
        "byte-order prelude: {:?}",
        parsed.diagnostics
    );
    parsed.module
}

/// Whether `t` mentions `i128` or `u128` (SH2 lane 2, #1174).
pub(crate) fn holds_wide(t: &Type) -> bool {
    match t {
        Type::Named(n, ts) => crate::numeric::is_wide_name(n) || ts.iter().any(holds_wide),
        Type::Tuple(ts) => ts.iter().any(holds_wide),
        _ => false,
    }
}

/// Parses generated `source` as if it began at byte `base` of a larger
/// file: every span of the result (module and diagnostics) is `base` plus
/// its offset in `source`. Preludes and generated wrappers used to pad
/// `source` with `base` spaces instead, so once a package's assembled
/// module passed the parser's 1 MiB input limit they parsed to nothing and
/// every builtin they declare went unknown (SH3 lane 7, #1246).
pub(crate) fn parse_at(source: &str, base: usize) -> crate::parser::Parse {
    fn shift(v: &mut serde_json::Value, by: usize) {
        match v {
            serde_json::Value::Object(m) => {
                // AST spans serialize as `{"start": n, "end": m}`; nothing
                // else in the AST has exactly those two fields (as in
                // `pact_frontend::mono`).
                let span = (m.len() == 2)
                    .then(|| Some((m.get("start")?.as_u64()?, m.get("end")?.as_u64()?)))
                    .flatten();
                if let Some((start, end)) = span {
                    m.insert("start".into(), (start as usize + by).into());
                    m.insert("end".into(), (end as usize + by).into());
                } else {
                    m.values_mut().for_each(|v| shift(v, by));
                }
            }
            serde_json::Value::Array(a) => a.iter_mut().for_each(|v| shift(v, by)),
            _ => {}
        }
    }
    fn shifted<T: serde::Serialize + serde::de::DeserializeOwned>(x: T, by: usize) -> T {
        let mut v = serde_json::to_value(&x).expect("AST serializes");
        shift(&mut v, by);
        serde_json::from_value(v).expect("shifted AST deserializes")
    }
    let mut parsed = crate::parser::parse(source);
    if base != 0 {
        parsed.module = shifted(std::mem::take(&mut parsed.module), base);
        parsed.diagnostics = shifted(std::mem::take(&mut parsed.diagnostics), base);
    }
    // The padded parse this replaces, wherever it still fits the parser's
    // limits: both must give the same module.
    #[cfg(debug_assertions)]
    if base > 0 && base + source.len() < 256 * 1024 {
        let padded = crate::parser::parse(&format!("{}{source}", " ".repeat(base)));
        debug_assert_eq!(padded.module, parsed.module, "parse_at({base})");
    }
    parsed
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
        if kind.wide() {
            // `a` is the value's bit pattern read as an `i128`.
            let p = a as u128;
            return match self {
                Self::LeadingZeros => i128::from(p.leading_zeros()),
                Self::TrailingZeros => i128::from(p.trailing_zeros()),
                Self::CountOnes => i128::from(p.count_ones()),
                Self::WrappingShl => p.wrapping_shl(n) as i128,
                Self::WrappingShr if kind.unsigned() => p.wrapping_shr(n) as i128,
                Self::WrappingShr => a.wrapping_shr(n),
                Self::WrappingNeg => a.wrapping_neg(),
            };
        }
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
    // A SWAR population count of the `u64` value `v0{s}` into `c{s}`.
    let swar = |s: &str| {
        format!(
            "let v1{s} = wrapping_sub(v0{s}, (v0{s} >> 1) & 0x5555555555555555); \
             let v2{s} = wrapping_add(v1{s} & 0x3333333333333333, (v1{s} >> 2) & 0x3333333333333333); \
             let v3{s} = wrapping_add(v2{s}, v2{s} >> 4) & 0x0f0f0f0f0f0f0f0f; \
             let c{s}: u32 = wrapping_u32(wrapping_mul(v3{s}, 0x0101010101010101) >> 56);"
        )
    };
    // The population count `c` of the `{un}` value `bits_of`: one word, or
    // the two words of a 128-bit kind.
    let popcount = |bits_of: &str| {
        if kind.wide() {
            format!(
                "let w: u128 = {bits_of}; let v0lo: u64 = wrapping_u64(w); {} \
                 let v0hi: u64 = wrapping_u64(w >> 64); {} let c: u32 = wrapping_add(clo, chi);",
                swar("lo"),
                swar("hi")
            )
        } else {
            format!("let v0: u64 = widen_u64({bits_of}); {}", swar(""))
        }
    };
    Some(match op {
        BitOp::CountOnes => format!("fn {name}(x: {k}) -> u32 {{ {} c }}", popcount(&pattern)),
        BitOp::TrailingZeros => format!(
            "fn {name}(x: {k}) -> u32 {{ let p: {un} = {pattern}; {} c }}",
            popcount("~p & wrapping_sub(p, 1)")
        ),
        BitOp::LeadingZeros => {
            let mut smear = format!("let mut p: {un} = {pattern}; ");
            let mut s = 1;
            while s < bits {
                smear += &format!("p = p | (p >> {s}); ");
                s *= 2;
            }
            format!(
                "fn {name}(x: {k}) -> u32 {{ {smear}{} \
                 let width: u32 = {bits}; wrapping_sub(width, c) }}",
                popcount("p")
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

/// A checked builtin ([module docs](self)): `checked_add_k`, `checked_sub_k`
/// and `checked_mul_k` return `Option[k]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckedOp {
    Add,
    Sub,
    Mul,
}

/// The checked builtin families, by name prefix (the suffix is the kind).
pub const CHECKED_BUILTINS: [(&str, CheckedOp); 3] = [
    ("checked_add_", CheckedOp::Add),
    ("checked_sub_", CheckedOp::Sub),
    ("checked_mul_", CheckedOp::Mul),
];

/// The checked builtin spelled `name`, with its integer kind.
pub fn checked_builtin(name: &str) -> Option<(CheckedOp, IntegerKind)> {
    CHECKED_BUILTINS.iter().find_map(|(prefix, op)| {
        let kind = IntegerKind::from_name(name.strip_prefix(prefix)?)?;
        Some((*op, kind))
    })
}

impl CheckedOp {
    /// The reference result: `None` where the checked operator overflows.
    pub fn eval(self, kind: IntegerKind, a: i128, b: i128) -> Option<i128> {
        let exact = match self {
            Self::Add => a.checked_add(b),
            Self::Sub => a.checked_sub(b),
            Self::Mul => a.checked_mul(b),
        }?;
        (kind.min()..=kind.max()).contains(&exact).then_some(exact)
    }
}

/// The portable source of the checked builtin `name` (see [`CheckedOp`]).
/// Every operation in it is total, so the function needs no `Fail`.
fn checked_source(name: &str) -> Option<String> {
    let (op, kind) = checked_builtin(name)?;
    let k = kind.name();
    let head = format!("fn {name}(a: {k}, b: {k}) -> Option[{k}]");
    let some = |v: &str| format!("Option::Some({v})");
    let body = match (op, kind.signed(), kind.bits()) {
        // Unsigned: a carry out of the top bit leaves the sum below `a`.
        (CheckedOp::Add, false, _) => format!(
            "let s: {k} = wrapping_add(a, b); if s < a {{ Option::None }} else {{ {} }}",
            some("s")
        ),
        (CheckedOp::Sub, false, _) => format!(
            "if b > a {{ Option::None }} else {{ {} }}",
            some("wrapping_sub(a, b)")
        ),
        // Signed: overflow flips the sign of a sum of like-signed operands,
        // or of a difference of unlike-signed ones.
        (CheckedOp::Add | CheckedOp::Sub, true, _) => {
            let (wrap, like) = if op == CheckedOp::Add {
                ("wrapping_add", "==")
            } else {
                ("wrapping_sub", "!=")
            };
            format!(
                "let s: {k} = {wrap}(a, b); \
                 if (a < 0) {like} (b < 0) && (s < 0) != (a < 0) {{ Option::None }} else {{ {} }}",
                some("s")
            )
        }
        // Narrow kinds: the exact product fits in 64 bits.
        (CheckedOp::Mul, false, bits) if bits <= 32 => format!(
            "let p: u64 = wrapping_mul(widen_u64(a), widen_u64(b)); \
             if p > {} {{ Option::None }} else {{ {} }}",
            kind.max(),
            some(&format!("wrapping_{k}(p)"))
        ),
        (CheckedOp::Mul, true, bits) if bits <= 32 => format!(
            "let p: i64 = wrapping_mul(widen_i64(a), widen_i64(b)); \
             if p < {} || p > {} {{ Option::None }} else {{ {} }}",
            kind.min(),
            kind.max(),
            some(&format!("wrapping_{k}(p)"))
        ),
        // 128-bit kinds: no wider product to read, so a nonzero factor
        // divides the wrapped product back exactly iff it did not wrap.
        (CheckedOp::Mul, false, 128) => format!(
            "if a == 0 {{ {} }} else {{ let p: {k} = wrapping_mul(a, b); \
             if p / a != b {{ Option::None }} else {{ {} }} }}",
            some("a"),
            some("p")
        ),
        (CheckedOp::Mul, true, 128) => format!(
            "let zero: {k} = 0; let uzero: u128 = 0; \
             let ua: u128 = if a < 0 {{ wrapping_u128(wrapping_sub(zero, a)) }} else {{ wrapping_u128(a) }}; \
             let ub: u128 = if b < 0 {{ wrapping_u128(wrapping_sub(zero, b)) }} else {{ wrapping_u128(b) }}; \
             if ua == 0 {{ {} }} else {{ \
                 let p: u128 = wrapping_mul(ua, ub); \
                 let one: u128 = 1; let limit: u128 = rotate_right(one, 1); \
                 if p / ua != ub {{ Option::None }} \
                 else if (a < 0) != (b < 0) {{ \
                     if p > limit {{ Option::None }} else {{ {} }} }} \
                 else if p >= limit {{ Option::None }} \
                 else {{ {} }} }}",
            some("zero"),
            some(&format!("wrapping_{k}(wrapping_sub(uzero, p))")),
            some(&format!("wrapping_{k}(p)"))
        ),
        // 64-bit kinds: the high word of the full product of the magnitudes.
        (CheckedOp::Mul, false, _) => format!(
            "let (lo, hi) = mul_wide(wrapping_u64(a), wrapping_u64(b)); \
             if hi != 0 {{ Option::None }} else {{ {} }}",
            some(&format!("wrapping_{k}(lo)"))
        ),
        (CheckedOp::Mul, true, _) => format!(
            "let zero: {k} = 0; \
             let ua: u64 = if a < 0 {{ wrapping_u64(wrapping_sub(zero, a)) }} else {{ wrapping_u64(a) }}; \
             let ub: u64 = if b < 0 {{ wrapping_u64(wrapping_sub(zero, b)) }} else {{ wrapping_u64(b) }}; \
             let (lo, hi) = mul_wide(ua, ub); \
             let limit: u64 = 9223372036854775807; \
             if hi != 0 {{ Option::None }} \
             else if (a < 0) != (b < 0) {{ \
                 if lo > wrapping_add(limit, 1) {{ Option::None }} \
                 else {{ {} }} }} \
             else if lo > limit {{ Option::None }} \
             else {{ {} }}",
            some(&format!("wrapping_{k}(wrapping_sub(0, lo))")),
            some(&format!("wrapping_{k}(lo)"))
        ),
    };
    Some(format!("{head} {{ {body} }}"))
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
