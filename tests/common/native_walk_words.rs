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

/// An instance `decode_entry_plans` is reading: its key, source function
/// and variables as type-table indices.
type OpenInstance = (String, String, Vec<(String, usize)>);

/// Decodes `native_walk_frames_bytes`'s answer to one task (parts 3b, 4a
/// and 4b): by instance key, the entry-block plan (`F` blocks), the internal
/// convention (`B key status words`) and the instance itself (`I name`,
/// `V var type-index`, with the types in the task's `W` table). `None` when
/// the task refused or a line does not parse, which leaves every plan to
/// Rust.
pub fn decode_entry_plans(words: &[u64]) -> Option<vesper::native::EntryPlans> {
    let text = unpack(words);
    let mut plans = vesper::native::EntryPlans::default();
    // The `F` block being read: its key and plan text.
    let mut current: Option<(String, String)> = None;
    // The instance the last `B` line opened: its key, source function and
    // variables as type indices.
    let mut instances: Vec<OpenInstance> = vec![];
    let mut table: Option<Vec<Type>> = None;
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
            finish(&mut plans, current.take())?;
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
            plans.abi.insert(key.clone(), abi);
            instances.push((key, String::new(), vec![]));
        } else if let Some(name) = line.strip_prefix("I ") {
            instances.last_mut()?.1 = name.to_string();
        } else if let Some(rest) = line.strip_prefix("V ") {
            let (var, index) = rest.rsplit_once(' ')?;
            let index = index.parse().ok()?;
            instances.last_mut()?.2.push((var.to_string(), index));
        } else if let Some(rest) = line.strip_prefix("W") {
            finish(&mut plans, current.take())?;
            let words: Vec<u64> = rest
                .split_whitespace()
                .map(|w| w.parse().ok())
                .collect::<Option<_>>()?;
            table = Some(check_words::decode_type_table(&words)?);
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
    let table = table?;
    for (key, name, vars) in instances {
        let vars = vars
            .into_iter()
            .map(|(var, i)| Some((var, table.get(i)?.clone())))
            .collect::<Option<BTreeMap<_, _>>>()?;
        plans.instances.insert(key, (name, vars));
    }
    Some(plans)
}

/// Encodes one graph's successor lists (`native::block_targets`) for
/// `@vesper/native-walk::native_walk_block_plan_bytes` (#2094): the block
/// count, then per block its successor count and successors, as
/// little-endian words.
pub fn block_targets_input(targets: &[Vec<usize>]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 * (1 + targets.iter().map(|t| 1 + t.len()).sum::<usize>()));
    let mut word = |w: usize| out.extend_from_slice(&(w as u64).to_le_bytes());
    word(targets.len());
    for t in targets {
        word(t.len());
        for &b in t {
            word(b);
        }
    }
    out
}

/// Decodes `native_walk_block_plan_bytes`'s answer for a graph of `n`
/// blocks: the order, then the cyclic and the sealed blocks, each a count
/// and the indices. `None` when the package declined (no words) or the
/// answer does not parse.
pub fn decode_block_plan(words: &[u64], n: usize) -> Option<vesper::native::BlockPlan> {
    let mut it = words.iter().map(|&w| usize::try_from(w).ok());
    let order = (0..n)
        .map(|_| it.next().flatten())
        .collect::<Option<Vec<_>>>()?;
    let mut set = || -> Option<BTreeSet<usize>> {
        let k = it.next().flatten()?;
        (0..k).map(|_| it.next().flatten()).collect()
    };
    let cyclic = set()?;
    let sealed = set()?;
    if it.next().is_some() {
        return None;
    }
    Some(vesper::native::BlockPlan {
        order,
        cyclic,
        sealed,
    })
}
