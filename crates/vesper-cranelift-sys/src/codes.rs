//! The integer codes of the C ABI: types, opcodes, condition codes, flags.
//! `include/vesper_cranelift.h` and `packages/vesper-cranelift/src/lib.ves`
//! spell the same numbers; `tests/header.rs` checks the header against
//! these tables.

use cranelift_codegen::binemit::Reloc;
use cranelift_codegen::ir::{
    MemFlags, TrapCode, Type, condcodes::FloatCC, condcodes::IntCC, types,
};
use cranelift_codegen::isa::CallConv;
use cranelift_module::Linkage;

/// A type code is `lane | (log2(lanes) << 8)`: `VCL_I64` is 4, `VCL_I8X16`
/// is `1 | (4 << 8)`.
pub const TYPE_LANES: &[(u32, &str, Type)] = &[
    (1, "I8", types::I8),
    (2, "I16", types::I16),
    (3, "I32", types::I32),
    (4, "I64", types::I64),
    (5, "I128", types::I128),
    (6, "F32", types::F32),
    (7, "F64", types::F64),
];

pub fn ty(code: u32) -> Option<Type> {
    let lane = TYPE_LANES.iter().find(|(c, _, _)| *c == code & 0xff)?.2;
    match (code >> 8) & 0xff {
        0 if code >> 16 == 0 => Some(lane),
        log2 @ 1..=6 if code >> 16 == 0 => lane.by(1 << log2),
        _ => None,
    }
}

pub fn ty_code(t: Type) -> u32 {
    let lane = TYPE_LANES
        .iter()
        .find(|(_, _, l)| *l == t.lane_type())
        .map_or(0, |(c, _, _)| *c);
    lane | t.log2_lane_count() << 8
}

macro_rules! table {
    ($(#[$m:meta])* $name:ident: $($code:literal $op:ident),* $(,)?) => {
        $(#[$m])*
        pub const $name: &[(u32, &str)] = &[$(($code, stringify!($op))),*];
    };
}

table!(
    /// `vcl_binary(b, op, x, y)`: one result of the type of `x`.
    BINARY: 1 iadd, 2 isub, 3 imul, 4 udiv, 5 sdiv, 6 urem, 7 srem,
    8 umulhi, 9 smulhi, 10 band, 11 bor, 12 bxor, 13 band_not, 14 bor_not,
    15 bxor_not, 16 ishl, 17 ushr, 18 sshr, 19 rotl, 20 rotr, 21 umin,
    22 umax, 23 smin, 24 smax, 25 fadd, 26 fsub, 27 fmul, 28 fdiv, 29 fmin,
    30 fmax, 31 fcopysign, 32 swizzle, 33 iadd_pairwise, 34 avg_round,
    35 uadd_sat, 36 sadd_sat, 37 usub_sat, 38 ssub_sat,
);
table!(
    /// `vcl_binary_imm(b, op, x, imm)`.
    BINARY_IMM: 1 iadd_imm, 2 imul_imm, 3 udiv_imm, 4 sdiv_imm, 5 urem_imm,
    6 srem_imm, 7 irsub_imm, 10 band_imm, 11 bor_imm, 12 bxor_imm,
    16 ishl_imm, 17 ushr_imm, 18 sshr_imm, 19 rotl_imm, 20 rotr_imm,
);
table!(
    /// `vcl_unary(b, op, x)`.
    UNARY: 1 ineg, 2 bnot, 3 clz, 4 ctz, 5 popcnt, 6 bswap, 7 bitrev,
    8 cls, 9 iabs, 10 fneg, 11 fabs, 12 sqrt, 13 ceil, 14 floor, 15 trunc,
    16 nearest, 18 vany_true, 19 vall_true, 20 fvpromote_low, 21 fvdemote,
);
table!(
    /// `vcl_convert(b, op, ty, x)`: the result type is `ty`.
    CONVERT: 1 uextend, 2 sextend, 3 ireduce, 4 bitcast, 5 fpromote,
    6 fdemote, 7 fcvt_to_uint, 8 fcvt_to_sint, 9 fcvt_to_uint_sat,
    10 fcvt_to_sint_sat, 11 fcvt_from_uint, 12 fcvt_from_sint, 13 splat,
    14 vhigh_bits, 15 bitcast_le,
);
table!(
    /// `vcl_overflow(b, op, x, y)`: two results, the value and the flag
    /// (read the flag with `vcl_inst_result(b, 1)`).
    OVERFLOW: 1 uadd_overflow, 2 sadd_overflow, 3 usub_overflow,
    4 ssub_overflow, 5 umul_overflow, 6 smul_overflow,
);
table!(
    /// `vcl_load(b, op, ty, flags, addr, offset)`.
    LOAD: 1 load, 2 uload8, 3 sload8, 4 uload16, 5 sload16, 6 uload32,
    7 sload32,
);
table!(
    /// `vcl_store(b, op, flags, x, addr, offset)`.
    STORE: 1 store, 2 istore8, 3 istore16, 4 istore32,
);
table!(
    /// `vcl_ternary(b, op, x, y, z)`.
    TERNARY: 1 select, 2 bitselect, 3 fma, 4 select_spectre_guard,
);
table!(
    /// `vcl_icmp(b, cc, x, y)`, `vcl_icmp_imm(b, cc, x, imm)`.
    INTCC: 0 eq, 1 ne, 2 slt, 3 sge, 4 sgt, 5 sle, 6 ult, 7 uge, 8 ugt, 9 ule,
);
table!(
    /// `vcl_fcmp(b, cc, x, y)`.
    FLOATCC: 0 ord, 1 uno, 2 eq, 3 ne, 4 one, 5 ueq, 6 lt, 7 le, 8 gt, 9 ge,
    10 ult, 11 ule, 12 ugt, 13 uge,
);
table!(
    /// `vcl_declare_function`/`vcl_declare_data` linkage.
    LINKAGE: 0 import, 1 local, 2 preemptible, 3 hidden, 4 export,
);
table!(
    /// `vcl_sig_new` calling convention; 0 is the target's default.
    CALLCONV: 0 default, 1 system_v, 2 windows_fastcall, 3 apple_aarch64,
    4 tail, 5 fast, 6 cold,
);
table!(
    /// `vcl_trap` codes: 1-4 are Cranelift's reserved codes, 16 + n is
    /// `TrapCode::user(n)`.
    TRAP: 1 stack_overflow, 2 integer_overflow, 3 heap_out_of_bounds,
    4 integer_division_by_zero, 5 bad_conversion_to_integer,
);
table!(
    /// Memory flag bits.
    MEMFLAG: 1 notrap, 2 aligned, 4 readonly, 8 big_endian, 16 little_endian,
);

pub fn intcc(code: u32) -> Option<IntCC> {
    use IntCC::*;
    Some(match code {
        0 => Equal,
        1 => NotEqual,
        2 => SignedLessThan,
        3 => SignedGreaterThanOrEqual,
        4 => SignedGreaterThan,
        5 => SignedLessThanOrEqual,
        6 => UnsignedLessThan,
        7 => UnsignedGreaterThanOrEqual,
        8 => UnsignedGreaterThan,
        9 => UnsignedLessThanOrEqual,
        _ => return None,
    })
}

pub fn floatcc(code: u32) -> Option<FloatCC> {
    use FloatCC::*;
    Some(match code {
        0 => Ordered,
        1 => Unordered,
        2 => Equal,
        3 => NotEqual,
        4 => OrderedNotEqual,
        5 => UnorderedOrEqual,
        6 => LessThan,
        7 => LessThanOrEqual,
        8 => GreaterThan,
        9 => GreaterThanOrEqual,
        10 => UnorderedOrLessThan,
        11 => UnorderedOrLessThanOrEqual,
        12 => UnorderedOrGreaterThan,
        13 => UnorderedOrGreaterThanOrEqual,
        _ => return None,
    })
}

pub fn linkage(code: u32) -> Option<Linkage> {
    Some(match code {
        0 => Linkage::Import,
        1 => Linkage::Local,
        2 => Linkage::Preemptible,
        3 => Linkage::Hidden,
        4 => Linkage::Export,
        _ => return None,
    })
}

pub fn call_conv(code: u32, default: CallConv) -> Option<CallConv> {
    Some(match code {
        0 => default,
        1 => CallConv::SystemV,
        2 => CallConv::WindowsFastcall,
        3 => CallConv::AppleAarch64,
        4 => CallConv::Tail,
        5 => CallConv::Fast,
        6 => CallConv::Cold,
        _ => return None,
    })
}

pub fn trap(code: u32) -> Option<TrapCode> {
    Some(match code {
        1 => TrapCode::STACK_OVERFLOW,
        2 => TrapCode::INTEGER_OVERFLOW,
        3 => TrapCode::HEAP_OUT_OF_BOUNDS,
        4 => TrapCode::INTEGER_DIVISION_BY_ZERO,
        5 => TrapCode::BAD_CONVERSION_TO_INTEGER,
        16..=271 => TrapCode::user(u8::try_from(code - 16).ok()?)?,
        _ => return None,
    })
}

pub fn mem_flags(bits: u32) -> Option<MemFlags> {
    if bits & !31 != 0 || bits & 24 == 24 {
        return None;
    }
    let mut f = MemFlags::new();
    if bits & 1 != 0 {
        f.set_notrap();
    }
    if bits & 2 != 0 {
        f.set_aligned();
    }
    if bits & 4 != 0 {
        f.set_readonly();
    }
    if bits & 8 != 0 {
        f.set_endianness(cranelift_codegen::ir::Endianness::Big);
    }
    if bits & 16 != 0 {
        f.set_endianness(cranelift_codegen::ir::Endianness::Little);
    }
    Some(f)
}

/// The name of `code` in `table`, for diagnostics.
pub fn name(table: &[(u32, &'static str)], code: u32) -> Option<&'static str> {
    table.iter().find(|(c, _)| *c == code).map(|(_, n)| *n)
}

// ---------------------------------------------------------------------------
// SH5 lane 2 (#1325): parameter extension, relocations and their targets.

table!(
    /// The `ext` argument of `vcl_sig_param_ext` and `vcl_sig_return_ext`.
    EXT: 0 none, 1 uext, 2 sext,
);

/// `AbiParam::new(t)` with extension `ext` ([`EXT`]).
pub fn abi_param(t: Type, ext: u32) -> Option<cranelift_codegen::ir::AbiParam> {
    let p = cranelift_codegen::ir::AbiParam::new(t);
    Some(match ext {
        0 => p,
        1 => p.uext(),
        2 => p.sext(),
        _ => return None,
    })
}

/// The linkage code of `l` (the inverse of [`linkage`]).
pub fn linkage_code(l: Linkage) -> u32 {
    match l {
        Linkage::Import => 0,
        Linkage::Local => 1,
        Linkage::Preemptible => 2,
        Linkage::Hidden => 3,
        Linkage::Export => 4,
    }
}

/// Relocation kinds, numbered in `cranelift_codegen::binemit::Reloc`'s
/// order from 1.
pub const RELOC: &[(u32, Reloc)] = &[
    (1, Reloc::Abs4),
    (2, Reloc::Abs8),
    (3, Reloc::X86PCRel4),
    (4, Reloc::X86CallPCRel4),
    (5, Reloc::X86CallPLTRel4),
    (6, Reloc::X86GOTPCRel4),
    (7, Reloc::X86SecRel),
    (8, Reloc::Arm32Call),
    (9, Reloc::Arm64Call),
    (10, Reloc::S390xPCRel32Dbl),
    (11, Reloc::S390xPLTRel32Dbl),
    (12, Reloc::MachOAarch64TlsAdrPage21),
    (13, Reloc::MachOAarch64TlsAdrPageOff12),
    (14, Reloc::Aarch64TlsDescAdrPage21),
    (15, Reloc::Aarch64TlsDescLd64Lo12),
    (16, Reloc::Aarch64TlsDescAddLo12),
    (17, Reloc::Aarch64TlsDescCall),
    (18, Reloc::Aarch64AdrGotPage21),
    (19, Reloc::Aarch64AdrPrelPgHi21),
    (20, Reloc::Aarch64AddAbsLo12Nc),
    (21, Reloc::Aarch64Ld64GotLo12Nc),
    (22, Reloc::RiscvCallPlt),
    (23, Reloc::RiscvTlsGdHi20),
    (24, Reloc::RiscvPCRelLo12I),
    (25, Reloc::RiscvGotHi20),
    (26, Reloc::RiscvPCRelHi20),
    (27, Reloc::S390xTlsGd64),
    (28, Reloc::S390xTlsGdCall),
    (29, Reloc::PulleyPcRel),
    (30, Reloc::PulleyCallIndirectHost),
];

pub fn reloc(code: u32) -> Option<Reloc> {
    RELOC.iter().find(|(c, _)| *c == code).map(|(_, r)| *r)
}

/// The code of `r` (0 for a kind newer than this table).
pub fn reloc_code(r: Reloc) -> u32 {
    RELOC.iter().find(|(_, k)| *k == r).map_or(0, |(c, _)| *c)
}

table!(
    /// What a relocation's target word names (its third word).
    RELOC_TARGET: 0 function, 1 data, 2 libcall, 3 known_symbol,
    4 function_offset, 5 user_name,
);
