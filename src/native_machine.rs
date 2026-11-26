//! Function definition swap for the experimental Vesper machine backend.
use cranelift_codegen::{
    entity::SecondaryMap,
    ir::{
        AbiParam, Block, BlockCall, Endianness, Function, Inst, InstructionData, MemFlags, Opcode,
        Type, Value, condcodes::IntCC, types,
    },
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

fn integer_width(ty: Type) -> Option<u64> {
    matches!(ty, types::I8 | types::I16 | types::I32 | types::I64).then(|| u64::from(ty.bits()))
}

fn compare_op(ty: Type) -> Option<IntegerOp> {
    match ty {
        types::I8 => Some(IntegerOp::CompareByte),
        types::I16 => Some(IntegerOp::CompareWord),
        types::I32 => Some(IntegerOp::CompareDword),
        types::I64 => Some(IntegerOp::Compare),
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
    match ty {
        types::I8 => u64::from(bits as u8),
        types::I16 => u64::from(bits as u16),
        types::I32 => u64::from(bits as u32),
        _ => bits as u64,
    }
}

type Values = SecondaryMap<Value, Option<u64>>;

fn supports_memory(flags: MemFlags) -> bool {
    flags.notrap() && !flags.checked() && flags.endianness(Endianness::Little) == Endianness::Little
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

struct IntegerBlocks<'a> {
    function: &'a Function,
    arity: usize,
    words: Vec<u64>,
}

impl IntegerBlocks<'_> {
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
                if integer_width(ty).is_none() || self.value_type(param) != ty {
                    return None;
                }
                values[dfg.resolve_aliases(arg)]
            })
            .collect();
        for (&param, arg) in params.iter().zip(arguments?) {
            values[param] = Some(arg);
        }
        Some(target)
    }

    // A block's arguments are already mapped by its incoming edge.
    fn emit_block(&mut self, block: Block, values: &mut Values) -> Option<Inst> {
        let function = self.function;
        let mut instructions = function.layout.block_insts(block);
        let terminator = instructions.next_back()?;
        for inst in instructions {
            let input = |value| values[function.dfg.resolve_aliases(value)];
            let ty = |value| function.dfg.value_type(function.dfg.resolve_aliases(value));
            if let InstructionData::Store {
                opcode,
                flags,
                args: [source, base],
                offset,
            } = function.dfg.insts[inst]
            {
                if !supports_memory(flags) || ty(base) != types::I64 {
                    return None;
                }
                let op = store_op(opcode, ty(source))?;
                self.append(
                    op,
                    [
                        input(base)?,
                        input(source)?,
                        u64::from(i32::from(offset) as u32),
                    ],
                );
                continue;
            }
            let [value] = *function.dfg.inst_results(inst) else {
                return None;
            };
            let output = function.dfg.value_type(value);
            let width = integer_width(output)?;
            let (op, operands) = match function.dfg.insts[inst] {
                InstructionData::UnaryImm {
                    opcode: Opcode::Iconst,
                    imm,
                } => (
                    IntegerOp::Constant,
                    [integer_bits(imm.bits(), output), 0, 0],
                ),
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
                } if supports_memory(flags) && ty(arg) == types::I64 => {
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
                    let load = [
                        input(arg)?,
                        u64::from(i32::from(offset) as u32),
                        access_width,
                    ];
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
                    let right = self.append(IntegerOp::Constant, [bits, 0, 0]);
                    (op, [left, right, condition(cond)])
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
                    opcode,
                    args: [left, right],
                } if ty(left) == output && ty(right) == output => {
                    (binary_op(opcode)?, [input(left)?, input(right)?, width])
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
                            let right = self.append(IntegerOp::Constant, [bits, 0, 0]);
                            (op, [left, right, width])
                        }
                    }
                }
                _ => return None,
            };
            values[value] = Some(self.append(op, operands));
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
            if visited[block] || !self.function.layout.is_block_inserted(block) {
                return None;
            }
            visited[block] = true;
            let terminator = self.emit_block(block, values)?;
            match self.function.dfg.insts[terminator] {
                InstructionData::Jump {
                    opcode: Opcode::Jump,
                    destination,
                } => block = self.edge(destination, values)?,
                _ => return Some(terminator),
            }
        }
    }

    fn returned(&self, terminator: Inst, values: &Values) -> Option<u64> {
        let dfg = &self.function.dfg;
        if dfg.insts[terminator].opcode() != Opcode::Return {
            return None;
        }
        let [value] = *dfg.inst_args(terminator) else {
            return None;
        };
        let value = dfg.resolve_aliases(value);
        if dfg.value_type(value) != types::I64 {
            return None;
        }
        values[value]
    }

    // Each arm follows its jump chain to a return. If the paths rejoin, the
    // shared suffix is emitted once per arm with that arm's argument bindings.
    // Each traversal includes the prefix in its visited set to reject cycles.
    fn returned_from_branch(
        &mut self,
        condition: Value,
        arms: [BlockCall; 2],
        values: Values,
        mut visited: SecondaryMap<Block, bool>,
    ) -> Option<u64> {
        integer_width(self.value_type(condition))?;
        let condition = values[self.function.dfg.resolve_aliases(condition)]?;
        self.append(IntegerOp::BranchZero, [condition, 0, 0]);
        let [yes_edge, no_edge] = arms;
        let mut yes_values = values.clone();
        let mut yes_visited = visited.clone();
        let yes_block = self.edge(yes_edge, &mut yes_values)?;
        let yes_return = self.follow_jumps(yes_block, &mut yes_values, &mut yes_visited)?;
        let yes = self.returned(yes_return, &yes_values)?;
        self.append(IntegerOp::EarlyReturn, [yes, 0, 0]);

        let mut no_values = values;
        let no_block = self.edge(no_edge, &mut no_values)?;
        let no_return = self.follow_jumps(no_block, &mut no_values, &mut visited)?;
        self.returned(no_return, &no_values)
    }
}

/// Encodes supported System V i64 paths into a value stream for the Vesper backend.
/// The header holds
/// the argument count and the return value ID. The remaining words are
/// (opcode, left, right, extra) records. The extra word is the result width
/// for operations such as loads and arithmetic, the condition code for comparisons, or
/// the false value ID for selects. Arguments come first in the value space,
/// followed by one slot per instruction. A store uses its extra word for
/// displacement and leaves an unreferenced slot. Operand IDs refer to prior
/// value-producing instructions or arguments.
/// A single BranchZero may skip to the instruction after EarlyReturn. These
/// records also leave unreferenced slots, and the false arm uses the header's
/// return value. Neither arm may read values defined in the other.
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

    let block = function.layout.blocks().next()?;

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

    let mut blocks = IntegerBlocks {
        function,
        arity,
        words: vec![0; HEADER_WORDS],
    };
    let mut visited = SecondaryMap::new();
    let terminator = blocks.follow_jumps(block, &mut values, &mut visited)?;
    let result = match function.dfg.insts[terminator] {
        InstructionData::Brif {
            opcode: Opcode::Brif,
            arg,
            blocks: arms,
        } => blocks.returned_from_branch(arg, arms, values, visited)?,
        _ => blocks.returned(terminator, &values)?,
    };
    blocks.words[ARGUMENT_COUNT] = arity as u64;
    blocks.words[RETURN_VALUE] = result;
    Some(blocks.words)
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
