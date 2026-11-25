//! Native walk (`register_locals`, entry-block plans) as words, both
//! directions (#1509).
//!
//! Used by `tests/self_host_native_walk.rs`,
//! `tests/self_host_native_walk_frame.rs` and `vesper-hybrid`'s
//! `hybrid_codegen_walk` bridge (`codegen_swap::register_locals` and
//! `codegen_swap::entry_plans`).
#![allow(dead_code)]

use super::{check_words, ir_words};
use std::collections::{BTreeMap, BTreeSet};
use vesper::ast::Type;
use vesper::check::World;
use vesper::ir::{FunctionIr, Op, Program};

/// The free names of every `Op::Handle` body of `f`.
pub fn handler_names(f: &FunctionIr) -> BTreeSet<String> {
    f.blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .filter_map(|i| match &i.op {
            Op::Handle { expr, .. } => Some(vesper::check::free_names(expr)),
            _ => None,
        })
        .flatten()
        .collect()
}

/// Encodes one `ir_graph_at` graph (`world_lean`, 1-function `Program`,
/// referenced callee signatures, `generics`, `nested`, handler free names)
/// for `@vesper/native-walk::native_walk_register_locals_bytes`.
pub fn function_input(
    world: &World,
    f: &FunctionIr,
    generics: &BTreeMap<String, Type>,
    nested: bool,
) -> Option<Vec<u8>> {
    let adts_fit = world.adts.values().all(|a| {
        a.variants
            .iter()
            .all(|v| v.fields.iter().all(|fld| ir_words::literal_fits(&fld.ty)))
    });
    if !adts_fit || !ir_words::function_fits(f) || !generics.values().all(ir_words::literal_fits) {
        return None;
    }
    let mut callees: BTreeSet<&str> = BTreeSet::new();
    for b in &f.blocks {
        for i in &b.instructions {
            if let Op::Call { callee, .. } = &i.op
                && world.functions.contains_key(callee)
            {
                callees.insert(callee.as_str());
            }
        }
    }
    for callee in &callees {
        let g = &world.functions[*callee];
        let sig_fits = ir_words::literal_fits(&g.result)
            && ir_words::literal_fits(&Type::Row(g.effects.clone()))
            && g.params.iter().all(|p| ir_words::literal_fits(&p.ty));
        if !sig_fits {
            return None;
        }
    }
    let program = Program {
        functions: BTreeMap::from([(f.name.clone(), f.clone())]),
    };
    let mut t = ir_words::Tables::of(&program);
    t.label_names();
    let mut tail = t.section();
    tail.push(callees.len() as u64);
    for callee in callees {
        let g = &world.functions[callee];
        tail.push(t.enc.name(callee));
        tail.push(g.generics.len() as u64);
        tail.push(t.enc.ty(&g.result));
        tail.push(t.enc.ty(&Type::Row(g.effects.clone())));
        tail.push(g.params.len() as u64);
        for q in &g.params {
            tail.push(t.enc.name(&q.name));
            tail.push(check_words::pass_word(q.pass));
            tail.push(t.enc.ty(&q.ty));
        }
    }
    tail.push(generics.len() as u64);
    for (k, v) in generics {
        tail.push(t.enc.name(k));
        tail.push(t.enc.ty(v));
    }
    tail.push(u64::from(nested));
    let names = handler_names(f);
    tail.push(names.len() as u64);
    for n in &names {
        tail.push(t.enc.name(n));
    }
    let world_words = t.enc.world_lean(world);
    Some(t.enc.finish(&world_words, &tail))
}

/// Decodes the packed-text words returned by
/// `native_walk_register_locals_bytes` into the set of register-allocated
/// local names.
pub fn decode_register_locals(words: &[u64]) -> BTreeSet<String> {
    if words.is_empty() {
        return BTreeSet::new();
    }
    let n = words[0] as usize;
    let mut bytes = Vec::with_capacity(n + 8);
    for w in &words[1..] {
        bytes.extend(w.to_le_bytes());
    }
    bytes.truncate(n);
    let out = String::from_utf8(bytes).expect("utf-8");
    out.lines()
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The text of `pack_text` words (a byte count, then the bytes).
fn unpack(words: &[u64]) -> String {
    let Some((n, rest)) = words.split_first() else {
        return String::new();
    };
    let mut bytes: Vec<u8> = rest.iter().flat_map(|w| w.to_le_bytes()).collect();
    bytes.truncate(*n as usize);
    String::from_utf8(bytes).expect("utf-8")
}

/// Decodes `native_walk_frames_bytes`'s answer to one task (parts 3b and
/// 4a): each instance's entry-block plan (`F` blocks) and internal
/// convention (`B key status words` lines) by key, or `None` when the task
/// refused or a line does not parse, which leaves every plan to Rust.
pub fn decode_entry_plans(words: &[u64]) -> Option<vesper::native::EntryPlans> {
    let text = unpack(words);
    let mut plans = vesper::native::EntryPlans::default();
    let mut current: Option<(String, String)> = None;
    let finish =
        |plans: &mut vesper::native::EntryPlans, current: Option<(String, String)>| -> Option<()> {
            if let Some((key, plan)) = current {
                plans
                    .frames
                    .insert(key, vesper::native::FramePlan::parse(&plan)?);
            }
            Some(())
        };
    for line in text.lines() {
        if line.starts_with("E ") {
            continue;
        }
        if line.starts_with("X ") {
            return None;
        }
        if let Some(rest) = line.strip_prefix("B ") {
            let mut fields = rest.rsplitn(3, ' ');
            let words: u32 = fields.next()?.parse().ok()?;
            let status = match fields.next()? {
                "0" => false,
                "1" => true,
                _ => return None,
            };
            let key = fields.next()?.to_string();
            let abi = vesper::native::FnAbi {
                status,
                result_words: (words != 0).then_some(words),
            };
            plans.abi.insert(key, abi);
        } else if let Some(key) = line.strip_prefix("F ") {
            finish(&mut plans, current.take())?;
            current = Some((key.to_string(), String::new()));
        } else {
            let (_, plan) = current.as_mut()?;
            plan.push_str(line);
            plan.push('\n');
        }
    }
    finish(&mut plans, current)?;
    Some(plans)
}
