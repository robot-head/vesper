//! Function definition swap for the experimental Vesper machine backend.
use cranelift_codegen::{
    ir::{AbiParam, Function, InstructionData, Opcode, types},
    isa::CallConv,
};
use std::sync::OnceLock;

/// Opcodes at the hybrid boundary. The Vesper backend owns instruction
/// selection for these operations.
#[repr(u64)]
#[derive(Clone, Copy)]
pub enum LeafOp {
    ReturnConstant = 0,
    ReturnArgument = 1,
    AddArguments = 2,
    SubArguments = 3,
    MulArguments = 4,
    AndArguments = 5,
    OrArguments = 6,
    XorArguments = 7,
}

type Encoder = fn(LeafOp, u64, u64) -> Option<Vec<u8>>;
static ENCODER: OnceLock<Encoder> = OnceLock::new();

/// Install the x86-64 leaf encoder. A declined request uses Cranelift.
pub fn install_leaf_encoder(encode: Encoder) -> Result<(), Encoder> {
    ENCODER.set(encode)
}

/// The first row handles frameless System V i64 functions with one block
/// and one return. Decline anything else before defining the symbol.
fn request(function: &Function) -> Option<(LeafOp, u64, u64)> {
    const GP_ARGUMENTS: usize = 6;
    let signature = &function.signature;
    if signature.call_conv != CallConv::SystemV
        || signature.params.len() > GP_ARGUMENTS
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

    let argument = |value| {
        function
            .dfg
            .block_params(block)
            .iter()
            .position(|&param| param == value)
            .map(|index| index as u64)
    };

    let mut instructions = function.layout.block_insts(block);
    let ret = instructions.next_back()?;
    if function.dfg.insts[ret].opcode() != Opcode::Return {
        return None;
    }
    let [value] = *function.dfg.inst_args(ret) else {
        return None;
    };
    let Some(producer) = instructions.next() else {
        return Some((LeafOp::ReturnArgument, argument(value)?, 0));
    };
    if instructions.next().is_some() || function.dfg.inst_results(producer) != [value] {
        return None;
    }

    let op = match function.dfg.insts[producer] {
        InstructionData::UnaryImm {
            opcode: Opcode::Iconst,
            imm,
        } => {
            return Some((LeafOp::ReturnConstant, imm.bits() as u64, 0));
        }
        InstructionData::Binary { opcode, args } => (opcode, args),
        _ => return None,
    };
    let (opcode, [left, right]) = op;
    let leaf = match opcode {
        Opcode::Iadd => LeafOp::AddArguments,
        Opcode::Isub => LeafOp::SubArguments,
        Opcode::Imul => LeafOp::MulArguments,
        Opcode::Band => LeafOp::AndArguments,
        Opcode::Bor => LeafOp::OrArguments,
        Opcode::Bxor => LeafOp::XorArguments,
        _ => return None,
    };
    Some((leaf, argument(left)?, argument(right)?))
}

fn encode_leaf(
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
    let (op, left, right) = request(function)?;
    encode(op, left, right)
}

/// Defines one already-lowered function. JIT and instrumented/observed
/// compilation pass `try_machine = false` and keep their existing path.
pub(super) fn define(
    module: &mut impl cranelift_module::Module,
    id: cranelift_module::FuncId,
    context: &mut cranelift_codegen::Context,
    try_machine: bool,
) -> Result<(), String> {
    let definition = if try_machine && let Some(bytes) = encode_leaf(module.isa(), &context.func) {
        let alignment = u64::from(module.isa().function_alignment().minimum);
        module.define_function_bytes(id, alignment, &bytes, &[])
    } else {
        module.define_function(id, context)
    };
    definition.map_err(|error| error.to_string())
}
