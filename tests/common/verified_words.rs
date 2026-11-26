// The input words of @vesper/verified-codegen's `compile_words` and its
// front-half entries (#1886): monomorphization's input (check-data's
// names, types and world, the Place IR program, the expression rows of
// its payload pool, the entry, the instance limit, the functions'
// generics and the calls' type arguments), the lowering's extras, and the
// target words of `compile_words`. Shared by
// tests/self_host_verified_codegen.rs and the vesper-hybrid
// `verified_codegen` swap.

#![allow(dead_code)]

use super::ir_words;
use vesper::verified_codegen as verified;

fn bytes_of(words: &[u64]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

pub fn put_string(s: &str, words: &mut Vec<u64>) {
    words.push(s.len() as u64);
    for chunk in s.as_bytes().chunks(8) {
        let mut b = [0u8; 8];
        b[..chunk.len()].copy_from_slice(chunk);
        words.push(u64::from_le_bytes(b));
    }
}

pub fn lowering_input(
    world: &vesper::check::World,
    program: &vesper::ir::Program,
    entry: &str,
    options: &verified::Options,
) -> Option<Vec<u8>> {
    if !ir_words::encodable(world, program) {
        return None;
    }
    let mut t = ir_words::Tables::of(program);
    t.label_names();
    let section = t.section();
    let encoded = t.enc.world(world);
    let mut words = expr_rows(&mut t)?;
    lowering_extras(world, entry, options, &mut words);
    let mut bytes = t.enc.finish(&encoded, &section);
    bytes.extend(bytes_of(&words));
    Some(bytes)
}

/// The lowering's extra section: the entry, the limits, whether the region
/// prelude is active and the names of the world's functions.
pub fn lowering_extras(
    world: &vesper::check::World,
    entry: &str,
    options: &verified::Options,
    words: &mut Vec<u64>,
) {
    put_string(entry, words);
    words.push(options.max_functions as u64);
    words.push(options.max_rows as u64);
    words.push(u64::from(vesper::regions::active(world)));
    words.push(world.functions.len() as u64);
    for name in world.functions.keys() {
        put_string(name, words);
    }
}

/// One clause of a `handle` expression: its operation name, patterns
/// (0 `_`, 1 a name and the name, 2 any other) and body span.
pub fn clause_row(t: &mut ir_words::Tables, c: &vesper::ast::Clause, out: &mut Vec<u64>) {
    use vesper::ast::PatternKind;
    let n = t.enc.name(&c.name);
    out.push(n);
    out.push(c.pats.len() as u64);
    for p in &c.pats {
        match &p.kind {
            PatternKind::Wild => out.push(0),
            PatternKind::Bind(b) => {
                out.push(1);
                let n = t.enc.name(b);
                out.push(n);
            }
            _ => out.push(2),
        }
    }
    out.extend([c.body.span.start as u64, c.body.span.end as u64]);
}

/// The expressions of the payload pool, by pool index: 0 for an operation;
/// else 1 (a closure) or 2 (a `handle`), the span and the free names
/// (`check::free_names`). A `handle` adds its effect (name, label + 1 or
/// 0, argument types), whether it is stateful, the body span, the `return`
/// clause and the operation clauses. `None` when a type cannot be encoded.
pub fn expr_rows(t: &mut ir_words::Tables) -> Option<Vec<u64>> {
    use vesper::ast::ExprKind;
    let items = t.pool.items.clone();
    let mut out = vec![items.len() as u64];
    for item in &items {
        let ir_words::Payload::Expr(e) = item else {
            out.push(0);
            continue;
        };
        let handle = matches!(e.kind, ExprKind::Handle { .. });
        out.push(if handle { 2 } else { 1 });
        out.extend([e.span.start as u64, e.span.end as u64]);
        let free = vesper::check::free_names(e);
        out.push(free.len() as u64);
        for n in &free {
            let i = t.enc.name(n);
            out.push(i);
        }
        let ExprKind::Handle {
            effect,
            state,
            body,
            ret,
            ops,
        } = &e.kind
        else {
            continue;
        };
        if !effect.args.iter().all(ir_words::literal_fits) {
            return None;
        }
        let n = t.enc.name(&effect.name);
        out.push(n);
        let label = effect.label.as_ref().map_or(0, |l| t.enc.name(l) + 1);
        out.push(label);
        out.push(effect.args.len() as u64);
        for a in &effect.args {
            let i = t.enc.ty(a);
            out.push(i);
        }
        out.push(u64::from(state.is_some()));
        out.extend([body.span.start as u64, body.span.end as u64]);
        clause_row(t, ret, &mut out);
        out.push(ops.len() as u64);
        for c in ops {
            clause_row(t, c, &mut out);
        }
    }
    Some(out)
}

/// The input of `mono_words`, and the payload pool its program refers to.
pub fn mono_input(
    world: &vesper::check::World,
    program: &vesper::ir::Program,
    entry: &str,
    max_instances: usize,
    lowering: Option<&verified::Options>,
) -> Option<(Vec<u8>, ir_words::Pool)> {
    mono_input_with(world, program, entry, max_instances, lowering, |_, _| {})
}

/// The input of `coverage_text` (mono.ves `coverage_input`) for
/// `verified::coverage(world, roots, optimize)`: `mono_input`'s with the
/// roots between the mono extras and the lowering's. Each root is a name,
/// a class (0 plannable, 1 generic, 2 effect-polymorphic) and the text of
/// its user effects (a name), as coverage.rs classifies them.
pub fn coverage_input(
    world: &vesper::check::World,
    roots: Option<&std::collections::BTreeSet<String>>,
    optimize: bool,
) -> Option<Vec<u8>> {
    let mut program = vesper::ir::lower_world(world);
    if optimize && vesper::ir_opt::optimize(world, &mut program).is_err() {
        return None;
    }
    let roots: Vec<String> = match roots {
        Some(r) => r.iter().cloned().collect(),
        None => program
            .functions
            .keys()
            .filter(|n| !n.starts_with('$'))
            .cloned()
            .collect(),
    };
    let user = |name: &str| name != "Fail" && name != vesper::regions::REGION_EFFECT;
    let classes: Vec<(String, u64, String)> = roots
        .iter()
        .map(|root| {
            if world
                .functions
                .get(root)
                .is_some_and(|f| !f.generics.is_empty())
            {
                return (root.clone(), 1, String::new());
            }
            if program
                .functions
                .get(root)
                .is_some_and(|f| f.effects.instances.iter().any(|e| user(&e.name)))
            {
                let effects: Vec<String> = world
                    .functions
                    .get(root)
                    .map(|f| {
                        f.effects
                            .instances
                            .iter()
                            .filter(|e| user(&e.name))
                            .map(|e| e.name.clone())
                            .collect()
                    })
                    .unwrap_or_default();
                return (root.clone(), 2, effects.join(", "));
            }
            (root.clone(), 0, String::new())
        })
        .collect();
    let entry = classes
        .iter()
        .find(|c| c.1 == 0)
        .map(|c| c.0.clone())
        .unwrap_or_default();
    let options = verified::Options {
        max_functions: usize::MAX,
        max_rows: usize::MAX,
        ..verified::Options::new(verified::Target::host()?)
    };
    mono_input_with(
        world,
        &program,
        &entry,
        usize::MAX,
        Some(&options),
        |t, words| {
            words.push(classes.len() as u64);
            for (name, class, effects) in &classes {
                let n = t.enc.name(name);
                words.push(n);
                words.push(*class);
                let e = t.enc.name(effects);
                words.push(e);
            }
        },
    )
    .map(|(bytes, _)| bytes)
}

/// `mono_input` with `between` writing a section after the mono extras.
pub fn mono_input_with(
    world: &vesper::check::World,
    program: &vesper::ir::Program,
    entry: &str,
    max_instances: usize,
    lowering: Option<&verified::Options>,
    between: impl FnOnce(&mut ir_words::Tables, &mut Vec<u64>),
) -> Option<(Vec<u8>, ir_words::Pool)> {
    if !ir_words::encodable(world, program)
        || !world
            .calls
            .values()
            .all(|m| m.values().all(ir_words::literal_fits))
    {
        return None;
    }
    let mut t = ir_words::Tables::of(program);
    t.label_names();
    let section = t.section();
    // Monomorphization reads only the ADTs of the world section (which
    // names are types), so without the lowering the expression types and
    // spans stay out.
    let encoded = if lowering.is_some() {
        t.enc.world(world)
    } else {
        t.enc.world(&vesper::check::World {
            adts: world.adts.clone(),
            effects: world.effects.clone(),
            ..Default::default()
        })
    };
    let mut words = expr_rows(&mut t)?;
    let e = t.enc.name(entry);
    words.push(e);
    words.push(max_instances as u64);
    // Only functions with a body are instantiated or called by instance,
    // and only the type arguments of the program's calls are looked up.
    let functions: Vec<_> = world
        .functions
        .iter()
        .filter(|(name, _)| program.functions.contains_key(*name))
        .collect();
    let spans: std::collections::BTreeSet<(usize, usize)> = program
        .functions
        .values()
        .flat_map(|f| &f.blocks)
        .flat_map(|b| &b.instructions)
        .filter(|i| matches!(i.op, vesper::ir::Op::Call { .. }))
        .map(|i| (i.span.start, i.span.end))
        .collect();
    let calls: Vec<_> = world
        .calls
        .iter()
        .filter(|(k, _)| spans.contains(k))
        .collect();
    words.push(functions.len() as u64);
    for (name, f) in functions {
        let n = t.enc.name(name);
        words.push(n);
        words.push(f.generics.len() as u64);
        for g in &f.generics {
            let n = t.enc.name(&g.name);
            words.push(n);
            words.push(u64::from(g.row));
        }
    }
    words.push(calls.len() as u64);
    for ((start, end), args) in calls {
        words.push(*start as u64);
        words.push(*end as u64);
        words.push(args.len() as u64);
        for (k, ty) in args {
            let n = t.enc.name(k);
            words.push(n);
            let i = t.enc.ty(ty);
            words.push(i);
        }
    }
    between(&mut t, &mut words);
    if let Some(options) = lowering {
        lowering_extras(world, entry, options, &mut words);
    }
    let mut bytes = t.enc.finish(&encoded, &section);
    bytes.extend(bytes_of(&words));
    Some((bytes, t.pool))
}

pub fn target_words(options: &verified::Options) -> [u64; 5] {
    let arch = match options.target.arch {
        verified::Arch::X86_64 => 0,
        verified::Arch::Aarch64 => 1,
        verified::Arch::Riscv64 => 2,
    };
    let os = match options.target.os {
        verified::Os::Linux => 0,
        verified::Os::MacOs => 1,
        verified::Os::Windows => 2,
    };
    let protocol = match options.protocol {
        verified::ResultProtocol::ExitStatus => 0,
        verified::ResultProtocol::Words => 1,
    };
    [
        arch,
        os,
        protocol,
        options.max_code_bytes as u64,
        u64::from(options.require_certificate),
    ]
}
