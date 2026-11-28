//! The foreign-call facts @vesper/codegen-scalar's body plans read after
//! the handler facts (#2751): for each foreign (`.vsys`) operation the
//! program calls, the C symbol and the call's form, as
//! `vesper::ffi_lowering::plan_probe::body_call` reads them from the
//! library plan of the compile in progress. A planner input without the
//! section (no foreign calls, or no plan installed) leaves every foreign
//! call on the Rust walk.

use std::collections::BTreeSet;
use vesper::check::World;
use vesper::ffi_lowering::{is_foreign_effect, plan_probe};
use vesper::ir::{Op, Program};

/// The first word of the section: "FOREIGNS" in ASCII, so that a planner
/// input without it reads on to the next section.
pub const FOREIGN_FACTS: u64 = u64::from_le_bytes(*b"FOREIGNS");

fn put_text(out: &mut Vec<u8>, s: &str) {
    out.extend((s.len() as u64).to_le_bytes());
    out.extend(s.as_bytes());
    while !out.len().is_multiple_of(8) {
        out.push(0);
    }
}

fn put(out: &mut Vec<u8>, x: u64) {
    out.extend(x.to_le_bytes());
}

/// The section, `[FOREIGN_FACTS, n, (callee, symbol, m, m words)...]`
/// over the program's foreign operations in name order, or nothing when
/// it calls none the plan knows.
pub fn facts(world: &World, program: &Program, target: &str) -> Vec<u8> {
    let callees: BTreeSet<&String> = program
        .functions
        .values()
        .flat_map(|f| f.blocks.iter())
        .flat_map(|b| &b.instructions)
        .filter_map(|i| match &i.op {
            Op::Call { callee, .. } if !world.functions.contains_key(callee) => Some(callee),
            _ => None,
        })
        .filter(|c| {
            c.split_once("::")
                .is_some_and(|(e, _)| is_foreign_effect(e))
        })
        .collect();
    let rows: Vec<(&String, String, Vec<u64>)> = callees
        .into_iter()
        .filter_map(|c| {
            let (effect, op) = c.split_once("::")?;
            let (symbol, words) = plan_probe::body_call(effect, op, target)?;
            Some((c, symbol, words))
        })
        .collect();
    let mut out = vec![];
    if rows.is_empty() {
        return out;
    }
    put(&mut out, FOREIGN_FACTS);
    put(&mut out, rows.len() as u64);
    for (callee, symbol, words) in rows {
        put_text(&mut out, callee);
        put_text(&mut out, &symbol);
        put(&mut out, words.len() as u64);
        for w in words {
            put(&mut out, w);
        }
    }
    out
}
