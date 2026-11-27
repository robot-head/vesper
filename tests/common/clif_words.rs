//! Cranelift IR as words for the Vesper CLIF passes (#1829), and the edit
//! scripts they answer with.
//!
//! `@vesper/clif` (packages/vesper-clif) reads a function in the words of
//! [`encode`] into tables, and a pass there (`@vesper/native-vectorize`)
//! changes those tables while it writes each change as an edit script.
//! [`apply`] replays the script on the Rust function with the same cursor
//! and builder calls the Rust pass makes, in the same order, so the entity
//! numbers come out the same and the two functions print the same.
//!
//! # Function words
//!
//! All little-endian `u64`s:
//!
//! - `VERSION`, flags ([`H_SIMD`]: the vectorizer's flag; [`H_NO_OUTLINE`]),
//!   `n_values`, `n_insts`, `n_blocks`, the pointer type (0 when not
//!   given), `n_returns`, the signature's return types;
//! - the sized stack slots: `n`, then per slot `kind` ([`S_EXPLICIT`] or
//!   [`S_OTHER`]), `size`, `align_shift`;
//! - per value: `kind | type << 8`, `a`, `b` where kind is [`V_RESULT`]
//!   (inst `a`, result `b`), [`V_PARAM`] (block `a`, param `b`), [`V_ALIAS`]
//!   (of `a`, already resolved) or [`V_UNION`];
//! - the layout: `n`, then per block `block`, `cold`, `n_params`, params,
//!   `n_insts`, insts;
//! - per instruction (all of the dfg's, in order): `opcode`, `effects`,
//!   `cond`, `imm`, `offset`, `mem`, `n_args`, args, `n_results`, results,
//!   `n_dests`, then per destination `block`, `n`, block args ([`NONE`] for
//!   an argument that is not a value).
//!
//! Opcodes and types are this file's own codes ([`OPCODES`], [`TYPES`]),
//! mirrored by packages/vesper-clif/src/codes.ves: 0 is "another one".
//!
//! # Edit scripts
//!
//! Each op names what it creates, which [`apply`] checks against what
//! Cranelift hands out:
//!
//! - [`E_BLOCK`] `block`: `make_block`;
//! - [`E_PARAM`] `block type value`: `append_block_param`;
//! - [`E_PLACE`] `block before`: `layout.insert_block`;
//! - [`E_RETARGET`] `inst from to`: every destination of `inst` at `from`
//!   goes to `to`;
//! - [`E_CLONE`] `block inst new n values...`: `clone_inst`, its values
//!   (`inst_values` order) replaced, put at the bottom of `block`;
//! - [`E_INS`] `block opcode type cond imm offset mem n args... n_dests
//!   (block n args...)... new`: one builder call at the bottom of `block`;
//! - [`E_REMOVE_INST`] `inst`: `layout.remove_inst`;
//! - [`E_REMOVE_BLOCK`] `block`: `layout.remove_block`;
//! - [`E_JUMP`] `inst block n args...`: `dfg.replace(inst).jump`;
//! - [`E_FCONST`] `inst type bits`: `dfg.replace(inst).f64const` or
//!   `f32const`;
//! - [`E_ARGS`] `inst n values...`: the instruction's fixed arguments;
//! - [`E_SLOT_SIZE`] `slot size`: a sized slot's size (the CLIF clean-up
//!   steps of #2362);
//! - [`E_REMOVE_PARAM`] `value`: `dfg.remove_block_param`;
//! - [`E_ALIAS`] `dest src`: `dfg.change_to_alias`;
//! - [`E_MOVE_INST`] `inst block`: `layout.remove_inst`, then
//!   `layout.append_inst` at the bottom of `block`;
//! - [`E_REMOVE_DEST_ARG`] `inst k a`: argument `a` of destination `k` of
//!   `inst` removed;
//! - [`E_INS_BEFORE`] `inst opcode ...`: [`E_INS`] with a cursor at `inst`
//!   in place of the block;
//! - [`E_SET_UNARY`] `inst opcode arg`: the instruction becomes the
//!   `Unary` opcode on `arg`, its results kept (the extend folds);
//! - [`E_SET_BINARY`] `inst opcode a b`: the instruction becomes the
//!   `Binary` opcode on `a` and `b`, its results kept;
//! - [`E_NEW_SLOT`] `size align_shift new`: `create_sized_stack_slot` of an
//!   explicit slot (the slot steps);
//! - [`E_ZERO_BEFORE`] `inst type new`: a zero of `type` just before `inst`,
//!   as `promote_slots` makes it (`vconst` of a new 16-byte zero constant,
//!   `f32const`, `f64const` or `iconst`).
//! - [`E_APPEND_DEST_ARG`] `inst k value`: `append_argument` on destination
//!   `k` of `inst` (the branch steps of #2423);
//! - [`E_SET_DEST`] `inst k block n args...`: destination `k` of `inst`
//!   becomes `dfg.block_call(block, args)`;
//! - [`E_BRIF`] `inst cond then n args... else n args...`:
//!   `dfg.replace(inst).brif`;
//! - [`E_DETACH_RESULTS`] `inst`: `dfg.detach_inst_results`;
//! - [`E_ICONST`] `inst type imm`: `dfg.replace(inst).iconst`;
//! - [`E_REPLACE_IMM`] `inst opcode arg imm`: `dfg.replace(inst)` as the
//!   `BinaryImm64` opcode (`iadd_imm` and the like);
//! - [`E_REPLACE_ICMP_IMM`] `inst cond arg imm`: `dfg.replace(inst).icmp_imm`;
//! - [`E_MEM_ADDR`] `inst addr offset`: a load's or store's address and
//!   offset.
#![allow(dead_code)]

use cranelift_codegen::cursor::{Cursor as _, FuncCursor};
use cranelift_codegen::ir::{
    self, Block, BlockArg, Inst, InstBuilder, InstructionData, MemFlags, Opcode, Type, Value,
    condcodes::IntCC, types,
};

pub const VERSION: u64 = 3;
pub const NONE: u64 = u64::MAX;

/// A sized slot's kind.
pub const S_EXPLICIT: u64 = 0;
pub const S_OTHER: u64 = 1;

pub const V_RESULT: u64 = 0;
pub const V_PARAM: u64 = 1;
pub const V_ALIAS: u64 = 2;
pub const V_UNION: u64 = 3;

pub const E_END: u64 = 0;
pub const E_BLOCK: u64 = 1;
pub const E_PARAM: u64 = 2;
pub const E_PLACE: u64 = 3;
pub const E_RETARGET: u64 = 4;
pub const E_CLONE: u64 = 5;
pub const E_INS: u64 = 6;
// 7 and 8 are the cold outliner's ([`E_OUTLINE`], [`E_SKIP`]).
pub const E_REMOVE_INST: u64 = 9;
pub const E_REMOVE_BLOCK: u64 = 10;
pub const E_JUMP: u64 = 11;
pub const E_FCONST: u64 = 12;
pub const E_ARGS: u64 = 13;
pub const E_SLOT_SIZE: u64 = 14;
pub const E_REMOVE_PARAM: u64 = 15;
pub const E_ALIAS: u64 = 16;
pub const E_MOVE_INST: u64 = 17;
pub const E_REMOVE_DEST_ARG: u64 = 18;
pub const E_INS_BEFORE: u64 = 19;
pub const E_SET_UNARY: u64 = 20;
pub const E_SET_BINARY: u64 = 21;
pub const E_NEW_SLOT: u64 = 22;
pub const E_ZERO_BEFORE: u64 = 23;
// 64 up: the branch and fold steps of #2423 (20 to 63 are #2362's).
pub const E_APPEND_DEST_ARG: u64 = 64;
pub const E_SET_DEST: u64 = 65;
pub const E_BRIF: u64 = 66;
pub const E_DETACH_RESULTS: u64 = 67;
pub const E_ICONST: u64 = 68;
pub const E_REPLACE_IMM: u64 = 69;
pub const E_REPLACE_ICMP_IMM: u64 = 70;
pub const E_MEM_ADDR: u64 = 71;

/// Memory flag bits of the `mem` word.
pub const M_READONLY: u64 = 1;
pub const M_CAN_MOVE: u64 = 2;
pub const M_ENDIAN: u64 = 4;
pub const M_LITTLE: u64 = 8;
pub const M_NOTRAP: u64 = 16;
pub const M_ALIGNED: u64 = 32;

/// Effect bits of the `effects` word.
pub const F_TERMINATOR: u64 = 1;
pub const F_BRANCH: u64 = 2;
pub const F_CALL: u64 = 4;
pub const F_RETURN: u64 = 8;
pub const F_TRAP: u64 = 16;
pub const F_SIDE_EFFECTS: u64 = 32;
pub const F_LOAD: u64 = 64;
pub const F_STORE: u64 = 128;
/// The instruction's format is `UnaryConst`.
pub const F_UNARY_CONST: u64 = 256;
/// A `UnaryGlobalValue` whose global value is a symbol, or `iadd_imm`s of
/// one (`global_is_pure_symbol` of the cold outliner).
pub const F_PURE_GV: u64 = 512;
/// A `StackLoad`/`StackStore` format: `imm` is the stack slot.
pub const F_STACK_SLOT: u64 = 1024;
/// The format is `Trap`: `imm` is the trap code's byte.
pub const F_TRAP_CODE: u64 = 2048;

/// Function flag bits (the second header word).
pub const H_SIMD: u64 = 1;
/// Dynamic stack slots, exception tables, or a signature parameter or
/// return whose purpose is not `Normal`: the cold outliner leaves it be.
pub const H_NO_OUTLINE: u64 = 2;

/// Opcode codes, 1 up (0 is any other opcode). Append only.
pub const OPCODES: &[Opcode] = &[
    Opcode::Iconst,
    Opcode::Iadd,
    Opcode::IaddImm,
    Opcode::Isub,
    Opcode::IrsubImm,
    Opcode::Imul,
    Opcode::ImulImm,
    Opcode::Ishl,
    Opcode::IshlImm,
    Opcode::Ushr,
    Opcode::UshrImm,
    Opcode::Band,
    Opcode::BandImm,
    Opcode::Bor,
    Opcode::BorImm,
    Opcode::Bxor,
    Opcode::BxorImm,
    Opcode::Icmp,
    Opcode::IcmpImm,
    Opcode::Uextend,
    Opcode::Sextend,
    Opcode::Ireduce,
    Opcode::Select,
    Opcode::Load,
    Opcode::Store,
    Opcode::Uload8,
    Opcode::Sload8,
    Opcode::Istore32,
    Opcode::FcvtFromSint,
    Opcode::Fmul,
    Opcode::Bitcast,
    Opcode::F32const,
    Opcode::Jump,
    Opcode::Brif,
    Opcode::BrTable,
    Opcode::Return,
    Opcode::Splat,
    Opcode::UaddOverflow,
    Opcode::Umulhi,
    Opcode::Bnot,
    Opcode::ScalarToVector,
    Opcode::SwidenLow,
    Opcode::SwidenHigh,
    Opcode::Call,
    Opcode::CallIndirect,
    Opcode::ReturnCall,
    Opcode::ReturnCallIndirect,
    Opcode::TryCall,
    Opcode::TryCallIndirect,
    Opcode::Trap,
    Opcode::Udiv,
    Opcode::Sdiv,
    Opcode::Urem,
    Opcode::Srem,
    Opcode::StackAddr,
    Opcode::StackLoad,
    Opcode::StackStore,
    Opcode::GetFramePointer,
    Opcode::GetStackPointer,
    Opcode::GetReturnAddress,
    Opcode::DynamicStackLoad,
    Opcode::DynamicStackStore,
    Opcode::Nop,
    Opcode::F64const,
    Opcode::F128const,
    Opcode::Vconst,
    Opcode::SymbolValue,
    Opcode::FuncAddr,
    Opcode::Fadd,
    Opcode::Sshr,
    Opcode::SshrImm,
    Opcode::Ineg,
    Opcode::Uload16,
    Opcode::Sload16,
    Opcode::Uload32,
    Opcode::Sload32,
    Opcode::Istore8,
    Opcode::Istore16,
];

/// Type codes, 1 up (0 is any other type).
pub const TYPES: &[Type] = &[
    types::I8,
    types::I16,
    types::I32,
    types::I64,
    types::I128,
    types::F16,
    types::F32,
    types::F64,
    types::F128,
    types::I8X16,
    types::I16X8,
    types::I32X4,
    types::I64X2,
    types::F32X4,
    types::F64X2,
];

const CONDS: &[IntCC] = &[
    IntCC::Equal,
    IntCC::NotEqual,
    IntCC::SignedLessThan,
    IntCC::SignedGreaterThanOrEqual,
    IntCC::SignedGreaterThan,
    IntCC::SignedLessThanOrEqual,
    IntCC::UnsignedLessThan,
    IntCC::UnsignedGreaterThanOrEqual,
    IntCC::UnsignedGreaterThan,
    IntCC::UnsignedLessThanOrEqual,
];

pub fn opcode_code(op: Opcode) -> u64 {
    static TABLE: std::sync::OnceLock<[u8; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut t = [0u8; 256];
        for (i, &o) in OPCODES.iter().enumerate() {
            t[o as usize] = i as u8 + 1;
        }
        t
    });
    table[op as usize] as u64
}

pub fn type_code(t: Type) -> u64 {
    match t {
        types::I8 => 1,
        types::I16 => 2,
        types::I32 => 3,
        types::I64 => 4,
        types::I128 => 5,
        types::F16 => 6,
        types::F32 => 7,
        types::F64 => 8,
        types::F128 => 9,
        types::I8X16 => 10,
        types::I16X8 => 11,
        types::I32X4 => 12,
        types::I64X2 => 13,
        types::F32X4 => 14,
        types::F64X2 => 15,
        _ => 0,
    }
}

fn opcode_of(code: u64) -> Result<Opcode, String> {
    OPCODES
        .get((code as usize).wrapping_sub(1))
        .copied()
        .ok_or_else(|| format!("opcode code {code}"))
}

fn type_of(code: u64) -> Result<Type, String> {
    TYPES
        .get((code as usize).wrapping_sub(1))
        .copied()
        .ok_or_else(|| format!("type code {code}"))
}

fn cond_code(c: IntCC) -> u64 {
    CONDS
        .iter()
        .position(|&x| x == c)
        .map_or(0, |i| i as u64 + 1)
}

fn cond_of(code: u64) -> Result<IntCC, String> {
    CONDS
        .get((code as usize).wrapping_sub(1))
        .copied()
        .ok_or_else(|| format!("condition code {code}"))
}

fn mem_word(f: MemFlags) -> u64 {
    let mut m = 0;
    if f.readonly() {
        m |= M_READONLY;
    }
    if f.can_move() {
        m |= M_CAN_MOVE;
    }
    if let Some(e) = f.explicit_endianness() {
        m |= M_ENDIAN;
        if e == ir::Endianness::Little {
            m |= M_LITTLE;
        }
    }
    if f.notrap() {
        m |= M_NOTRAP;
    }
    if f.aligned() {
        m |= M_ALIGNED;
    }
    m
}

/// The flags of a builder-made access: `MemFlags::new()`, or with an
/// explicit endianness.
fn mem_flags(m: u64) -> Result<MemFlags, String> {
    if m & !(M_ENDIAN | M_LITTLE | M_NOTRAP | M_ALIGNED) != 0 {
        return Err(format!("memory flags {m:#x} on a new instruction"));
    }
    let mut f = MemFlags::new();
    if m & M_NOTRAP != 0 {
        f = f.with_notrap();
    }
    if m & M_ALIGNED != 0 {
        f = f.with_aligned();
    }
    Ok(if m & M_ENDIAN == 0 {
        f
    } else if m & M_LITTLE != 0 {
        f.with_endianness(ir::Endianness::Little)
    } else {
        f.with_endianness(ir::Endianness::Big)
    })
}

fn effects(op: Opcode) -> u64 {
    let mut e = 0;
    if op.is_terminator() {
        e |= F_TERMINATOR;
    }
    if op.is_branch() {
        e |= F_BRANCH;
    }
    if op.is_call() {
        e |= F_CALL;
    }
    if op.is_return() {
        e |= F_RETURN;
    }
    if op.can_trap() {
        e |= F_TRAP;
    }
    if op.other_side_effects() {
        e |= F_SIDE_EFFECTS;
    }
    if op.can_load() {
        e |= F_LOAD;
    }
    if op.can_store() {
        e |= F_STORE;
    }
    e
}

fn pure_symbol(func: &ir::Function, gv: ir::GlobalValue) -> bool {
    match &func.global_values[gv] {
        ir::GlobalValueData::Symbol { .. } => true,
        ir::GlobalValueData::IAddImm { base, .. } => pure_symbol(func, *base),
        _ => false,
    }
}

fn no_outline(func: &ir::Function) -> bool {
    !func.dynamic_stack_slots.is_empty()
        || !func.dfg.exception_tables.is_empty()
        || !func
            .signature
            .params
            .iter()
            .chain(&func.signature.returns)
            .all(|p| matches!(p.purpose, ir::ArgumentPurpose::Normal))
}

/// The words of `func` (see the module comment), as bytes.
pub fn encode(func: &ir::Function, simd: bool) -> Vec<u8> {
    encode_with(func, simd, None)
}

/// [`encode`] with the module's pointer type (the cold outliner's).
pub fn encode_with(func: &ir::Function, simd: bool, ptr: Option<Type>) -> Vec<u8> {
    let dfg = &func.dfg;
    let mut w: Vec<u64> =
        Vec::with_capacity(16 + 3 * dfg.num_values() + 14 * dfg.num_insts() + 6 * dfg.num_blocks());
    w.extend([
        VERSION,
        (simd as u64) | if no_outline(func) { H_NO_OUTLINE } else { 0 },
        dfg.num_values() as u64,
        dfg.num_insts() as u64,
        dfg.num_blocks() as u64,
        ptr.map_or(0, type_code),
        func.signature.returns.len() as u64,
    ]);
    w.extend(
        func.signature
            .returns
            .iter()
            .map(|r| type_code(r.value_type)),
    );
    w.push(func.sized_stack_slots.len() as u64);
    for (_, data) in func.sized_stack_slots.iter() {
        let kind = match data.kind {
            ir::StackSlotKind::ExplicitSlot => S_EXPLICIT,
            _ => S_OTHER,
        };
        w.extend([kind, u64::from(data.size), u64::from(data.align_shift)]);
    }
    for v in dfg.values() {
        let ty = type_code(dfg.value_type(v)) << 8;
        let r = dfg.resolve_aliases(v);
        if r != v {
            w.extend([V_ALIAS | ty, r.as_u32() as u64, 0]);
            continue;
        }
        match dfg.value_def(v) {
            ir::ValueDef::Result(i, n) => w.extend([V_RESULT | ty, i.as_u32() as u64, n as u64]),
            ir::ValueDef::Param(b, n) => w.extend([V_PARAM | ty, b.as_u32() as u64, n as u64]),
            ir::ValueDef::Union(..) => w.extend([V_UNION | ty, 0, 0]),
        }
    }
    let blocks: Vec<Block> = func.layout.blocks().collect();
    w.push(blocks.len() as u64);
    for b in blocks {
        w.push(b.as_u32() as u64);
        w.push(func.layout.is_cold(b) as u64);
        let params = dfg.block_params(b);
        w.push(params.len() as u64);
        w.extend(params.iter().map(|p| p.as_u32() as u64));
        let insts: Vec<Inst> = func.layout.block_insts(b).collect();
        w.push(insts.len() as u64);
        w.extend(insts.iter().map(|i| i.as_u32() as u64));
    }
    for inst in (0..dfg.num_insts()).map(|i| Inst::from_u32(i as u32)) {
        let data = &dfg.insts[inst];
        let op = data.opcode();
        let (mut cond, mut imm, mut offset, mut mem) = (0, 0, 0, 0);
        let mut extra = 0;
        match *data {
            InstructionData::StackLoad {
                stack_slot,
                offset: o,
                ..
            }
            | InstructionData::StackStore {
                stack_slot,
                offset: o,
                ..
            } => {
                extra = F_STACK_SLOT;
                imm = stack_slot.as_u32() as u64;
                offset = i64::from(o) as u64;
            }
            InstructionData::Trap { code, .. } => {
                extra = F_TRAP_CODE;
                imm = code.as_raw().get() as u64;
            }
            InstructionData::UnaryConst { .. } => extra = F_UNARY_CONST,
            InstructionData::UnaryGlobalValue { global_value, .. } => {
                if pure_symbol(func, global_value) {
                    extra = F_PURE_GV;
                }
            }
            InstructionData::IntCompare { cond: c, .. } => cond = cond_code(c),
            InstructionData::IntCompareImm {
                cond: c, imm: i, ..
            } => {
                cond = cond_code(c);
                imm = i.bits() as u64;
            }
            InstructionData::UnaryImm { imm: i, .. }
            | InstructionData::BinaryImm64 { imm: i, .. } => imm = i.bits() as u64,
            InstructionData::UnaryIeee32 { imm: i, .. } => imm = i.bits() as u64,
            InstructionData::UnaryIeee64 { imm: i, .. } => imm = i.bits(),
            InstructionData::BinaryImm8 { imm: i, .. }
            | InstructionData::TernaryImm8 { imm: i, .. } => imm = i as u64,
            InstructionData::Load {
                flags, offset: o, ..
            }
            | InstructionData::Store {
                flags, offset: o, ..
            } => {
                mem = mem_word(flags);
                offset = i64::from(i32::from(o)) as u64;
            }
            InstructionData::LoadNoOffset { flags, .. }
            | InstructionData::StoreNoOffset { flags, .. } => mem = mem_word(flags),
            _ => {}
        }
        w.extend([opcode_code(op), effects(op) | extra, cond, imm, offset, mem]);
        let args = dfg.inst_args(inst);
        w.push(args.len() as u64);
        w.extend(args.iter().map(|a| a.as_u32() as u64));
        let results = dfg.inst_results(inst);
        w.push(results.len() as u64);
        w.extend(results.iter().map(|r| r.as_u32() as u64));
        let dests = data.branch_destination(&dfg.jump_tables, &dfg.exception_tables);
        w.push(dests.len() as u64);
        for c in dests {
            w.push(c.block(&dfg.value_lists).as_u32() as u64);
            let at = w.len();
            w.push(0);
            for a in c.args(&dfg.value_lists) {
                w.push(match a {
                    BlockArg::Value(v) => v.as_u32() as u64,
                    _ => NONE,
                });
            }
            w[at] = (w.len() - at - 1) as u64;
        }
    }
    w.iter().flat_map(|x| x.to_le_bytes()).collect()
}

struct Script<'a> {
    w: &'a [u64],
    at: usize,
}

impl Script<'_> {
    fn next(&mut self) -> Result<u64, String> {
        let x = *self.w.get(self.at).ok_or("edit script ends early")?;
        self.at += 1;
        Ok(x)
    }
    fn value(&mut self) -> Result<Value, String> {
        Ok(Value::from_u32(self.next()? as u32))
    }
    fn block(&mut self) -> Result<Block, String> {
        Ok(Block::from_u32(self.next()? as u32))
    }
    fn values(&mut self) -> Result<Vec<Value>, String> {
        let n = self.next()?;
        (0..n).map(|_| self.value()).collect()
    }
}

fn expect<T: PartialEq + std::fmt::Debug>(what: &str, got: T, want: T) -> Result<(), String> {
    if got == want {
        Ok(())
    } else {
        Err(format!(
            "{what}: Cranelift made {got:?}, the script names {want:?}"
        ))
    }
}

/// Replays the edit script `words` (see the module comment) on `func`.
pub fn apply(func: &mut ir::Function, words: &[u64]) -> Result<(), String> {
    let mut s = Script { w: words, at: 0 };
    loop {
        match s.next()? {
            E_END => return Ok(()),
            E_BLOCK => {
                let want = s.block()?;
                expect("block", func.dfg.make_block(), want)?;
            }
            E_PARAM => {
                let b = s.block()?;
                let ty = type_of(s.next()?)?;
                let want = s.value()?;
                expect("param", func.dfg.append_block_param(b, ty), want)?;
            }
            E_PLACE => {
                let b = s.block()?;
                let before = s.block()?;
                func.layout.insert_block(b, before);
            }
            E_RETARGET => {
                let inst = Inst::from_u32(s.next()? as u32);
                let from = s.block()?;
                let to = s.block()?;
                let ir::DataFlowGraph {
                    insts,
                    jump_tables,
                    exception_tables,
                    value_lists,
                    ..
                } = &mut func.dfg;
                for c in insts[inst].branch_destination_mut(jump_tables, exception_tables) {
                    if c.block(value_lists) == from {
                        c.set_block(to, value_lists);
                    }
                }
            }
            E_CLONE => {
                let b = s.block()?;
                let inst = Inst::from_u32(s.next()? as u32);
                let want = Inst::from_u32(s.next()? as u32);
                let vals = s.values()?;
                let new = func.dfg.clone_inst(inst);
                expect("clone", new, want)?;
                let mut it = vals.into_iter();
                let mut short = false;
                func.dfg.map_inst_values(new, |v| {
                    it.next().unwrap_or_else(|| {
                        short = true;
                        v
                    })
                });
                if short || it.next().is_some() {
                    return Err(format!("clone of {inst}: wrong value count"));
                }
                FuncCursor::new(func).at_bottom(b).insert_inst(new);
            }
            E_INS => {
                let b = s.block()?;
                let made = build(func, Place::Bottom(b), &mut s)?;
                let want = Inst::from_u32(s.next()? as u32);
                expect("instruction", made, want)?;
            }
            E_REMOVE_INST => {
                let inst = Inst::from_u32(s.next()? as u32);
                func.layout.remove_inst(inst);
            }
            E_REMOVE_BLOCK => {
                let b = s.block()?;
                func.layout.remove_block(b);
            }
            E_JUMP => {
                let inst = Inst::from_u32(s.next()? as u32);
                let b = s.block()?;
                let args: Vec<BlockArg> = s.values()?.into_iter().map(BlockArg::Value).collect();
                func.dfg.replace(inst).jump(b, &args);
            }
            E_FCONST => {
                use cranelift_codegen::ir::immediates::{Ieee32, Ieee64};
                let inst = Inst::from_u32(s.next()? as u32);
                let ty = type_of(s.next()?)?;
                let bits = s.next()?;
                if ty == types::F64 {
                    func.dfg.replace(inst).f64const(Ieee64::with_bits(bits));
                } else if ty == types::F32 {
                    func.dfg
                        .replace(inst)
                        .f32const(Ieee32::with_bits(bits as u32));
                } else {
                    return Err(format!("float constant of type {ty}"));
                }
            }
            E_ARGS => {
                let inst = Inst::from_u32(s.next()? as u32);
                let vals = s.values()?;
                let args = func.dfg.inst_args_mut(inst);
                if args.len() != vals.len() {
                    return Err(format!(
                        "{inst}: {} arguments for {}",
                        vals.len(),
                        args.len()
                    ));
                }
                args.copy_from_slice(&vals);
            }
            E_REMOVE_PARAM => {
                let p = s.value()?;
                func.dfg.remove_block_param(p);
            }
            E_ALIAS => {
                let dest = s.value()?;
                let src = s.value()?;
                func.dfg.change_to_alias(dest, src);
            }
            E_MOVE_INST => {
                let inst = Inst::from_u32(s.next()? as u32);
                let b = s.block()?;
                func.layout.remove_inst(inst);
                func.layout.append_inst(inst, b);
            }
            E_REMOVE_DEST_ARG => {
                let inst = Inst::from_u32(s.next()? as u32);
                let k = s.next()? as usize;
                let a = s.next()? as usize;
                let ir::DataFlowGraph {
                    insts,
                    jump_tables,
                    exception_tables,
                    value_lists,
                    ..
                } = &mut func.dfg;
                insts[inst]
                    .branch_destination_mut(jump_tables, exception_tables)
                    .get_mut(k)
                    .ok_or_else(|| format!("{inst}: no destination {k}"))?
                    .remove(a, value_lists);
            }
            E_INS_BEFORE => {
                let before = Inst::from_u32(s.next()? as u32);
                let made = build(func, Place::Before(before), &mut s)?;
                let want = Inst::from_u32(s.next()? as u32);
                expect("instruction", made, want)?;
            }
            E_SET_UNARY => {
                let inst = Inst::from_u32(s.next()? as u32);
                let opcode = opcode_of(s.next()?)?;
                let arg = s.value()?;
                func.dfg.insts[inst] = InstructionData::Unary { opcode, arg };
            }
            E_SET_BINARY => {
                let inst = Inst::from_u32(s.next()? as u32);
                let opcode = opcode_of(s.next()?)?;
                let a = s.value()?;
                let b = s.value()?;
                func.dfg.insts[inst] = InstructionData::Binary {
                    opcode,
                    args: [a, b],
                };
            }
            E_NEW_SLOT => {
                let size = u32::try_from(s.next()?).map_err(|e| e.to_string())?;
                let align = u8::try_from(s.next()?).map_err(|e| e.to_string())?;
                let want = ir::StackSlot::from_u32(s.next()? as u32);
                let made = func.create_sized_stack_slot(ir::StackSlotData::new(
                    ir::StackSlotKind::ExplicitSlot,
                    size,
                    align,
                ));
                expect("slot", made, want)?;
            }
            E_ZERO_BEFORE => {
                let inst = Inst::from_u32(s.next()? as u32);
                let ty = type_of(s.next()?)?;
                let want = Inst::from_u32(s.next()? as u32);
                if ty.is_vector() {
                    let zero = func.dfg.constants.insert(vec![0u8; 16].into());
                    FuncCursor::new(func).at_inst(inst).ins().vconst(ty, zero);
                } else {
                    let mut pos = FuncCursor::new(func).at_inst(inst);
                    match ty {
                        types::F32 => pos.ins().f32const(0.0),
                        types::F64 => pos.ins().f64const(0.0),
                        _ => pos.ins().iconst(ty, 0),
                    };
                }
                expect("zero", made_inst(func, Place::Before(inst))?, want)?;
            }
            E_APPEND_DEST_ARG => {
                let inst = Inst::from_u32(s.next()? as u32);
                let k = s.next()? as usize;
                let v = s.value()?;
                let ir::DataFlowGraph {
                    insts,
                    jump_tables,
                    exception_tables,
                    value_lists,
                    ..
                } = &mut func.dfg;
                insts[inst]
                    .branch_destination_mut(jump_tables, exception_tables)
                    .get_mut(k)
                    .ok_or_else(|| format!("{inst}: no destination {k}"))?
                    .append_argument(v, value_lists);
            }
            E_SET_DEST => {
                let inst = Inst::from_u32(s.next()? as u32);
                let k = s.next()? as usize;
                let b = s.block()?;
                let args = block_args(&mut s)?;
                let new = func.dfg.block_call(b, &args);
                let ir::DataFlowGraph {
                    insts,
                    jump_tables,
                    exception_tables,
                    ..
                } = &mut func.dfg;
                *insts[inst]
                    .branch_destination_mut(jump_tables, exception_tables)
                    .get_mut(k)
                    .ok_or_else(|| format!("{inst}: no destination {k}"))? = new;
            }
            E_BRIF => {
                let inst = Inst::from_u32(s.next()? as u32);
                let cond = s.value()?;
                let t = s.block()?;
                let ta = block_args(&mut s)?;
                let e = s.block()?;
                let ea = block_args(&mut s)?;
                func.dfg.replace(inst).brif(cond, t, &ta, e, &ea);
            }
            E_DETACH_RESULTS => {
                let inst = Inst::from_u32(s.next()? as u32);
                func.dfg.detach_inst_results(inst);
            }
            E_ICONST => {
                let inst = Inst::from_u32(s.next()? as u32);
                let ty = type_of(s.next()?)?;
                let imm = s.next()? as i64;
                func.dfg.replace(inst).iconst(ty, imm);
            }
            E_REPLACE_IMM => {
                let inst = Inst::from_u32(s.next()? as u32);
                let code = s.next()?;
                let arg = s.value()?;
                let imm = s.next()? as i64;
                let op = opcode_of(code)?;
                let r = func.dfg.replace(inst);
                match op {
                    Opcode::IaddImm => r.iadd_imm(arg, imm),
                    Opcode::ImulImm => r.imul_imm(arg, imm),
                    Opcode::BandImm => r.band_imm(arg, imm),
                    Opcode::BorImm => r.bor_imm(arg, imm),
                    Opcode::BxorImm => r.bxor_imm(arg, imm),
                    Opcode::IshlImm => r.ishl_imm(arg, imm),
                    Opcode::UshrImm => r.ushr_imm(arg, imm),
                    Opcode::SshrImm => r.sshr_imm(arg, imm),
                    _ => return Err(format!("no immediate replacement as {op}")),
                };
            }
            E_REPLACE_ICMP_IMM => {
                let inst = Inst::from_u32(s.next()? as u32);
                let cond = cond_of(s.next()?)?;
                let arg = s.value()?;
                let imm = s.next()? as i64;
                func.dfg.replace(inst).icmp_imm(cond, arg, imm);
            }
            E_MEM_ADDR => {
                let inst = Inst::from_u32(s.next()? as u32);
                let addr = s.value()?;
                let off = ir::immediates::Offset32::new(s.next()? as i64 as i32);
                match &mut func.dfg.insts[inst] {
                    InstructionData::Load { arg, offset, .. } => {
                        *arg = addr;
                        *offset = off;
                    }
                    InstructionData::Store { args, offset, .. } => {
                        args[1] = addr;
                        *offset = off;
                    }
                    _ => return Err(format!("{inst}: no address to fold into")),
                }
            }
            E_SLOT_SIZE => {
                let slot = ir::StackSlot::from_u32(s.next()? as u32);
                let size = u32::try_from(s.next()?).map_err(|e| e.to_string())?;
                func.sized_stack_slots[slot].size = size;
            }
            op => return Err(format!("edit op {op}")),
        }
    }
}

/// `n` then `n` values, as block arguments; a [`NONE`] (an argument that is
/// not a value) has no block argument to stand for.
fn block_args(s: &mut Script) -> Result<Vec<BlockArg>, String> {
    let n = s.next()?;
    (0..n)
        .map(|_| match s.next()? {
            v if v as u32 == u32::MAX => Err("a block argument that is not a value".into()),
            v => Ok(BlockArg::Value(Value::from_u32(v as u32))),
        })
        .collect()
}

/// Where [`build`] puts its instruction.
#[derive(Clone, Copy)]
enum Place {
    /// At the bottom of the block.
    Bottom(Block),
    /// Just before the instruction (a cursor `at_inst`).
    Before(Inst),
}

/// One builder call at `place`, as the Rust passes make it.
fn build(func: &mut ir::Function, place: Place, s: &mut Script) -> Result<Inst, String> {
    let code = s.next()?;
    let ty_word = s.next()?;
    let cond_word = s.next()?;
    let imm = s.next()? as i64;
    let offset = s.next()? as i64 as i32;
    let mem = s.next()?;
    let args = s.values()?;
    let n_dests = s.next()?;
    let mut dests: Vec<(Block, Vec<Value>)> = vec![];
    for _ in 0..n_dests {
        let b = s.block()?;
        dests.push((b, s.values()?));
    }
    let op = opcode_of(code)?;
    let arity = |n: usize| -> Result<(), String> {
        if args.len() == n {
            Ok(())
        } else {
            Err(format!("{op}: {} arguments", args.len()))
        }
    };
    let a = |i: usize| args[i];
    let mut pos = match place {
        Place::Bottom(block) => FuncCursor::new(func).at_bottom(block),
        Place::Before(inst) => FuncCursor::new(func).at_inst(inst),
    };
    let ins = pos.ins();
    let made = match op {
        Opcode::StackAddr => {
            arity(0)?;
            let slot = ir::StackSlot::from_u32(u32::try_from(imm).map_err(|e| e.to_string())?);
            ins.stack_addr(type_of(ty_word)?, slot, offset)
        }
        Opcode::Iconst => {
            arity(0)?;
            ins.iconst(type_of(ty_word)?, imm)
        }
        Opcode::IaddImm => {
            arity(1)?;
            ins.iadd_imm(a(0), imm)
        }
        Opcode::ImulImm => {
            arity(1)?;
            ins.imul_imm(a(0), imm)
        }
        Opcode::IcmpImm => {
            arity(1)?;
            ins.icmp_imm(cond_of(cond_word)?, a(0), imm)
        }
        Opcode::Icmp => {
            arity(2)?;
            ins.icmp(cond_of(cond_word)?, a(0), a(1))
        }
        Opcode::Iadd | Opcode::Isub | Opcode::Band | Opcode::Bor | Opcode::Bxor => {
            arity(2)?;
            match op {
                Opcode::Iadd => ins.iadd(a(0), a(1)),
                Opcode::Isub => ins.isub(a(0), a(1)),
                Opcode::Band => ins.band(a(0), a(1)),
                Opcode::Bor => ins.bor(a(0), a(1)),
                _ => ins.bxor(a(0), a(1)),
            }
        }
        Opcode::Umulhi => {
            arity(2)?;
            ins.umulhi(a(0), a(1))
        }
        Opcode::Fmul => {
            arity(2)?;
            ins.fmul(a(0), a(1))
        }
        Opcode::UaddOverflow => {
            arity(2)?;
            ins.uadd_overflow(a(0), a(1)).0
        }
        Opcode::Bnot => {
            arity(1)?;
            ins.bnot(a(0))
        }
        Opcode::Splat => {
            arity(1)?;
            ins.splat(type_of(ty_word)?, a(0))
        }
        Opcode::ScalarToVector => {
            arity(1)?;
            ins.scalar_to_vector(type_of(ty_word)?, a(0))
        }
        Opcode::SwidenLow => {
            arity(1)?;
            ins.swiden_low(a(0))
        }
        Opcode::SwidenHigh => {
            arity(1)?;
            ins.swiden_high(a(0))
        }
        Opcode::FcvtFromSint => {
            arity(1)?;
            ins.fcvt_from_sint(type_of(ty_word)?, a(0))
        }
        Opcode::Bitcast => {
            arity(1)?;
            ins.bitcast(type_of(ty_word)?, mem_flags(mem)?, a(0))
        }
        Opcode::Load => {
            arity(1)?;
            ins.load(type_of(ty_word)?, mem_flags(mem)?, a(0), offset)
        }
        Opcode::Store => {
            arity(2)?;
            ins.store(mem_flags(mem)?, a(0), a(1), offset);
            return made_inst(func, place);
        }
        Opcode::Brif => {
            arity(1)?;
            let [(t, ta), (e, ea)] = dests.as_slice() else {
                return Err("brif: destinations".into());
            };
            let ta: Vec<BlockArg> = ta.iter().map(|&v| BlockArg::Value(v)).collect();
            let ea: Vec<BlockArg> = ea.iter().map(|&v| BlockArg::Value(v)).collect();
            ins.brif(a(0), *t, &ta, *e, &ea);
            return made_inst(func, place);
        }
        Opcode::Jump => {
            arity(0)?;
            let [(b, ba)] = dests.as_slice() else {
                return Err("jump: destinations".into());
            };
            let ba: Vec<BlockArg> = ba.iter().map(|&v| BlockArg::Value(v)).collect();
            ins.jump(*b, &ba);
            return made_inst(func, place);
        }
        _ => return Err(format!("no builder for {op}")),
    };
    let _ = made;
    made_inst(func, place)
}

/// The instruction [`build`] just put at `place`.
fn made_inst(func: &ir::Function, place: Place) -> Result<Inst, String> {
    match place {
        Place::Bottom(block) => last_inst(func, block),
        Place::Before(inst) => func
            .layout
            .prev_inst(inst)
            .ok_or_else(|| "nothing before the instruction after a build".into()),
    }
}

fn last_inst(func: &ir::Function, block: Block) -> Result<Inst, String> {
    func.layout
        .last_inst(block)
        .ok_or_else(|| "empty block after a build".into())
}

/// The cold outliner's edit ops: [`E_OUTLINE`] `idx root owned shared
/// has_return trap live_ins remat shared_slots exclusive_slots ordered`
/// (lists as `n` then entries, `remat` as (value, inst) pairs, `trap` the
/// trap code's byte or 0), or [`E_SKIP`] for a round whose region the
/// analysis refused.
pub const E_OUTLINE: u64 = 7;
pub const E_SKIP: u64 = 8;

/// Replays the cold outliner's script on `func`: each [`E_OUTLINE`] goes
/// through `native_cold_outline::apply_extraction`, which builds the helper
/// and the stub from the port's decisions. Returns the helpers.
pub fn apply_outline<M: cranelift_module::Module>(
    m: &mut M,
    parent_key: &str,
    func: &mut ir::Function,
    words: &[u64],
) -> Result<Vec<(cranelift_module::FuncId, String, ir::Function)>, String> {
    use vesper::native_cold_outline::{Extraction, apply_extraction};
    let mut s = Script { w: words, at: 0 };
    let mut out = vec![];
    let blocks = |s: &mut Script| -> Result<Vec<Block>, String> {
        let n = s.next()?;
        (0..n).map(|_| s.block()).collect()
    };
    let slots = |s: &mut Script| -> Result<Vec<ir::StackSlot>, String> {
        let n = s.next()?;
        (0..n)
            .map(|_| Ok(ir::StackSlot::from_u32(s.next()? as u32)))
            .collect()
    };
    loop {
        match s.next()? {
            E_END => return Ok(out),
            E_SKIP => {}
            E_OUTLINE => {
                let idx = s.next()? as usize;
                let root = s.block()?;
                let owned = blocks(&mut s)?.into_iter().collect();
                let _shared = blocks(&mut s)?;
                let has_return = s.next()? != 0;
                let trap = s.next()?;
                let first_trap = std::num::NonZeroU8::new(trap as u8).map(ir::TrapCode::from_raw);
                let live_ins = s.values()?;
                let n = s.next()?;
                let mut remat = vec![];
                for _ in 0..n {
                    let v = s.value()?;
                    remat.push((v, Inst::from_u32(s.next()? as u32)));
                }
                let shared_slots = slots(&mut s)?;
                let exclusive_slots = slots(&mut s)?;
                let ordered_blocks = blocks(&mut s)?;
                let plan = Extraction {
                    root,
                    owned,
                    has_return,
                    first_trap,
                    live_ins,
                    remat,
                    shared_slots,
                    exclusive_slots,
                    ordered_blocks,
                };
                out.push(apply_extraction(m, parent_key, idx, func, &plan)?);
            }
            op => return Err(format!("outline op {op}")),
        }
    }
}
