//! Function definition swap for the experimental Vesper machine backend.
use cranelift_codegen::{
    binemit::Reloc,
    entity::SecondaryMap,
    ir::{
        AbiParam, AtomicRmwOp, Block, BlockCall, Endianness, ExternalName, Function, GlobalValue,
        GlobalValueData, Inst, InstructionData, LibCall, MemFlags, Opcode, Signature, StackSlot,
        StackSlotKind, TrapCode, Type, Value, ValueDef,
        condcodes::{FloatCC, IntCC},
        immediates::{Ieee32, Ieee64, Offset32},
        types,
    },
    isa::{CallConv, TargetIsa},
};
use cranelift_module::{DataId, Linkage, ModuleDeclarations, ModuleReloc, ModuleRelocTarget};
use std::{collections::BTreeSet, sync::OnceLock};

/// Opcodes in the architecture-independent integer block format.
#[repr(u64)]
#[derive(Clone, Copy)]
enum IntegerOp {
    Constant = 0,
    Add = 1,
    Sub = 2,
    Mul = 3,
    And = 4,
    Or = 5,
    Xor = 6,
    Neg = 7,
    Not = 8,
    ShiftLeft = 9,
    ShiftRightLogical = 10,
    ShiftRightArithmetic = 11,
    Compare = 12,
    CompareByte = 13,
    Select = 14,
    Truncate = 15,
    ZeroExtend = 16,
    SignExtend = 17,
    CompareWord = 18,
    CompareDword = 19,
    ShiftLeftRegister = 20,
    ShiftRightLogicalRegister = 21,
    ShiftRightArithmeticRegister = 22,
    Load = 23,
    StoreByte = 24,
    StoreWord = 25,
    StoreDword = 26,
    StoreQword = 27,
    BranchZero = 28,
    EarlyReturn = 29,
    StackSlot = 30,
    StackAddress = 31,
    StackLoad = 32,
    StackStoreByte = 33,
    StackStoreWord = 34,
    StackStoreDword = 35,
    StackStoreQword = 36,
    ReturnPair = 37,
    Call = 38,
    CallArgument = 39,
    CallVoid = 40,
    CallPair = 41,
    CallHigh = 42,
    SymbolRelative = 43,
    SymbolGot = 44,
    Trap = 45,
    SignedDiv = 46,
    UnsignedDiv = 47,
    SignedRem = 48,
    UnsignedRem = 49,
    SignedMulHigh = 50,
    UnsignedMulHigh = 51,
    AddImmediate = 52,
    SubImmediate = 53,
    MulImmediate = 54,
    AndImmediate = 55,
    OrImmediate = 56,
    XorImmediate = 57,
    CompareImmediate = 58,
    CompareByteImmediate = 59,
    CompareWordImmediate = 60,
    CompareDwordImmediate = 61,
    BlockLabel = 62,
    EdgeLabel = 63,
    JumpLabel = 64,
    BranchLabel = 65,
    BlockReturn = 66,
    BranchBlock = 67,
    UnsignedAddOverflow = 68,
    SignedAddOverflow = 69,
    UnsignedSubOverflow = 70,
    SignedSubOverflow = 71,
    UnsignedMulOverflow = 72,
    SignedMulOverflow = 73,
    OverflowFlag = 74,
    BranchTable = 75,
    BranchTableTarget = 76,
    AddCarry = 77,
    SubBorrow = 78,
    FloatArgument = 79,
    FloatConstant = 80,
    FloatAdd = 81,
    FloatSub = 82,
    FloatMul = 83,
    FloatDiv = 84,
    FloatSqrt = 85,
    FloatNeg = 86,
    FloatAbs = 87,
    FloatCompare32 = 88,
    FloatCompare64 = 89,
    FloatLoad = 90,
    FloatStore32 = 91,
    FloatStore64 = 92,
    BitsToFloat = 93,
    FloatToBits = 94,
    FloatSelect = 95,
    FloatReturn = 96,
    CallFloat = 97,
    FloatStackLoad = 98,
    FloatStackStore32 = 99,
    FloatStackStore64 = 100,
    FloatCopySign = 101,
    AtomicAdd = 102,
    AtomicSub = 103,
    AtomicExchange = 104,
    Fence = 105,
    ByteSwap = 106,
    LeadingZeros = 107,
    TrailingZeros = 108,
    PopulationCount = 109,
    ReverseBits = 110,
    LeadingSignBits = 111,
    RotateRight = 112,
    RotateRightRegister = 113,
    RotateLeftRegister = 114,
    FunnelRight = 115,
    AndNot = 116,
    OrNot = 117,
    XorNot = 118,
    Abs = 119,
    AtomicLoad = 120,
    AtomicStore = 121,
    AtomicCasByte = 122,
    AtomicCasWord = 123,
    AtomicCasDword = 124,
    AtomicCasQword = 125,
    AtomicAnd = 126,
    AtomicNand = 127,
    AtomicOr = 128,
    AtomicXor = 129,
    AtomicUnsignedMin = 130,
    AtomicUnsignedMax = 131,
    AtomicSignedMin = 132,
    AtomicSignedMax = 133,
    CallFloat32 = 134,
    FloatPromote = 135,
    FloatDemote = 136,
    SignedToFloat = 137,
    UnsignedToFloat = 138,
    FloatToSignedSat = 139,
    FloatToUnsignedSat = 140,
    FloatCeil = 141,
    FloatFloor = 142,
    FloatTrunc = 143,
    FloatNearest = 144,
    FloatMin = 145,
    FloatMax = 146,
    FloatFma32 = 147,
    FloatFma64 = 148,
    StackArgument = 149,
    FloatStackArgument = 150,
    CallStackArgument = 151,
    FunctionGot = 152,
    TrapIfZero = 153,
    TrapIfNonzero = 154,
    StackPointer = 155,
    SpectreBarrier = 156,
    FloatToIntegerTrunc = 157,
    VectorConstant = 158,
    VectorSplat = 159,
    VectorLoad = 160,
    VectorStore = 161,
    VectorAnd = 162,
    VectorOr = 163,
    VectorXor = 164,
    VectorAdd = 165,
    VectorSub = 166,
    VectorSignedSatAdd = 167,
    VectorUnsignedSatAdd = 168,
    VectorSignedSatSub = 169,
    VectorUnsignedSatSub = 170,
    VectorRoundedAverage = 171,
    VectorPromoteLow = 172,
    VectorDemote = 173,
    VectorPairwiseAdd = 174,
    VectorSwizzle = 175,
    VectorInsert = 176,
    VectorExtract = 177,
    VectorAnyTrue = 178,
    VectorAllTrue = 179,
    VectorHighBits = 180,
    TailCall = 181,
    VectorIntegerCompare = 182,
    VectorFloatAdd = 185,
    VectorFloatSub = 186,
    VectorFloatMul = 187,
    VectorFloatDiv = 188,
    VectorFloatSqrt = 189,
    VectorFloatCompare = 190,
    VectorFloatNeg = 191,
    VectorFloatAbs = 192,
    VectorFloatCopySign = 193,
    VectorFloatFma32 = 194,
    VectorFloatFma64 = 195,
    VectorFloatCeil = 196,
    VectorFloatFloor = 197,
    VectorFloatTrunc = 198,
    VectorFloatNearest = 199,
    VectorFloatMin = 200,
    VectorFloatMax = 201,
    VectorMul = 202,
    VectorShiftLeft = 203,
    VectorShiftRightLogical = 204,
    VectorShiftRightArithmetic = 205,
    VectorIntegerMinMax = 206,
    VectorWiden = 207,
    VectorNarrow = 208,
}

fn integer_bit_op(op: Opcode) -> Option<IntegerOp> {
    Some(match op {
        Opcode::Clz => IntegerOp::LeadingZeros,
        Opcode::Ctz => IntegerOp::TrailingZeros,
        Opcode::Popcnt => IntegerOp::PopulationCount,
        Opcode::Bitrev => IntegerOp::ReverseBits,
        Opcode::Cls => IntegerOp::LeadingSignBits,
        _ => return None,
    })
}

fn right_rotation_count(op: Opcode, count: i64) -> u64 {
    let count = count as u64;
    if matches!(op, Opcode::Rotl | Opcode::RotlImm) {
        count.wrapping_neg()
    } else {
        count
    }
}

fn rotate_register_op(op: Opcode) -> IntegerOp {
    if op == Opcode::Rotl {
        IntegerOp::RotateLeftRegister
    } else {
        IntegerOp::RotateRightRegister
    }
}

type Encoder = fn(&[u64]) -> Option<Vec<u8>>;
static ENCODER: OnceLock<Encoder> = OnceLock::new();
static RELOCATION_ENCODER: OnceLock<Encoder> = OnceLock::new();
static AARCH64_ENCODER: OnceLock<Encoder> = OnceLock::new();
static WINDOWS_ENCODER: OnceLock<Encoder> = OnceLock::new();

/// Install the x86-64 integer encoder. A declined request uses Cranelift.
pub fn install_integer_encoder(encode: Encoder) -> Result<(), Encoder> {
    ENCODER.set(encode)
}

/// Installs the encoder for functions that can reference external symbols.
pub fn install_relocation_encoder(encode: Encoder) -> Result<(), Encoder> {
    RELOCATION_ENCODER.set(encode)
}

/// Installs the AArch64 encoder for ordinary native objects, including relocation packets.
pub fn install_aarch64_encoder(encode: Encoder) -> Result<(), Encoder> {
    AARCH64_ENCODER.set(encode)
}

/// Installs the Win64 encoder for code and its unwind record.
pub fn install_windows_encoder(encode: Encoder) -> Result<(), Encoder> {
    WINDOWS_ENCODER.set(encode)
}

const ARGUMENT_COUNT: usize = 0;
const RETURN_VALUE: usize = 1;
const HEADER_WORDS: usize = 2;
const INSTRUCTION_WORDS: usize = 4;
const NO_VALUE: u64 = u64::MAX;
const TRAP_VALUE: u64 = NO_VALUE - 1;
const GP_REGISTER_ARGUMENTS: usize = 6;
const AARCH64_GP_ARGUMENTS: usize = 8;
const AARCH64_TAIL_RESERVED_GP: usize = 2;
const XMM_REGISTER_ARGUMENTS: usize = 8;
const AARCH64_FP_ARGUMENTS: usize = 8;
const AARCH64_CALL_FLOAT_BITS: u32 = 4;
const INDIRECT_CALL: u64 = 1 << (u64::BITS - 1);
const WINDOWS_ABI: u64 = 1 << (u64::BITS - 1);
const WINDOWS_REGISTER_ARGUMENTS: usize = 4;
const STACK_PROBE_INTERVAL: usize = 4096;
const CFG_HOME_BYTES: u64 = size_of::<u64>() as u64;
const MAX_GP_ARGUMENTS: usize = GP_REGISTER_ARGUMENTS + STACK_PROBE_INTERVAL / size_of::<u64>() - 1;
const GP_RESULTS: usize = 2;
const FLOAT_RETURN_LAYOUT: u64 = GP_RESULTS as u64 + 1;
const WIDE_INTEGER_CELLS: usize = 2;
const INTEGER_CELL_BITS: u64 = u64::BITS as u64;
const SIMD_BYTES: u32 = 2 * size_of::<u64>() as u32;
const SIMD_LANE_INDEX_SHIFT: u32 = 8;
const SIMD_CONDITION_SHIFT: u32 = 8;
const X86_VECTOR_MUL_SSE41: u64 = 1 << SIMD_LANE_INDEX_SHIFT;
const VECTOR_SHIFT_IMMEDIATE: u64 = 1 << SIMD_LANE_INDEX_SHIFT;
const VECTOR_MINMAX_SIGNED: u64 = 1 << SIMD_CONDITION_SHIFT;
const VECTOR_MINMAX_MINIMUM: u64 = VECTOR_MINMAX_SIGNED << 1;
const VECTOR_CONVERSION_SIGNED: u64 = 1 << SIMD_CONDITION_SHIFT;
const VECTOR_WIDEN_HIGH: u64 = VECTOR_CONVERSION_SIGNED << 1;
const X86_VECTOR_WIDEN_SSE41: u64 = VECTOR_WIDEN_HIGH << 1;
const VECTOR_NARROW_UNSIGNED_INPUT: u64 = VECTOR_CONVERSION_SIGNED << 1;
const X86_HARDWARE_POPCOUNT: u64 = 1;
const X86_SWIZZLE_ZERO_HIGH: u64 = 1;
const X86_SWIZZLE_ZERO_INDEX: u8 = 0x80;
const MAX_LINEARIZED_BRANCHES: usize = 32;
// Keep small joins linear. Compare tape sizes once repeated bodies are substantial.
const MIN_DUPLICATED_INSTRUCTIONS_FOR_CFG: usize = 32;
pub(super) const FUNCTION_NAMESPACE: u32 = 0;
pub(super) const DATA_NAMESPACE: u32 = 1;
const REL32_BYTES: i64 = size_of::<i32>() as i64;
const PLT_CALL: u64 = 0;
const RELATIVE_DATA: u64 = 1;
const GOT_DATA: u64 = 2;
const PLT_LIBCALL: u64 = 3;
const ARM64_CALL: u64 = 4;
const ARM64_LIBCALL: u64 = 5;
const ARM64_GOT_PAGE: u64 = 6;
const ARM64_GOT_LOAD: u64 = 7;
const ARM64_FUNCTION_GOT_PAGE: u64 = 8;
const ARM64_FUNCTION_GOT_LOAD: u64 = 9;
const X86_FUNCTION_GOT: u64 = 10;
const WIN64_CALL: u64 = 11;
const LIBCALL_TARGET_BASE: u64 = u32::MAX as u64 + 1;
const MEMMOVE_LIBCALL: u32 = 0;
const MEMCPY_LIBCALL: u32 = 1;

fn binary_op(opcode: Opcode) -> Option<IntegerOp> {
    Some(match opcode {
        Opcode::Iadd | Opcode::IaddImm => IntegerOp::Add,
        Opcode::Isub => IntegerOp::Sub,
        Opcode::Imul | Opcode::ImulImm => IntegerOp::Mul,
        Opcode::Band | Opcode::BandImm => IntegerOp::And,
        Opcode::Bor | Opcode::BorImm => IntegerOp::Or,
        Opcode::Bxor | Opcode::BxorImm => IntegerOp::Xor,
        _ => return None,
    })
}

fn minmax_condition(opcode: Opcode) -> Option<IntCC> {
    Some(match opcode {
        Opcode::Smin => IntCC::SignedLessThan,
        Opcode::Smax => IntCC::SignedGreaterThan,
        Opcode::Umin => IntCC::UnsignedLessThan,
        Opcode::Umax => IntCC::UnsignedGreaterThan,
        _ => return None,
    })
}

fn division_op(opcode: Opcode) -> Option<IntegerOp> {
    Some(match opcode {
        Opcode::Sdiv | Opcode::SdivImm => IntegerOp::SignedDiv,
        Opcode::Udiv | Opcode::UdivImm => IntegerOp::UnsignedDiv,
        Opcode::Srem | Opcode::SremImm => IntegerOp::SignedRem,
        Opcode::Urem | Opcode::UremImm => IntegerOp::UnsignedRem,
        _ => return None,
    })
}

fn checked_binary_op(opcode: Opcode) -> Option<IntegerOp> {
    Some(match opcode {
        Opcode::UaddOverflow => IntegerOp::UnsignedAddOverflow,
        Opcode::SaddOverflow => IntegerOp::SignedAddOverflow,
        Opcode::UsubOverflow => IntegerOp::UnsignedSubOverflow,
        Opcode::SsubOverflow => IntegerOp::SignedSubOverflow,
        Opcode::UmulOverflow => IntegerOp::UnsignedMulOverflow,
        Opcode::SmulOverflow => IntegerOp::SignedMulOverflow,
        _ => return None,
    })
}

fn integer_width(ty: Type) -> Option<u64> {
    matches!(ty, types::I8 | types::I16 | types::I32 | types::I64).then(|| u64::from(ty.bits()))
}

fn float_width(ty: Type) -> Option<u64> {
    matches!(ty, types::F32 | types::F64).then(|| u64::from(ty.bits()))
}

fn windows_scalar(param: &AbiParam) -> bool {
    (integer_width(param.value_type).is_some() || float_width(param.value_type).is_some())
        && *param == AbiParam::new(param.value_type)
}

fn vector_type(ty: Type) -> bool {
    ty.is_vector() && ty.bytes() == SIMD_BYTES
}

fn vector_shift_op(opcode: Opcode) -> Option<IntegerOp> {
    match opcode {
        Opcode::Ishl | Opcode::IshlImm => Some(IntegerOp::VectorShiftLeft),
        Opcode::Ushr | Opcode::UshrImm => Some(IntegerOp::VectorShiftRightLogical),
        Opcode::Sshr | Opcode::SshrImm => Some(IntegerOp::VectorShiftRightArithmetic),
        _ => None,
    }
}

fn float_condition(cond: FloatCC) -> u64 {
    match cond {
        FloatCC::Ordered => 0,
        FloatCC::Unordered => 1,
        FloatCC::Equal => 2,
        FloatCC::NotEqual => 3,
        FloatCC::OrderedNotEqual => 4,
        FloatCC::UnorderedOrEqual => 5,
        FloatCC::LessThan => 6,
        FloatCC::LessThanOrEqual => 7,
        FloatCC::GreaterThan => 8,
        FloatCC::GreaterThanOrEqual => 9,
        FloatCC::UnorderedOrLessThan => 10,
        FloatCC::UnorderedOrLessThanOrEqual => 11,
        FloatCC::UnorderedOrGreaterThan => 12,
        FloatCC::UnorderedOrGreaterThanOrEqual => 13,
    }
}

#[derive(Clone, Copy)]
enum Aarch64Argument {
    Gp(usize),
    Float(usize),
    Stack(usize),
    StackPair(usize),
}

enum CallTarget {
    Direct(u64),
    Indirect(u64),
}

struct Aarch64Arguments {
    locations: Vec<Aarch64Argument>,
    gp: usize,
    fp: usize,
}

fn aarch64_arguments(params: &[AbiParam], convention: CallConv) -> Option<Aarch64Arguments> {
    let mut args = Aarch64Arguments {
        locations: Vec::with_capacity(params.len()),
        gp: if convention == CallConv::Tail {
            AARCH64_TAIL_RESERVED_GP
        } else {
            0
        },
        fp: 0,
    };
    let mut stack: usize = 0;
    let mut wide_on_stack = false;
    for param in params {
        let ty = param.value_type;
        let wide = ty == types::I128;
        let float = float_width(ty).is_some();
        if *param != AbiParam::new(ty) || (!wide && !float && integer_width(ty).is_none()) {
            return None;
        }
        let register = if wide {
            // AAPCS64 aligns pairs to an even register. Apple packs them.
            let first = if convention != CallConv::AppleAarch64 {
                args.gp.next_multiple_of(WIDE_INTEGER_CELLS)
            } else {
                args.gp
            };
            let end = first + WIDE_INTEGER_CELLS;
            if end <= AARCH64_GP_ARGUMENTS {
                args.gp = end;
                Some(Aarch64Argument::Gp(first))
            } else {
                wide_on_stack = true;
                None
            }
        } else if float {
            if args.fp < AARCH64_FP_ARGUMENTS {
                let register = args.fp;
                args.fp += 1;
                Some(Aarch64Argument::Float(register))
            } else {
                None
            }
        } else if args.gp < AARCH64_GP_ARGUMENTS {
            // Cranelift may still assign x7 after a wide stack argument. Keep
            // the fallback for this signature because AAPCS64 exhausts the bank.
            if wide_on_stack {
                return None;
            }
            let register = args.gp;
            args.gp += 1;
            Some(Aarch64Argument::Gp(register))
        } else {
            None
        };
        if let Some(register) = register {
            args.locations.push(register);
        } else {
            let bytes = ty.bytes() as usize;
            let slot = if convention == CallConv::AppleAarch64 {
                bytes
            } else {
                bytes.max(size_of::<u64>())
            };
            let offset = stack.next_multiple_of(slot);
            stack = offset + slot;
            if stack > STACK_PROBE_INTERVAL {
                return None;
            }
            args.locations.push(if wide {
                Aarch64Argument::StackPair(offset)
            } else {
                Aarch64Argument::Stack(offset)
            });
        }
    }
    Some(args)
}

fn x86_argument_cells(params: &[AbiParam]) -> Option<(usize, usize)> {
    let mut cells = 0;
    let mut floats = 0;
    for param in params {
        if integer_width(param.value_type).is_some() && *param == AbiParam::new(param.value_type) {
            cells += 1;
        } else if *param == AbiParam::new(types::I128) {
            cells += WIDE_INTEGER_CELLS;
            if cells > GP_REGISTER_ARGUMENTS {
                return None;
            }
        } else if float_width(param.value_type).is_some()
            && *param == AbiParam::new(param.value_type)
        {
            floats += 1;
        } else {
            return None;
        }
    }
    (cells <= MAX_GP_ARGUMENTS && floats <= XMM_REGISTER_ARGUMENTS).then_some((cells, floats))
}

fn integer_return_cells(returns: &[AbiParam]) -> Option<usize> {
    if returns == [AbiParam::new(types::I128)] {
        return Some(WIDE_INTEGER_CELLS);
    }
    (returns.len() <= GP_RESULTS
        && returns.iter().all(|param| {
            integer_width(param.value_type).is_some() && *param == AbiParam::new(param.value_type)
        }))
    .then_some(returns.len())
}

fn returns_float(returns: &[AbiParam]) -> bool {
    matches!(returns, [param] if float_width(param.value_type).is_some() && *param == AbiParam::new(param.value_type))
}

fn compare_op(ty: Type, immediate: bool) -> Option<IntegerOp> {
    match (ty, immediate) {
        (types::I8, false) => Some(IntegerOp::CompareByte),
        (types::I16, false) => Some(IntegerOp::CompareWord),
        (types::I32, false) => Some(IntegerOp::CompareDword),
        (types::I64, false) => Some(IntegerOp::Compare),
        (types::I8, true) => Some(IntegerOp::CompareByteImmediate),
        (types::I16, true) => Some(IntegerOp::CompareWordImmediate),
        (types::I32, true) => Some(IntegerOp::CompareDwordImmediate),
        (types::I64, true) => Some(IntegerOp::CompareImmediate),
        _ => None,
    }
}

fn immediate_op(op: IntegerOp) -> IntegerOp {
    match op {
        IntegerOp::Add => IntegerOp::AddImmediate,
        IntegerOp::Sub => IntegerOp::SubImmediate,
        IntegerOp::Mul => IntegerOp::MulImmediate,
        IntegerOp::And => IntegerOp::AndImmediate,
        IntegerOp::Or => IntegerOp::OrImmediate,
        IntegerOp::Xor => IntegerOp::XorImmediate,
        _ => unreachable!(),
    }
}

fn sign_extended_immediate(bits: i64, ty: Type) -> Option<u64> {
    let signed = match ty {
        types::I8 => i32::from(bits as i8),
        types::I16 => i32::from(bits as i16),
        types::I32 => bits as i32,
        types::I64 => i32::try_from(bits).ok()?,
        _ => return None,
    };
    Some(u64::from(signed as u32))
}

// Keep the transport independent of Cranelift's discriminants and x86 encodings.
fn condition(cc: IntCC) -> u64 {
    match cc {
        IntCC::Equal => 0,
        IntCC::NotEqual => 1,
        IntCC::SignedLessThan => 2,
        IntCC::SignedGreaterThanOrEqual => 3,
        IntCC::SignedGreaterThan => 4,
        IntCC::SignedLessThanOrEqual => 5,
        IntCC::UnsignedLessThan => 6,
        IntCC::UnsignedGreaterThanOrEqual => 7,
        IntCC::UnsignedGreaterThan => 8,
        IntCC::UnsignedLessThanOrEqual => 9,
    }
}

fn integer_bits(bits: i64, ty: Type) -> u64 {
    match ty {
        types::I8 => u64::from(bits as u8),
        types::I16 => u64::from(bits as u16),
        types::I32 => u64::from(bits as u32),
        _ => bits as u64,
    }
}

#[derive(Clone, Default)]
struct Values {
    low: SecondaryMap<Value, Option<u64>>,
    high: SecondaryMap<Value, Option<u64>>,
}

fn entry_values(blocks: &mut IntegerBlocks<'_>, block: Block) -> Option<Values> {
    let function = blocks.function;
    let params = function.dfg.block_params(block);
    if params.len() != function.signature.params.len() {
        return None;
    }
    let mut values = Values::default();
    let mut next = 0;
    let mut xmm = 0;
    let windows = function.signature.call_conv == CallConv::WindowsFastcall;
    for (index, (&value, param)) in params.iter().zip(&function.signature.params).enumerate() {
        let ty = function.dfg.value_type(value);
        if ty != param.value_type {
            return None;
        }
        if let Some(layout) = &blocks.aarch64_entry {
            let (op, offset, width) = match layout.locations[index] {
                Aarch64Argument::Gp(_) => continue,
                Aarch64Argument::Float(register) => {
                    (IntegerOp::FloatArgument, register, float_width(ty)?)
                }
                Aarch64Argument::Stack(offset) => {
                    if let Some(width) = float_width(ty) {
                        (IntegerOp::FloatStackArgument, offset, width)
                    } else {
                        (IntegerOp::StackArgument, offset, integer_width(ty)?)
                    }
                }
                Aarch64Argument::StackPair(offset) => {
                    values.low[value] = Some(blocks.append(
                        IntegerOp::StackArgument,
                        [offset as u64, 0, INTEGER_CELL_BITS],
                    ));
                    values.high[value] = Some(blocks.append(
                        IntegerOp::StackArgument,
                        [(offset + size_of::<u64>()) as u64, 0, INTEGER_CELL_BITS],
                    ));
                    continue;
                }
            };
            values.low[value] = Some(blocks.append(op, [offset as u64, 0, width]));
        } else if let Some(width) = float_width(ty) {
            if windows {
                if index < WINDOWS_REGISTER_ARGUMENTS {
                    values.low[value] =
                        Some(blocks.append(IntegerOp::FloatArgument, [index as u64, 0, width]));
                }
            } else {
                values.low[value] = Some(blocks.append(IntegerOp::FloatArgument, [xmm, 0, width]));
                xmm += 1;
            }
        }
    }
    for (index, &value) in params.iter().enumerate() {
        let ty = function.dfg.value_type(value);
        if let Some(layout) = &blocks.aarch64_entry {
            let Aarch64Argument::Gp(register) = layout.locations[index] else {
                continue;
            };
            next = register;
        } else if windows {
            values.low[value] = if let Some(width) = float_width(ty) {
                if index < WINDOWS_REGISTER_ARGUMENTS {
                    values.low[value]
                } else {
                    Some(blocks.append(IntegerOp::FloatStackArgument, [index as u64, 0, width]))
                }
            } else {
                Some(blocks.normalize_abi_integer(index as u64, ty))
            };
            continue;
        } else if float_width(ty).is_some() {
            continue;
        }
        values.low[value] = Some(blocks.normalize_abi_integer(next as u64, ty));
        next += 1;
        if ty == types::I128 {
            values.high[value] = Some(next as u64);
            next += 1;
        }
    }
    Some(values)
}

enum PathExit {
    Return(u64, Option<u64>),
    Trap,
}

fn supports_memory_format(flags: MemFlags) -> bool {
    !flags.checked() && flags.endianness(Endianness::Little) == Endianness::Little
}

fn store_op(opcode: Opcode, ty: Type) -> Option<IntegerOp> {
    Some(match (opcode, ty) {
        (Opcode::Store, types::I8) | (Opcode::Istore8, types::I16 | types::I32 | types::I64) => {
            IntegerOp::StoreByte
        }
        (Opcode::Store, types::I16) | (Opcode::Istore16, types::I32 | types::I64) => {
            IntegerOp::StoreWord
        }
        (Opcode::Store, types::I32) | (Opcode::Istore32, types::I64) => IntegerOp::StoreDword,
        (Opcode::Store, types::I64) => IntegerOp::StoreQword,
        _ => return None,
    })
}

fn stack_store_op(ty: Type) -> Option<IntegerOp> {
    match ty {
        types::I8 => Some(IntegerOp::StackStoreByte),
        types::I16 => Some(IntegerOp::StackStoreWord),
        types::I32 => Some(IntegerOp::StackStoreDword),
        types::I64 => Some(IntegerOp::StackStoreQword),
        types::F32 => Some(IntegerOp::FloatStackStore32),
        types::F64 => Some(IntegerOp::FloatStackStore64),
        _ => None,
    }
}

fn float_store_op(ty: Type) -> Option<IntegerOp> {
    match ty {
        types::F32 => Some(IntegerOp::FloatStore32),
        types::F64 => Some(IntegerOp::FloatStore64),
        _ => None,
    }
}

struct IntegerBlocks<'a> {
    function: &'a Function,
    arity: usize,
    return_cells: usize,
    float_return: bool,
    words: Vec<u64>,
    relocations: bool,
    branches: usize,
    emitted_blocks: SecondaryMap<Block, bool>,
    duplicated_instructions: usize,
    needs_cfg: bool,
    allow_hardware_faults: bool,
    canonicalize_nans: bool,
    aarch64: bool,
    x86_isa: Option<&'a dyn TargetIsa>,
    aarch64_entry: Option<Aarch64Arguments>,
    declarations: Option<&'a ModuleDeclarations>,
}

impl IntegerBlocks<'_> {
    fn has_x86_shuffle(&self) -> bool {
        self.x86_isa.is_some_and(TargetIsa::has_x86_pshufb_lowering)
    }

    fn has_x86_sse41(&self) -> bool {
        const SSE41_FLAG: &str = "has_sse41";
        self.x86_isa.is_some_and(|isa| {
            isa.isa_flags()
                .iter()
                .any(|flag| flag.name == SSE41_FLAG && flag.as_bool() == Some(true))
        })
    }

    fn has_x86_fma(&self) -> bool {
        const AVX_FLAG: &str = "has_avx";
        self.x86_isa.is_some_and(|isa| {
            isa.has_native_fma()
                && isa
                    .isa_flags()
                    .iter()
                    .any(|flag| flag.name == AVX_FLAG && flag.as_bool() == Some(true))
        })
    }

    fn bit_operation_mode(&self, opcode: Opcode) -> u64 {
        const POPCNT_FLAG: &str = "has_popcnt";
        if opcode == Opcode::Popcnt
            && self.x86_isa.is_some_and(|isa| {
                isa.isa_flags()
                    .iter()
                    .any(|flag| flag.name == POPCNT_FLAG && flag.as_bool() == Some(true))
            })
        {
            X86_HARDWARE_POPCOUNT
        } else {
            0
        }
    }

    fn normalize_abi_integer(&mut self, cell: u64, ty: Type) -> u64 {
        // Plain narrow ABI slots only define the low bits. The machine
        // tape keeps narrow values zero-extended between operations.
        match integer_width(ty) {
            Some(width) if width < INTEGER_CELL_BITS => {
                self.append(IntegerOp::Truncate, [cell, INTEGER_CELL_BITS, width])
            }
            _ => cell,
        }
    }

    fn supports_memory_access(&self, flags: MemFlags) -> bool {
        flags.notrap()
            || (self.allow_hardware_faults
                && flags.trap_code() == Some(TrapCode::HEAP_OUT_OF_BOUNDS))
    }

    fn supports_atomic_access(&self, flags: MemFlags) -> bool {
        supports_memory_format(flags) && self.supports_memory_access(flags)
    }

    fn append(&mut self, op: IntegerOp, operands: [u64; 3]) -> u64 {
        let value = self.arity + (self.words.len() - HEADER_WORDS) / INSTRUCTION_WORDS;
        self.words.push(op as u64);
        self.words.extend(operands);
        value as u64
    }

    fn value_type(&self, value: Value) -> Type {
        self.function
            .dfg
            .value_type(self.function.dfg.resolve_aliases(value))
    }

    fn constant_bits(&self, value: Value) -> Option<i64> {
        let dfg = &self.function.dfg;
        let ValueDef::Result(inst, _) = dfg.value_def(dfg.resolve_aliases(value)) else {
            return None;
        };
        match dfg.insts[inst] {
            InstructionData::UnaryImm {
                opcode: Opcode::Iconst,
                imm,
            } => Some(imm.bits()),
            _ => None,
        }
    }

    fn complemented_bitwise_operands(
        &self,
        opcode: Opcode,
        left: Value,
        right: Value,
    ) -> Option<(IntegerOp, Value, Value)> {
        let op = match opcode {
            Opcode::BandNot | Opcode::Band => IntegerOp::AndNot,
            Opcode::BorNot | Opcode::Bor => IntegerOp::OrNot,
            Opcode::BxorNot | Opcode::Bxor => IntegerOp::XorNot,
            _ => return None,
        };
        if matches!(opcode, Opcode::BandNot | Opcode::BorNot | Opcode::BxorNot) {
            return Some((op, left, right));
        }
        let dfg = &self.function.dfg;
        let complement = |value| {
            let ValueDef::Result(inst, _) = dfg.value_def(dfg.resolve_aliases(value)) else {
                return None;
            };
            match dfg.insts[inst] {
                InstructionData::Unary {
                    opcode: Opcode::Bnot,
                    arg,
                } => Some(arg),
                _ => None,
            }
        };
        let (left, source) = if let Some(source) = complement(right) {
            (left, source)
        } else {
            (right, complement(left)?)
        };
        // Keep the ordinary immediate path when its operand is already a constant.
        if self.constant_bits(left).is_some() || self.constant_bits(source).is_some() {
            return None;
        }
        Some((op, left, source))
    }

    fn stack_location(&self, slot: StackSlot, offset: Offset32, access: u32) -> Option<[u64; 2]> {
        let size = self.function.sized_stack_slots.get(slot)?.size;
        let offset = u32::try_from(i32::from(offset)).ok()?;
        (offset.checked_add(access)? <= size)
            .then_some([u64::from(slot.as_u32()), u64::from(offset)])
    }

    fn stack_operand(&self, base: Value, offset: Offset32, access: u32) -> Option<[u64; 2]> {
        let dfg = &self.function.dfg;
        let ValueDef::Result(instruction, _) = dfg.value_def(dfg.resolve_aliases(base)) else {
            return None;
        };
        let InstructionData::StackLoad {
            opcode: Opcode::StackAddr,
            stack_slot,
            offset: base_offset,
        } = dfg.insts[instruction]
        else {
            return None;
        };
        let offset = i32::from(base_offset).checked_add(i32::from(offset))?;
        self.stack_location(stack_slot, Offset32::new(offset), access)
    }

    fn edge(&self, edge: BlockCall, values: &mut Values) -> Option<Block> {
        let dfg = &self.function.dfg;
        let target = edge.block(&dfg.value_lists);
        let params = dfg.block_params(target);
        if params.len() != edge.len(&dfg.value_lists) {
            return None;
        }
        let arguments: Option<Vec<_>> = params
            .iter()
            .zip(edge.args(&dfg.value_lists))
            .map(|(&param, arg)| {
                let arg = arg.as_value()?;
                let ty = self.value_type(arg);
                if (ty != types::I128 && integer_width(ty).is_none() && float_width(ty).is_none())
                    || self.value_type(param) != ty
                {
                    return None;
                }
                let arg = dfg.resolve_aliases(arg);
                let high = if ty == types::I128 {
                    Some(values.high[arg]?)
                } else {
                    None
                };
                Some((values.low[arg]?, high))
            })
            .collect();
        for (&param, (low, high)) in params.iter().zip(arguments?) {
            values.low[param] = Some(low);
            if let Some(high) = high {
                values.high[param] = Some(high);
            }
        }
        Some(target)
    }

    fn direct_call(
        &mut self,
        inst: Inst,
        callee: &cranelift_codegen::ir::ExtFuncData,
        values: &mut Values,
    ) -> Option<()> {
        let dfg = &self.function.dfg;
        let signature = &dfg.signatures[callee.signature];
        let target = match callee.name {
            ExternalName::User(name) => {
                let name = self.function.params.user_named_funcs().get(name)?;
                if name.namespace != FUNCTION_NAMESPACE {
                    return None;
                }
                u64::from(name.index)
            }
            ExternalName::LibCall(call @ (LibCall::Memmove | LibCall::Memcpy)) => {
                if *signature != call.signature(signature.call_conv, types::I64) {
                    return None;
                }
                let libcall = if call == LibCall::Memmove {
                    MEMMOVE_LIBCALL
                } else {
                    MEMCPY_LIBCALL
                };
                LIBCALL_TARGET_BASE + u64::from(libcall)
            }
            _ => return None,
        };
        // Non-colocated Windows calls can require COFF reference pointers.
        if signature.call_conv == CallConv::WindowsFastcall
            && (!callee.colocated
                || target > u64::from(u32::MAX)
                || self.declarations.is_some_and(|declarations| {
                    target as usize >= declarations.get_functions().size_hint().0
                }))
        {
            return None;
        }
        if dfg.insts[inst].opcode() == Opcode::ReturnCall {
            if signature.call_conv != CallConv::Tail
                || signature.returns != self.function.signature.returns
                || signature.params.len() != dfg.inst_args(inst).len()
                || target > u64::from(u32::MAX)
            {
                return None;
            }
            let arguments = dfg.inst_args(inst);
            let [count, floats] = if self.aarch64 {
                let layout = aarch64_arguments(&signature.params, signature.call_conv)?;
                if layout.locations.iter().any(|arg| {
                    matches!(
                        arg,
                        Aarch64Argument::Stack(_) | Aarch64Argument::StackPair(_)
                    )
                }) {
                    return None;
                }
                self.aarch64_call_arguments(arguments, &signature.params, values, &layout)?
            } else {
                self.x86_call_arguments(
                    arguments,
                    &signature.params,
                    values,
                    GP_REGISTER_ARGUMENTS,
                )?
            };
            self.relocations = true;
            self.append(IntegerOp::TailCall, [target, count, floats]);
            return Some(());
        }
        self.call(
            inst,
            signature,
            dfg.inst_args(inst),
            CallTarget::Direct(target),
            values,
        )
    }

    fn call(
        &mut self,
        inst: Inst,
        signature: &Signature,
        args: &[Value],
        target: CallTarget,
        values: &mut Values,
    ) -> Option<()> {
        let dfg = &self.function.dfg;
        if signature.call_conv != self.function.signature.call_conv
            || signature.call_conv == CallConv::Tail
            || signature.params.len() != args.len()
        {
            return None;
        }
        let float_return = returns_float(&signature.returns);
        let return_cells = if float_return {
            1
        } else {
            integer_return_cells(&signature.returns)?
        };
        let results = dfg.inst_results(inst);
        if signature.returns.len() != results.len()
            || signature
                .returns
                .iter()
                .zip(results)
                .any(|(param, &value)| param.value_type != dfg.value_type(value))
        {
            return None;
        }
        let op = match return_cells {
            0 => IntegerOp::CallVoid,
            1 if self.aarch64 && float_return && signature.returns[0].value_type == types::F32 => {
                IntegerOp::CallFloat32
            }
            1 if float_return => IntegerOp::CallFloat,
            1 => IntegerOp::Call,
            GP_RESULTS => IntegerOp::CallPair,
            _ => return None,
        };
        let [argument_records, float_and_stack] =
            if signature.call_conv == CallConv::WindowsFastcall {
                if !matches!(target, CallTarget::Direct(_)) || !windows_signature(signature) {
                    return None;
                }
                for (&arg, param) in args.iter().zip(&signature.params) {
                    let arg = dfg.resolve_aliases(arg);
                    if dfg.value_type(arg) != param.value_type {
                        return None;
                    }
                    self.append(
                        IntegerOp::CallArgument,
                        [
                            values.low[arg]?,
                            0,
                            float_width(param.value_type).unwrap_or(0),
                        ],
                    );
                }
                [args.len() as u64, 0]
            } else if self.aarch64 {
                let layout = aarch64_arguments(&signature.params, signature.call_conv)?;
                self.aarch64_call_arguments(args, &signature.params, values, &layout)?
            } else {
                self.x86_call_arguments(args, &signature.params, values, MAX_GP_ARGUMENTS)?
            };
        let (target, layout) = match target {
            CallTarget::Direct(target) => {
                self.relocations = true;
                (target, float_and_stack)
            }
            CallTarget::Indirect(address) => (address, float_and_stack | INDIRECT_CALL),
        };
        let id = self.append(op, [target, argument_records, layout]);
        let high = (return_cells == GP_RESULTS).then(|| self.append(IntegerOp::CallHigh, [0; 3]));
        if let Some(&value) = results.first() {
            values.low[value] = Some(self.normalize_abi_integer(id, dfg.value_type(value)));
        }
        if let Some(high) = high {
            match *results {
                [wide] => values.high[wide] = Some(high),
                [_, value] => {
                    values.low[value] =
                        Some(self.normalize_abi_integer(high, dfg.value_type(value)));
                }
                _ => return None,
            }
        }
        Some(())
    }

    fn x86_call_arguments(
        &mut self,
        args: &[Value],
        params: &[AbiParam],
        values: &Values,
        gp_limit: usize,
    ) -> Option<[u64; 2]> {
        let (gp_arguments, float_arguments) = x86_argument_cells(params)?;
        if gp_arguments > gp_limit {
            return None;
        }
        let dfg = &self.function.dfg;
        let mut xmm = [0; XMM_REGISTER_ARGUMENTS];
        let mut xmm_count = 0;
        for (&arg, param) in args.iter().zip(params) {
            let arg = dfg.resolve_aliases(arg);
            let ty = dfg.value_type(arg);
            if ty != param.value_type {
                return None;
            }
            let low = values.low[arg]?;
            if float_width(ty).is_some() {
                xmm[xmm_count] = low;
                xmm_count += 1;
            } else {
                self.append(IntegerOp::CallArgument, [low, 0, 0]);
                if ty == types::I128 {
                    self.append(IntegerOp::CallArgument, [values.high[arg]?, 0, 0]);
                }
            }
        }
        for &arg in &xmm[..float_arguments] {
            self.append(IntegerOp::CallArgument, [arg, 0, 0]);
        }
        Some([
            (gp_arguments + float_arguments) as u64,
            float_arguments as u64,
        ])
    }

    fn aarch64_call_arguments(
        &mut self,
        args: &[Value],
        params: &[AbiParam],
        values: &Values,
        layout: &Aarch64Arguments,
    ) -> Option<[u64; 2]> {
        let dfg = &self.function.dfg;
        let mut gp = [NO_VALUE; AARCH64_GP_ARGUMENTS];
        let mut fp = [NO_VALUE; AARCH64_FP_ARGUMENTS];
        let mut stack = 0;
        for ((&arg, param), &location) in args.iter().zip(params).zip(&layout.locations) {
            let arg = dfg.resolve_aliases(arg);
            let ty = dfg.value_type(arg);
            if ty != param.value_type {
                return None;
            }
            let low = values.low[arg]?;
            match location {
                Aarch64Argument::Gp(register) => {
                    gp[register] = low;
                    if ty == types::I128 {
                        gp[register + 1] = values.high[arg]?;
                    }
                }
                Aarch64Argument::Float(register) => fp[register] = low,
                Aarch64Argument::Stack(offset) => {
                    let width = integer_width(ty).or_else(|| float_width(ty))?;
                    self.append(IntegerOp::CallStackArgument, [low, offset as u64, width]);
                    stack += 1;
                }
                Aarch64Argument::StackPair(offset) => {
                    self.append(
                        IntegerOp::CallStackArgument,
                        [low, offset as u64, INTEGER_CELL_BITS],
                    );
                    self.append(
                        IntegerOp::CallStackArgument,
                        [
                            values.high[arg]?,
                            (offset + size_of::<u64>()) as u64,
                            INTEGER_CELL_BITS,
                        ],
                    );
                    stack += WIDE_INTEGER_CELLS;
                }
            }
        }
        for &value in gp[..layout.gp].iter().chain(&fp[..layout.fp]) {
            self.append(IntegerOp::CallArgument, [value, 0, 0]);
        }
        Some([
            (stack + layout.gp + layout.fp) as u64,
            ((stack as u64) << AARCH64_CALL_FLOAT_BITS) | layout.fp as u64,
        ])
    }

    fn symbol_address(&mut self, global: GlobalValue, width: u64) -> Option<u64> {
        let GlobalValueData::Symbol {
            name: ExternalName::User(name),
            offset,
            colocated,
            tls: false,
        } = self.function.global_values[global]
        else {
            return None;
        };
        let name = self.function.params.user_named_funcs().get(name)?;
        if name.namespace != DATA_NAMESPACE {
            return None;
        }
        let local = if let Some(declarations) = self.declarations {
            if name.index as usize >= declarations.get_data_objects().size_hint().0 {
                return None;
            }
            let data = declarations.get_data_decl(DataId::from_u32(name.index));
            matches!(data.linkage, Linkage::Local | Linkage::Hidden)
        } else {
            false
        };
        // Exported ELF data may be interposed when used in a shared object.
        let op = if colocated && local {
            IntegerOp::SymbolRelative
        } else {
            IntegerOp::SymbolGot
        };
        self.relocations = true;
        let address = self.append(op, [u64::from(name.index), 0, 0]);
        if offset.bits() == 0 {
            return Some(address);
        }
        let offset = self.append(IntegerOp::Constant, [offset.bits() as u64, 0, 0]);
        Some(self.append(IntegerOp::Add, [address, offset, width]))
    }

    fn supports_division(&self, ty: Type) -> bool {
        self.aarch64 || (self.allow_hardware_faults && matches!(ty, types::I32 | types::I64))
    }

    fn division_immediate(
        &mut self,
        op: IntegerOp,
        numerator: u64,
        divisor: u64,
        width: u64,
    ) -> (IntegerOp, [u64; 3]) {
        let all_bits = u64::MAX >> (INTEGER_CELL_BITS - width);
        match op {
            IntegerOp::UnsignedDiv if divisor.is_power_of_two() => (
                IntegerOp::ShiftRightLogical,
                [numerator, u64::from(divisor.trailing_zeros()), width],
            ),
            IntegerOp::UnsignedRem if divisor == 1 => (IntegerOp::Constant, [0; 3]),
            IntegerOp::UnsignedRem if divisor.is_power_of_two() => {
                let mask = divisor - 1;
                if i32::try_from(mask).is_ok() {
                    (IntegerOp::AndImmediate, [numerator, mask, width])
                } else {
                    let mask = self.append(IntegerOp::Constant, [mask, 0, 0]);
                    (IntegerOp::And, [numerator, mask, width])
                }
            }
            IntegerOp::SignedDiv if divisor == 1 => {
                (IntegerOp::ShiftRightLogical, [numerator, 0, width])
            }
            IntegerOp::SignedRem if divisor == 1 || divisor == all_bits => {
                (IntegerOp::Constant, [0; 3])
            }
            _ => {
                if matches!(op, IntegerOp::SignedDiv | IntegerOp::SignedRem) {
                    let negative = divisor & (1 << (width - 1)) != 0;
                    let magnitude = if negative {
                        divisor.wrapping_neg() & all_bits
                    } else {
                        divisor
                    };
                    if magnitude > 1 && magnitude.is_power_of_two() {
                        let count = u64::from(magnitude.trailing_zeros());
                        // Bias negative inputs before shifting to truncate toward zero.
                        let sign = self.append(
                            IntegerOp::ShiftRightArithmetic,
                            [numerator, width - 1, width],
                        );
                        let bias =
                            self.append(IntegerOp::ShiftRightLogical, [sign, width - count, width]);
                        let adjusted = self.append(IntegerOp::Add, [numerator, bias, width]);
                        if matches!(op, IntegerOp::SignedDiv) && !negative {
                            return (IntegerOp::ShiftRightArithmetic, [adjusted, count, width]);
                        }
                        let quotient =
                            self.append(IntegerOp::ShiftRightArithmetic, [adjusted, count, width]);
                        return if matches!(op, IntegerOp::SignedRem) {
                            let product =
                                self.append(IntegerOp::ShiftLeft, [quotient, count, width]);
                            (IntegerOp::Sub, [numerator, product, width])
                        } else {
                            (IntegerOp::Neg, [quotient, width, width])
                        };
                    }
                }
                let divisor = self.append(IntegerOp::Constant, [divisor, 0, 0]);
                (op, [numerator, divisor, width])
            }
        }
    }

    fn compare_wide(&mut self, cond: IntCC, left: [u64; 2], right: [u64; 2]) -> u64 {
        if matches!(cond, IntCC::Equal | IntCC::NotEqual) {
            let low = self.append(IntegerOp::Xor, [left[0], right[0], INTEGER_CELL_BITS]);
            let high = self.append(IntegerOp::Xor, [left[1], right[1], INTEGER_CELL_BITS]);
            let either = self.append(IntegerOp::Or, [low, high, INTEGER_CELL_BITS]);
            return self.append(IntegerOp::CompareImmediate, [either, 0, condition(cond)]);
        }
        let (high_cond, low_cond) = match cond {
            IntCC::SignedLessThan => (IntCC::SignedLessThan, IntCC::UnsignedLessThan),
            IntCC::SignedLessThanOrEqual => (IntCC::SignedLessThan, IntCC::UnsignedLessThanOrEqual),
            IntCC::SignedGreaterThan => (IntCC::SignedGreaterThan, IntCC::UnsignedGreaterThan),
            IntCC::SignedGreaterThanOrEqual => {
                (IntCC::SignedGreaterThan, IntCC::UnsignedGreaterThanOrEqual)
            }
            IntCC::UnsignedLessThan | IntCC::UnsignedLessThanOrEqual => {
                (IntCC::UnsignedLessThan, cond)
            }
            IntCC::UnsignedGreaterThan | IntCC::UnsignedGreaterThanOrEqual => {
                (IntCC::UnsignedGreaterThan, cond)
            }
            IntCC::Equal | IntCC::NotEqual => unreachable!(),
        };
        let high_equal = self.append(
            IntegerOp::Compare,
            [left[1], right[1], condition(IntCC::Equal)],
        );
        let high_order = self.append(
            IntegerOp::Compare,
            [left[1], right[1], condition(high_cond)],
        );
        let low_order = self.append(IntegerOp::Compare, [left[0], right[0], condition(low_cond)]);
        self.append(IntegerOp::Select, [high_equal, low_order, high_order])
    }

    fn wide_rotate_immediate(&mut self, [mut low, mut high]: [u64; 2], amount: u64) -> [u64; 2] {
        if amount & INTEGER_CELL_BITS != 0 {
            std::mem::swap(&mut low, &mut high);
        }
        let shift = amount & (INTEGER_CELL_BITS - 1);
        if shift == 0 {
            return [low, high];
        }
        [
            self.append(IntegerOp::FunnelRight, [low, high, shift]),
            self.append(IntegerOp::FunnelRight, [high, low, shift]),
        ]
    }

    fn wrapping_wide(&mut self, subtract: bool, left: [u64; 2], right: [u64; 2]) -> [u64; 2] {
        let (head, tail) = if subtract {
            (IntegerOp::UnsignedSubOverflow, IntegerOp::SubBorrow)
        } else {
            (IntegerOp::UnsignedAddOverflow, IntegerOp::AddCarry)
        };
        let low = self.append(head, [left[0], right[0], INTEGER_CELL_BITS]);
        let carry = self.append(IntegerOp::OverflowFlag, [0; 3]);
        let high = self.append(tail, [left[1], right[1], carry]);
        [low, high]
    }

    fn bitselect(&mut self, mask: u64, yes: u64, no: u64, width: u64) -> u64 {
        let difference = self.append(IntegerOp::Xor, [yes, no, width]);
        let selected = self.append(IntegerOp::And, [mask, difference, width]);
        self.append(IntegerOp::Xor, [no, selected, width])
    }

    fn truthy_value(&mut self, value: Value, values: &Values) -> Option<u64> {
        let value = self.function.dfg.resolve_aliases(value);
        let low = values.low[value]?;
        let ty = self.value_type(value);
        if ty == types::I128 {
            let high = values.high[value]?;
            Some(self.append(IntegerOp::Or, [low, high, INTEGER_CELL_BITS]))
        } else {
            integer_width(ty)?;
            Some(low)
        }
    }

    fn guarded_select(&mut self, inst: Inst, args: [Value; 3], values: &mut Values) -> Option<()> {
        let dfg = &self.function.dfg;
        let [result] = *dfg.inst_results(inst) else {
            return None;
        };
        let [test, yes, no] = args.map(|value| dfg.resolve_aliases(value));
        let ty = dfg.value_type(result);
        if dfg.value_type(yes) != ty || dfg.value_type(no) != ty {
            return None;
        }
        let test = self.truthy_value(test, values)?;
        let yes_low = values.low[yes]?;
        let no_low = values.low[no]?;
        let low = if let Some(width) = float_width(ty) {
            // x86's ordinary float select branches. Select the raw bits in GPRs.
            let yes = self.append(IntegerOp::FloatToBits, [yes_low, 0, width]);
            let no = self.append(IntegerOp::FloatToBits, [no_low, 0, width]);
            let selected = self.append(IntegerOp::Select, [test, yes, no]);
            self.append(IntegerOp::BitsToFloat, [selected, 0, width])
        } else {
            if ty == types::I128 {
                let yes = values.high[yes]?;
                let no = values.high[no]?;
                values.high[result] = Some(self.append(IntegerOp::Select, [test, yes, no]));
            } else {
                integer_width(ty)?;
            }
            self.append(IntegerOp::Select, [test, yes_low, no_low])
        };
        if self.aarch64 {
            // CSDB prevents speculative use of the CSEL result.
            self.append(IntegerOp::SpectreBarrier, [0; 3]);
        }
        values.low[result] = Some(low);
        Some(())
    }

    fn checked_float_to_integer(
        &mut self,
        input: u64,
        source: Type,
        output: Type,
        signed: bool,
    ) -> Option<u64> {
        if !self.allow_hardware_faults && !self.aarch64 {
            return None;
        }
        let source_width = float_width(source)?;
        let width = integer_width(output)?;
        let single = source == types::F32;
        let compare = if single {
            IntegerOp::FloatCompare32
        } else {
            IntegerOp::FloatCompare64
        };
        let bits = |value: f64| {
            if single {
                u64::from((value as f32).to_bits())
            } else {
                value.to_bits()
            }
        };
        let upper = (1_u128 << (width - u64::from(signed))) as f64;
        let minimum = if signed { -upper } else { 0.0 };
        let below_minimum = minimum - 1.0;
        // Fractional values immediately below the minimum truncate to the minimum.
        // When the source precision cannot distinguish the two bounds, use >=.
        let (lower, bound) = if bits(below_minimum) == bits(minimum) {
            (minimum, FloatCC::GreaterThanOrEqual)
        } else {
            (below_minimum, FloatCC::GreaterThan)
        };
        for (limit, cond) in [(lower, bound), (upper, FloatCC::LessThan)] {
            let limit = self.append(IntegerOp::FloatConstant, [bits(limit), 0, source_width]);
            let valid = self.append(compare, [input, limit, float_condition(cond)]);
            self.append(IntegerOp::TrapIfZero, [valid, 0, 0]);
        }
        if self.aarch64 {
            let op = if signed {
                IntegerOp::FloatToSignedSat
            } else {
                IntegerOp::FloatToUnsignedSat
            };
            return Some(self.append(op, [input, source_width, width]));
        }
        // SSE2 truncates to a signed destination. Reduce the upper half of a
        // u64 to the signed range before converting, then restore its top bit.
        let mut source_value = input;
        let high = if !signed && width == INTEGER_CELL_BITS {
            let high_bit = INTEGER_CELL_BITS - 1;
            let split = (1_u64 << high_bit) as f64;
            let split = self.append(IntegerOp::FloatConstant, [bits(split), 0, source_width]);
            let upper = self.append(
                compare,
                [input, split, float_condition(FloatCC::GreaterThanOrEqual)],
            );
            let adjusted = self.append(IntegerOp::FloatSub, [input, split, source_width]);
            source_value = self.append(IntegerOp::FloatSelect, [upper, adjusted, input]);
            Some(self.append(IntegerOp::ShiftLeft, [upper, high_bit, INTEGER_CELL_BITS]))
        } else {
            None
        };
        let dword = u64::from(types::I32.bits());
        let conversion_width = if width < dword || (signed && width == dword) {
            dword
        } else {
            INTEGER_CELL_BITS
        };
        let result = self.append(
            IntegerOp::FloatToIntegerTrunc,
            [source_value, source_width, conversion_width],
        );
        Some(if let Some(high) = high {
            self.append(IntegerOp::Or, [result, high, width])
        } else if width < conversion_width {
            self.append(IntegerOp::Truncate, [result, conversion_width, width])
        } else {
            result
        })
    }

    fn wide_rotate_register(&mut self, [low, high]: [u64; 2], amount: u64) -> [u64; 2] {
        let width = INTEGER_CELL_BITS;
        let other_shift = self.append(IntegerOp::Not, [amount, 0, width]);
        let rotated = [(low, high), (high, low)].map(|(limb, other)| {
            let right = self.append(IntegerOp::ShiftRightLogicalRegister, [limb, amount, width]);
            // Split the complementary left shift so it yields zero when the count is 0 mod 64.
            let left = self.append(IntegerOp::ShiftLeft, [other, 1, width]);
            let left = self.append(IntegerOp::ShiftLeftRegister, [left, other_shift, width]);
            self.append(IntegerOp::Or, [right, left, width])
        });
        let swap = self.append(IntegerOp::AndImmediate, [amount, width, width]);
        [
            self.append(IntegerOp::Select, [swap, rotated[1], rotated[0]]),
            self.append(IntegerOp::Select, [swap, rotated[0], rotated[1]]),
        ]
    }

    fn wide_instruction(&mut self, inst: Inst, values: &mut Values) -> Option<()> {
        let dfg = &self.function.dfg;
        let resolve = |value| dfg.resolve_aliases(value);
        let ty = |value| dfg.value_type(resolve(value));
        let low = |value| values.low[resolve(value)];
        let pair = |value| {
            (ty(value) == types::I128).then_some([low(value)?, values.high[resolve(value)]?])
        };
        let results = dfg.inst_results(inst);
        if let InstructionData::Unary {
            opcode: Opcode::Isplit,
            arg,
        } = dfg.insts[inst]
        {
            let [low, high] = pair(arg)?;
            let [a, b] = *results else {
                return None;
            };
            if ty(a) != types::I64 || ty(b) != types::I64 {
                return None;
            }
            values.low[a] = Some(low);
            values.low[b] = Some(high);
            return Some(());
        }
        let [result] = *results else {
            return None;
        };
        let output = ty(result);
        if let Some(width) = integer_width(output)
            && let InstructionData::Binary {
                opcode: opcode @ (Opcode::Rotl | Opcode::Rotr),
                args: [value, amount],
            } = dfg.insts[inst]
            && ty(value) == output
            && ty(amount) == types::I128
        {
            values.low[result] = Some(self.append(
                rotate_register_op(opcode),
                [low(value)?, low(amount)?, width],
            ));
            return Some(());
        }
        let [lo, hi] = match dfg.insts[inst] {
            InstructionData::Binary {
                opcode: Opcode::Iconcat,
                args: [a, b],
            } if output == types::I128 && ty(a) == types::I64 && ty(b) == types::I64 => {
                [low(a)?, low(b)?]
            }
            InstructionData::Binary {
                opcode: opcode @ (Opcode::Rotl | Opcode::Rotr),
                args: [value, amount],
            } if output == types::I128
                && (integer_width(ty(amount)).is_some() || ty(amount) == types::I128) =>
            {
                let value = pair(value)?;
                if let Some(count) = self.constant_bits(amount) {
                    self.wide_rotate_immediate(value, right_rotation_count(opcode, count))
                } else {
                    let amount = low(amount)?;
                    let amount = if opcode == Opcode::Rotl {
                        self.append(IntegerOp::Neg, [amount, 0, INTEGER_CELL_BITS])
                    } else {
                        amount
                    };
                    self.wide_rotate_register(value, amount)
                }
            }
            InstructionData::BinaryImm64 {
                opcode: opcode @ (Opcode::RotlImm | Opcode::RotrImm),
                arg,
                imm,
            } if output == types::I128 => {
                self.wide_rotate_immediate(pair(arg)?, right_rotation_count(opcode, imm.bits()))
            }
            InstructionData::BinaryImm64 {
                opcode: Opcode::IrsubImm,
                arg,
                imm,
            } if output == types::I128 => {
                let signed = imm.bits();
                let low = self.append(IntegerOp::Constant, [signed as u64, 0, 0]);
                let high = self.append(
                    IntegerOp::Constant,
                    [if signed < 0 { u64::MAX } else { 0 }, 0, 0],
                );
                self.wrapping_wide(true, [low, high], pair(arg)?)
            }
            InstructionData::Binary {
                opcode,
                args: [a, b],
            } if output == types::I128 => {
                let [a_low, a_high] = pair(a)?;
                let [b_low, b_high] = pair(b)?;
                match opcode {
                    Opcode::Iadd | Opcode::Isub => {
                        self.wrapping_wide(opcode == Opcode::Isub, [a_low, a_high], [b_low, b_high])
                    }
                    Opcode::Smin | Opcode::Smax | Opcode::Umin | Opcode::Umax => {
                        let choose_left = self.compare_wide(
                            minmax_condition(opcode)?,
                            [a_low, a_high],
                            [b_low, b_high],
                        );
                        [
                            self.append(IntegerOp::Select, [choose_left, a_low, b_low]),
                            self.append(IntegerOp::Select, [choose_left, a_high, b_high]),
                        ]
                    }
                    _ => {
                        let (op, left, right) = if let Some((op, left, right)) =
                            self.complemented_bitwise_operands(opcode, a, b)
                            && let (Some(left), Some(right)) = (pair(left), pair(right))
                        {
                            (op, left, right)
                        } else if matches!(opcode, Opcode::Band | Opcode::Bor | Opcode::Bxor) {
                            (binary_op(opcode)?, [a_low, a_high], [b_low, b_high])
                        } else {
                            return None;
                        };
                        [
                            self.append(op, [left[0], right[0], INTEGER_CELL_BITS]),
                            self.append(op, [left[1], right[1], INTEGER_CELL_BITS]),
                        ]
                    }
                }
            }
            InstructionData::Ternary {
                opcode: Opcode::Bitselect,
                args: [mask, yes, no],
            } if output == types::I128 => {
                let mask = pair(mask)?;
                let yes = pair(yes)?;
                let no = pair(no)?;
                [
                    self.bitselect(mask[0], yes[0], no[0], INTEGER_CELL_BITS),
                    self.bitselect(mask[1], yes[1], no[1], INTEGER_CELL_BITS),
                ]
            }
            InstructionData::Ternary {
                opcode: Opcode::Select,
                args: [test, yes, no],
            } if output == types::I128 && integer_width(ty(test)).is_some() => {
                let test = low(test)?;
                let yes = pair(yes)?;
                let no = pair(no)?;
                [
                    self.append(IntegerOp::Select, [test, yes[0], no[0]]),
                    self.append(IntegerOp::Select, [test, yes[1], no[1]]),
                ]
            }
            InstructionData::Unary {
                opcode: Opcode::Iabs,
                arg,
            } if output == types::I128 => {
                let [low, high] = pair(arg)?;
                let width = INTEGER_CELL_BITS;
                let sign = self.append(IntegerOp::ShiftRightArithmetic, [high, width - 1, width]);
                let low = self.append(IntegerOp::Xor, [low, sign, width]);
                let high = self.append(IntegerOp::Xor, [high, sign, width]);
                self.wrapping_wide(true, [low, high], [sign; WIDE_INTEGER_CELLS])
            }
            InstructionData::Unary {
                opcode: Opcode::Bnot,
                arg,
            } if output == types::I128 => {
                let [a, b] = pair(arg)?;
                [
                    self.append(IntegerOp::Not, [a, 0, INTEGER_CELL_BITS]),
                    self.append(IntegerOp::Not, [b, 0, INTEGER_CELL_BITS]),
                ]
            }
            InstructionData::Unary {
                opcode: opcode @ (Opcode::Bswap | Opcode::Bitrev),
                arg,
            } if output == types::I128 => {
                let [low, high] = pair(arg)?;
                let op = if opcode == Opcode::Bswap {
                    IntegerOp::ByteSwap
                } else {
                    IntegerOp::ReverseBits
                };
                [
                    self.append(op, [high, 0, INTEGER_CELL_BITS]),
                    self.append(op, [low, 0, INTEGER_CELL_BITS]),
                ]
            }
            InstructionData::Unary {
                opcode: Opcode::Cls,
                arg,
            } if output == types::I128 => {
                let [low, high] = pair(arg)?;
                let width = INTEGER_CELL_BITS;
                let sign_bits = width - 1;
                let head = self.append(IntegerOp::LeadingSignBits, [high, 0, width]);
                let sign = self.append(IntegerOp::ShiftRightArithmetic, [high, sign_bits, width]);
                let unsigned_tail = self.append(IntegerOp::Xor, [low, sign, width]);
                let tail = self.append(IntegerOp::LeadingZeros, [unsigned_tail, 0, width]);
                let extended = self.append(IntegerOp::AddImmediate, [tail, sign_bits, width]);
                let all_sign = self.append(
                    IntegerOp::CompareImmediate,
                    [head, sign_bits, condition(IntCC::Equal)],
                );
                let count = self.append(IntegerOp::Select, [all_sign, extended, head]);
                [count, self.append(IntegerOp::Constant, [0; 3])]
            }
            InstructionData::Unary {
                opcode: opcode @ (Opcode::Clz | Opcode::Ctz | Opcode::Popcnt),
                arg,
            } if output == types::I128 => {
                let [low, high] = pair(arg)?;
                let (first, second) = if opcode == Opcode::Clz {
                    (high, low)
                } else {
                    (low, high)
                };
                let op = integer_bit_op(opcode)?;
                let mode = self.bit_operation_mode(opcode);
                let head = self.append(op, [first, mode, INTEGER_CELL_BITS]);
                let tail = self.append(op, [second, mode, INTEGER_CELL_BITS]);
                let count = if opcode == Opcode::Popcnt {
                    self.append(IntegerOp::Add, [head, tail, INTEGER_CELL_BITS])
                } else {
                    let extended = self.append(
                        IntegerOp::AddImmediate,
                        [tail, INTEGER_CELL_BITS, INTEGER_CELL_BITS],
                    );
                    self.append(IntegerOp::Select, [first, head, extended])
                };
                [count, self.append(IntegerOp::Constant, [0; 3])]
            }
            InstructionData::Unary {
                opcode: opcode @ (Opcode::Uextend | Opcode::Sextend),
                arg,
            } if output == types::I128 => {
                let width = integer_width(ty(arg))?;
                let arg = low(arg)?;
                if opcode == Opcode::Uextend {
                    [arg, self.append(IntegerOp::Constant, [0; 3])]
                } else {
                    let low = if width == INTEGER_CELL_BITS {
                        arg
                    } else {
                        self.append(IntegerOp::SignExtend, [arg, width, INTEGER_CELL_BITS])
                    };
                    let high = self.append(
                        IntegerOp::ShiftRightArithmetic,
                        [low, INTEGER_CELL_BITS - 1, INTEGER_CELL_BITS],
                    );
                    [low, high]
                }
            }
            InstructionData::Unary {
                opcode: Opcode::Ireduce,
                arg,
            } if ty(arg) == types::I128 => {
                let width = integer_width(output)?;
                let low = low(arg)?;
                values.low[result] = Some(if width == INTEGER_CELL_BITS {
                    low
                } else {
                    self.append(IntegerOp::Truncate, [low, INTEGER_CELL_BITS, width])
                });
                return Some(());
            }
            InstructionData::IntCompare {
                opcode: Opcode::Icmp,
                cond,
                args: [a, b],
            } if output == types::I8 => {
                values.low[result] = Some(self.compare_wide(cond, pair(a)?, pair(b)?));
                return Some(());
            }
            _ => return None,
        };
        values.low[result] = Some(lo);
        values.high[result] = Some(hi);
        Some(())
    }

    fn float_instruction(&mut self, inst: Inst, value: Value, values: &Values) -> Option<u64> {
        let dfg = &self.function.dfg;
        let input = |value| values.low[dfg.resolve_aliases(value)];
        let ty = |value| self.value_type(value);
        let output = ty(value);
        let (op, operands) = match dfg.insts[inst] {
            InstructionData::Unary {
                opcode: opcode @ (Opcode::FcvtToSint | Opcode::FcvtToUint),
                arg,
            } => {
                return self.checked_float_to_integer(
                    input(arg)?,
                    ty(arg),
                    output,
                    opcode == Opcode::FcvtToSint,
                );
            }
            InstructionData::UnaryIeee32 {
                opcode: Opcode::F32const,
                imm,
            } if output == types::F32 => (
                IntegerOp::FloatConstant,
                [u64::from(imm.bits()), 0, float_width(output)?],
            ),
            InstructionData::UnaryIeee64 {
                opcode: Opcode::F64const,
                imm,
            } if output == types::F64 => (
                IntegerOp::FloatConstant,
                [imm.bits(), 0, float_width(output)?],
            ),
            InstructionData::Binary {
                opcode,
                args: [left, right],
            } if ty(left) == output && ty(right) == output => {
                let op = match opcode {
                    Opcode::Fadd => IntegerOp::FloatAdd,
                    Opcode::Fsub => IntegerOp::FloatSub,
                    Opcode::Fmul => IntegerOp::FloatMul,
                    Opcode::Fdiv => IntegerOp::FloatDiv,
                    Opcode::Fcopysign => IntegerOp::FloatCopySign,
                    Opcode::Fmin => IntegerOp::FloatMin,
                    Opcode::Fmax => IntegerOp::FloatMax,
                    _ => return None,
                };
                (op, [input(left)?, input(right)?, float_width(output)?])
            }
            InstructionData::Unary { opcode, arg } if ty(arg) == output => {
                let has_round = self.aarch64 || self.x86_isa.is_some_and(TargetIsa::has_round);
                let op = match opcode {
                    Opcode::Sqrt => IntegerOp::FloatSqrt,
                    Opcode::Fneg => IntegerOp::FloatNeg,
                    Opcode::Fabs => IntegerOp::FloatAbs,
                    Opcode::Ceil if has_round => IntegerOp::FloatCeil,
                    Opcode::Floor if has_round => IntegerOp::FloatFloor,
                    Opcode::Trunc if has_round => IntegerOp::FloatTrunc,
                    Opcode::Nearest if has_round => IntegerOp::FloatNearest,
                    _ => return None,
                };
                (op, [input(arg)?, 0, float_width(output)?])
            }
            InstructionData::Unary { opcode, arg } => {
                let source = ty(arg);
                let (op, source_width, result_width) = match (opcode, source, output) {
                    (Opcode::Fpromote, types::F32, types::F64) => (
                        IntegerOp::FloatPromote,
                        float_width(source)?,
                        float_width(output)?,
                    ),
                    (Opcode::Fdemote, types::F64, types::F32) => (
                        IntegerOp::FloatDemote,
                        float_width(source)?,
                        float_width(output)?,
                    ),
                    (Opcode::FcvtFromSint, _, _) => (
                        IntegerOp::SignedToFloat,
                        integer_width(source)?,
                        float_width(output)?,
                    ),
                    (Opcode::FcvtFromUint, _, _) => (
                        IntegerOp::UnsignedToFloat,
                        integer_width(source)?,
                        float_width(output)?,
                    ),
                    (Opcode::FcvtToSintSat, _, _) => (
                        IntegerOp::FloatToSignedSat,
                        float_width(source)?,
                        integer_width(output)?,
                    ),
                    (Opcode::FcvtToUintSat, _, _) => (
                        IntegerOp::FloatToUnsignedSat,
                        float_width(source)?,
                        integer_width(output)?,
                    ),
                    _ => return None,
                };
                (op, [input(arg)?, source_width, result_width])
            }
            InstructionData::FloatCompare {
                opcode: Opcode::Fcmp,
                cond,
                args: [left, right],
            } if output == types::I8 && ty(left) == ty(right) => {
                let op = match ty(left) {
                    types::F32 => IntegerOp::FloatCompare32,
                    types::F64 => IntegerOp::FloatCompare64,
                    _ => return None,
                };
                (op, [input(left)?, input(right)?, float_condition(cond)])
            }
            InstructionData::Ternary {
                opcode: Opcode::Fma,
                args: [left, right, addend],
            } if (self.aarch64 || self.has_x86_fma())
                && ty(left) == output
                && ty(right) == output
                && ty(addend) == output =>
            {
                let op = match output {
                    types::F32 => IntegerOp::FloatFma32,
                    types::F64 => IntegerOp::FloatFma64,
                    _ => return None,
                };
                (op, [input(left)?, input(right)?, input(addend)?])
            }
            InstructionData::Ternary {
                opcode: Opcode::Bitselect,
                args: [mask, yes, no],
            } if ty(mask) == output && ty(yes) == output && ty(no) == output => {
                let width = float_width(output)?;
                let mask = self.append(IntegerOp::FloatToBits, [input(mask)?, 0, width]);
                let yes = self.append(IntegerOp::FloatToBits, [input(yes)?, 0, width]);
                let no = self.append(IntegerOp::FloatToBits, [input(no)?, 0, width]);
                let selected = self.bitselect(mask, yes, no, width);
                (IntegerOp::BitsToFloat, [selected, 0, width])
            }
            InstructionData::Ternary {
                opcode: Opcode::Select,
                args: [test, yes, no],
            } if float_width(output).is_some()
                && ty(yes) == output
                && ty(no) == output
                && integer_width(ty(test)).is_some() =>
            {
                (
                    IntegerOp::FloatSelect,
                    [input(test)?, input(yes)?, input(no)?],
                )
            }
            InstructionData::LoadNoOffset {
                opcode: Opcode::Bitcast,
                arg,
                ..
            } => match (ty(arg), output) {
                (types::F32, types::I32) | (types::F64, types::I64) => (
                    IntegerOp::FloatToBits,
                    [input(arg)?, 0, float_width(ty(arg))?],
                ),
                (types::I32, types::F32) | (types::I64, types::F64) => (
                    IntegerOp::BitsToFloat,
                    [input(arg)?, 0, float_width(output)?],
                ),
                _ => return None,
            },
            InstructionData::StackLoad {
                opcode: Opcode::StackLoad,
                stack_slot,
                offset,
            } => {
                let [slot, offset] = self.stack_location(stack_slot, offset, output.bytes())?;
                (
                    IntegerOp::FloatStackLoad,
                    [slot, offset, float_width(output)?],
                )
            }
            InstructionData::Load {
                opcode: Opcode::Load,
                flags,
                arg,
                offset,
            } if ty(arg) == types::I64 && supports_memory_format(flags) => {
                let width = float_width(output)?;
                if let Some([slot, offset]) = self.stack_operand(arg, offset, output.bytes()) {
                    (IntegerOp::FloatStackLoad, [slot, offset, width])
                } else {
                    if !self.supports_memory_access(flags) {
                        return None;
                    }
                    (
                        IntegerOp::FloatLoad,
                        [input(arg)?, u64::from(i32::from(offset) as u32), width],
                    )
                }
            }
            _ => return None,
        };
        let result = self.append(op, operands);
        if self.canonicalize_nans
            && matches!(
                op,
                IntegerOp::FloatAdd
                    | IntegerOp::FloatSub
                    | IntegerOp::FloatMul
                    | IntegerOp::FloatDiv
                    | IntegerOp::FloatSqrt
                    | IntegerOp::FloatPromote
                    | IntegerOp::FloatDemote
                    | IntegerOp::FloatCeil
                    | IntegerOp::FloatFloor
                    | IntegerOp::FloatTrunc
                    | IntegerOp::FloatNearest
                    | IntegerOp::FloatMin
                    | IntegerOp::FloatMax
                    | IntegerOp::FloatFma32
                    | IntegerOp::FloatFma64
            )
        {
            let (compare, bits) = match output {
                types::F32 => (IntegerOp::FloatCompare32, u64::from(Ieee32::NAN.bits())),
                types::F64 => (IntegerOp::FloatCompare64, Ieee64::NAN.bits()),
                _ => return None,
            };
            let nan = self.append(
                compare,
                [result, result, float_condition(FloatCC::Unordered)],
            );
            let canonical = self.append(IntegerOp::FloatConstant, [bits, 0, float_width(output)?]);
            return Some(self.append(IntegerOp::FloatSelect, [nan, canonical, result]));
        }
        Some(result)
    }

    fn vector_constant(&mut self, bytes: &[u8]) -> Option<u64> {
        let ([low, high], []) = bytes.as_chunks::<{ size_of::<u64>() }>() else {
            return None;
        };
        Some(self.append(
            IntegerOp::VectorConstant,
            [u64::from_le_bytes(*low), u64::from_le_bytes(*high), 0],
        ))
    }

    fn vector_bitselect(&mut self, mask: u64, yes: u64, no: u64) -> u64 {
        let difference = self.append(IntegerOp::VectorXor, [yes, no, 0]);
        let selected = self.append(IntegerOp::VectorAnd, [mask, difference, 0]);
        self.append(IntegerOp::VectorXor, [no, selected, 0])
    }

    fn vector_integer_min_max(
        &mut self,
        opcode: Opcode,
        lane: Type,
        left: u64,
        right: u64,
    ) -> Option<u64> {
        let width = integer_width(lane)?;
        let signed = matches!(opcode, Opcode::Smin | Opcode::Smax);
        let min = matches!(opcode, Opcode::Smin | Opcode::Umin);
        if self.aarch64
            || (lane != types::I64
                && (matches!((lane, signed), (types::I8, false) | (types::I16, true))
                    || self.has_x86_sse41()))
        {
            let mode = width
                | if signed { VECTOR_MINMAX_SIGNED } else { 0 }
                | if min { VECTOR_MINMAX_MINIMUM } else { 0 };
            return Some(self.append(IntegerOp::VectorIntegerMinMax, [left, right, mode]));
        }
        if lane == types::I16 && !signed {
            // A saturating difference is zero when left <= right.
            let difference = self.append(IntegerOp::VectorUnsignedSatSub, [left, right, width]);
            return Some(if min {
                self.append(IntegerOp::VectorSub, [left, difference, width])
            } else {
                self.append(IntegerOp::VectorAdd, [right, difference, width])
            });
        }
        let greater = if signed {
            IntCC::SignedGreaterThan
        } else {
            IntCC::UnsignedGreaterThan
        };
        let mask = self.append(
            IntegerOp::VectorIntegerCompare,
            [
                left,
                right,
                width | (condition(greater) << SIMD_CONDITION_SHIFT),
            ],
        );
        let (yes, no) = if min { (right, left) } else { (left, right) };
        Some(self.vector_bitselect(mask, yes, no))
    }

    fn vector_integer_splat(&mut self, lane: Type, value: u64) -> Option<u64> {
        let bytes = value.to_le_bytes();
        let lane_bytes = lane.bytes() as usize;
        let vector: [u8; SIMD_BYTES as usize] = std::array::from_fn(|i| bytes[i % lane_bytes]);
        self.vector_constant(&vector)
    }

    fn vector_narrow(
        &mut self,
        opcode: Opcode,
        lane: Type,
        mut left: u64,
        mut right: u64,
    ) -> Option<u64> {
        let width = integer_width(lane)?;
        let narrow_width = width / 2;
        let signed = opcode == Opcode::Snarrow;
        let unsigned_input = opcode == Opcode::Uunarrow;
        if self.aarch64 {
            let mode = width
                | if signed { VECTOR_CONVERSION_SIGNED } else { 0 }
                | if unsigned_input {
                    VECTOR_NARROW_UNSIGNED_INPUT
                } else {
                    0
                };
            return Some(self.append(IntegerOp::VectorNarrow, [left, right, mode]));
        }
        if unsigned_input || lane == types::I64 {
            let signed_limit = (1 << (narrow_width - 1)) - 1;
            let max = self.vector_integer_splat(
                lane,
                if signed {
                    signed_limit
                } else {
                    (1 << narrow_width) - 1
                },
            )?;
            let min_op = if unsigned_input {
                Opcode::Umin
            } else {
                Opcode::Smin
            };
            left = self.vector_integer_min_max(min_op, lane, left, max)?;
            right = self.vector_integer_min_max(min_op, lane, right, max)?;
            if lane == types::I64 && !unsigned_input {
                let min =
                    self.vector_integer_splat(lane, if signed { !signed_limit } else { 0 })?;
                left = self.vector_integer_min_max(Opcode::Smax, lane, left, min)?;
                right = self.vector_integer_min_max(Opcode::Smax, lane, right, min)?;
            }
            if lane == types::I64 {
                return Some(self.append(IntegerOp::VectorNarrow, [left, right, width]));
            }
        }
        if !signed && lane == types::I32 && !self.has_x86_sse41() {
            // Bias nonnegative lanes into the range of SSE2's signed pack.
            // Clamping negatives first also prevents wraparound on subtraction.
            if !unsigned_input {
                let zero = self.vector_integer_splat(lane, 0)?;
                let positive =
                    width | (condition(IntCC::SignedGreaterThan) << SIMD_CONDITION_SHIFT);
                let left_mask =
                    self.append(IntegerOp::VectorIntegerCompare, [left, zero, positive]);
                left = self.append(IntegerOp::VectorAnd, [left, left_mask, 0]);
                let right_mask =
                    self.append(IntegerOp::VectorIntegerCompare, [right, zero, positive]);
                right = self.append(IntegerOp::VectorAnd, [right, right_mask, 0]);
            }
            let bias = 1 << (narrow_width - 1);
            let wide_bias = self.vector_integer_splat(lane, bias)?;
            let narrow_bias = self.vector_integer_splat(types::I16, bias)?;
            let left = self.append(IntegerOp::VectorSub, [left, wide_bias, width]);
            let right = self.append(IntegerOp::VectorSub, [right, wide_bias, width]);
            let packed = self.append(
                IntegerOp::VectorNarrow,
                [left, right, width | VECTOR_CONVERSION_SIGNED],
            );
            return Some(self.append(IntegerOp::VectorXor, [packed, narrow_bias, 0]));
        }
        let mode = width | if signed { VECTOR_CONVERSION_SIGNED } else { 0 };
        Some(self.append(IntegerOp::VectorNarrow, [left, right, mode]))
    }

    fn vector_shuffle(&mut self, mask: &[u8], left: u64, right: u64) -> Option<u64> {
        const LANES: u8 = SIMD_BYTES as u8;
        let mask: &[u8; SIMD_BYTES as usize] = mask.try_into().ok()?;
        if mask.iter().any(|&index| index >= LANES * 2) {
            return None;
        }
        if mask.iter().copied().eq(0..LANES) {
            return Some(left);
        }
        if mask.iter().copied().eq(LANES..LANES * 2) {
            return Some(right);
        }
        if left == right && mask.iter().map(|&index| index % LANES).eq(0..LANES) {
            return Some(left);
        }
        let x86_shuffle = self.has_x86_shuffle();
        if !self.aarch64 && !x86_shuffle {
            return None;
        }
        let mode = if x86_shuffle {
            X86_SWIZZLE_ZERO_HIGH
        } else {
            0
        };
        if left == right {
            let indices = self.vector_constant(&mask.map(|index| index % LANES))?;
            return Some(self.append(IntegerOp::VectorSwizzle, [left, indices, mode]));
        }
        let first = if mask.iter().any(|&index| index < LANES) {
            let indices = mask.map(|index| {
                if x86_shuffle && index >= LANES {
                    X86_SWIZZLE_ZERO_INDEX
                } else {
                    index
                }
            });
            let indices = self.vector_constant(&indices)?;
            Some(self.append(IntegerOp::VectorSwizzle, [left, indices, mode]))
        } else {
            None
        };
        if mask.iter().all(|&index| index < LANES) {
            return first;
        }
        // The table lookup returns zero for wrapped indices in the other half.
        let indices = mask.map(|index| {
            if x86_shuffle && index < LANES {
                X86_SWIZZLE_ZERO_INDEX
            } else {
                index.wrapping_sub(LANES)
            }
        });
        let indices = self.vector_constant(&indices)?;
        let second = self.append(IntegerOp::VectorSwizzle, [right, indices, mode]);
        Some(if let Some(first) = first {
            self.append(IntegerOp::VectorOr, [first, second, 0])
        } else {
            second
        })
    }

    fn vector_instruction(&mut self, inst: Inst, value: Value, values: &Values) -> Option<u64> {
        let dfg = &self.function.dfg;
        let input = |value| values.low[dfg.resolve_aliases(value)];
        let ty = |value| self.value_type(value);
        let output = ty(value);
        if !vector_type(output) {
            return None;
        }
        let (op, operands) = match dfg.insts[inst] {
            InstructionData::UnaryConst {
                opcode: Opcode::Vconst,
                constant_handle,
            } => {
                return self.vector_constant(dfg.constants.get(constant_handle).as_slice());
            }
            InstructionData::Unary {
                opcode: Opcode::Splat,
                arg,
            } if ty(arg) == output.lane_type() => {
                let lane = ty(arg);
                let mut source = input(arg)?;
                let width = if let Some(width) = float_width(lane) {
                    source = self.append(IntegerOp::FloatToBits, [source, 0, width]);
                    width
                } else {
                    integer_width(lane)?
                };
                (IntegerOp::VectorSplat, [source, 0, width])
            }
            InstructionData::Unary { opcode, arg }
                if matches!(
                    opcode,
                    Opcode::SwidenLow | Opcode::SwidenHigh | Opcode::UwidenLow | Opcode::UwidenHigh
                ) && vector_type(ty(arg))
                    && ty(arg).merge_lanes() == Some(output) =>
            {
                let width = integer_width(ty(arg).lane_type())?;
                let signed = matches!(opcode, Opcode::SwidenLow | Opcode::SwidenHigh);
                let high = matches!(opcode, Opcode::SwidenHigh | Opcode::UwidenHigh);
                let mode = width
                    | if signed { VECTOR_CONVERSION_SIGNED } else { 0 }
                    | if high { VECTOR_WIDEN_HIGH } else { 0 }
                    | if self.has_x86_sse41() && (!high || (signed && ty(arg) == types::I32X4)) {
                        X86_VECTOR_WIDEN_SSE41
                    } else {
                        0
                    };
                (IntegerOp::VectorWiden, [input(arg)?, 0, mode])
            }
            InstructionData::Binary {
                opcode: opcode @ (Opcode::Snarrow | Opcode::Unarrow | Opcode::Uunarrow),
                args: [left, right],
            } if vector_type(ty(left))
                && ty(left) == ty(right)
                && ty(left).split_lanes() == Some(output) =>
            {
                return self.vector_narrow(
                    opcode,
                    ty(left).lane_type(),
                    input(left)?,
                    input(right)?,
                );
            }
            InstructionData::Binary {
                opcode: opcode @ (Opcode::Ishl | Opcode::Ushr | Opcode::Sshr),
                args: [value, count],
            } if ty(value) == output && integer_width(ty(count)).is_some() => {
                let width = integer_width(output.lane_type())?;
                let (count, mode) = if let Some(bits) = self.constant_bits(count) {
                    ((bits as u64) & (width - 1), width | VECTOR_SHIFT_IMMEDIATE)
                } else {
                    (input(count)?, width)
                };
                (vector_shift_op(opcode)?, [input(value)?, count, mode])
            }
            InstructionData::BinaryImm64 {
                opcode: opcode @ (Opcode::IshlImm | Opcode::UshrImm | Opcode::SshrImm),
                arg,
                imm,
            } if ty(arg) == output => {
                let width = integer_width(output.lane_type())?;
                (
                    vector_shift_op(opcode)?,
                    [
                        input(arg)?,
                        imm.bits() as u64 & (width - 1),
                        width | VECTOR_SHIFT_IMMEDIATE,
                    ],
                )
            }
            InstructionData::Binary {
                opcode,
                args: [left, right],
            } if ty(left) == output && ty(right) == output => {
                let lane = output.lane_type();
                let (op, width) = match opcode {
                    Opcode::Band => (IntegerOp::VectorAnd, 0),
                    Opcode::Bor => (IntegerOp::VectorOr, 0),
                    Opcode::Bxor => (IntegerOp::VectorXor, 0),
                    Opcode::Iadd => (IntegerOp::VectorAdd, integer_width(lane)?),
                    Opcode::Isub => (IntegerOp::VectorSub, integer_width(lane)?),
                    Opcode::Smin | Opcode::Smax | Opcode::Umin | Opcode::Umax => {
                        return self.vector_integer_min_max(
                            opcode,
                            lane,
                            input(left)?,
                            input(right)?,
                        );
                    }
                    Opcode::Imul => {
                        let mode =
                            if matches!(lane, types::I32 | types::I64) && self.has_x86_sse41() {
                                X86_VECTOR_MUL_SSE41
                            } else {
                                0
                            };
                        (IntegerOp::VectorMul, integer_width(lane)? | mode)
                    }
                    Opcode::Fcopysign => (IntegerOp::VectorFloatCopySign, float_width(lane)?),
                    Opcode::Fmin | Opcode::Fmax if !self.canonicalize_nans => (
                        if opcode == Opcode::Fmin {
                            IntegerOp::VectorFloatMin
                        } else {
                            IntegerOp::VectorFloatMax
                        },
                        float_width(lane)?,
                    ),
                    Opcode::Fadd | Opcode::Fsub | Opcode::Fmul | Opcode::Fdiv
                        if !self.canonicalize_nans =>
                    {
                        let op = match opcode {
                            Opcode::Fadd => IntegerOp::VectorFloatAdd,
                            Opcode::Fsub => IntegerOp::VectorFloatSub,
                            Opcode::Fmul => IntegerOp::VectorFloatMul,
                            _ => IntegerOp::VectorFloatDiv,
                        };
                        (op, float_width(lane)?)
                    }
                    Opcode::IaddPairwise
                        if (self.aarch64 || self.has_x86_shuffle()) && lane != types::I64 =>
                    {
                        (IntegerOp::VectorPairwiseAdd, integer_width(lane)?)
                    }
                    Opcode::Swizzle
                        if (self.aarch64 || self.has_x86_shuffle()) && output == types::I8X16 =>
                    {
                        (IntegerOp::VectorSwizzle, 0)
                    }
                    _ if self.aarch64 || matches!(lane, types::I8 | types::I16) => {
                        let op = match opcode {
                            Opcode::SaddSat => IntegerOp::VectorSignedSatAdd,
                            Opcode::UaddSat => IntegerOp::VectorUnsignedSatAdd,
                            Opcode::SsubSat => IntegerOp::VectorSignedSatSub,
                            Opcode::UsubSat => IntegerOp::VectorUnsignedSatSub,
                            Opcode::AvgRound if lane != types::I64 => {
                                IntegerOp::VectorRoundedAverage
                            }
                            _ => return None,
                        };
                        (op, integer_width(lane)?)
                    }
                    _ => return None,
                };
                (op, [input(left)?, input(right)?, width])
            }
            InstructionData::TernaryImm8 {
                opcode: Opcode::Insertlane,
                args: [vector, scalar],
                imm,
            } if ty(vector) == output && ty(scalar) == output.lane_type() => {
                let lane = u64::from(imm);
                if lane >= u64::from(output.lane_count()) {
                    return None;
                }
                let scalar_ty = ty(scalar);
                let mut scalar = input(scalar)?;
                let width = if let Some(width) = float_width(scalar_ty) {
                    scalar = self.append(IntegerOp::FloatToBits, [scalar, 0, width]);
                    width
                } else {
                    integer_width(scalar_ty)?
                };
                (
                    IntegerOp::VectorInsert,
                    [
                        input(vector)?,
                        scalar,
                        (lane << SIMD_LANE_INDEX_SHIFT) | width,
                    ],
                )
            }
            InstructionData::IntCompare {
                opcode: Opcode::Icmp,
                cond,
                args: [left, right],
            } if ty(left) == output && ty(right) == output => (
                IntegerOp::VectorIntegerCompare,
                [
                    input(left)?,
                    input(right)?,
                    integer_width(output.lane_type())? | (condition(cond) << SIMD_CONDITION_SHIFT),
                ],
            ),
            InstructionData::FloatCompare {
                opcode: Opcode::Fcmp,
                cond,
                args: [left, right],
            } if matches!(ty(left), types::F32X4 | types::F64X2)
                && ty(right) == ty(left)
                && output == ty(left).as_int() =>
            {
                (
                    IntegerOp::VectorFloatCompare,
                    [
                        input(left)?,
                        input(right)?,
                        float_width(ty(left).lane_type())?
                            | (float_condition(cond) << SIMD_CONDITION_SHIFT),
                    ],
                )
            }
            InstructionData::Shuffle {
                opcode: Opcode::Shuffle,
                args: [left, right],
                imm,
            } if output == types::I8X16 && ty(left) == output && ty(right) == output => {
                return self.vector_shuffle(
                    dfg.immediates[imm].as_slice(),
                    input(left)?,
                    input(right)?,
                );
            }
            InstructionData::Unary {
                opcode: opcode @ (Opcode::Fneg | Opcode::Fabs),
                arg,
            } if ty(arg) == output => (
                if opcode == Opcode::Fneg {
                    IntegerOp::VectorFloatNeg
                } else {
                    IntegerOp::VectorFloatAbs
                },
                [input(arg)?, 0, float_width(output.lane_type())?],
            ),
            InstructionData::Unary {
                opcode: Opcode::Sqrt,
                arg,
            } if !self.canonicalize_nans && ty(arg) == output => (
                IntegerOp::VectorFloatSqrt,
                [input(arg)?, 0, float_width(output.lane_type())?],
            ),
            InstructionData::Unary {
                opcode: opcode @ (Opcode::Ceil | Opcode::Floor | Opcode::Trunc | Opcode::Nearest),
                arg,
            } if !self.canonicalize_nans
                && (self.aarch64 || self.x86_isa.is_some_and(TargetIsa::has_round))
                && ty(arg) == output =>
            {
                let op = match opcode {
                    Opcode::Ceil => IntegerOp::VectorFloatCeil,
                    Opcode::Floor => IntegerOp::VectorFloatFloor,
                    Opcode::Trunc => IntegerOp::VectorFloatTrunc,
                    _ => IntegerOp::VectorFloatNearest,
                };
                (op, [input(arg)?, 0, float_width(output.lane_type())?])
            }
            InstructionData::Unary {
                opcode: Opcode::FvpromoteLow,
                arg,
            } if !self.canonicalize_nans && ty(arg) == types::F32X4 && output == types::F64X2 => {
                (IntegerOp::VectorPromoteLow, [input(arg)?, 0, 0])
            }
            InstructionData::Unary {
                opcode: Opcode::Fvdemote,
                arg,
            } if !self.canonicalize_nans && ty(arg) == types::F64X2 && output == types::F32X4 => {
                (IntegerOp::VectorDemote, [input(arg)?, 0, 0])
            }
            InstructionData::Ternary {
                opcode: Opcode::Fma,
                args: [left, right, addend],
            } if !self.canonicalize_nans
                && (self.aarch64 || self.has_x86_fma())
                && ty(left) == output
                && ty(right) == output
                && ty(addend) == output =>
            {
                let op = match output {
                    types::F32X4 => IntegerOp::VectorFloatFma32,
                    types::F64X2 => IntegerOp::VectorFloatFma64,
                    _ => return None,
                };
                (op, [input(left)?, input(right)?, input(addend)?])
            }
            InstructionData::Ternary {
                opcode: Opcode::Bitselect,
                args: [mask, yes, no],
            } if ty(mask) == output && ty(yes) == output && ty(no) == output => {
                return Some(self.vector_bitselect(input(mask)?, input(yes)?, input(no)?));
            }
            InstructionData::LoadNoOffset {
                opcode: Opcode::Bitcast,
                flags,
                arg,
            } if vector_type(ty(arg)) && supports_memory_format(flags) => return input(arg),
            InstructionData::StackLoad {
                opcode: Opcode::StackLoad,
                stack_slot,
                offset,
            } => {
                let [slot, offset] = self.stack_location(stack_slot, offset, SIMD_BYTES)?;
                let address = self.append(IntegerOp::StackAddress, [slot, offset, 0]);
                (IntegerOp::VectorLoad, [address, 0, 0])
            }
            InstructionData::Load {
                opcode: Opcode::Load,
                flags,
                arg,
                offset,
            } if ty(arg) == types::I64
                && supports_memory_format(flags)
                && (self.supports_memory_access(flags)
                    || self.stack_operand(arg, offset, SIMD_BYTES).is_some()) =>
            {
                (
                    IntegerOp::VectorLoad,
                    [input(arg)?, u64::from(i32::from(offset) as u32), 0],
                )
            }
            _ => return None,
        };
        Some(self.append(op, operands))
    }

    fn vector_scalar_instruction(
        &mut self,
        inst: Inst,
        value: Value,
        values: &Values,
    ) -> Option<u64> {
        let dfg = &self.function.dfg;
        let input = |value| values.low[dfg.resolve_aliases(value)];
        let output = self.value_type(value);
        match dfg.insts[inst] {
            InstructionData::BinaryImm8 {
                opcode: Opcode::Extractlane,
                arg,
                imm,
            } => {
                let ty = self.value_type(arg);
                let lane = u64::from(imm);
                if !vector_type(ty)
                    || output != ty.lane_type()
                    || lane >= u64::from(ty.lane_count())
                {
                    return None;
                }
                let float = float_width(output);
                let width = float.or_else(|| integer_width(output))?;
                let result = self.append(IntegerOp::VectorExtract, [input(arg)?, lane, width]);
                Some(if float.is_some() {
                    self.append(IntegerOp::BitsToFloat, [result, 0, width])
                } else {
                    result
                })
            }
            InstructionData::Unary { opcode, arg } => {
                let ty = self.value_type(arg);
                if !vector_type(ty) {
                    return None;
                }
                let output_width = integer_width(output)?;
                let (op, width) = match opcode {
                    Opcode::VanyTrue => (IntegerOp::VectorAnyTrue, integer_width(ty.lane_type())?),
                    Opcode::VallTrue => (IntegerOp::VectorAllTrue, integer_width(ty.lane_type())?),
                    Opcode::VhighBits => {
                        (IntegerOp::VectorHighBits, u64::from(ty.lane_type().bits()))
                    }
                    _ => return None,
                };
                let result = self.append(op, [input(arg)?, 0, width]);
                Some(
                    if opcode == Opcode::VhighBits && output_width < u64::from(ty.lane_count()) {
                        self.append(
                            IntegerOp::Truncate,
                            [result, INTEGER_CELL_BITS, output_width],
                        )
                    } else {
                        result
                    },
                )
            }
            _ => None,
        }
    }

    // A block's arguments are already mapped by its incoming edge.
    fn emit_block(&mut self, block: Block, values: &mut Values) -> Option<Inst> {
        let function = self.function;
        let mut instructions = function.layout.block_insts(block);
        let terminator = instructions.next_back()?;
        for inst in instructions {
            let input = |value| values.low[function.dfg.resolve_aliases(value)];
            let ty = |value| function.dfg.value_type(function.dfg.resolve_aliases(value));
            if let InstructionData::CondTrap { opcode, arg, .. } = function.dfg.insts[inst] {
                if !self.allow_hardware_faults && !self.aarch64 {
                    return None;
                }
                let condition = self.truthy_value(arg, values)?;
                let op = match opcode {
                    Opcode::Trapz => IntegerOp::TrapIfZero,
                    Opcode::Trapnz => IntegerOp::TrapIfNonzero,
                    _ => return None,
                };
                self.append(op, [condition, 0, 0]);
                continue;
            }
            if let InstructionData::Ternary {
                opcode: Opcode::SelectSpectreGuard,
                args,
            } = function.dfg.insts[inst]
            {
                self.guarded_select(inst, args, values)?;
                continue;
            }
            if function.dfg.insts[inst].opcode() == Opcode::Fence {
                self.append(IntegerOp::Fence, [0; 3]);
                continue;
            }
            if let InstructionData::StoreNoOffset {
                opcode: Opcode::AtomicStore,
                flags,
                args: [source, address],
            } = function.dfg.insts[inst]
            {
                if ty(address) != types::I64 || !self.supports_atomic_access(flags) {
                    return None;
                }
                // AArch64 uses a release store. x86 needs an exchange to
                // order the store before subsequent loads.
                let op = if self.aarch64 {
                    IntegerOp::AtomicStore
                } else {
                    IntegerOp::AtomicExchange
                };
                self.append(
                    op,
                    [input(address)?, input(source)?, integer_width(ty(source))?],
                );
                continue;
            }
            if let InstructionData::StackStore {
                opcode: Opcode::StackStore,
                arg,
                stack_slot,
                offset,
            } = function.dfg.insts[inst]
            {
                let source = ty(arg);
                let [slot, offset] = self.stack_location(stack_slot, offset, source.bytes())?;
                if vector_type(source) {
                    let address = self.append(IntegerOp::StackAddress, [slot, offset, 0]);
                    self.append(IntegerOp::VectorStore, [address, input(arg)?, 0]);
                } else {
                    self.append(stack_store_op(source)?, [input(arg)?, slot, offset]);
                }
                continue;
            }
            if let InstructionData::Store {
                opcode,
                flags,
                args: [source, base],
                offset,
            } = function.dfg.insts[inst]
            {
                if !supports_memory_format(flags) || ty(base) != types::I64 {
                    return None;
                }
                let address = input(base)?;
                if opcode == Opcode::Store && vector_type(ty(source)) {
                    if !self.supports_memory_access(flags)
                        && self.stack_operand(base, offset, SIMD_BYTES).is_none()
                    {
                        return None;
                    }
                    self.append(
                        IntegerOp::VectorStore,
                        [address, input(source)?, u64::from(i32::from(offset) as u32)],
                    );
                    continue;
                }
                if opcode == Opcode::Store
                    && let Some([slot, offset]) =
                        self.stack_operand(base, offset, ty(source).bytes())
                {
                    self.append(stack_store_op(ty(source))?, [input(source)?, slot, offset]);
                    continue;
                }
                if !self.supports_memory_access(flags) {
                    return None;
                }
                let op = if opcode == Opcode::Store && float_width(ty(source)).is_some() {
                    float_store_op(ty(source))?
                } else {
                    store_op(opcode, ty(source))?
                };
                self.append(
                    op,
                    [address, input(source)?, u64::from(i32::from(offset) as u32)],
                );
                continue;
            }
            if let InstructionData::Call {
                opcode: Opcode::Call,
                func_ref,
                ..
            } = function.dfg.insts[inst]
            {
                self.direct_call(inst, &function.dfg.ext_funcs[func_ref], values)?;
                continue;
            }
            if let InstructionData::CallIndirect {
                opcode: Opcode::CallIndirect,
                sig_ref,
                ..
            } = function.dfg.insts[inst]
            {
                let (&address, args) = function.dfg.inst_args(inst).split_first()?;
                if ty(address) != types::I64 {
                    return None;
                }
                self.call(
                    inst,
                    &function.dfg.signatures[sig_ref],
                    args,
                    CallTarget::Indirect(input(address)?),
                    values,
                )?;
                continue;
            }
            if let InstructionData::Binary {
                opcode,
                args: [left, right],
            } = function.dfg.insts[inst]
                && let Some(op) = checked_binary_op(opcode)
            {
                let [result, overflow] = *function.dfg.inst_results(inst) else {
                    return None;
                };
                let width = ty(left);
                if !matches!(width, types::I32 | types::I64)
                    || ty(right) != width
                    || ty(result) != width
                    || ty(overflow) != types::I8
                {
                    return None;
                }
                values.low[result] =
                    Some(self.append(op, [input(left)?, input(right)?, u64::from(width.bits())]));
                values.low[overflow] = Some(self.append(IntegerOp::OverflowFlag, [0; 3]));
                continue;
            }
            if function
                .dfg
                .inst_results(inst)
                .iter()
                .chain(function.dfg.inst_args(inst))
                .any(|&value| ty(value) == types::I128)
            {
                self.wide_instruction(inst, values)?;
                continue;
            }
            let [value] = *function.dfg.inst_results(inst) else {
                return None;
            };
            if vector_type(ty(value)) {
                values.low[value] = Some(self.vector_instruction(inst, value, values)?);
                continue;
            }
            if matches!(
                function.dfg.insts[inst].opcode(),
                Opcode::Extractlane | Opcode::VanyTrue | Opcode::VallTrue | Opcode::VhighBits
            ) {
                values.low[value] = Some(self.vector_scalar_instruction(inst, value, values)?);
                continue;
            }
            if float_width(ty(value)).is_some()
                || function
                    .dfg
                    .inst_args(inst)
                    .iter()
                    .any(|&value| float_width(ty(value)).is_some())
            {
                values.low[value] = Some(self.float_instruction(inst, value, values)?);
                continue;
            }
            let output = function.dfg.value_type(value);
            let width = integer_width(output)?;
            let (op, operands) = match function.dfg.insts[inst] {
                InstructionData::NullAry {
                    opcode: Opcode::GetStackPointer,
                } if output == types::I64 => (IntegerOp::StackPointer, [0; 3]),
                InstructionData::LoadNoOffset {
                    opcode: Opcode::AtomicLoad,
                    flags,
                    arg,
                } if ty(arg) == types::I64 && self.supports_atomic_access(flags) => {
                    // x86 orders a plain load strongly enough for a CLIF atomic load.
                    let op = if self.aarch64 {
                        IntegerOp::AtomicLoad
                    } else {
                        IntegerOp::Load
                    };
                    (op, [input(arg)?, 0, width])
                }
                InstructionData::AtomicRmw {
                    opcode: Opcode::AtomicRmw,
                    flags,
                    op,
                    args: [address, source],
                } if ty(address) == types::I64
                    && ty(source) == output
                    && self.supports_atomic_access(flags) =>
                {
                    let op = match op {
                        AtomicRmwOp::Add => IntegerOp::AtomicAdd,
                        AtomicRmwOp::Sub => IntegerOp::AtomicSub,
                        AtomicRmwOp::Xchg => IntegerOp::AtomicExchange,
                        AtomicRmwOp::And => IntegerOp::AtomicAnd,
                        AtomicRmwOp::Nand => IntegerOp::AtomicNand,
                        AtomicRmwOp::Or => IntegerOp::AtomicOr,
                        AtomicRmwOp::Xor => IntegerOp::AtomicXor,
                        AtomicRmwOp::Umin => IntegerOp::AtomicUnsignedMin,
                        AtomicRmwOp::Umax => IntegerOp::AtomicUnsignedMax,
                        AtomicRmwOp::Smin => IntegerOp::AtomicSignedMin,
                        AtomicRmwOp::Smax => IntegerOp::AtomicSignedMax,
                    };
                    (op, [input(address)?, input(source)?, width])
                }
                InstructionData::AtomicCas {
                    opcode: Opcode::AtomicCas,
                    flags,
                    args: [address, expected, replacement],
                } if ty(address) == types::I64
                    && ty(expected) == output
                    && ty(replacement) == output
                    && self.supports_atomic_access(flags) =>
                {
                    let op = match output {
                        types::I8 => IntegerOp::AtomicCasByte,
                        types::I16 => IntegerOp::AtomicCasWord,
                        types::I32 => IntegerOp::AtomicCasDword,
                        types::I64 => IntegerOp::AtomicCasQword,
                        _ => return None,
                    };
                    (op, [input(address)?, input(expected)?, input(replacement)?])
                }
                InstructionData::Unary {
                    opcode: Opcode::Bswap,
                    arg,
                } if ty(arg) == output && output != types::I8 => {
                    (IntegerOp::ByteSwap, [input(arg)?, 0, width])
                }
                InstructionData::Unary {
                    opcode:
                        opcode @ (Opcode::Clz
                        | Opcode::Ctz
                        | Opcode::Popcnt
                        | Opcode::Bitrev
                        | Opcode::Cls),
                    arg,
                } if ty(arg) == output => (
                    integer_bit_op(opcode)?,
                    [input(arg)?, self.bit_operation_mode(opcode), width],
                ),
                InstructionData::Unary {
                    opcode: Opcode::Iabs,
                    arg,
                } if ty(arg) == output => (IntegerOp::Abs, [input(arg)?, 0, width]),
                InstructionData::UnaryGlobalValue {
                    opcode: Opcode::GlobalValue | Opcode::SymbolValue,
                    global_value,
                } if output == types::I64 => {
                    values.low[value] = Some(self.symbol_address(global_value, width)?);
                    continue;
                }
                InstructionData::FuncAddr {
                    opcode: Opcode::FuncAddr,
                    func_ref,
                } if output == types::I64 => {
                    let ExternalName::User(name) = function.dfg.ext_funcs[func_ref].name else {
                        return None;
                    };
                    let name = function.params.user_named_funcs().get(name)?;
                    if name.namespace != FUNCTION_NAMESPACE
                        || self.declarations.is_some_and(|declarations| {
                            name.index as usize >= declarations.get_functions().size_hint().0
                        })
                    {
                        return None;
                    }
                    self.relocations = true;
                    (IntegerOp::FunctionGot, [u64::from(name.index), 0, 0])
                }
                InstructionData::UnaryImm {
                    opcode: Opcode::Iconst,
                    imm,
                } => (
                    IntegerOp::Constant,
                    [integer_bits(imm.bits(), output), 0, 0],
                ),
                InstructionData::StackLoad {
                    opcode: Opcode::StackAddr,
                    stack_slot,
                    offset,
                } if output == types::I64 => {
                    let [slot, offset] = self.stack_location(stack_slot, offset, 0)?;
                    (IntegerOp::StackAddress, [slot, offset, 0])
                }
                InstructionData::StackLoad {
                    opcode: Opcode::StackLoad,
                    stack_slot,
                    offset,
                } => {
                    let [slot, offset] = self.stack_location(stack_slot, offset, output.bytes())?;
                    (IntegerOp::StackLoad, [slot, offset, width])
                }
                InstructionData::Unary { opcode, arg } => {
                    let source_width = integer_width(ty(arg))?;
                    let op = match opcode {
                        Opcode::Ineg if source_width == width => IntegerOp::Neg,
                        Opcode::Bnot if source_width == width => IntegerOp::Not,
                        Opcode::Ireduce if source_width > width => IntegerOp::Truncate,
                        Opcode::Uextend if source_width < width => IntegerOp::ZeroExtend,
                        Opcode::Sextend if source_width < width => IntegerOp::SignExtend,
                        _ => return None,
                    };
                    (op, [input(arg)?, source_width, width])
                }
                InstructionData::Load {
                    opcode,
                    flags,
                    arg,
                    offset,
                } if supports_memory_format(flags) && ty(arg) == types::I64 => {
                    let address = input(arg)?;
                    if opcode == Opcode::Load
                        && let Some([slot, offset]) =
                            self.stack_operand(arg, offset, output.bytes())
                    {
                        values.low[value] =
                            Some(self.append(IntegerOp::StackLoad, [slot, offset, width]));
                        continue;
                    }
                    if !self.supports_memory_access(flags) {
                        return None;
                    }
                    let (access_width, signed) = match opcode {
                        Opcode::Load => (width, false),
                        Opcode::Uload8 => (u64::from(types::I8.bits()), false),
                        Opcode::Uload16 => (u64::from(types::I16.bits()), false),
                        Opcode::Uload32 => (u64::from(types::I32.bits()), false),
                        Opcode::Sload8 => (u64::from(types::I8.bits()), true),
                        Opcode::Sload16 => (u64::from(types::I16.bits()), true),
                        Opcode::Sload32 => (u64::from(types::I32.bits()), true),
                        _ => return None,
                    };
                    if opcode != Opcode::Load && access_width >= width {
                        return None;
                    }
                    let load = [address, u64::from(i32::from(offset) as u32), access_width];
                    if signed {
                        let value = self.append(IntegerOp::Load, load);
                        (IntegerOp::SignExtend, [value, access_width, width])
                    } else {
                        (IntegerOp::Load, load)
                    }
                }
                InstructionData::IntCompare {
                    opcode: Opcode::Icmp,
                    cond,
                    args: [left, right],
                } if output == types::I8 && ty(left) == ty(right) => {
                    let left_id = input(left)?;
                    let right_id = input(right)?;
                    if let Some(bits) = self
                        .constant_bits(right)
                        .and_then(|bits| sign_extended_immediate(bits, ty(right)))
                    {
                        (
                            compare_op(ty(left), true)?,
                            [left_id, bits, condition(cond)],
                        )
                    } else {
                        (
                            compare_op(ty(left), false)?,
                            [left_id, right_id, condition(cond)],
                        )
                    }
                }
                InstructionData::IntCompareImm {
                    opcode: Opcode::IcmpImm,
                    cond,
                    arg,
                    imm,
                } if output == types::I8 => {
                    let left = input(arg)?;
                    if let Some(bits) = sign_extended_immediate(imm.bits(), ty(arg)) {
                        (compare_op(ty(arg), true)?, [left, bits, condition(cond)])
                    } else {
                        let bits = integer_bits(imm.bits(), ty(arg));
                        let right = self.append(IntegerOp::Constant, [bits, 0, 0]);
                        (compare_op(ty(arg), false)?, [left, right, condition(cond)])
                    }
                }
                InstructionData::Ternary {
                    opcode: Opcode::Bitselect,
                    args: [mask, yes, no],
                } if ty(mask) == output && ty(yes) == output && ty(no) == output => {
                    values.low[value] =
                        Some(self.bitselect(input(mask)?, input(yes)?, input(no)?, width));
                    continue;
                }
                InstructionData::Ternary {
                    opcode: Opcode::Select,
                    args: [cond, yes, no],
                } if integer_width(ty(cond)).is_some() && ty(yes) == output && ty(no) == output => {
                    (IntegerOp::Select, [input(cond)?, input(yes)?, input(no)?])
                }
                InstructionData::Binary {
                    opcode: opcode @ (Opcode::Rotl | Opcode::Rotr),
                    args: [value, amount],
                } if ty(value) == output && integer_width(ty(amount)).is_some() => {
                    let value = input(value)?;
                    if let Some(count) = self.constant_bits(amount) {
                        (
                            IntegerOp::RotateRight,
                            [value, right_rotation_count(opcode, count), width],
                        )
                    } else {
                        (rotate_register_op(opcode), [value, input(amount)?, width])
                    }
                }
                InstructionData::Binary {
                    opcode: opcode @ (Opcode::Ishl | Opcode::Ushr | Opcode::Sshr),
                    args: [left, right],
                } if (self.aarch64 || matches!(output, types::I32 | types::I64))
                    && ty(left) == output
                    && integer_width(ty(right)).is_some() =>
                {
                    let op = match opcode {
                        Opcode::Ishl => IntegerOp::ShiftLeftRegister,
                        Opcode::Ushr => IntegerOp::ShiftRightLogicalRegister,
                        _ => IntegerOp::ShiftRightArithmeticRegister,
                    };
                    (op, [input(left)?, input(right)?, width])
                }
                InstructionData::Binary {
                    opcode: opcode @ (Opcode::Sdiv | Opcode::Udiv | Opcode::Srem | Opcode::Urem),
                    args: [left, right],
                } if self.supports_division(output)
                    && ty(left) == output
                    && ty(right) == output =>
                {
                    let op = division_op(opcode)?;
                    let numerator = input(left)?;
                    if self.aarch64
                        && let Some(bits) = self.constant_bits(right)
                    {
                        self.division_immediate(op, numerator, integer_bits(bits, output), width)
                    } else {
                        (op, [numerator, input(right)?, width])
                    }
                }
                InstructionData::Binary {
                    opcode: opcode @ (Opcode::Smulhi | Opcode::Umulhi),
                    args: [left, right],
                } if matches!(output, types::I32 | types::I64)
                    && ty(left) == output
                    && ty(right) == output =>
                {
                    let op = match opcode {
                        Opcode::Smulhi => IntegerOp::SignedMulHigh,
                        _ => IntegerOp::UnsignedMulHigh,
                    };
                    (op, [input(left)?, input(right)?, width])
                }
                InstructionData::Binary {
                    opcode,
                    args: [left, right],
                } if ty(left) == output && ty(right) == output => {
                    if let Some(cond) = minmax_condition(opcode) {
                        let left = input(left)?;
                        let right = input(right)?;
                        let choose_left =
                            self.append(compare_op(output, false)?, [left, right, condition(cond)]);
                        values.low[value] =
                            Some(self.append(IntegerOp::Select, [choose_left, left, right]));
                        continue;
                    }
                    if let Some((op, left, right)) =
                        self.complemented_bitwise_operands(opcode, left, right)
                        && let (Some(left), Some(right)) = (input(left), input(right))
                    {
                        values.low[value] = Some(self.append(op, [left, right, width]));
                        continue;
                    }
                    let op = binary_op(opcode)?;
                    let left_id = input(left)?;
                    let right_id = input(right)?;
                    if let Some(bits) = self
                        .constant_bits(right)
                        .and_then(|bits| sign_extended_immediate(bits, output))
                    {
                        (immediate_op(op), [left_id, bits, width])
                    } else if opcode != Opcode::Isub
                        && let Some(bits) = self
                            .constant_bits(left)
                            .and_then(|bits| sign_extended_immediate(bits, output))
                    {
                        (immediate_op(op), [right_id, bits, width])
                    } else {
                        (op, [left_id, right_id, width])
                    }
                }
                InstructionData::BinaryImm64 { opcode, arg, imm } if ty(arg) == output => {
                    let left = input(arg)?;
                    let bits = integer_bits(imm.bits(), output);
                    match opcode {
                        Opcode::IshlImm => (IntegerOp::ShiftLeft, [left, bits, width]),
                        Opcode::UshrImm => (IntegerOp::ShiftRightLogical, [left, bits, width]),
                        Opcode::SshrImm => (IntegerOp::ShiftRightArithmetic, [left, bits, width]),
                        Opcode::IrsubImm if bits == 0 => (IntegerOp::Neg, [left, width, width]),
                        Opcode::IrsubImm => {
                            let minuend = self.append(IntegerOp::Constant, [bits, 0, 0]);
                            (IntegerOp::Sub, [minuend, left, width])
                        }
                        Opcode::SdivImm | Opcode::UdivImm | Opcode::SremImm | Opcode::UremImm
                            if self.supports_division(output) =>
                        {
                            self.division_immediate(division_op(opcode)?, left, bits, width)
                        }
                        Opcode::RotlImm | Opcode::RotrImm => (
                            IntegerOp::RotateRight,
                            [left, right_rotation_count(opcode, imm.bits()), width],
                        ),
                        _ => {
                            let op = binary_op(opcode)?;
                            if let Some(bits) = sign_extended_immediate(imm.bits(), output) {
                                (immediate_op(op), [left, bits, width])
                            } else {
                                let right = self.append(IntegerOp::Constant, [bits, 0, 0]);
                                (op, [left, right, width])
                            }
                        }
                    }
                }
                _ => return None,
            };
            values.low[value] = Some(self.append(op, operands));
        }
        Some(terminator)
    }

    fn follow_jumps(
        &mut self,
        mut block: Block,
        values: &mut Values,
        visited: &mut SecondaryMap<Block, bool>,
    ) -> Option<Inst> {
        loop {
            if visited[block] {
                self.needs_cfg = true;
                return None;
            }
            if !self.function.layout.is_block_inserted(block) {
                return None;
            }
            visited[block] = true;
            let start = self.words.len();
            let terminator = self.emit_block(block, values)?;
            if self.emitted_blocks[block] {
                self.duplicated_instructions += (self.words.len() - start) / INSTRUCTION_WORDS;
            }
            self.emitted_blocks[block] = true;
            match self.function.dfg.insts[terminator] {
                InstructionData::Jump {
                    opcode: Opcode::Jump,
                    destination,
                } => block = self.edge(destination, values)?,
                _ => return Some(terminator),
            }
        }
    }

    fn returned(&self, terminator: Inst, values: &Values) -> Option<(u64, Option<u64>)> {
        let dfg = &self.function.dfg;
        if dfg.insts[terminator].opcode() != Opcode::Return {
            return None;
        }
        let arguments = dfg.inst_args(terminator);
        let returns = &self.function.signature.returns;
        if arguments.len() != returns.len()
            || arguments
                .iter()
                .zip(returns)
                .any(|(&value, param)| self.value_type(value) != param.value_type)
        {
            return None;
        }
        let result = |value| values.low[dfg.resolve_aliases(value)];
        match *arguments {
            [] => Some((NO_VALUE, None)),
            [wide] if self.return_cells == GP_RESULTS => {
                Some((result(wide)?, Some(values.high[dfg.resolve_aliases(wide)]?)))
            }
            [low] => Some((result(low)?, None)),
            [low, high] => Some((result(low)?, Some(result(high)?))),
            _ => None,
        }
    }

    fn emits_trap(&self, inst: Inst) -> bool {
        (self.allow_hardware_faults || self.aarch64)
            && self.function.dfg.insts[inst].opcode() == Opcode::Trap
    }

    // Each arm follows its path to a return or trap. If the paths rejoin, the shared
    // suffix is emitted for each arm with that arm's argument bindings.
    // Each traversal includes the prefix in its visited set to reject cycles.
    fn path_exit(
        &mut self,
        entry: Block,
        mut values: Values,
        mut visited: SecondaryMap<Block, bool>,
    ) -> Option<PathExit> {
        let terminator = self.follow_jumps(entry, &mut values, &mut visited)?;
        if self.function.dfg.insts[terminator].opcode() == Opcode::BrTable {
            self.needs_cfg = true;
            return None;
        }
        let InstructionData::Brif {
            opcode: Opcode::Brif,
            arg: condition,
            blocks: [yes_edge, no_edge],
        } = self.function.dfg.insts[terminator]
        else {
            return if self.emits_trap(terminator) {
                Some(PathExit::Trap)
            } else {
                self.returned(terminator, &values)
                    .map(|(low, high)| PathExit::Return(low, high))
            };
        };
        if self.branches == MAX_LINEARIZED_BRANCHES {
            self.needs_cfg = true;
            return None;
        }
        self.branches += 1;
        integer_width(self.value_type(condition))?;
        let condition = values.low[self.function.dfg.resolve_aliases(condition)]?;
        let branch = self.append(IntegerOp::BranchZero, [condition, 0, 0]);
        let mut yes_values = values.clone();
        let yes_block = self.edge(yes_edge, &mut yes_values)?;
        let (yes_low, yes_high) = match self.path_exit(yes_block, yes_values, visited.clone())? {
            PathExit::Return(low, high) => (low, high.unwrap_or(0)),
            PathExit::Trap => (TRAP_VALUE, 0),
        };
        self.append(IntegerOp::EarlyReturn, [yes_low, yes_high, branch]);

        let no_block = self.edge(no_edge, &mut values)?;
        self.path_exit(no_block, values, visited)
    }

    fn cfg_edge(
        &mut self,
        edge: BlockCall,
        labels: &SecondaryMap<Block, Option<u64>>,
        homes: &SecondaryMap<Value, Option<u64>>,
        slot: u64,
        values: &Values,
    ) -> Option<()> {
        let dfg = &self.function.dfg;
        let target = edge.block(&dfg.value_lists);
        let params = dfg.block_params(target);
        if params.len() != edge.len(&dfg.value_lists) {
            return None;
        }
        // Source IDs are SSA snapshots. Stores cannot overwrite another
        // source even when the destinations swap two block parameters.
        for (&param, argument) in params.iter().zip(edge.args(&dfg.value_lists)) {
            let arg = dfg.resolve_aliases(argument.as_value()?);
            let ty = dfg.value_type(param);
            if ty != dfg.value_type(arg) {
                return None;
            }
            let destination = homes[param]?;
            // All homed definitions have been saved before the edge, and no
            // other parameter of the target can have this same destination.
            if homes[arg] == Some(destination) {
                continue;
            }
            self.store_cfg_value(ty, arg, values, slot, destination)?;
        }
        self.append(IntegerOp::JumpLabel, [labels[target]?, 0, 0]);
        Some(())
    }

    fn store_cfg_value(
        &mut self,
        ty: Type,
        value: Value,
        values: &Values,
        slot: u64,
        offset: u64,
    ) -> Option<()> {
        if ty == types::I128 {
            self.append(
                IntegerOp::StackStoreQword,
                [values.low[value]?, slot, offset],
            );
            self.append(
                IntegerOp::StackStoreQword,
                [values.high[value]?, slot, offset + CFG_HOME_BYTES],
            );
        } else {
            self.append(stack_store_op(ty)?, [values.low[value]?, slot, offset]);
        }
        Some(())
    }

    fn labeled_cfg(&mut self, entry: Block) -> Option<()> {
        let function = self.function;
        let dfg = &function.dfg;
        let layout = &function.layout;
        let mut labels = SecondaryMap::new();
        let mut predecessors = SecondaryMap::<Block, Vec<Block>>::new();
        let mut order = vec![entry];
        labels[entry] = Some(0);
        let mut index = 0;
        while let Some(&block) = order.get(index) {
            let end = layout.last_inst(block)?;
            let terminator = &dfg.insts[end];
            if !matches!(
                terminator.opcode(),
                Opcode::Jump | Opcode::Brif | Opcode::BrTable | Opcode::Return | Opcode::Trap
            ) {
                return None;
            }
            for edge in terminator.branch_destination(&dfg.jump_tables, &dfg.exception_tables) {
                let target = edge.block(&dfg.value_lists);
                if target == entry || !layout.is_block_inserted(target) {
                    return None;
                }
                predecessors[target].push(block);
                if labels[target].is_none() {
                    labels[target] = Some(order.len() as u64);
                    order.push(target);
                }
            }
            index += 1;
        }

        // A jump into a block with one predecessor can keep its SSA values
        // in the same machine block. Conditional edges and joins start a
        // separate block and still use the home protocol.
        let mut groups = SecondaryMap::<Block, Option<Block>>::new();
        let mut continuation = SecondaryMap::<Block, Option<Block>>::new();
        let mut label_count = 0;
        for &block in &order {
            if let &[predecessor] = predecessors[block].as_slice()
                && dfg.insts[layout.last_inst(predecessor)?].opcode() == Opcode::Jump
            {
                groups[block] = groups[predecessor];
                labels[block] = None;
                continuation[predecessor] = Some(block);
            } else {
                groups[block] = Some(block);
                labels[block] = Some(label_count);
                label_count += 1;
            }
        }

        let mut home_values = Vec::new();
        let mut needs_home = SecondaryMap::<Value, bool>::new();
        let mut uses = SecondaryMap::<Value, Vec<Block>>::new();
        let mut incoming = SecondaryMap::<Block, BTreeSet<Value>>::new();
        let owner = |value| match dfg.value_def(value) {
            ValueDef::Result(inst, _) => layout.inst_block(inst),
            ValueDef::Param(block, _) => Some(block),
            ValueDef::Union(_, _) => None,
        };
        let mut mark_home = |value| {
            if !needs_home[value] {
                needs_home[value] = true;
                home_values.push(value);
            }
        };
        for &block in &order {
            let group = groups[block]?;
            if block == group && block != entry {
                for &param in dfg.block_params(block) {
                    mark_home(param);
                }
            }
            for inst in layout.block_insts(block) {
                for arg in dfg.inst_values(inst) {
                    let arg = dfg.resolve_aliases(arg);
                    let owner = owner(arg)?;
                    let parameter = matches!(dfg.value_def(arg), ValueDef::Param(..));
                    if (groups[owner]? != group || (parameter && owner == group && group != entry))
                        && incoming[group].insert(arg)
                    {
                        mark_home(arg);
                        uses[arg].push(group);
                    }
                }
            }
        }

        let mut homes = SecondaryMap::<Value, Option<u64>>::new();
        let mut cells: Vec<(Type, BTreeSet<Block>)> = Vec::new();
        for value in home_values {
            let ty = dfg.value_type(value);
            let width = if ty == types::I128 {
                WIDE_INTEGER_CELLS
            } else {
                integer_width(ty).or_else(|| float_width(ty))?;
                1
            };
            // Walk backwards from loads to the definition. A parameter is
            // written by each incoming edge, so its lifetime starts at the
            // target block. Edge sources have already been loaded into SSA
            // temporaries before their homes are overwritten.
            let mut live = BTreeSet::from([groups[owner(value)?]?]);
            let mut pending = std::mem::take(&mut uses[value]);
            while let Some(block) = pending.pop() {
                if live.insert(block) {
                    for &predecessor in &predecessors[block] {
                        pending.push(groups[predecessor]?);
                    }
                }
            }
            let start = cells
                .windows(width)
                .position(|slots| {
                    slots
                        .iter()
                        .all(|(class, occupied)| *class == ty && occupied.is_disjoint(&live))
                })
                .unwrap_or(cells.len());
            if start == cells.len() {
                let max_cells = STACK_PROBE_INTERVAL as u64 / CFG_HOME_BYTES;
                if (start + width) as u64 > max_cells {
                    return None;
                }
                cells.extend((0..width).map(|_| (ty, BTreeSet::new())));
            }
            for (_, occupied) in &mut cells[start..start + width] {
                occupied.extend(live.iter().copied());
            }
            homes[value] = Some(start as u64 * CFG_HOME_BYTES);
        }
        let home_bytes = cells.len() as u64 * CFG_HOME_BYTES;
        let slot = function.sized_stack_slots.len() as u64;
        if home_bytes != 0 {
            self.append(IntegerOp::StackSlot, [home_bytes, CFG_HOME_BYTES, 0]);
        }
        let results = if self.float_return {
            FLOAT_RETURN_LAYOUT
        } else {
            self.return_cells as u64
        };
        let mut next_edge = label_count;
        for block in order {
            if groups[block] != Some(block) {
                continue;
            }
            let home_slot = if block == entry && home_bytes != 0 {
                slot + 1
            } else {
                0
            };
            self.append(IntegerOp::BlockLabel, [labels[block]?, home_slot, results]);
            let mut values = if block == entry {
                entry_values(self, block)?
            } else {
                Values::default()
            };
            for &value in &incoming[block] {
                let ty = dfg.value_type(value);
                let offset = homes[value]?;
                let width = if ty == types::I128 {
                    INTEGER_CELL_BITS
                } else {
                    integer_width(ty).or_else(|| float_width(ty))?
                };
                let op = if float_width(ty).is_some() {
                    IntegerOp::FloatStackLoad
                } else {
                    IntegerOp::StackLoad
                };
                values.low[value] = Some(self.append(op, [slot, offset, width]));
                if ty == types::I128 {
                    values.high[value] = Some(
                        self.append(IntegerOp::StackLoad, [slot, offset + CFG_HOME_BYTES, width]),
                    );
                }
            }
            let mut current = block;
            let end = loop {
                let end = self.emit_block(current, &mut values)?;
                let definitions = dfg
                    .block_params(current)
                    .iter()
                    .copied()
                    .filter(|_| current != block || block == entry)
                    .chain(
                        layout
                            .block_insts(current)
                            .flat_map(|inst| dfg.inst_results(inst).iter().copied()),
                    );
                for value in definitions {
                    if let Some(offset) = homes[value] {
                        self.store_cfg_value(dfg.value_type(value), value, &values, slot, offset)?;
                    }
                }
                let Some(next) = continuation[current] else {
                    break end;
                };
                let InstructionData::Jump { destination, .. } = dfg.insts[end] else {
                    return None;
                };
                current = self.edge(destination, &mut values)?;
                debug_assert_eq!(current, next);
            };
            match dfg.insts[end] {
                InstructionData::Jump { destination, .. } => {
                    self.cfg_edge(destination, &labels, &homes, slot, &values)?;
                }
                InstructionData::Brif {
                    arg,
                    blocks: [yes, no],
                    ..
                } => {
                    integer_width(self.value_type(arg))?;
                    let condition = values.low[dfg.resolve_aliases(arg)]?;
                    if yes.len(&dfg.value_lists) == 0 && no.len(&dfg.value_lists) == 0 {
                        let target = labels[no.block(&dfg.value_lists)]?;
                        self.append(IntegerOp::BranchBlock, [condition, target, 0]);
                        self.cfg_edge(yes, &labels, &homes, slot, &values)?;
                    } else {
                        self.append(IntegerOp::BranchLabel, [condition, next_edge, 0]);
                        self.cfg_edge(yes, &labels, &homes, slot, &values)?;
                        self.append(IntegerOp::EdgeLabel, [next_edge, 0, 0]);
                        self.cfg_edge(no, &labels, &homes, slot, &values)?;
                        next_edge += 1;
                    }
                }
                InstructionData::BranchTable { arg, table, .. } => {
                    if self.value_type(arg) != types::I32 {
                        return None;
                    }
                    let table = &dfg.jump_tables[table];
                    let target = |edge: BlockCall| {
                        // Table offsets target whole blocks. An edge that transfers
                        // block arguments would need its own landing code.
                        let block = edge.block(&dfg.value_lists);
                        if edge.len(&dfg.value_lists) != 0 || !dfg.block_params(block).is_empty() {
                            return None;
                        }
                        labels[block]
                    };
                    let default = target(table.default_block())?;
                    let cases = table.as_slice();
                    if cases.is_empty() {
                        self.append(IntegerOp::JumpLabel, [default, 0, 0]);
                    } else {
                        let count = u32::try_from(cases.len()).ok()?;
                        let index = values.low[dfg.resolve_aliases(arg)]?;
                        self.append(IntegerOp::BranchTable, [index, u64::from(count), default]);
                        for &case in cases {
                            self.append(IntegerOp::BranchTableTarget, [target(case)?, 0, 0]);
                        }
                    }
                }
                _ => {
                    let (low, high) = if self.emits_trap(end) {
                        (TRAP_VALUE, None)
                    } else {
                        self.returned(end, &values)?
                    };
                    self.append(IntegerOp::BlockReturn, [low, high.unwrap_or(0), 0]);
                }
            }
        }
        Some(())
    }
}

/// Encodes supported System V scalar paths into a value stream for the Vesper backend.
/// The header holds
/// the argument count and the return value ID, with NO_VALUE for a procedure.
/// The remaining words are
/// (opcode, left, right, extra) records. The extra word is the result width
/// for operations such as loads and arithmetic, the condition code for comparisons, or
/// the false value ID for selects. Arguments come first in the value space,
/// followed by one slot per instruction. A store uses its extra word for
/// displacement and leaves an unreferenced slot. Operand IDs refer to prior
/// value-producing instructions or arguments.
/// Immediate arithmetic and comparisons use a 32-bit literal in the right
/// word. It is sign-extended to 64 bits or truncated to the operand width.
/// BranchZero skips to the instruction after its matching EarlyReturn. Each
/// EarlyReturn's extra word is the slot ID of its BranchZero. These records
/// leave unreferenced slots. An early return from a procedure uses NO_VALUE.
/// A trapping arm uses TRAP_VALUE, and a final Trap records the return arity.
/// No arm may read values defined in a sibling arm.
/// A trailing ReturnPair gives the second result ID and makes EarlyReturn's
/// right operand a second result ID as well.
/// Leading StackSlot records declare frame slots by size and alignment.
/// Stack addresses and accesses use a slot ordinal and an offset within the slot.
/// Each call is preceded by its CallArgument records. SysV on x86-64 groups
/// integer arguments before floating-point arguments. AArch64 places CallStackArgument records before
/// both register groups. These carry the value ID, stack offset, and width.
/// Win64 instead keeps CallArgument records in signature order, with a zero
/// extra for an integer argument or the floating-point width.
/// The call's left operand is the function ID and its right operand is the total
/// argument record count. The extra operand encodes the float register count in
/// the low bits and the AArch64 stack argument count in the high bits.
/// The top bit marks an indirect call, whose left operand is an address value ID.
/// AAPCS64 uses a NO_VALUE argument for a register skipped before a pair.
/// Argument records leave unreferenced slots. CallFloat returns through the
/// first floating-point result register. AArch64 uses CallFloat32 for f32.
/// Call targets above the u32 symbol-ID range identify approved runtime libcalls.
/// CallPair writes its low result and the immediately following CallHigh's
/// slot holds its high result.
/// Checked arithmetic writes its integer result, followed by OverflowFlag
/// for the boolean result of the same instruction.
/// AddCarry and SubBorrow consume the immediately preceding flag as their
/// third operand, with two i64 inputs and an i64 result.
/// Atomic read-modify-write records take an address, operand, and width.
/// They remain live for their memory effect even when the old value is unused.
/// Compare-and-swap records have a width-specific opcode and three operand IDs.
/// AArch64 AtomicLoad and AtomicStore encode ordered accesses without an offset.
/// The load remains live even when its result is unused.
/// After the stack declarations, FloatArgument binds successive registers
/// from the ABI's floating-point argument bank and occupies a normal value ID.
/// AArch64 interleaves StackArgument and FloatStackArgument in this entry prefix.
/// Each binds a value at its byte offset from the caller's stack pointer.
/// A stacked i128 occupies two consecutive integer records.
/// A trailing FloatReturn selects the ABI's single floating-point result.
/// SymbolRelative and SymbolGot read the data symbol ID from the left operand.
/// FunctionGot reads the function symbol ID from the left operand.
/// TrapIfZero and TrapIfNonzero read a single integer condition and continue on
/// the non-trapping path. StackPointer reads the active SP after the prologue.
/// SpectreBarrier orders AArch64 guarded selects before subsequent uses.
/// VectorConstant stores the two little-endian halves of a 128-bit value.
/// VectorSplat reads the scalar bits and the lane width. VectorLoad and
/// VectorStore use signed byte displacements and transfer all 128 bits.
/// A labeled function starts with BlockLabel after the stack declarations
/// and returns through BlockReturn records. The label's extra word describes
/// the integer return arity or a single floating-point result. The entry argument
/// records follow the first label. BranchLabel skips the taken edge
/// transfers and its jump to the matching EdgeLabel. Cross-block operands
/// reload from frame homes, and JumpLabel targets a non-entry BlockLabel.
/// BranchTable reads an i32 index, case count, and default label. Its following
/// BranchTableTarget records provide one label per index, in order.
/// The entry label's right word identifies the private home slot plus one,
/// or zero when the function needs no cross-block storage.
pub fn integer_block_words(function: &Function) -> Option<Vec<u64>> {
    integer_program(function, false, false, None, CallConv::SystemV, false, None)
        .map(|(primary, _)| primary.words)
}

/// Packs an x86-64 function with the instruction set enabled by the target.
pub fn x86_64_block_words(function: &Function, isa: &dyn TargetIsa) -> Option<Vec<u64>> {
    let call_conv = function.signature.call_conv;
    if call_conv == CallConv::WindowsFastcall {
        return windows_program(function, isa, None).map(|(primary, _)| primary.words);
    }
    if isa.triple().architecture != target_lexicon::Architecture::X86_64
        || !matches!(call_conv, CallConv::SystemV | CallConv::Tail)
    {
        return None;
    }
    integer_program(
        function,
        false,
        isa.flags().enable_nan_canonicalization(),
        None,
        call_conv,
        false,
        Some(isa),
    )
    .map(|(primary, _)| primary.words)
}

fn windows_signature(signature: &Signature) -> bool {
    signature.params.len() <= MAX_GP_ARGUMENTS
        && signature.returns.len() <= 1
        && signature
            .params
            .iter()
            .chain(&signature.returns)
            .all(windows_scalar)
}

fn windows_program(
    function: &Function,
    isa: &dyn TargetIsa,
    declarations: Option<&ModuleDeclarations>,
) -> Option<(IntegerProgram, Option<IntegerProgram>)> {
    let signature = &function.signature;
    if super::windows::check_target(isa.triple()).is_err()
        || isa.flags().preserve_frame_pointers()
        || isa.flags().enable_pinned_reg()
        || !windows_signature(signature)
    {
        return None;
    }
    let (mut primary, mut fallback) = integer_program(
        function,
        false,
        isa.flags().enable_nan_canonicalization(),
        declarations,
        CallConv::WindowsFastcall,
        false,
        Some(isa),
    )?;
    primary.words[ARGUMENT_COUNT] |= WINDOWS_ABI;
    if let Some(linear) = &mut fallback {
        linear.words[ARGUMENT_COUNT] |= WINDOWS_ABI;
    }
    Some((primary, fallback))
}

pub(super) struct WindowsCode {
    pub code: MachineCode,
    pub unwind: Option<Vec<u8>>,
}

/// Windows functions include their relocations and an unwind record for any frame.
pub(super) fn encode_windows_function(
    isa: &dyn TargetIsa,
    function: &Function,
    declarations: &ModuleDeclarations,
) -> Option<WindowsCode> {
    let encode = WINDOWS_ENCODER.get()?;
    let (primary, fallback) = windows_program(function, isa, Some(declarations))?;
    let emit = |program: IntegerProgram| {
        let packet = encode(&program.words)?;
        let (bytes, rest) = split_length_prefixed(&packet)?;
        let (unwind, records) = split_length_prefixed(rest)?;
        if !unwind.is_empty() {
            const UNWIND_VERSION_NO_FLAGS: u8 = 1;
            let ([version_flags, prologue, nodes, frame_register], codes) =
                unwind.split_first_chunk::<4>()?;
            let nodes = usize::from(*nodes);
            if *version_flags != UNWIND_VERSION_NO_FLAGS
                || *frame_register != 0
                || *prologue == 0
                || usize::from(*prologue) >= bytes.len()
                || nodes == 0
                || codes.len() != nodes.next_multiple_of(2) * size_of::<u16>()
            {
                return None;
            }
        }
        let code = decode_machine_code(bytes, records)?;
        if (program.relocations && (code.relocations.is_empty() || unwind.is_empty()))
            || code
                .relocations
                .iter()
                .any(|entry| entry.kind != Reloc::X86CallPCRel4)
        {
            return None;
        }
        Some(WindowsCode {
            code,
            unwind: (!unwind.is_empty()).then(|| unwind.to_vec()),
        })
    };
    emit(primary).or_else(|| fallback.and_then(emit))
}

/// Packs an AAPCS64 function with ordinary scalar parameters.
pub fn aarch64_leaf_words(function: &Function) -> Option<Vec<u64>> {
    let (program, _) = aarch64_program(function, None, false, false)?;
    (!program.relocations).then_some(program.words)
}

fn aarch64_program(
    function: &Function,
    declarations: Option<&ModuleDeclarations>,
    allow_hardware_faults: bool,
    canonicalize_nans: bool,
) -> Option<(IntegerProgram, Option<IntegerProgram>)> {
    let signature = &function.signature;
    if !matches!(
        signature.call_conv,
        CallConv::SystemV | CallConv::AppleAarch64 | CallConv::Tail
    ) {
        return None;
    }
    integer_program(
        function,
        allow_hardware_faults,
        canonicalize_nans,
        declarations,
        signature.call_conv,
        true,
        None,
    )
}

struct IntegerProgram {
    words: Vec<u64>,
    relocations: bool,
}

fn integer_program(
    function: &Function,
    allow_hardware_faults: bool,
    canonicalize_nans: bool,
    declarations: Option<&ModuleDeclarations>,
    call_conv: CallConv,
    aarch64: bool,
    x86_isa: Option<&dyn TargetIsa>,
) -> Option<(IntegerProgram, Option<IntegerProgram>)> {
    let signature = &function.signature;
    if signature.call_conv != call_conv || !function.dynamic_stack_slots.is_empty() {
        return None;
    }
    let tail = call_conv == CallConv::Tail;
    let aarch64_entry = if aarch64 {
        let layout = aarch64_arguments(&signature.params, call_conv)?;
        if tail
            && layout.locations.iter().any(|arg| {
                matches!(
                    arg,
                    Aarch64Argument::Stack(_) | Aarch64Argument::StackPair(_)
                )
            })
        {
            return None;
        }
        Some(layout)
    } else {
        None
    };
    let arity = if let Some(layout) = &aarch64_entry {
        layout.gp
    } else if call_conv == CallConv::WindowsFastcall {
        signature.params.len()
    } else {
        x86_argument_cells(&signature.params)?.0
    };
    let float_return = returns_float(&signature.returns);
    let return_cells = if float_return {
        1
    } else {
        integer_return_cells(&signature.returns)?
    };
    let block = function.layout.blocks().next()?;
    if tail
        && ((!aarch64 && arity > GP_REGISTER_ARGUMENTS)
            || function.layout.blocks().nth(1).is_some())
    {
        return None;
    }

    let mut blocks = IntegerBlocks {
        function,
        arity,
        return_cells,
        float_return,
        words: vec![0; HEADER_WORDS],
        relocations: false,
        branches: 0,
        emitted_blocks: SecondaryMap::new(),
        duplicated_instructions: 0,
        needs_cfg: false,
        allow_hardware_faults,
        canonicalize_nans,
        aarch64,
        x86_isa,
        aarch64_entry,
        declarations,
    };
    for slot in function.sized_stack_slots.values() {
        if slot.kind != StackSlotKind::ExplicitSlot {
            return None;
        }
        let alignment = 1u64.checked_shl(u32::from(slot.align_shift))?;
        blocks.append(IntegerOp::StackSlot, [u64::from(slot.size), alignment, 0]);
    }
    blocks.words[ARGUMENT_COUNT] = arity as u64;
    let prefix = blocks.words.len();
    let mut values = entry_values(&mut blocks, block)?;
    if tail {
        let terminator = blocks.emit_block(block, &mut values)?;
        let InstructionData::Call {
            opcode: Opcode::ReturnCall,
            func_ref,
            ..
        } = function.dfg.insts[terminator]
        else {
            return None;
        };
        blocks.direct_call(terminator, &function.dfg.ext_funcs[func_ref], &mut values)?;
        blocks.words[RETURN_VALUE] = NO_VALUE;
        return Some((
            IntegerProgram {
                words: blocks.words,
                relocations: true,
            },
            None,
        ));
    }
    match blocks.path_exit(block, values, SecondaryMap::new()) {
        Some(PathExit::Return(low, high)) => {
            blocks.words[RETURN_VALUE] = low;
            if let Some(high) = high {
                blocks.append(IntegerOp::ReturnPair, [high, 0, 0]);
            } else if float_return {
                blocks.append(IntegerOp::FloatReturn, [0; 3]);
            }
        }
        Some(PathExit::Trap) => {
            if float_return {
                return None;
            }
            blocks.words[RETURN_VALUE] = NO_VALUE;
            blocks.append(IntegerOp::Trap, [return_cells as u64, 0, 0]);
        }
        None if blocks.needs_cfg => {
            blocks.words.truncate(prefix);
            blocks.words[RETURN_VALUE] = NO_VALUE;
            blocks.relocations = false;
            blocks.labeled_cfg(block)?;
            return Some((
                IntegerProgram {
                    words: blocks.words,
                    relocations: blocks.relocations,
                },
                None,
            ));
        }
        None => return None,
    }
    let linear = IntegerProgram {
        words: std::mem::take(&mut blocks.words),
        relocations: blocks.relocations,
    };
    if blocks.duplicated_instructions >= MIN_DUPLICATED_INSTRUCTIONS_FOR_CFG {
        blocks.words.extend_from_slice(&linear.words[..prefix]);
        blocks.words[RETURN_VALUE] = NO_VALUE;
        blocks.relocations = false;
        if blocks.labeled_cfg(block).is_some() && blocks.words.len() < linear.words.len() {
            // CFG homes increase frame pressure. Keep the linear form available
            // if the Vesper encoder cannot allocate the new frame.
            let labeled = IntegerProgram {
                words: blocks.words,
                relocations: blocks.relocations,
            };
            return Some((labeled, Some(linear)));
        }
    }
    Some((linear, None))
}

pub(super) struct MachineCode {
    pub bytes: Vec<u8>,
    pub relocations: Vec<ModuleReloc>,
}

fn split_length_prefixed(packet: &[u8]) -> Option<(&[u8], &[u8])> {
    let (length, body) = packet.split_first_chunk::<{ size_of::<u64>() }>()?;
    let length = usize::try_from(u64::from_le_bytes(*length)).ok()?;
    body.split_at_checked(length)
}

fn decode_relocatable(packet: &[u8]) -> Option<MachineCode> {
    let (bytes, records) = split_length_prefixed(packet)?;
    decode_machine_code(bytes, records)
}

fn decode_machine_code(bytes: &[u8], records: &[u8]) -> Option<MachineCode> {
    let (records, remainder) = records.as_chunks::<{ 3 * size_of::<u64>() }>();
    if bytes.is_empty() || !remainder.is_empty() {
        return None;
    }
    let mut relocations = Vec::with_capacity(records.len());
    for record in records {
        let ([offset, kind, target], _) = record.as_chunks::<{ size_of::<u64>() }>() else {
            return None;
        };
        let offset = u32::try_from(u64::from_le_bytes(*offset)).ok()?;
        let target = u32::try_from(u64::from_le_bytes(*target)).ok()?;
        let packet_kind = u64::from_le_bytes(*kind);
        let (kind, name) = match packet_kind {
            PLT_CALL => (
                Reloc::X86CallPLTRel4,
                ModuleRelocTarget::user(FUNCTION_NAMESPACE, target),
            ),
            WIN64_CALL => (
                Reloc::X86CallPCRel4,
                ModuleRelocTarget::user(FUNCTION_NAMESPACE, target),
            ),
            RELATIVE_DATA => (
                Reloc::X86PCRel4,
                ModuleRelocTarget::user(DATA_NAMESPACE, target),
            ),
            GOT_DATA => (
                Reloc::X86GOTPCRel4,
                ModuleRelocTarget::user(DATA_NAMESPACE, target),
            ),
            X86_FUNCTION_GOT => (
                Reloc::X86GOTPCRel4,
                ModuleRelocTarget::user(FUNCTION_NAMESPACE, target),
            ),
            ARM64_CALL => (
                Reloc::Arm64Call,
                ModuleRelocTarget::user(FUNCTION_NAMESPACE, target),
            ),
            ARM64_GOT_PAGE => (
                Reloc::Aarch64AdrGotPage21,
                ModuleRelocTarget::user(DATA_NAMESPACE, target),
            ),
            ARM64_GOT_LOAD => (
                Reloc::Aarch64Ld64GotLo12Nc,
                ModuleRelocTarget::user(DATA_NAMESPACE, target),
            ),
            ARM64_FUNCTION_GOT_PAGE => (
                Reloc::Aarch64AdrGotPage21,
                ModuleRelocTarget::user(FUNCTION_NAMESPACE, target),
            ),
            ARM64_FUNCTION_GOT_LOAD => (
                Reloc::Aarch64Ld64GotLo12Nc,
                ModuleRelocTarget::user(FUNCTION_NAMESPACE, target),
            ),
            PLT_LIBCALL | ARM64_LIBCALL => {
                let call = match target {
                    MEMMOVE_LIBCALL => LibCall::Memmove,
                    MEMCPY_LIBCALL => LibCall::Memcpy,
                    _ => return None,
                };
                let kind = if packet_kind == ARM64_LIBCALL {
                    Reloc::Arm64Call
                } else {
                    Reloc::X86CallPLTRel4
                };
                (kind, ModuleRelocTarget::LibCall(call))
            }
            _ => return None,
        };
        if (offset as usize).checked_add(REL32_BYTES as usize)? > bytes.len() {
            return None;
        }
        relocations.push(ModuleReloc {
            offset,
            kind,
            name,
            addend: match kind {
                Reloc::X86CallPCRel4
                | Reloc::X86CallPLTRel4
                | Reloc::X86PCRel4
                | Reloc::X86GOTPCRel4 => -REL32_BYTES,
                _ => 0,
            },
        });
    }
    Some(MachineCode {
        bytes: bytes.to_vec(),
        relocations,
    })
}

fn encode_integer_block(
    isa: &dyn TargetIsa,
    function: &Function,
    declarations: Option<&ModuleDeclarations>,
) -> Option<MachineCode> {
    use target_lexicon::{Aarch64Architecture, Architecture, BinaryFormat, OperatingSystem};

    if isa.flags().preserve_frame_pointers() {
        return None;
    }
    let triple = isa.triple();
    if triple.architecture == Architecture::Aarch64(Aarch64Architecture::Aarch64)
        && matches!(
            (triple.operating_system, triple.binary_format),
            (OperatingSystem::Linux, BinaryFormat::Elf)
                | (
                    OperatingSystem::Darwin(_) | OperatingSystem::MacOSX(_),
                    BinaryFormat::Macho
                )
        )
    {
        const AARCH64_ENTRY_EXIT_FLAGS: [&str; 2] = ["use_bti", "sign_return_address"];
        let encode = AARCH64_ENCODER.get()?;
        if (function.signature.call_conv != isa.default_call_conv()
            && function.signature.call_conv != CallConv::Tail)
            || isa.flags().enable_pinned_reg()
            || isa.isa_flags().iter().any(|flag| {
                AARCH64_ENTRY_EXIT_FLAGS.contains(&flag.name) && flag.as_bool() == Some(true)
            })
        {
            return None;
        }
        // Hosted objects use CPU memory faults directly, with no trap table.
        let (primary, fallback) = aarch64_program(
            function,
            declarations,
            true,
            isa.flags().enable_nan_canonicalization(),
        )?;
        let emit = |program: IntegerProgram| decode_relocatable(&encode(&program.words)?);
        return emit(primary).or_else(|| fallback.and_then(emit));
    }
    if triple.architecture != Architecture::X86_64
        || triple.operating_system != OperatingSystem::Linux
    {
        return None;
    }
    let encode = ENCODER.get()?;
    let call_conv = function.signature.call_conv;
    if !matches!(call_conv, CallConv::SystemV | CallConv::Tail) {
        return None;
    }
    // Ordinary native ELF objects use CPU memory faults directly. They carry
    // no Cranelift trap table, unlike JIT and freestanding emission.
    let (primary, fallback) = integer_program(
        function,
        true,
        isa.flags().enable_nan_canonicalization(),
        declarations,
        call_conv,
        false,
        Some(isa),
    )?;
    let emit = |program: IntegerProgram| {
        if program.relocations {
            let packet = RELOCATION_ENCODER.get()?(&program.words)?;
            decode_relocatable(&packet)
        } else {
            Some(MachineCode {
                bytes: encode(&program.words)?,
                relocations: Vec::new(),
            })
        }
    };
    emit(primary).or_else(|| fallback.and_then(emit))
}

/// Defines one already-lowered function. JIT and instrumented/observed
/// compilation pass `try_machine = false` and keep their existing path.
pub(super) fn define(
    module: &mut impl cranelift_module::Module,
    id: cranelift_module::FuncId,
    context: &mut cranelift_codegen::Context,
    try_machine: bool,
) -> Result<(), String> {
    let definition = if try_machine
        && let Some(code) =
            encode_integer_block(module.isa(), &context.func, Some(module.declarations()))
    {
        let alignment = u64::from(module.isa().function_alignment().minimum);
        module.define_function_bytes(id, alignment, &code.bytes, &code.relocations)
    } else {
        module.define_function(id, context)
    };
    definition.map_err(|error| error.to_string())
}
