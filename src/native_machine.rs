//! Function definition swap for the experimental Vesper machine backend.
use cranelift_codegen::{
    binemit::Reloc,
    entity::SecondaryMap,
    ir::{
        AbiParam, AtomicRmwOp, Block, BlockCall, Endianness, ExternalName, Function, GlobalValue,
        GlobalValueData, Inst, InstructionData, LibCall, MemFlags, Opcode, StackSlot,
        StackSlotKind, TrapCode, Type, Value, ValueDef,
        condcodes::{FloatCC, IntCC},
        immediates::{Ieee32, Ieee64, Offset32},
        types,
    },
    isa::CallConv,
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
}

type Encoder = fn(&[u64]) -> Option<Vec<u8>>;
static ENCODER: OnceLock<Encoder> = OnceLock::new();
static RELOCATION_ENCODER: OnceLock<Encoder> = OnceLock::new();
static AARCH64_ENCODER: OnceLock<Encoder> = OnceLock::new();

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

const ARGUMENT_COUNT: usize = 0;
const RETURN_VALUE: usize = 1;
const HEADER_WORDS: usize = 2;
const INSTRUCTION_WORDS: usize = 4;
const NO_VALUE: u64 = u64::MAX;
const TRAP_VALUE: u64 = NO_VALUE - 1;
const GP_REGISTER_ARGUMENTS: usize = 6;
const AARCH64_GP_ARGUMENTS: usize = 8;
const XMM_REGISTER_ARGUMENTS: usize = 8;
const STACK_PROBE_INTERVAL: usize = 4096;
const CFG_HOME_BYTES: u64 = size_of::<u64>() as u64;
const MAX_GP_ARGUMENTS: usize = GP_REGISTER_ARGUMENTS + STACK_PROBE_INTERVAL / size_of::<u64>() - 1;
const GP_RESULTS: usize = 2;
const FLOAT_RETURN_LAYOUT: u64 = GP_RESULTS as u64 + 1;
const WIDE_INTEGER_CELLS: usize = 2;
const INTEGER_CELL_BITS: u64 = u64::BITS as u64;
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

fn argument_cells(params: &[AbiParam]) -> Option<(usize, usize)> {
    let mut cells = 0;
    let mut floats = 0;
    for param in params {
        if integer_width(param.value_type).is_some() && *param == AbiParam::new(param.value_type) {
            cells += 1;
        } else if *param == AbiParam::new(types::I128)
            && cells + WIDE_INTEGER_CELLS <= GP_REGISTER_ARGUMENTS
        {
            // SysV passes a wide argument on the stack if both registers
            // are unavailable. That layout requires aligned stack slots.
            cells += WIDE_INTEGER_CELLS;
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
    for (&value, param) in params.iter().zip(&function.signature.params) {
        let ty = function.dfg.value_type(value);
        if ty != param.value_type {
            return None;
        }
        if let Some(width) = float_width(ty) {
            values.low[value] = Some(blocks.append(IntegerOp::FloatArgument, [xmm, 0, width]));
            xmm += 1;
            continue;
        }
        values.low[value] = Some(blocks.normalize_abi_integer(next, ty));
        next += 1;
        if ty == types::I128 {
            values.high[value] = Some(next);
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
    declarations: Option<&'a ModuleDeclarations>,
}

impl IntegerBlocks<'_> {
    fn normalize_abi_integer(&mut self, cell: u64, ty: Type) -> u64 {
        // Plain narrow SysV slots only define the low bits. The machine
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
        let args = dfg.inst_args(inst);
        if signature.call_conv != self.function.signature.call_conv
            || signature.params.len() != args.len()
            || (self.aarch64 && !aarch64_register_signature(&signature.params, &signature.returns))
        {
            return None;
        }
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
        let (gp_arguments, float_arguments) = argument_cells(&signature.params)?;
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
            1 if float_return => IntegerOp::CallFloat,
            1 => IntegerOp::Call,
            GP_RESULTS => IntegerOp::CallPair,
            _ => return None,
        };
        let mut xmm = [0; XMM_REGISTER_ARGUMENTS];
        let mut xmm_count = 0;
        for (&arg, param) in args.iter().zip(&signature.params) {
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
        self.relocations = true;
        let id = self.append(
            op,
            [
                target,
                (gp_arguments + float_arguments) as u64,
                float_arguments as u64,
            ],
        );
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
        let [lo, hi] = match dfg.insts[inst] {
            InstructionData::Binary {
                opcode: Opcode::Iconcat,
                args: [a, b],
            } if output == types::I128 && ty(a) == types::I64 && ty(b) == types::I64 => {
                [low(a)?, low(b)?]
            }
            InstructionData::Binary {
                opcode,
                args: [a, b],
            } if output == types::I128 => {
                let [a_low, a_high] = pair(a)?;
                let [b_low, b_high] = pair(b)?;
                match opcode {
                    Opcode::Iadd | Opcode::Isub => {
                        let (head, tail) = if opcode == Opcode::Iadd {
                            (IntegerOp::UnsignedAddOverflow, IntegerOp::AddCarry)
                        } else {
                            (IntegerOp::UnsignedSubOverflow, IntegerOp::SubBorrow)
                        };
                        let low = self.append(head, [a_low, b_low, INTEGER_CELL_BITS]);
                        let carry = self.append(IntegerOp::OverflowFlag, [0; 3]);
                        let high = self.append(tail, [a_high, b_high, carry]);
                        [low, high]
                    }
                    Opcode::Band | Opcode::Bor | Opcode::Bxor => {
                        let op = binary_op(opcode)?;
                        [
                            self.append(op, [a_low, b_low, INTEGER_CELL_BITS]),
                            self.append(op, [a_high, b_high, INTEGER_CELL_BITS]),
                        ]
                    }
                    _ => return None,
                }
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
                opcode: Opcode::Bswap,
                arg,
            } if output == types::I128 => {
                let [low, high] = pair(arg)?;
                [
                    self.append(IntegerOp::ByteSwap, [high, 0, INTEGER_CELL_BITS]),
                    self.append(IntegerOp::ByteSwap, [low, 0, INTEGER_CELL_BITS]),
                ]
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
                    _ => return None,
                };
                (op, [input(left)?, input(right)?, float_width(output)?])
            }
            InstructionData::Unary { opcode, arg } if ty(arg) == output => {
                let op = match opcode {
                    Opcode::Sqrt => IntegerOp::FloatSqrt,
                    Opcode::Fneg => IntegerOp::FloatNeg,
                    Opcode::Fabs => IntegerOp::FloatAbs,
                    _ => return None,
                };
                (op, [input(arg)?, 0, float_width(output)?])
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

    // A block's arguments are already mapped by its incoming edge.
    fn emit_block(&mut self, block: Block, values: &mut Values) -> Option<Inst> {
        let function = self.function;
        let mut instructions = function.layout.block_insts(block);
        let terminator = instructions.next_back()?;
        for inst in instructions {
            let input = |value| values.low[function.dfg.resolve_aliases(value)];
            let ty = |value| function.dfg.value_type(function.dfg.resolve_aliases(value));
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
                // A memory exchange implements a sequentially consistent
                // store on x86. Its previous value is dead.
                self.append(
                    IntegerOp::AtomicExchange,
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
                self.append(stack_store_op(source)?, [input(arg)?, slot, offset]);
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
                InstructionData::LoadNoOffset {
                    opcode: Opcode::AtomicLoad,
                    flags,
                    arg,
                } if ty(arg) == types::I64 && self.supports_atomic_access(flags) => {
                    // x86 orders a plain load strongly enough for a CLIF atomic load.
                    (IntegerOp::Load, [input(arg)?, 0, width])
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
                        _ => return None,
                    };
                    (op, [input(address)?, input(source)?, width])
                }
                InstructionData::Unary {
                    opcode: Opcode::Bswap,
                    arg,
                } if ty(arg) == output && output != types::I8 => {
                    (IntegerOp::ByteSwap, [input(arg)?, 0, width])
                }
                InstructionData::UnaryGlobalValue {
                    opcode: Opcode::GlobalValue | Opcode::SymbolValue,
                    global_value,
                } if output == types::I64 => {
                    values.low[value] = Some(self.symbol_address(global_value, width)?);
                    continue;
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
                    opcode: Opcode::Select,
                    args: [cond, yes, no],
                } if integer_width(ty(cond)).is_some() && ty(yes) == output && ty(no) == output => {
                    (IntegerOp::Select, [input(cond)?, input(yes)?, input(no)?])
                }
                InstructionData::Binary {
                    opcode: opcode @ (Opcode::Ishl | Opcode::Ushr | Opcode::Sshr),
                    args: [left, right],
                } if matches!(output, types::I32 | types::I64)
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
                    opcode:
                        opcode @ (Opcode::Sdiv
                        | Opcode::Udiv
                        | Opcode::Srem
                        | Opcode::Urem
                        | Opcode::Smulhi
                        | Opcode::Umulhi),
                    args: [left, right],
                } if matches!(output, types::I32 | types::I64)
                    && ty(left) == output
                    && ty(right) == output =>
                {
                    let op = match opcode {
                        Opcode::Smulhi => IntegerOp::SignedMulHigh,
                        Opcode::Umulhi => IntegerOp::UnsignedMulHigh,
                        _ if !self.allow_hardware_faults && !self.aarch64 => return None,
                        Opcode::Sdiv => IntegerOp::SignedDiv,
                        Opcode::Udiv => IntegerOp::UnsignedDiv,
                        Opcode::Srem => IntegerOp::SignedRem,
                        Opcode::Urem => IntegerOp::UnsignedRem,
                        _ => unreachable!(),
                    };
                    (op, [input(left)?, input(right)?, width])
                }
                InstructionData::Binary {
                    opcode,
                    args: [left, right],
                } if ty(left) == output && ty(right) == output => {
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
/// Each call is preceded by its CallArgument records, integer cells first,
/// then XMM cells. The call's left operand is the function ID, its right
/// operand is the total cell count, and its extra operand is the XMM count.
/// Argument records leave unreferenced slots. CallFloat returns through XMM0.
/// Call targets above the u32 symbol-ID range identify approved runtime libcalls.
/// CallPair writes its low result and the immediately following CallHigh's
/// slot holds its high result.
/// Checked arithmetic writes its integer result, followed by OverflowFlag
/// for the boolean result of the same instruction.
/// AddCarry and SubBorrow consume the immediately preceding flag as their
/// third operand, with two i64 inputs and an i64 result.
/// Atomic read-modify-write records take an address, operand, and width.
/// They remain live for their memory effect even when the old value is unused.
/// After the stack declarations, FloatArgument binds successive SysV XMM
/// arguments. These use their own register bank and occupy normal value IDs.
/// A trailing FloatReturn selects XMM0 for the single return value.
/// SymbolRelative and SymbolGot read the data symbol ID from the left operand.
/// A labeled function starts with BlockLabel after the stack declarations
/// and returns through BlockReturn records. The label's extra word describes
/// the integer return arity or a single XMM result. Entry FloatArgument records
/// follow the first label. BranchLabel skips the taken edge
/// transfers and its jump to the matching EdgeLabel. Cross-block operands
/// reload from frame homes, and JumpLabel targets a non-entry BlockLabel.
/// BranchTable reads an i32 index, case count, and default label. Its following
/// BranchTableTarget records provide one label per index, in order.
/// The entry label's right word identifies the private home slot plus one,
/// or zero when the function needs no cross-block storage.
pub fn integer_block_words(function: &Function) -> Option<Vec<u64>> {
    integer_program(function, false, false, None, CallConv::SystemV, false)
        .map(|(primary, _)| primary.words)
}

/// Packs an AAPCS64 function with ordinary register integer parameters.
pub fn aarch64_leaf_words(function: &Function) -> Option<Vec<u64>> {
    let (program, _) = aarch64_program(function, None, false)?;
    (!program.relocations).then_some(program.words)
}

fn aarch64_register_signature(params: &[AbiParam], returns: &[AbiParam]) -> bool {
    params.len() <= AARCH64_GP_ARGUMENTS
        && returns.len() <= GP_RESULTS
        && params
            .iter()
            .chain(returns)
            .all(|param| *param == AbiParam::new(types::I64))
}

fn aarch64_program(
    function: &Function,
    declarations: Option<&ModuleDeclarations>,
    allow_hardware_faults: bool,
) -> Option<(IntegerProgram, Option<IntegerProgram>)> {
    let signature = &function.signature;
    if !matches!(
        signature.call_conv,
        CallConv::SystemV | CallConv::AppleAarch64
    ) || !aarch64_register_signature(&signature.params, &signature.returns)
    {
        return None;
    }
    integer_program(
        function,
        allow_hardware_faults,
        false,
        declarations,
        signature.call_conv,
        true,
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
) -> Option<(IntegerProgram, Option<IntegerProgram>)> {
    let signature = &function.signature;
    if signature.call_conv != call_conv || !function.dynamic_stack_slots.is_empty() {
        return None;
    }
    let (arity, _) = argument_cells(&signature.params)?;
    let float_return = returns_float(&signature.returns);
    let return_cells = if float_return {
        1
    } else {
        integer_return_cells(&signature.returns)?
    };
    let block = function.layout.blocks().next()?;

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
    let values = entry_values(&mut blocks, block)?;
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

struct MachineCode {
    bytes: Vec<u8>,
    relocations: Vec<ModuleReloc>,
}

fn decode_relocatable(packet: &[u8]) -> Option<MachineCode> {
    let (length, body) = packet.split_first_chunk::<{ size_of::<u64>() }>()?;
    let length = usize::try_from(u64::from_le_bytes(*length)).ok()?;
    let (bytes, records) = body.split_at_checked(length)?;
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
            RELATIVE_DATA => (
                Reloc::X86PCRel4,
                ModuleRelocTarget::user(DATA_NAMESPACE, target),
            ),
            GOT_DATA => (
                Reloc::X86GOTPCRel4,
                ModuleRelocTarget::user(DATA_NAMESPACE, target),
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
                Reloc::X86CallPLTRel4 | Reloc::X86PCRel4 | Reloc::X86GOTPCRel4 => -REL32_BYTES,
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
    isa: &dyn cranelift_codegen::isa::TargetIsa,
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
        if function.signature.call_conv != isa.default_call_conv()
            || isa.flags().enable_pinned_reg()
            || isa.isa_flags().iter().any(|flag| {
                AARCH64_ENTRY_EXIT_FLAGS.contains(&flag.name) && flag.as_bool() == Some(true)
            })
        {
            return None;
        }
        // Hosted objects use CPU memory faults directly, with no trap table.
        let (primary, fallback) = aarch64_program(function, declarations, true)?;
        let emit = |program: IntegerProgram| decode_relocatable(&encode(&program.words)?);
        return emit(primary).or_else(|| fallback.and_then(emit));
    }
    if triple.architecture != Architecture::X86_64
        || triple.operating_system != OperatingSystem::Linux
    {
        return None;
    }
    let encode = ENCODER.get()?;
    // Ordinary native ELF objects use CPU memory faults directly. They carry
    // no Cranelift trap table, unlike JIT and freestanding emission.
    let (primary, fallback) = integer_program(
        function,
        true,
        isa.flags().enable_nan_canonicalization(),
        declarations,
        CallConv::SystemV,
        false,
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
