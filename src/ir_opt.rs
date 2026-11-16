//! Effectful equality saturation over memory observations and ordered state
//! tokens. Calls are opaque barriers. A PCG disjointness fact is required to
//! forward a read across a write to another place; scalar owned reads alone
//! may be replaced, so borrowed argument evidence is preserved.
use crate::{
    ast::*,
    check::{Place, World},
    ir::{self, Op, Terminator, ValueId},
};
use egglog::Read;
use std::collections::BTreeMap;

const RULES: &str = r#"(sort V)
(sort S)
(constructor Ref (i64) V)
(constructor Start (i64) S)
(constructor Write (S i64 V) S)
(constructor Barrier (S i64) S)
(constructor Read (S i64) V)
(relation disjoint (i64 i64))
(rewrite (Read (Write s p v) p) v)
(rule ((= r (Read (Write s p v) q)) (disjoint p q))
      ((union r (Read s q))))
"#;

fn scalar(t: &Type) -> bool {
    matches!(
        t.name(),
        "u8" | "u32" | "u64" | "i64" | "f64" | "bool" | "Unit"
    ) || *t == Type::unit()
}
fn place(root: &str, path: &[String]) -> Place {
    Place {
        root: root.into(),
        path: path.to_vec(),
    }
}
fn resolve(mut v: ValueId, aliases: &BTreeMap<ValueId, ValueId>) -> ValueId {
    while let Some(next) = aliases.get(&v) {
        if *next == v {
            break;
        }
        v = *next;
    }
    v
}
pub fn optimize(world: &World, program: &mut ir::Program) -> Result<usize, String> {
    let mut eliminated = 0;
    for f in program.functions.values_mut() {
        let mut aliases = BTreeMap::new();
        for block in &mut f.blocks {
            let mut places = BTreeMap::<Place, i64>::new();
            let mut ids = vec![];
            for i in &block.instructions {
                let p = match &i.op {
                    Op::Read { root, path, .. } if f.locals.contains_key(root) => {
                        Some(place(root, path))
                    }
                    Op::Write { place: p, .. } => {
                        let mut path = p.split('.');
                        Some(place(
                            path.next().unwrap(),
                            &path.map(str::to_owned).collect::<Vec<_>>(),
                        ))
                    }
                    Op::Bind { pattern, .. } => {
                        if let PatternKind::Bind(n) = &pattern.kind {
                            Some(place(n, &[]))
                        } else {
                            None
                        }
                    }
                    _ => None,
                };
                if let Some(p) = p {
                    let next = places.len() as i64;
                    places.entry(p).or_insert(next);
                }
            }
            let mut code = RULES.to_owned();
            let mut graph = ir::PlaceGraph::default();
            for p in places.keys() {
                graph.insert(p);
            }
            for (p, a) in &places {
                for (q, b) in &places {
                    if !graph.overlaps(p, q) {
                        code.push_str(&format!("(disjoint {a} {b})\n"));
                    }
                }
            }
            code.push_str(&format!(
                "(let s{} (Start {}))\n",
                block.entry_token, block.entry_token
            ));
            // Every SSA value has an opaque representative. Equalities never
            // synthesize an owned aggregate or duplicate a non-copy value.
            for v in f.values.keys() {
                code.push_str(&format!("(let v{v} (Ref {v}))\n"));
            }
            let mut prior = block
                .params
                .iter()
                .copied()
                .filter(|v| scalar(&f.values[v]))
                .collect::<Vec<_>>();
            for i in &block.instructions {
                if let Op::Read {
                    root,
                    path,
                    pass: Pass::Sink,
                    ..
                } = &i.op
                    && scalar(&i.ty)
                    && let Some(p) = places.get(&place(root, path))
                {
                    let input = i.token.ok_or("read lacks state evidence")?.0;
                    code.push_str(&format!("(union v{} (Read s{input} {p}))\n", i.dest));
                    ids.push((
                        i.dest,
                        prior
                            .iter()
                            .copied()
                            .filter(|v| f.values[v] == i.ty)
                            .collect::<Vec<_>>(),
                    ));
                }
                if let Some((input, output)) = i.token
                    && input != output
                {
                    let write = match &i.op {
                        Op::Write {
                            place: p,
                            op,
                            value,
                        } if op == "=" && scalar(&f.values[value]) => {
                            let mut path = p.split('.');
                            places
                                .get(&place(
                                    path.next().unwrap(),
                                    &path.map(str::to_owned).collect::<Vec<_>>(),
                                ))
                                .map(|p| (*p, *value))
                        }
                        Op::Bind { pattern, value } if scalar(&f.values[value]) => {
                            if let PatternKind::Bind(n) = &pattern.kind {
                                places.get(&place(n, &[])).map(|p| (*p, *value))
                            } else {
                                None
                            }
                        }
                        _ => None,
                    };
                    if let Some((p, v)) = write {
                        code.push_str(&format!("(let s{output} (Write s{input} {p} v{v}))\n"));
                    } else {
                        code.push_str(&format!("(let s{output} (Barrier s{input} {}))\n", i.dest));
                    }
                }
                if scalar(&i.ty) {
                    prior.push(i.dest);
                }
            }
            if ids.is_empty() {
                continue;
            }
            // These rules only reduce read/write depth; bounded rounds also
            // cap compile work for very large source blocks.
            code.push_str("(run 12)\n");
            let mut egraph = egglog::EGraph::default();
            egraph
                .parse_and_run_program(None, &code)
                .map_err(|e| format!("effect equality saturation: {e}"))?;
            for (dest, candidates) in ids {
                let equivalent = egraph
                    .update(|fs| -> Result<Option<ValueId>, egglog::Error> {
                        let root = fs.eclass_of("Ref", i64::from(dest))?;
                        for candidate in candidates {
                            if fs.eclass_of("Ref", i64::from(candidate))? == root {
                                return Ok(Some(candidate));
                            }
                        }
                        Ok(None)
                    })
                    .map_err(|e| e.to_string())?;
                if let Some(v) = equivalent {
                    aliases.insert(dest, resolve(v, &aliases));
                    eliminated += 1;
                }
            }
        }
        for b in &mut f.blocks {
            b.instructions.retain(|i| !aliases.contains_key(&i.dest));
            for i in &mut b.instructions {
                operands(&mut i.op, |v| resolve(v, &aliases));
            }
            match &mut b.terminator {
                Terminator::Return(v) => *v = resolve(*v, &aliases),
                Terminator::Jump { args, .. } => {
                    for v in args {
                        *v = resolve(*v, &aliases);
                    }
                }
                Terminator::Branch { condition, .. } => *condition = resolve(*condition, &aliases),
                Terminator::Match { value, .. } => *value = resolve(*value, &aliases),
                _ => {}
            }
        }
    }
    eliminated += fip_reuse_pass(world, program)?;
    ir::thread_tokens(program);
    let errors = crate::ir_check::validate(world, program);
    if !errors.is_empty() {
        return Err(format!(
            "optimized IR failed ownership validation: {errors:?}"
        ));
    }
    Ok(eliminated)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ReuseSlotInfo {
    slot_id: i64,
    bind_dest: ValueId,
    slot_index: usize,
    key: String,
    rank: i64,
    size: u32,
}

fn flows_to_return(f: &ir::FunctionIr, mut b: usize, mut v: ValueId) -> bool {
    for _ in 0..=f.blocks.len() {
        match &f.blocks[b].terminator {
            Terminator::Return(r) => return *r == v,
            Terminator::Jump { block, args }
                if args.len() == 1
                    && args[0] == v
                    && f.blocks[*block].instructions.is_empty()
                    && f.blocks[*block].params.len() == 1 =>
            {
                v = f.blocks[*block].params[0];
                b = *block;
            }
            _ => return false,
        }
    }
    false
}

const FIP_RULES: &str = r#"(sort ReuseObj)
(constructor SlotRef (i64) ReuseObj)
(constructor AllocRef (i64) ReuseObj)
(relation slot_tag (i64 String i64))
(relation alloc_tag (i64 String i64))
(rule ((= a (AllocRef aid)) (= s (SlotRef sid))
       (alloc_tag aid k r) (slot_tag sid k r))
      ((union a s)))
"#;

pub fn fip_reuse_pass(world: &World, program: &mut ir::Program) -> Result<usize, String> {
    if !crate::fip::active(world) {
        return Ok(0);
    }
    let caller_slot_fns = crate::fip::caller_slot_functions(world);
    let mut matched = 0usize;
    for f in program.functions.values_mut() {
        let Some(src_fn) = world.functions.get(&f.name).filter(|s| s.fip) else {
            continue;
        };
        let n_blocks = f.blocks.len();
        if n_blocks == 0 {
            continue;
        }
        let mut preds = vec![vec![]; n_blocks];
        let mut succs = vec![vec![]; n_blocks];
        let mut indegree = vec![0usize; n_blocks];
        for (id, b) in f.blocks.iter().enumerate() {
            let targets = match &b.terminator {
                Terminator::Jump { block, .. } => vec![*block],
                Terminator::Branch { yes, no, .. } => vec![*yes, *no],
                Terminator::Match { arms, .. } => arms.iter().map(|(_, t)| *t).collect(),
                Terminator::Return(_) | Terminator::Unreachable => vec![],
            };
            for t in targets {
                if !succs[id].contains(&t) {
                    succs[id].push(t);
                    preds[t].push(id);
                    indegree[t] += 1;
                }
            }
        }
        let mut reachable = vec![false; n_blocks];
        let mut stack = vec![0usize];
        while let Some(b) = stack.pop() {
            if reachable[b] {
                continue;
            }
            reachable[b] = true;
            for &s in &succs[b] {
                stack.push(s);
            }
        }
        let mut order = Vec::with_capacity(n_blocks);
        let mut queue = std::collections::VecDeque::new();
        for (b, &deg) in indegree.iter().enumerate() {
            if deg == 0 {
                queue.push_back(b);
            }
        }
        while let Some(b) = queue.pop_front() {
            order.push(b);
            for &s in &succs[b] {
                indegree[s] -= 1;
                if indegree[s] == 0 {
                    queue.push_back(s);
                }
            }
        }
        if order.len() != n_blocks {
            continue;
        }

        f.fip_join_slots.clear();
        let mut next_join_id = f.values.keys().copied().max().map_or(0, |m| m + 1);
        let mut exit_slots: Vec<Vec<ReuseSlotInfo>> = vec![vec![]; n_blocks];
        let mut exit_caller_used = vec![false; n_blocks];
        let ret_slot_key = crate::fip::box_inner(world, &src_fn.result)
            .and_then(|inner| crate::fip::slot_key(world, &src_fn.generics, inner).ok());

        for &b in &order {
            if !reachable[b] {
                continue;
            }
            let r_preds: Vec<usize> = preds[b].iter().copied().filter(|&p| reachable[p]).collect();
            let non_never_preds: Vec<usize> = r_preds
                .iter()
                .copied()
                .filter(|&p| {
                    !matches!(
                        &f.blocks[p].terminator,
                        Terminator::Jump { block, args }
                            if *block == b
                                && args.iter().any(|id| {
                                    f.values.get(id).is_some_and(|t| t.name() == "Never")
                                })
                    )
                })
                .collect();
            let active_preds = if non_never_preds.is_empty() {
                &r_preds
            } else {
                &non_never_preds
            };
            let mut live: Vec<ReuseSlotInfo> =
                if let Some((&first, rest)) = active_preds.split_first() {
                    if rest.is_empty() {
                        exit_slots[first].clone()
                    } else {
                        let mut joined = Vec::new();
                        let mut used: Vec<Vec<bool>> = rest
                            .iter()
                            .map(|&p| vec![false; exit_slots[p].len()])
                            .collect();
                        let mut used_first = vec![false; exit_slots[first].len()];
                        // First keep identical slots (same bind_dest & slot_index) across all live predecessors.
                        for (i0, s0) in exit_slots[first].iter().enumerate() {
                            let mut matched_indices = Vec::with_capacity(rest.len());
                            let mut all_have = true;
                            for (pi, &p) in rest.iter().enumerate() {
                                if let Some(idx) =
                                    exit_slots[p].iter().enumerate().position(|(idx, o)| {
                                        !used[pi][idx]
                                            && o.bind_dest == s0.bind_dest
                                            && o.slot_index == s0.slot_index
                                    })
                                {
                                    matched_indices.push(idx);
                                } else {
                                    all_have = false;
                                    break;
                                }
                            }
                            if all_have {
                                used_first[i0] = true;
                                for (pi, idx) in matched_indices.into_iter().enumerate() {
                                    used[pi][idx] = true;
                                }
                                joined.push(s0.clone());
                            }
                        }
                        // Next merge remaining branch-local slots that share the same slot key across all live predecessors.
                        for (i0, s0) in exit_slots[first].iter().enumerate() {
                            if used_first[i0] {
                                continue;
                            }
                            let mut matched_indices = Vec::with_capacity(rest.len());
                            let mut all_have = true;
                            for (pi, &p) in rest.iter().enumerate() {
                                if let Some(idx) = exit_slots[p]
                                    .iter()
                                    .enumerate()
                                    .position(|(idx, o)| !used[pi][idx] && o.key == s0.key)
                                {
                                    matched_indices.push(idx);
                                } else {
                                    all_have = false;
                                    break;
                                }
                            }
                            if all_have {
                                used_first[i0] = true;
                                let canon_bind = next_join_id;
                                next_join_id += 1;
                                let mut incoming = Vec::with_capacity(active_preds.len());
                                incoming.push((first, s0.bind_dest, s0.slot_index));
                                for (pi, idx) in matched_indices.into_iter().enumerate() {
                                    used[pi][idx] = true;
                                    let p = rest[pi];
                                    let sp = &exit_slots[p][idx];
                                    incoming.push((p, sp.bind_dest, sp.slot_index));
                                }
                                f.fip_join_slots.entry(b).or_default().push(
                                    crate::fip::FipJoinSlot {
                                        dest_slot: (canon_bind, 0),
                                        incoming,
                                    },
                                );
                                joined.push(ReuseSlotInfo {
                                    slot_id: 0,
                                    bind_dest: canon_bind,
                                    slot_index: 0,
                                    key: s0.key.clone(),
                                    rank: 0,
                                    size: s0.size,
                                });
                            }
                        }
                        joined
                    }
                } else {
                    vec![]
                };
            let mut caller_used = active_preds.iter().any(|&p| exit_caller_used[p]);

            let mut next_slot_id: i64 = 0;
            let mut next_alloc_id: i64 = 0;
            let mut key_ranks: BTreeMap<String, i64> = BTreeMap::new();
            let mut code = FIP_RULES.to_owned();
            for s in &mut live {
                let r = *key_ranks.get(&s.key).unwrap_or(&0);
                key_ranks.insert(s.key.clone(), r + 1);
                s.slot_id = next_slot_id;
                s.rank = r;
                code.push_str(&format!("(let sl{0} (SlotRef {0}))\n", s.slot_id));
                code.push_str(&format!(
                    "(slot_tag {} {:?} {})\n",
                    s.slot_id, s.key, s.rank
                ));
                next_slot_id += 1;
            }

            #[derive(Clone)]
            enum PendingKind {
                BoxNew(usize),
                CallSlot(ValueId),
            }
            let mut pending: Vec<(i64, ReuseSlotInfo, PendingKind)> = vec![];
            let block_len = f.blocks[b].instructions.len();
            for (inst_idx, inst) in f.blocks[b].instructions.iter().enumerate() {
                match &inst.op {
                    Op::Bind { pattern, value } => {
                        let val_ty = &f.values[value];
                        for (slot_index, (_path, inner_ty)) in
                            crate::fip::pattern_box_slots(world, pattern, val_ty)
                                .into_iter()
                                .enumerate()
                        {
                            let (key, size) =
                                crate::fip::slot_key(world, &src_fn.generics, &inner_ty)?;
                            let r = *key_ranks.get(&key).unwrap_or(&0);
                            key_ranks.insert(key.clone(), r + 1);
                            let sid = next_slot_id;
                            next_slot_id += 1;
                            code.push_str(&format!("(let sl{sid} (SlotRef {sid}))\n"));
                            code.push_str(&format!("(slot_tag {sid} {key:?} {r})\n"));
                            live.push(ReuseSlotInfo {
                                slot_id: sid,
                                bind_dest: inst.dest,
                                slot_index,
                                key,
                                rank: r,
                                size,
                            });
                        }
                    }
                    Op::Fip {
                        op: crate::fip::FipOp::BoxNew,
                        ..
                    } => {
                        if let Some(inner) = crate::fip::box_inner(world, &inst.ty) {
                            let (need_key, _) =
                                crate::fip::slot_key(world, &src_fn.generics, inner)?;
                            if let Some(pos) = live.iter().position(|s| s.key == need_key) {
                                let chosen = live.remove(pos);
                                let aid = next_alloc_id;
                                next_alloc_id += 1;
                                code.push_str(&format!("(let al{aid} (AllocRef {aid}))\n"));
                                code.push_str(&format!(
                                    "(alloc_tag {aid} {need_key:?} {})\n",
                                    chosen.rank
                                ));
                                pending.push((aid, chosen, PendingKind::BoxNew(inst_idx)));
                            }
                        }
                    }
                    Op::Fip {
                        op: crate::fip::FipOp::BoxNewInCaller,
                        ..
                    } => {
                        caller_used = true;
                    }
                    Op::Call { callee, .. } if caller_slot_fns.contains(callee) => {
                        if let Some(inner) = crate::fip::box_inner(world, &inst.ty) {
                            let (need_key, _) =
                                crate::fip::slot_key(world, &src_fn.generics, inner)?;
                            let is_tail = inst_idx + 1 == block_len
                                && flows_to_return(f, b, inst.dest)
                                && !caller_used
                                && ret_slot_key.as_ref().is_some_and(|(rk, _)| rk == &need_key);
                            if is_tail {
                                caller_used = true;
                                f.fip_call_slots
                                    .insert(inst.dest, crate::fip::CallSlot::CallerSlot);
                            } else if let Some(pos) = live.iter().position(|s| s.key == need_key) {
                                let chosen = live.remove(pos);
                                let aid = next_alloc_id;
                                next_alloc_id += 1;
                                code.push_str(&format!("(let al{aid} (AllocRef {aid}))\n"));
                                code.push_str(&format!(
                                    "(alloc_tag {aid} {need_key:?} {})\n",
                                    chosen.rank
                                ));
                                pending.push((aid, chosen, PendingKind::CallSlot(inst.dest)));
                            }
                        }
                    }
                    _ => {}
                }
            }

            if !pending.is_empty() {
                code.push_str("(run 4)\n");
                let mut egraph = egglog::EGraph::default();
                egraph
                    .parse_and_run_program(None, &code)
                    .map_err(|e| format!("@fip egglog constructor reuse: {e}"))?;
                for (aid, chosen, kind) in pending {
                    let verified = egraph
                        .update(|fs| -> Result<bool, egglog::Error> {
                            Ok(fs.eclass_of("AllocRef", aid)?
                                == fs.eclass_of("SlotRef", chosen.slot_id)?)
                        })
                        .map_err(|e| e.to_string())?;
                    if verified {
                        matched += 1;
                        match kind {
                            PendingKind::BoxNew(inst_idx) => {
                                if let Op::Fip { op, .. } =
                                    &mut f.blocks[b].instructions[inst_idx].op
                                {
                                    *op = crate::fip::FipOp::BoxReuse {
                                        bind_dest: chosen.bind_dest,
                                        slot_index: chosen.slot_index,
                                        size: chosen.size,
                                    };
                                }
                            }
                            PendingKind::CallSlot(dest) => {
                                f.fip_call_slots.insert(
                                    dest,
                                    crate::fip::CallSlot::ReuseSlot {
                                        bind_dest: chosen.bind_dest,
                                        slot_index: chosen.slot_index,
                                        size: chosen.size,
                                    },
                                );
                            }
                        }
                    }
                }
            }

            exit_slots[b] = live;
            exit_caller_used[b] = caller_used;
        }
    }
    Ok(matched)
}
fn operands(op: &mut Op, resolve: impl Fn(ValueId) -> ValueId) {
    match op {
        Op::Bind { value, .. }
        | Op::Write { value, .. }
        | Op::Try(value)
        | Op::Unary(_, value)
        | Op::Project(value, _) => *value = resolve(*value),
        Op::Binary(_, a, b) | Op::Index(a, b) => {
            *a = resolve(*a);
            *b = resolve(*b);
        }
        Op::Tuple(vs) | Op::Vector { args: vs, .. } => {
            for v in vs {
                *v = resolve(*v);
            }
        }
        Op::Data(_, fields) => {
            for (_, v) in fields {
                *v = resolve(*v);
            }
        }
        Op::Handle {
            state_value: Some(value),
            ..
        } => *value = resolve(*value),
        Op::Call { args, .. } | Op::Tiled { args, .. } | Op::Fip { args, .. } => {
            for a in args {
                a.value = resolve(a.value);
            }
        }
        Op::Indirect { callee, args } => {
            *callee = resolve(*callee);
            for a in args {
                a.value = resolve(a.value);
            }
        }
        _ => {}
    }
}
