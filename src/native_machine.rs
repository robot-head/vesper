//! Function definition swap for the experimental Vesper machine backend.
use cranelift_codegen::{
    entity::SecondaryMap,
    ir::{AbiParam, Function, InstructionData, Opcode, types},
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
const INSTRUCTION_WORDS: usize = 3;
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

fn append(words: &mut Vec<u64>, arity: usize, op: IntegerOp, left: u64, right: u64) -> u64 {
    let value = arity + (words.len() - HEADER_WORDS) / INSTRUCTION_WORDS;
    words.extend([op as u64, left, right]);
    value as u64
}

/// Encodes one System V i64 block for the Vesper backend. The header holds
/// the argument count and the return value ID. The remaining words are
/// (opcode, left, right) records. Arguments come first in the value space,
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
        if function.dfg.value_type(value) != types::I64 {
            return None;
        }
        let input = |value| values[function.dfg.resolve_aliases(value)];
        let (op, left, right) = match function.dfg.insts[inst] {
            InstructionData::UnaryImm {
                opcode: Opcode::Iconst,
                imm,
            } => (IntegerOp::Constant, imm.bits() as u64, 0),
            InstructionData::Unary { opcode, arg } => {
                let op = match opcode {
                    Opcode::Ineg => IntegerOp::Neg,
                    Opcode::Bnot => IntegerOp::Not,
                    _ => return None,
                };
                (op, input(arg)?, 0)
            }
            InstructionData::Binary {
                opcode,
                args: [left, right],
            } => (binary_op(opcode)?, input(left)?, input(right)?),
            InstructionData::BinaryImm64 { opcode, arg, imm } => {
                let left = input(arg)?;
                let bits = imm.bits() as u64;
                match opcode {
                    Opcode::IshlImm => (IntegerOp::ShiftLeft, left, bits),
                    Opcode::UshrImm => (IntegerOp::ShiftRightLogical, left, bits),
                    Opcode::SshrImm => (IntegerOp::ShiftRightArithmetic, left, bits),
                    _ => {
                        let op = binary_op(opcode)?;
                        let right = append(&mut words, arity, IntegerOp::Constant, bits, 0);
                        (op, left, right)
                    }
                }
            }
            _ => return None,
        };
        values[value] = Some(append(&mut words, arity, op, left, right));
    }
    words[RETURN_VALUE] = values[function.dfg.resolve_aliases(result)]?;
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
