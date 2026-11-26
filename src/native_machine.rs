//! Function definition swap for the experimental Vesper machine backend.
use cranelift_codegen::{
    binemit::Reloc,
    entity::SecondaryMap,
    ir::{
        AbiParam, Block, BlockCall, Endianness, ExternalName, Function, GlobalValue,
        GlobalValueData, Inst, InstructionData, MemFlags, Opcode, StackSlot, StackSlotKind,
        TrapCode, Type, Value, ValueDef, condcodes::IntCC, immediates::Offset32, types,
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
}

type Encoder = fn(&[u64]) -> Option<Vec<u8>>;
static ENCODER: OnceLock<Encoder> = OnceLock::new();
static RELOCATION_ENCODER: OnceLock<Encoder> = OnceLock::new();

/// Install the x86-64 integer encoder. A declined request uses Cranelift.
pub fn install_integer_encoder(encode: Encoder) -> Result<(), Encoder> {
    ENCODER.set(encode)
}

/// Installs the encoder for functions that can reference external symbols.
pub fn install_relocation_encoder(encode: Encoder) -> Result<(), Encoder> {
    RELOCATION_ENCODER.set(encode)
}

const ARGUMENT_COUNT: usize = 0;
const RETURN_VALUE: usize = 1;
const HEADER_WORDS: usize = 2;
const INSTRUCTION_WORDS: usize = 4;
const NO_VALUE: u64 = u64::MAX;
const TRAP_VALUE: u64 = NO_VALUE - 1;
const GP_REGISTER_ARGUMENTS: usize = 6;
const STACK_PROBE_INTERVAL: usize = 4096;
const CFG_HOME_BYTES: u64 = size_of::<u64>() as u64;
const MAX_GP_ARGUMENTS: usize = GP_REGISTER_ARGUMENTS + STACK_PROBE_INTERVAL / size_of::<u64>() - 1;
const GP_RESULTS: usize = 2;
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

fn integer_argument_cells(params: &[AbiParam]) -> Option<usize> {
    let mut cells = 0;
    for param in params {
        if *param == AbiParam::new(types::I64) {
            cells += 1;
        } else if *param == AbiParam::new(types::I128)
            && cells + WIDE_INTEGER_CELLS <= GP_REGISTER_ARGUMENTS
        {
            // SysV passes a wide argument on the stack if both registers
            // are unavailable. That layout requires aligned stack slots.
            cells += WIDE_INTEGER_CELLS;
        } else {
            return None;
        }
    }
    (cells <= MAX_GP_ARGUMENTS).then_some(cells)
}

fn integer_return_cells(returns: &[AbiParam]) -> Option<usize> {
    if returns == [AbiParam::new(types::I128)] {
        return Some(WIDE_INTEGER_CELLS);
    }
    (returns.len() <= GP_RESULTS
        && returns
            .iter()
            .all(|param| *param == AbiParam::new(types::I64)))
    .then_some(returns.len())
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

fn entry_values(function: &Function, block: Block) -> Option<Values> {
    let params = function.dfg.block_params(block);
    if params.len() != function.signature.params.len() {
        return None;
    }
    let mut values = Values::default();
    let mut next = 0;
    for (&value, param) in params.iter().zip(&function.signature.params) {
        let ty = function.dfg.value_type(value);
        if ty != param.value_type {
            return None;
        }
        values.low[value] = Some(next);
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
        _ => None,
    }
}

struct IntegerBlocks<'a> {
    function: &'a Function,
    arity: usize,
    return_cells: usize,
    words: Vec<u64>,
    relocations: bool,
    branches: usize,
    emitted_blocks: SecondaryMap<Block, bool>,
    duplicated_instructions: usize,
    needs_cfg: bool,
    allow_hardware_faults: bool,
    declarations: Option<&'a ModuleDeclarations>,
}

impl IntegerBlocks<'_> {
    fn supports_memory_access(&self, flags: MemFlags) -> bool {
        flags.notrap()
            || (self.allow_hardware_faults
                && flags.trap_code() == Some(TrapCode::HEAP_OUT_OF_BOUNDS))
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
                if (ty != types::I128 && integer_width(ty).is_none())
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
        let ExternalName::User(name) = callee.name else {
            return None;
        };
        let name = self.function.params.user_named_funcs().get(name)?;
        let signature = &dfg.signatures[callee.signature];
        let args = dfg.inst_args(inst);
        if name.namespace != FUNCTION_NAMESPACE
            || signature.call_conv != CallConv::SystemV
            || signature.params.len() != args.len()
        {
            return None;
        }
        let argument_cells = integer_argument_cells(&signature.params)?;
        let return_cells = integer_return_cells(&signature.returns)?;
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
            1 => IntegerOp::Call,
            GP_RESULTS => IntegerOp::CallPair,
            _ => return None,
        };
        for (&arg, param) in args.iter().zip(&signature.params) {
            let arg = dfg.resolve_aliases(arg);
            let ty = dfg.value_type(arg);
            if ty != param.value_type {
                return None;
            }
            self.append(IntegerOp::CallArgument, [values.low[arg]?, 0, 0]);
            if ty == types::I128 {
                self.append(IntegerOp::CallArgument, [values.high[arg]?, 0, 0]);
            }
        }
        self.relocations = true;
        let id = self.append(op, [u64::from(name.index), argument_cells as u64, 0]);
        if let Some(&value) = results.first() {
            values.low[value] = Some(id);
        }
        if return_cells == GP_RESULTS {
            let id = self.append(IntegerOp::CallHigh, [0; 3]);
            match *results {
                [wide] => values.high[wide] = Some(id),
                [_, high] => values.low[high] = Some(id),
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

    // A block's arguments are already mapped by its incoming edge.
    fn emit_block(&mut self, block: Block, values: &mut Values) -> Option<Inst> {
        let function = self.function;
        let mut instructions = function.layout.block_insts(block);
        let terminator = instructions.next_back()?;
        for inst in instructions {
            let input = |value| values.low[function.dfg.resolve_aliases(value)];
            let ty = |value| function.dfg.value_type(function.dfg.resolve_aliases(value));
            if let InstructionData::StackStore {
                opcode: Opcode::StackStore,
                arg,
                stack_slot,
                offset,
            } = function.dfg.insts[inst]
            {
                let source = ty(arg);
                let op = stack_store_op(source)?;
                let [slot, offset] = self.stack_location(stack_slot, offset, source.bytes())?;
                self.append(op, [input(arg)?, slot, offset]);
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
                let op = store_op(opcode, ty(source))?;
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
            let output = function.dfg.value_type(value);
            let width = integer_width(output)?;
            let (op, operands) = match function.dfg.insts[inst] {
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
                        _ if !self.allow_hardware_faults => return None,
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
            return if self.allow_hardware_faults
                && self.function.dfg.insts[terminator].opcode() == Opcode::Trap
            {
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
            if arg == param {
                continue;
            }
            self.store_cfg_value(ty, arg, values, slot, homes[param]?)?;
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
                if labels[target].is_none() {
                    labels[target] = Some(order.len() as u64);
                    order.push(target);
                }
            }
            index += 1;
        }

        let mut homes: SecondaryMap<Value, Option<u64>> = SecondaryMap::new();
        let mut home_bytes = 0;
        let mut incoming: SecondaryMap<Block, BTreeSet<Value>> = SecondaryMap::new();
        let mut home = |value| {
            let ty = dfg.value_type(value);
            let cells = if ty == types::I128 {
                WIDE_INTEGER_CELLS as u64
            } else {
                integer_width(ty)?;
                1
            };
            if homes[value].is_none() {
                homes[value] = Some(home_bytes);
                home_bytes += cells * CFG_HOME_BYTES;
            }
            Some(())
        };
        for &block in &order {
            if block != entry {
                for &param in dfg.block_params(block) {
                    home(param)?;
                }
            }
            let mut use_value = |operand| {
                let operand = dfg.resolve_aliases(operand);
                let (owner, param) = match dfg.value_def(operand) {
                    ValueDef::Result(inst, _) => (layout.inst_block(inst)?, false),
                    ValueDef::Param(block, _) => (block, true),
                    ValueDef::Union(_, _) => return None,
                };
                if owner != block || (param && block != entry) {
                    home(operand)?;
                    incoming[block].insert(operand);
                }
                Some(())
            };
            for inst in layout.block_insts(block) {
                for arg in dfg.inst_values(inst) {
                    use_value(arg)?;
                }
            }
        }
        if home_bytes > STACK_PROBE_INTERVAL as u64 {
            return None;
        }
        let slot = function.sized_stack_slots.len() as u64;
        if home_bytes != 0 {
            self.append(IntegerOp::StackSlot, [home_bytes, CFG_HOME_BYTES, 0]);
        }
        let results = self.return_cells as u64;
        let mut next_edge = order.len() as u64;
        for block in order {
            let home_slot = if block == entry && home_bytes != 0 {
                slot + 1
            } else {
                0
            };
            self.append(IntegerOp::BlockLabel, [labels[block]?, home_slot, results]);
            let mut values = if block == entry {
                entry_values(function, block)?
            } else {
                Values::default()
            };
            for &value in &incoming[block] {
                let ty = dfg.value_type(value);
                let offset = homes[value]?;
                let width = if ty == types::I128 {
                    INTEGER_CELL_BITS
                } else {
                    integer_width(ty)?
                };
                values.low[value] = Some(self.append(IntegerOp::StackLoad, [slot, offset, width]));
                if ty == types::I128 {
                    values.high[value] = Some(
                        self.append(IntegerOp::StackLoad, [slot, offset + CFG_HOME_BYTES, width]),
                    );
                }
            }
            let end = self.emit_block(block, &mut values)?;
            let definitions = dfg
                .block_params(block)
                .iter()
                .copied()
                .filter(|_| block == entry)
                .chain(
                    layout
                        .block_insts(block)
                        .flat_map(|inst| dfg.inst_results(inst).iter().copied()),
                );
            for value in definitions {
                if let Some(offset) = homes[value] {
                    self.store_cfg_value(dfg.value_type(value), value, &values, slot, offset)?;
                }
            }
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
                    let (low, high) =
                        if self.allow_hardware_faults && dfg.insts[end].opcode() == Opcode::Trap {
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

/// Encodes supported System V i64 paths into a value stream for the Vesper backend.
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
/// Each call is preceded by its CallArgument records, in argument order.
/// The call's left operand is the function ID and its right operand is the
/// argument count. Argument records leave unreferenced slots.
/// CallPair writes its low result and the immediately following CallHigh's
/// slot holds its high result.
/// Checked arithmetic writes its integer result, followed by OverflowFlag
/// for the boolean result of the same instruction.
/// AddCarry and SubBorrow consume the immediately preceding flag as their
/// third operand, with two i64 inputs and an i64 result.
/// SymbolRelative and SymbolGot read the data symbol ID from the left operand.
/// A labeled function starts with BlockLabel after the stack declarations
/// and returns through BlockReturn records. BranchLabel skips the taken edge
/// transfers and its jump to the matching EdgeLabel. Cross-block operands
/// reload from frame homes, and JumpLabel targets a non-entry BlockLabel.
/// BranchTable reads an i32 index, case count, and default label. Its following
/// BranchTableTarget records provide one label per index, in order.
/// The entry label's right word identifies the private home slot plus one,
/// or zero when the function needs no cross-block storage.
pub fn integer_block_words(function: &Function) -> Option<Vec<u64>> {
    integer_program(function, false, None).map(|(primary, _)| primary.words)
}

struct IntegerProgram {
    words: Vec<u64>,
    relocations: bool,
}

fn integer_program(
    function: &Function,
    allow_hardware_faults: bool,
    declarations: Option<&ModuleDeclarations>,
) -> Option<(IntegerProgram, Option<IntegerProgram>)> {
    let signature = &function.signature;
    if signature.call_conv != CallConv::SystemV || !function.dynamic_stack_slots.is_empty() {
        return None;
    }
    let arity = integer_argument_cells(&signature.params)?;
    let return_cells = integer_return_cells(&signature.returns)?;
    let block = function.layout.blocks().next()?;
    let values = entry_values(function, block)?;

    let mut blocks = IntegerBlocks {
        function,
        arity,
        return_cells,
        words: vec![0; HEADER_WORDS],
        relocations: false,
        branches: 0,
        emitted_blocks: SecondaryMap::new(),
        duplicated_instructions: 0,
        needs_cfg: false,
        allow_hardware_faults,
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
    match blocks.path_exit(block, values, SecondaryMap::new()) {
        Some(PathExit::Return(low, high)) => {
            blocks.words[RETURN_VALUE] = low;
            if let Some(high) = high {
                blocks.append(IntegerOp::ReturnPair, [high, 0, 0]);
            }
        }
        Some(PathExit::Trap) => {
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
        let (kind, namespace) = match u64::from_le_bytes(*kind) {
            PLT_CALL => (Reloc::X86CallPLTRel4, FUNCTION_NAMESPACE),
            RELATIVE_DATA => (Reloc::X86PCRel4, DATA_NAMESPACE),
            GOT_DATA => (Reloc::X86GOTPCRel4, DATA_NAMESPACE),
            _ => return None,
        };
        if (offset as usize).checked_add(REL32_BYTES as usize)? > bytes.len() {
            return None;
        }
        relocations.push(ModuleReloc {
            offset,
            kind,
            name: ModuleRelocTarget::user(namespace, target),
            addend: -REL32_BYTES,
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
    let encode = ENCODER.get()?;
    let triple = isa.triple();
    if triple.architecture != target_lexicon::Architecture::X86_64
        || triple.operating_system != target_lexicon::OperatingSystem::Linux
        || isa.flags().preserve_frame_pointers()
    {
        return None;
    }
    // Ordinary native ELF objects use CPU memory faults directly. They carry
    // no Cranelift trap table, unlike JIT and freestanding emission.
    let (primary, fallback) = integer_program(function, true, declarations)?;
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
