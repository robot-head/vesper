//! Vector temporaries in registers (#1500, `native_vector::registers`).
//!
//! `Vec[T, N]` values are aggregates in stack slots. On the native
//! optimize path the CLIF clean-up promotes those slots to SSA values, so a
//! vector loop's loads, lane operations and stores happen in registers:
//! no 128-bit load or store of the emitted functions addresses a stack
//! slot. Each program also runs on the evaluator and both JIT
//! configurations, which must agree.
#[path = "common/vector_programs.rs"]
mod vector_programs;

use cranelift_codegen::ir::{self, InstructionData, Opcode};
use std::collections::HashSet;
use vector_programs::{ACC4, RAMP, SUMS, TICK};
use vesper::{check, native, parser, runtime::Runtime};

fn accepted(source: &str) -> check::Checked {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:#?}", c.diagnostics);
    c
}

/// The 128-bit loads and stores whose address is a stack slot, over every
/// emitted function (as `function: instruction`).
fn vector_stack_traffic(source: &str) -> Vec<String> {
    let c = accepted(source);
    let mut out = vec![];
    native::object_with_observer(
        &c.world,
        "main",
        None,
        true,
        Some(&mut |f: native::EmittedFunction| {
            let func = &f.function;
            let mut slot_addr: HashSet<ir::Value> = HashSet::new();
            loop {
                let before = slot_addr.len();
                for block in func.layout.blocks() {
                    for inst in func.layout.block_insts(block) {
                        match func.dfg.insts[inst] {
                            InstructionData::StackLoad {
                                opcode: Opcode::StackAddr,
                                ..
                            } => {
                                slot_addr.insert(func.dfg.first_result(inst));
                            }
                            InstructionData::BinaryImm64 {
                                opcode: Opcode::IaddImm,
                                arg,
                                ..
                            } if slot_addr.contains(&func.dfg.resolve_aliases(arg)) => {
                                slot_addr.insert(func.dfg.first_result(inst));
                            }
                            _ => {}
                        }
                    }
                }
                if slot_addr.len() == before {
                    break;
                }
            }
            for block in func.layout.blocks() {
                for inst in func.layout.block_insts(block) {
                    let (base, ty) = match func.dfg.insts[inst] {
                        InstructionData::Load {
                            opcode: Opcode::Load,
                            arg,
                            ..
                        } => (arg, func.dfg.value_type(func.dfg.first_result(inst))),
                        InstructionData::Store {
                            opcode: Opcode::Store,
                            args,
                            ..
                        } => (args[1], func.dfg.value_type(args[0])),
                        _ => continue,
                    };
                    if ty.is_vector() && slot_addr.contains(&func.dfg.resolve_aliases(base)) {
                        out.push(format!("{}: {}", f.source, func.dfg.display_inst(inst)));
                    }
                }
            }
        }),
    )
    .unwrap();
    out
}

fn agree(source: &str) -> String {
    let c = accepted(source);
    let evaluated = Runtime::new(&c.world)
        .run("main", vec![])
        .unwrap()
        .to_string();
    for optimize in [true, false] {
        let jit = native::jit_with_options(&c.world, "main", optimize)
            .unwrap()
            .to_string();
        assert_eq!(jit, evaluated, "JIT (optimize {optimize})");
    }
    evaluated
}

fn in_registers(source: &str, expected: &str) {
    let traffic = vector_stack_traffic(source);
    assert!(
        traffic.is_empty(),
        "vector temporaries still go through the stack:\n{}",
        traffic.join("\n")
    );
    assert_eq!(agree(source), expected);
}

#[test]
fn acc4_adds_in_registers() {
    in_registers(ACC4, "480");
}

#[test]
fn f32_tick_in_registers() {
    let expected = agree(TICK);
    in_registers(TICK, &expected);
}

#[test]
fn masks_and_selects_in_registers() {
    let expected = agree(RAMP);
    in_registers(RAMP, &expected);
}

#[test]
fn accumulators_stay_in_registers() {
    in_registers(SUMS, "34722480");
}
