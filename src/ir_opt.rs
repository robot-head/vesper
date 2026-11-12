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
    ir::thread_tokens(program);
    let errors = crate::ir_check::validate(world, program);
    if !errors.is_empty() {
        return Err(format!(
            "optimized IR failed ownership validation: {errors:?}"
        ));
    }
    Ok(eliminated)
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
        Op::Tuple(vs) => {
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
        Op::Call { args, .. } => {
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
