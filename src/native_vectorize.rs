//! Two-lane vectorization of counted element loops on the optimized native
//! path (#1474, gap 2b of SH6 lane 7's report, #1402).
//!
//! Cranelift does not vectorize loops. Bounds hoisting (`crate::bounds_hoist`)
//! leaves each counted loop over a sequence with an unchecked fast copy whose
//! body is a straight line of element loads, integer arithmetic and element
//! stores, such as Bench's subset-sum step `dist[s + rank] += dist[s]` for
//! `s` from `max` down to 0. This pass, run on each function's CLIF after
//! `crate::native_clif_opt`, puts a two-lane prologue in front of such a
//! loop. While at least two iterations remain and no store of one iteration
//! writes the element a load of the next iteration reads, the prologue runs
//! iterations `j` and `j + 1` as `i64x2` loads, lane-wise arithmetic and
//! `i64x2` stores, then goes on with `j + 2`. The scalar loop, unchanged,
//! runs what is left.
//!
//! A loop qualifies when (all of it read from the CLIF):
//!
//! - its header has one parameter `j` and branches on `j <u B` or `j <=u B`
//!   with `B` invariant, to a body block or the exit;
//! - the body has no parameters and goes to the exit when `j == B` (the same
//!   `B`) or straight on to a latch that only jumps back with `j + 1`, or
//!   jumps back with `j + 1` itself;
//! - every body instruction is an invariant (a constant, a pure operation of
//!   invariants, or a `readonly can_move` load of an invariant address), a
//!   pure integer operation of `j`, an `i64` load or store whose address
//!   moves by 8 bytes per iteration (all in the same direction), or
//!   `iadd`/`isub`/`band`/`bor`/`bxor` of loaded values and invariants;
//! - every load comes before every store, and `j` itself never reaches a
//!   stored value.
//!
//! Two lanes compute what two scalar iterations compute when the earlier
//! iteration's stores do not write an element the later iteration loads
//! (loads of both lanes happen before stores of both), and no store of the
//! earlier iteration overlaps an earlier store of the later one (each store
//! runs for both lanes before the next store does). Every access moves
//! by the same 8 bytes per iteration, so the distance from a store of
//! iteration `j` to an access of iteration `j + 1` is the same for every `j`:
//! the prologue checks once, before the loop, that each such distance is at
//! least 8 bytes (no two 8-byte elements even partly overlap), and goes to
//! the scalar loop if not. Lanes are in memory order: a loop whose
//! addresses go down keeps iteration `j + 1` in lane 0, and since every
//! access goes the same way, lane-wise arithmetic pairs each iteration with
//! itself.
//!
//! The verified backend never runs this pass. `VESPER_VECTORIZE=off` turns
//! it off.
use cranelift_codegen::cursor::{Cursor as _, FuncCursor};
use cranelift_codegen::flowgraph::ControlFlowGraph;
use cranelift_codegen::ir::{
    self, Block, BlockArg, Inst, InstBuilder, InstructionData, MemFlags, Opcode, Value,
    condcodes::IntCC, types,
};
use std::collections::{HashMap, HashSet};

/// Vectorizes every qualifying loop of `func`. `simd` says whether the
/// target has 128-bit integer vectors (x86-64 and AArch64); without them
/// the pass does nothing.
pub fn run(func: &mut ir::Function, simd: bool) -> usize {
    if !simd || std::env::var_os("VESPER_VECTORIZE").is_some_and(|v| v == "off") {
        return 0;
    }
    let headers: Vec<Block> = func
        .layout
        .blocks()
        .filter(|&b| func.dfg.block_params(b).len() == 1)
        .collect();
    let mut done = 0;
    for h in headers {
        let cfg = ControlFlowGraph::with_function(func);
        if let Some(plan) = plan(func, &cfg, h) {
            emit(func, &plan);
            done += 1;
            if std::env::var_os("VESPER_VECTORIZE").is_some_and(|v| v == "debug") {
                let flags =
                    cranelift_codegen::settings::Flags::new(cranelift_codegen::settings::builder());
                if let Err(e) = cranelift_codegen::verify_function(func, &flags) {
                    eprintln!(
                        "{}",
                        cranelift_codegen::print_errors::pretty_verifier_error(func, None, e)
                    );
                }
            }
        }
    }
    done
}

/// What a body instruction is, for the two-lane copy.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    /// The same in every iteration: emitted once, ahead of the pair.
    Invariant,
    /// A scalar that depends on `j`: emitted once per lane iteration.
    /// `step` is how much it grows per iteration, when it is affine.
    Scalar { step: Option<i64> },
    /// A loaded value or lane-wise arithmetic of one: an `i64x2`.
    Lane,
    /// A store.
    Store,
}

struct Plan {
    header: Block,
    /// The header's parameter.
    j: Value,
    /// The bound `j` is compared with.
    bound: Value,
    /// The body's instructions in order, with their kinds.
    insts: Vec<(Inst, Kind)>,
    /// +1 when addresses grow with `j`, -1 when they shrink.
    direction: i64,
    /// The branches outside the loop that enter the header.
    entries: Vec<Inst>,
}

fn resolve(func: &ir::Function, v: Value) -> Value {
    func.dfg.resolve_aliases(v)
}

/// The block that defines `v`.
fn def_block(func: &ir::Function, v: Value) -> Option<Block> {
    match func.dfg.value_def(resolve(func, v)) {
        ir::ValueDef::Result(inst, _) => func.layout.inst_block(inst),
        ir::ValueDef::Param(block, _) => Some(block),
        _ => None,
    }
}

/// `c` with `uextend` and `icmp_imm ne _, 0` taken off: the compare a
/// branch really tests.
fn strip_condition(func: &ir::Function, mut c: Value) -> Value {
    loop {
        c = resolve(func, c);
        let ir::ValueDef::Result(inst, 0) = func.dfg.value_def(c) else {
            return c;
        };
        match func.dfg.insts[inst] {
            InstructionData::Unary {
                opcode: Opcode::Uextend,
                arg,
            } => c = arg,
            InstructionData::IntCompareImm {
                opcode: Opcode::IcmpImm,
                cond: IntCC::NotEqual,
                arg,
                imm,
            } if imm.bits() == 0 => c = arg,
            _ => return c,
        }
    }
}

/// The `(cond, lhs, rhs)` of the integer compare `c`.
fn compare(func: &ir::Function, c: Value) -> Option<(IntCC, Value, Value)> {
    let ir::ValueDef::Result(inst, 0) = func.dfg.value_def(resolve(func, c)) else {
        return None;
    };
    match func.dfg.insts[inst] {
        InstructionData::IntCompare {
            opcode: Opcode::Icmp,
            cond,
            args: [a, b],
        } => Some((cond, resolve(func, a), resolve(func, b))),
        _ => None,
    }
}

/// The target and arguments of a block call.
fn call(func: &ir::Function, c: ir::BlockCall) -> (Block, Vec<BlockArg>) {
    let pool = &func.dfg.value_lists;
    (c.block(pool), c.args(pool).collect())
}

fn plan(func: &ir::Function, cfg: &ControlFlowGraph, h: Block) -> Option<Plan> {
    let j = resolve(func, func.dfg.block_params(h)[0]);
    if func.dfg.value_type(j) != types::I64 {
        return None;
    }
    // The header: only the compare, then `brif`.
    let term = func.layout.last_inst(h)?;
    let InstructionData::Brif {
        arg,
        blocks: [then_call, else_call],
        ..
    } = func.dfg.insts[term]
    else {
        return None;
    };
    let (body, body_args) = call(func, then_call);
    let (exit, exit_args) = call(func, else_call);
    if !body_args.is_empty() || !exit_args.is_empty() || body == h || exit == h {
        return None;
    }
    let (cond, lhs, bound) = compare(func, strip_condition(func, arg))?;
    if lhs != j
        || !matches!(
            cond,
            IntCC::UnsignedLessThan | IntCC::UnsignedLessThanOrEqual
        )
    {
        return None;
    }
    for inst in func.layout.block_insts(h) {
        if inst != term
            && !matches!(
                func.dfg.insts[inst].opcode(),
                Opcode::Icmp | Opcode::IcmpImm | Opcode::Uextend | Opcode::Iconst
            )
        {
            return None;
        }
    }
    // The body: no parameters; on to the latch, or to the exit at `j == B`.
    if !func.dfg.block_params(body).is_empty() {
        return None;
    }
    let body_term = func.layout.last_inst(body)?;
    // `None` when the body jumps back to the header itself.
    let latch = match func.dfg.insts[body_term] {
        InstructionData::Jump { destination, .. } => {
            let (latch, args) = call(func, destination);
            match args.as_slice() {
                [] => Some(latch),
                [BlockArg::Value(next)] if latch == h && is_increment(func, *next, j) => None,
                _ => return None,
            }
        }
        InstructionData::Brif {
            arg,
            blocks: [a, b],
            ..
        } => {
            let (out, out_args) = call(func, a);
            let (latch, latch_args) = call(func, b);
            let (cond, x, y) = compare(func, strip_condition(func, arg))?;
            if out != exit
                || !out_args.is_empty()
                || !latch_args.is_empty()
                || cond != IntCC::Equal
                || x != j
                || y != bound
            {
                return None;
            }
            Some(latch)
        }
        _ => return None,
    };
    if let Some(latch) = latch {
        if latch == h || latch == body || !func.dfg.block_params(latch).is_empty() {
            return None;
        }
        // The latch: `j + 1` back to the header.
        let latch_term = func.layout.last_inst(latch)?;
        let InstructionData::Jump { destination, .. } = func.dfg.insts[latch_term] else {
            return None;
        };
        let (back, back_args) = call(func, destination);
        let [BlockArg::Value(next)] = back_args.as_slice() else {
            return None;
        };
        if back != h || !is_increment(func, *next, j) {
            return None;
        }
        for inst in func.layout.block_insts(latch) {
            if inst != latch_term
                && !matches!(
                    func.dfg.insts[inst].opcode(),
                    Opcode::Iconst | Opcode::Iadd | Opcode::IaddImm
                )
            {
                return None;
            }
        }
    }
    // The block that jumps back to the header.
    let back_edge = latch.unwrap_or(body);
    // Edges: the body only from the header, the latch only from the body,
    // the header from the latch and from outside.
    let preds =
        |b: Block| -> Vec<(Block, Inst)> { cfg.pred_iter(b).map(|p| (p.block, p.inst)).collect() };
    if preds(body).iter().any(|(p, _)| *p != h)
        || latch.is_some_and(|l| preds(l).iter().any(|(p, _)| *p != body))
    {
        return None;
    }
    let mut entries = vec![];
    for (p, inst) in preds(h) {
        if p == back_edge {
            continue;
        }
        if p == h || p == body {
            return None;
        }
        entries.push(inst);
    }
    if entries.is_empty() {
        return None;
    }
    let looped: HashSet<Block> = [Some(h), Some(body), latch].into_iter().flatten().collect();
    if looped.contains(&def_block(func, bound)?) {
        return None;
    }
    // The body's instructions.
    let mut kinds: HashMap<Value, Kind> = HashMap::new();
    kinds.insert(j, Kind::Scalar { step: Some(1) });
    let kind_of = |kinds: &HashMap<Value, Kind>, v: Value| -> Option<Kind> {
        let v = resolve(func, v);
        if let Some(k) = kinds.get(&v) {
            return Some(*k);
        }
        // Defined outside the loop.
        (!looped.contains(&def_block(func, v)?)).then_some(Kind::Invariant)
    };
    let mut insts = vec![];
    let mut direction = 0i64;
    let mut stores = false;
    for inst in func.layout.block_insts(body) {
        if inst == body_term {
            break;
        }
        let data = func.dfg.insts[inst];
        let args: Vec<Value> = func
            .dfg
            .inst_args(inst)
            .iter()
            .map(|&v| resolve(func, v))
            .collect();
        let arg_kinds: Vec<Kind> = args
            .iter()
            .map(|&v| kind_of(&kinds, v))
            .collect::<Option<_>>()?;
        let results = func.dfg.inst_results(inst);
        let kind = match data {
            InstructionData::Load {
                opcode: Opcode::Load,
                flags,
                ..
            } => {
                let ty = func.dfg.value_type(results[0]);
                if arg_kinds[0] == Kind::Invariant && flags.readonly() && flags.can_move() {
                    Kind::Invariant
                } else if ty == types::I64 && !stores && plain(flags) {
                    let Kind::Scalar { step: Some(step) } = arg_kinds[0] else {
                        return None;
                    };
                    direction = same_direction(direction, step)?;
                    Kind::Lane
                } else {
                    return None;
                }
            }
            InstructionData::Store {
                opcode: Opcode::Store,
                flags,
                ..
            } => {
                let ty = func.dfg.value_type(args[0]);
                let Kind::Scalar { step: Some(step) } = arg_kinds[1] else {
                    return None;
                };
                if ty != types::I64 || !plain(flags) || arg_kinds[0] == Kind::Store {
                    return None;
                }
                if !matches!(arg_kinds[0], Kind::Lane | Kind::Invariant) {
                    return None;
                }
                direction = same_direction(direction, step)?;
                stores = true;
                Kind::Store
            }
            _ => {
                let opcode = data.opcode();
                if results.len() != 1 || !pure(opcode) {
                    return None;
                }
                if arg_kinds.iter().all(|k| *k == Kind::Invariant) {
                    Kind::Invariant
                } else if arg_kinds.contains(&Kind::Lane) {
                    let lane_op = matches!(
                        opcode,
                        Opcode::Iadd | Opcode::Isub | Opcode::Band | Opcode::Bor | Opcode::Bxor
                    );
                    let operands_ok = arg_kinds
                        .iter()
                        .all(|k| matches!(k, Kind::Lane | Kind::Invariant));
                    if !lane_op || !operands_ok || func.dfg.value_type(results[0]) != types::I64 {
                        return None;
                    }
                    Kind::Lane
                } else {
                    Kind::Scalar {
                        step: step(func, &data, &args, &arg_kinds),
                    }
                }
            }
        };
        if let Some(&r) = results.first() {
            kinds.insert(resolve(func, r), kind);
        }
        insts.push((inst, kind));
    }
    if !stores || direction == 0 {
        return None;
    }
    // Lane values stay in the body: nothing else may read them.
    for b in [Some(h), latch].into_iter().flatten() {
        for inst in func.layout.block_insts(b) {
            for &v in func.dfg.inst_args(inst) {
                if matches!(
                    kinds.get(&resolve(func, v)),
                    Some(Kind::Lane) | Some(Kind::Scalar { .. })
                ) && resolve(func, v) != j
                {
                    return None;
                }
            }
        }
    }
    Some(Plan {
        header: h,
        j,
        bound,
        insts,
        direction,
        entries,
    })
}

/// Whether `v` is `j + 1`.
fn is_increment(func: &ir::Function, v: Value, j: Value) -> bool {
    let ir::ValueDef::Result(inst, 0) = func.dfg.value_def(resolve(func, v)) else {
        return false;
    };
    match func.dfg.insts[inst] {
        InstructionData::BinaryImm64 {
            opcode: Opcode::IaddImm,
            arg,
            imm,
        } => resolve(func, arg) == j && imm.bits() == 1,
        InstructionData::Binary {
            opcode: Opcode::Iadd,
            args: [a, b],
        } => {
            let (a, b) = (resolve(func, a), resolve(func, b));
            (a == j && constant(func, b) == Some(1)) || (b == j && constant(func, a) == Some(1))
        }
        _ => false,
    }
}

fn constant(func: &ir::Function, v: Value) -> Option<i64> {
    let ir::ValueDef::Result(inst, 0) = func.dfg.value_def(resolve(func, v)) else {
        return None;
    };
    match func.dfg.insts[inst] {
        InstructionData::UnaryImm {
            opcode: Opcode::Iconst,
            imm,
        } => Some(imm.bits()),
        _ => None,
    }
}

/// A load or store with nothing special about it (no atomics, no explicit
/// endianness): its two-lane form is the same access twice as wide.
fn plain(flags: MemFlags) -> bool {
    flags.explicit_endianness().is_none() && !flags.readonly()
}

/// Operations without side effects or traps, safe to emit for both lanes.
fn pure(opcode: Opcode) -> bool {
    matches!(
        opcode,
        Opcode::Iconst
            | Opcode::Iadd
            | Opcode::IaddImm
            | Opcode::Isub
            | Opcode::IrsubImm
            | Opcode::Imul
            | Opcode::ImulImm
            | Opcode::Ishl
            | Opcode::IshlImm
            | Opcode::Ushr
            | Opcode::UshrImm
            | Opcode::Band
            | Opcode::BandImm
            | Opcode::Bor
            | Opcode::BorImm
            | Opcode::Bxor
            | Opcode::BxorImm
            | Opcode::Icmp
            | Opcode::IcmpImm
            | Opcode::Uextend
            | Opcode::Sextend
            | Opcode::Ireduce
            | Opcode::Select
    )
}

/// How much an operation of `j` grows per iteration, when it is affine.
fn step(
    func: &ir::Function,
    data: &InstructionData,
    args: &[Value],
    kinds: &[Kind],
) -> Option<i64> {
    let s = |k: &Kind| match k {
        Kind::Invariant => Some(0),
        Kind::Scalar { step } => *step,
        _ => None,
    };
    match *data {
        InstructionData::Binary { opcode, .. } => {
            let (a, b) = (s(&kinds[0])?, s(&kinds[1])?);
            match opcode {
                Opcode::Iadd => a.checked_add(b),
                Opcode::Isub => a.checked_sub(b),
                Opcode::Imul if kinds[1] == Kind::Invariant => {
                    a.checked_mul(constant(func, args[1])?)
                }
                Opcode::Imul if kinds[0] == Kind::Invariant => {
                    b.checked_mul(constant(func, args[0])?)
                }
                _ => None,
            }
        }
        InstructionData::BinaryImm64 { opcode, imm, .. } => {
            let a = s(&kinds[0])?;
            match opcode {
                Opcode::IaddImm => Some(a),
                Opcode::IrsubImm => a.checked_neg(),
                Opcode::ImulImm => a.checked_mul(imm.bits()),
                Opcode::IshlImm if (0..32).contains(&imm.bits()) => {
                    a.checked_mul(1i64 << imm.bits())
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// The common direction of the accesses so far, with one more of `step`
/// bytes per iteration: every access moves by 8 bytes the same way.
fn same_direction(direction: i64, step: i64) -> Option<i64> {
    let d = match step {
        8 => 1,
        -8 => -1,
        _ => return None,
    };
    (direction == 0 || direction == d).then_some(d)
}

/// Copies `inst` at the bottom of `block` with its operands mapped through
/// `map` (values not in it stay), and returns the copy's first result.
fn copy_inst(
    func: &mut ir::Function,
    block: Block,
    inst: Inst,
    map: &HashMap<Value, Value>,
) -> (Inst, Option<Value>) {
    let new = func.dfg.clone_inst(inst);
    let args: Vec<Value> = func.dfg.inst_values(new).collect();
    let mapped: HashMap<Value, Value> = args
        .into_iter()
        .map(|v| {
            let r = resolve(func, v);
            (v, map.get(&r).copied().unwrap_or(r))
        })
        .collect();
    func.dfg.map_inst_values(new, |v| mapped[&v]);
    let mut pos = FuncCursor::new(func).at_bottom(block);
    pos.insert_inst(new);
    let result = func.dfg.inst_results(new).first().copied();
    (new, result)
}

/// The effective addresses of the body's loads and stores, in order, with
/// the scalars of one iteration emitted at the bottom of `block` under `map`
/// (which gains them). Invariants are expected in `map` already.
fn addresses(
    func: &mut ir::Function,
    plan: &Plan,
    block: Block,
    map: &mut HashMap<Value, Value>,
) -> (Vec<Value>, Vec<Value>) {
    let (mut loads, mut stores) = (vec![], vec![]);
    for &(inst, kind) in &plan.insts {
        match kind {
            Kind::Invariant => {}
            Kind::Scalar { .. } => {
                let old = resolve(func, func.dfg.inst_results(inst)[0]);
                let (_, new) = copy_inst(func, block, inst, map);
                map.insert(old, new.expect("a scalar has a result"));
            }
            Kind::Lane | Kind::Store => {
                let (addr, offset) = match func.dfg.insts[inst] {
                    InstructionData::Load { arg, offset, .. } => (arg, offset),
                    InstructionData::Store { args, offset, .. } => (args[1], offset),
                    // Lane arithmetic has no address.
                    _ => continue,
                };
                let addr = resolve(func, addr);
                let addr = map.get(&addr).copied().unwrap_or(addr);
                let offset = i64::from(offset);
                let e = if offset == 0 {
                    addr
                } else {
                    FuncCursor::new(func)
                        .at_bottom(block)
                        .ins()
                        .iadd_imm(addr, offset)
                };
                if kind == Kind::Lane {
                    loads.push(e);
                } else {
                    stores.push(e);
                }
            }
        }
    }
    (loads, stores)
}

/// Puts the two-lane loop in front of the header:
///
/// - `pre(j0)`, entered instead of the header, computes the invariants and
///   checks once that no store of an iteration overlaps a load of the next
///   (every access moves by the same 8 bytes, so that distance is the same
///   for every pair), then goes to `lanes(j0)`, or to the header if they
///   may overlap or fewer than two iterations are left;
/// - `lanes(jv)` runs iterations `jv` and `jv + 1` as `i64x2` operations,
///   then goes on with `jv + 2`, to itself while two more iterations are
///   left and to the header for the rest.
fn emit(func: &mut ir::Function, plan: &Plan) {
    let i64x2 = types::I64X2;
    let pre = func.dfg.make_block();
    let j0 = func.dfg.append_block_param(pre, types::I64);
    let lanes = func.dfg.make_block();
    let jv = func.dfg.append_block_param(lanes, types::I64);
    func.layout.insert_block(pre, plan.header);
    func.layout.insert_block(lanes, plan.header);
    for &inst in &plan.entries {
        let ir::DataFlowGraph {
            insts,
            jump_tables,
            exception_tables,
            value_lists,
            ..
        } = &mut func.dfg;
        for c in insts[inst].branch_destination_mut(jump_tables, exception_tables) {
            if c.block(value_lists) == plan.header {
                c.set_block(pre, value_lists);
            }
        }
    }
    // `pre`: the invariants, then the overlap check on iterations j0 and
    // j0 + 1.
    let mut invariants: HashMap<Value, Value> = HashMap::new();
    for &(inst, kind) in &plan.insts {
        if kind == Kind::Invariant {
            let old = func
                .dfg
                .inst_results(inst)
                .first()
                .map(|&r| resolve(func, r));
            let (_, new) = copy_inst(func, pre, inst, &invariants);
            if let (Some(old), Some(new)) = (old, new) {
                invariants.insert(old, new);
            }
        }
    }
    let mut first = invariants.clone();
    first.insert(plan.j, j0);
    let (_, stores0) = addresses(func, plan, pre, &mut first);
    let j01 = FuncCursor::new(func).at_bottom(pre).ins().iadd_imm(j0, 1);
    let mut second = invariants.clone();
    second.insert(plan.j, j01);
    let (loads1, stores1) = addresses(func, plan, pre, &mut second);
    {
        let mut pos = FuncCursor::new(func).at_bottom(pre);
        // Two iterations left: `j0 < B` and `j0 + 1 < B` (which cannot
        // wrap once `j0 < B`).
        let a = pos.ins().icmp(IntCC::UnsignedLessThan, j0, plan.bound);
        let b = pos.ins().icmp(IntCC::UnsignedLessThan, j01, plan.bound);
        let mut ok = pos.ins().band(a, b);
        // The pairs of accesses whose order the lanes change: a store of
        // the first iteration and a load of the next (the lanes load both
        // iterations first), and a later store of the first iteration and
        // an earlier store of the next (the lanes run each store for both
        // iterations before the next store).
        let mut pairs: Vec<(Value, Value)> = vec![];
        for &s in &stores0 {
            for &l in &loads1 {
                pairs.push((s, l));
            }
        }
        for (y, &later) in stores0.iter().enumerate() {
            for &earlier in &stores1[..y] {
                pairs.push((later, earlier));
            }
        }
        for (a, b) in pairs {
            // `|a - b| >= 8`: the two 8-byte elements do not overlap.
            let gap = pos.ins().isub(a, b);
            let gap = pos.ins().iadd_imm(gap, 7);
            let apart = pos.ins().icmp_imm(IntCC::UnsignedGreaterThan, gap, 14);
            ok = pos.ins().band(ok, apart);
        }
        pos.ins().brif(
            ok,
            lanes,
            &[BlockArg::Value(j0)],
            plan.header,
            &[BlockArg::Value(j0)],
        );
    }
    let mut lane0 = invariants.clone();
    lane0.insert(plan.j, jv);
    // `lanes`: iteration jv's addresses; the vectors start at the lower of
    // each access's two elements, jv's own when addresses grow, jv + 1's
    // (8 bytes below) when they shrink.
    let (loads, stores) = addresses(func, plan, lanes, &mut lane0);
    let lower = |func: &mut ir::Function, e: Value| -> Value {
        if plan.direction > 0 {
            e
        } else {
            FuncCursor::new(func).at_bottom(lanes).ins().iadd_imm(e, -8)
        }
    };
    let mut vectors: HashMap<Value, Value> = HashMap::new();
    let mut splats: HashMap<Value, Value> = HashMap::new();
    let mut splat = |func: &mut ir::Function, v: Value| -> Value {
        let scalar = lane0.get(&v).copied().unwrap_or(v);
        *splats.entry(scalar).or_insert_with(|| {
            FuncCursor::new(func)
                .at_bottom(lanes)
                .ins()
                .splat(i64x2, scalar)
        })
    };
    let (mut li, mut si) = (0, 0);
    for &(inst, kind) in &plan.insts {
        let data = func.dfg.insts[inst];
        match kind {
            Kind::Lane => {
                let result = resolve(func, func.dfg.inst_results(inst)[0]);
                let v = match data {
                    InstructionData::Load { .. } => {
                        let addr = lower(func, loads[li]);
                        li += 1;
                        FuncCursor::new(func).at_bottom(lanes).ins().load(
                            i64x2,
                            MemFlags::new(),
                            addr,
                            0,
                        )
                    }
                    InstructionData::Binary { opcode, args } => {
                        let mut operand = |func: &mut ir::Function, v: Value| -> Value {
                            let v = resolve(func, v);
                            match vectors.get(&v) {
                                Some(&x) => x,
                                None => splat(func, v),
                            }
                        };
                        let a = operand(func, args[0]);
                        let b = operand(func, args[1]);
                        let mut pos = FuncCursor::new(func).at_bottom(lanes);
                        match opcode {
                            Opcode::Iadd => pos.ins().iadd(a, b),
                            Opcode::Isub => pos.ins().isub(a, b),
                            Opcode::Band => pos.ins().band(a, b),
                            Opcode::Bor => pos.ins().bor(a, b),
                            _ => pos.ins().bxor(a, b),
                        }
                    }
                    _ => unreachable!("lane values are loads or lane arithmetic"),
                };
                vectors.insert(result, v);
            }
            Kind::Store => {
                let InstructionData::Store { args, .. } = data else {
                    unreachable!("a store")
                };
                let value = resolve(func, args[0]);
                let v = match vectors.get(&value) {
                    Some(&x) => x,
                    None => splat(func, value),
                };
                let addr = lower(func, stores[si]);
                si += 1;
                FuncCursor::new(func)
                    .at_bottom(lanes)
                    .ins()
                    .store(MemFlags::new(), v, addr, 0);
            }
            _ => {}
        }
    }
    // On while two more iterations are left: `j2 < B` and `j2 + 1 < B`.
    // `jv + 1 < B`, so `j2 = jv + 2` cannot wrap, and `j2 + 1` wraps only
    // when `j2` is `u64::MAX`, which fails `j2 < B`.
    let mut pos = FuncCursor::new(func).at_bottom(lanes);
    let j2 = pos.ins().iadd_imm(jv, 2);
    let j3 = pos.ins().iadd_imm(jv, 3);
    let a = pos.ins().icmp(IntCC::UnsignedLessThan, j2, plan.bound);
    let b = pos.ins().icmp(IntCC::UnsignedLessThan, j3, plan.bound);
    let more = pos.ins().band(a, b);
    pos.ins().brif(
        more,
        lanes,
        &[BlockArg::Value(j2)],
        plan.header,
        &[BlockArg::Value(j2)],
    );
}
