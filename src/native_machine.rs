//! Function definition swap for the experimental Vesper machine backend.
use cranelift_codegen::{
    entity::SecondaryMap,
    ir::{AbiParam, Function, InstructionData, Opcode, Type, condcodes::IntCC, types},
    isa::CallConv,
};
use std::sync::OnceLock;

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
    ReduceByte = 15,
    ZeroExtendByte = 16,
    SignExtendByte = 17,
}

type Encoder = fn(&[u64]) -> Option<Vec<u8>>;
static ENCODER: OnceLock<Encoder> = OnceLock::new();

/// Install the x86-64 integer encoder. A declined request uses Cranelift.
pub fn install_integer_encoder(encode: Encoder) -> Result<(), Encoder> {
    ENCODER.set(encode)
}

const ARGUMENT_COUNT: usize = 0;
const RETURN_VALUE: usize = 1;
const HEADER_WORDS: usize = 2;
const INSTRUCTION_WORDS: usize = 4;
const GP_ARGUMENTS: usize = 6;

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

fn compare_op(ty: Type) -> Option<IntegerOp> {
    match ty {
        types::I64 => Some(IntegerOp::Compare),
        types::I8 => Some(IntegerOp::CompareByte),
        _ => None,
    }
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
    if ty == types::I8 {
        u64::from(bits as u8)
    } else {
        bits as u64
    }
}

fn append(words: &mut Vec<u64>, arity: usize, op: IntegerOp, operands: [u64; 3]) -> u64 {
    let value = arity + (words.len() - HEADER_WORDS) / INSTRUCTION_WORDS;
    words.push(op as u64);
    words.extend(operands);
    value as u64
}

/// Encodes one System V i64 block for the Vesper backend. The header holds
/// the argument count and the return value ID. The remaining words are
/// (opcode, left, right, extra) records. The extra word is the condition
/// code for comparisons or the false value ID for selects. Arguments come first in the value space,
/// followed by one value per instruction. Operand IDs refer to prior values.
pub fn integer_block_words(function: &Function) -> Option<Vec<u64>> {
    let signature = &function.signature;
    let arity = signature.params.len();
    if signature.call_conv != CallConv::SystemV
        || arity > GP_ARGUMENTS
        || signature.returns != [AbiParam::new(types::I64)]
        || signature
            .params
            .iter()
            .any(|param| *param != AbiParam::new(types::I64))
        || !function.sized_stack_slots.is_empty()
        || !function.dynamic_stack_slots.is_empty()
    {
        return None;
    }

    let mut blocks = function.layout.blocks();
    let block = blocks.next()?;
    if blocks.next().is_some() {
        return None;
    }

    let params = function.dfg.block_params(block);
    if params.len() != arity
        || params
            .iter()
            .any(|&param| function.dfg.value_type(param) != types::I64)
    {
        return None;
    }
    let mut values = SecondaryMap::new();
    for (index, &param) in params.iter().enumerate() {
        values[param] = Some(index as u64);
    }

    let mut instructions = function.layout.block_insts(block);
    let ret = instructions.next_back()?;
    if function.dfg.insts[ret].opcode() != Opcode::Return {
        return None;
    }
    let [result] = *function.dfg.inst_args(ret) else {
        return None;
    };

    let mut words = vec![0; HEADER_WORDS];
    words[ARGUMENT_COUNT] = arity as u64;
    for inst in instructions {
        let [value] = *function.dfg.inst_results(inst) else {
            return None;
        };
        let output = function.dfg.value_type(value);
        let input = |value| values[function.dfg.resolve_aliases(value)];
        let ty = |value| function.dfg.value_type(function.dfg.resolve_aliases(value));
        let (op, operands) = match function.dfg.insts[inst] {
            InstructionData::UnaryImm {
                opcode: Opcode::Iconst,
                imm,
            } if matches!(output, types::I64 | types::I8) => (
                IntegerOp::Constant,
                [integer_bits(imm.bits(), output), 0, 0],
            ),
            InstructionData::Unary { opcode, arg } => {
                let op = match (opcode, ty(arg), output) {
                    (Opcode::Ineg, types::I64, types::I64) => IntegerOp::Neg,
                    (Opcode::Bnot, types::I64, types::I64) => IntegerOp::Not,
                    (Opcode::Ireduce, types::I64, types::I8) => IntegerOp::ReduceByte,
                    (Opcode::Uextend, types::I8, types::I64) => IntegerOp::ZeroExtendByte,
                    (Opcode::Sextend, types::I8, types::I64) => IntegerOp::SignExtendByte,
                    _ => return None,
                };
                (op, [input(arg)?, 0, 0])
            }
            InstructionData::IntCompare {
                opcode: Opcode::Icmp,
                cond,
                args: [left, right],
            } if output == types::I8 && ty(left) == ty(right) => (
                compare_op(ty(left))?,
                [input(left)?, input(right)?, condition(cond)],
            ),
            InstructionData::IntCompareImm {
                opcode: Opcode::IcmpImm,
                cond,
                arg,
                imm,
            } if output == types::I8 => {
                let op = compare_op(ty(arg))?;
                let left = input(arg)?;
                let bits = integer_bits(imm.bits(), ty(arg));
                let right = append(&mut words, arity, IntegerOp::Constant, [bits, 0, 0]);
                (op, [left, right, condition(cond)])
            }
            InstructionData::Ternary {
                opcode: Opcode::Select,
                args: [cond, yes, no],
            } if matches!(ty(cond), types::I8 | types::I64)
                && matches!(output, types::I8 | types::I64)
                && ty(yes) == output
                && ty(no) == output =>
            {
                (IntegerOp::Select, [input(cond)?, input(yes)?, input(no)?])
            }
            InstructionData::Binary {
                opcode,
                args: [left, right],
            } if output == types::I64
                || output == types::I8
                    && matches!(opcode, Opcode::Band | Opcode::Bor | Opcode::Bxor) =>
            {
                (binary_op(opcode)?, [input(left)?, input(right)?, 0])
            }
            InstructionData::BinaryImm64 { opcode, arg, imm }
                if output == types::I64
                    || output == types::I8
                        && matches!(opcode, Opcode::BandImm | Opcode::BorImm | Opcode::BxorImm) =>
            {
                let left = input(arg)?;
                let bits = integer_bits(imm.bits(), output);
                match opcode {
                    Opcode::IshlImm => (IntegerOp::ShiftLeft, [left, bits, 0]),
                    Opcode::UshrImm => (IntegerOp::ShiftRightLogical, [left, bits, 0]),
                    Opcode::SshrImm => (IntegerOp::ShiftRightArithmetic, [left, bits, 0]),
                    _ => {
                        let op = binary_op(opcode)?;
                        let right = append(&mut words, arity, IntegerOp::Constant, [bits, 0, 0]);
                        (op, [left, right, 0])
                    }
                }
            }
            _ => return None,
        };
        values[value] = Some(append(&mut words, arity, op, operands));
    }
    let result = function.dfg.resolve_aliases(result);
    if function.dfg.value_type(result) != types::I64 {
        return None;
    }
    words[RETURN_VALUE] = values[result]?;
    Some(words)
}

fn encode_integer_block(
    isa: &dyn cranelift_codegen::isa::TargetIsa,
    function: &Function,
) -> Option<Vec<u8>> {
    let encode = ENCODER.get()?;
    let triple = isa.triple();
    if triple.architecture != target_lexicon::Architecture::X86_64
        || triple.operating_system != target_lexicon::OperatingSystem::Linux
        || isa.flags().preserve_frame_pointers()
    {
        return None;
    }
    let words = integer_block_words(function)?;
    encode(&words)
}

/// Defines one already-lowered function. JIT and instrumented/observed
/// compilation pass `try_machine = false` and keep their existing path.
pub(super) fn define(
    module: &mut impl cranelift_module::Module,
    id: cranelift_module::FuncId,
    context: &mut cranelift_codegen::Context,
    try_machine: bool,
) -> Result<(), String> {
    let definition =
        if try_machine && let Some(bytes) = encode_integer_block(module.isa(), &context.func) {
            let alignment = u64::from(module.isa().function_alignment().minimum);
            module.define_function_bytes(id, alignment, &bytes, &[])
        } else {
            module.define_function(id, context)
        };
    definition.map_err(|error| error.to_string())
}
