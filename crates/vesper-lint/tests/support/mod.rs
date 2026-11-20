//! Shared by tests/differential.rs and examples/abba.rs: checked facts of
//! repository sources (the fact extraction of `checked_source` in
//! src/tooling/lint.rs, through the compiler's public modules), the
//! conversion of the seed's fact types to this crate's, and a small
//! deterministic generator.
#![allow(dead_code)]

use lint_seed as seed;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use vesper::{ast, check, ir, parser};
use vesper_core::ast::{Effect, Pass, Type};

/// The repository root.
pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every file under the repository with extension `ext`, sorted, skipping
/// build output and VCS directories.
pub fn files(ext: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                if !name.starts_with('.') && name != "target" && name != "node_modules" {
                    stack.push(path);
                }
            } else if path.extension().is_some_and(|e| e == ext) {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// The checked facts of `source`, as `checked_source` builds them, or
/// `None` when it does not parse or check.
pub fn facts(source: &str) -> Option<seed::CheckedProgram> {
    let parsed = parser::parse(source);
    if !parsed.diagnostics.is_empty() {
        return None;
    }
    let checked = check::check(&parsed.module);
    if !checked.diagnostics.is_empty() {
        return None;
    }
    let program = ir::lower(&checked);
    let world = &checked.world;
    let mut facts = seed::CheckedProgram {
        functions: BTreeMap::new(),
    };
    for (name, f) in &world.functions {
        let mut actions = vec![];
        if let Some(body) = program.functions.get(name) {
            for instruction in body.blocks.iter().flat_map(|b| &b.instructions) {
                let key = (instruction.span.start, instruction.span.end);
                if world.overflow_ops.contains(&key) {
                    actions.push(seed::Action::Perform(Effect {
                        label: None,
                        name: "Fail".into(),
                        args: vec![Type::named("ArithOverflow")],
                    }));
                }
                if world.partial_ops.contains(&key) {
                    actions.push(seed::Action::Perform(Effect {
                        label: None,
                        name: "Fail".into(),
                        args: vec![Type::named("RuntimeError")],
                    }));
                }
                match &instruction.op {
                    ir::Op::Call {
                        callee, capability, ..
                    } => {
                        let vars = world.calls.get(&key).cloned().unwrap_or_default();
                        if world.functions.contains_key(callee) {
                            actions.push(seed::Action::Call {
                                callee: callee.clone(),
                                substitutions: vars,
                            });
                        } else if let Some((effect, operation)) = callee.split_once("::") {
                            match world
                                .effects
                                .get(effect)
                                .map(|d| (d, d.ops.iter().find(|f| f.name == operation)))
                            {
                                Some((declaration, Some(op))) => {
                                    if !op.generics.is_empty()
                                        || op.body.is_some()
                                        || op.effects.tail.is_some()
                                        || !op.effects.instances.is_empty()
                                    {
                                        actions.push(seed::Action::Opaque(
                                            "operation-local generics/outgoing effects".into(),
                                        ));
                                    } else {
                                        actions.push(seed::Action::Perform(Effect {
                                            label: capability.clone(),
                                            name: effect.into(),
                                            args: declaration
                                                .generics
                                                .iter()
                                                .map(|g| {
                                                    vars.get(&g.name)
                                                        .cloned()
                                                        .unwrap_or(Type::Infer)
                                                })
                                                .collect(),
                                        }));
                                    }
                                }
                                Some((_, None)) => actions.push(seed::Action::Opaque(
                                    "unresolved effect operation".into(),
                                )),
                                None => actions.push(seed::Action::Opaque(
                                    "unresolved qualified callee".into(),
                                )),
                            }
                        } else {
                            actions.push(seed::Action::Opaque("unresolved direct callee".into()));
                        }
                    }
                    ir::Op::Read {
                        root,
                        path,
                        pass,
                        projections,
                    } => {
                        let moves_noncopy = *pass == Pass::Sink
                            && world.mode(&instruction.ty, &body.bounds) != ast::Mode::Copy;
                        if projections.is_empty() {
                            actions.push(seed::Action::Access {
                                root: root.clone(),
                                path: path.clone(),
                                pass: *pass,
                                moves_noncopy,
                            });
                        } else {
                            for projection in projections {
                                let mut selected = path.clone();
                                selected.extend(projection.clone());
                                actions.push(seed::Action::Access {
                                    root: root.clone(),
                                    path: selected,
                                    pass: *pass,
                                    moves_noncopy,
                                });
                            }
                        }
                    }
                    ir::Op::Write { place, .. } => {
                        let mut pieces = place.split('.');
                        let root = pieces.next().unwrap_or("");
                        if root.is_empty() {
                            actions
                                .push(seed::Action::Opaque("unresolved assignment place".into()));
                        } else {
                            actions.push(seed::Action::Access {
                                root: root.into(),
                                path: pieces.map(str::to_owned).collect(),
                                pass: Pass::Inout,
                                moves_noncopy: false,
                            });
                        }
                    }
                    ir::Op::Try(value) => match body.values.get(value) {
                        Some(Type::Named(name, arguments))
                            if name == "Result" && arguments.len() == 2 =>
                        {
                            actions.push(seed::Action::Perform(Effect {
                                label: None,
                                name: "Fail".into(),
                                args: vec![arguments[1].clone()],
                            }));
                        }
                        _ => actions.push(seed::Action::Opaque(
                            "unresolved Result error type for '?'".into(),
                        )),
                    },
                    ir::Op::Indirect { .. } => {
                        actions.push(seed::Action::Opaque("indirect call".into()))
                    }
                    ir::Op::Closure { .. } => {
                        actions.push(seed::Action::Opaque("closure capture/effect scope".into()))
                    }
                    ir::Op::Handle { .. } => {
                        actions.push(seed::Action::Opaque("handler capability scope".into()))
                    }
                    _ => (),
                }
            }
        } else if f.body.is_some() {
            actions.push(seed::Action::Opaque("checked body missing from IR".into()));
        }
        let parameters = f
            .params
            .iter()
            .map(|p| seed::ParameterFacts {
                parameter: p.clone(),
                struct_fields: world
                    .adts
                    .get(p.ty.name())
                    .filter(|a| !a.is_enum)
                    .map(|a| {
                        a.variants
                            .iter()
                            .flat_map(|v| &v.fields)
                            .map(|f| f.name.clone())
                            .collect()
                    })
                    .unwrap_or_default(),
            })
            .collect();
        facts.functions.insert(
            name.clone(),
            seed::FunctionFacts {
                declaration: f.clone(),
                actions,
                parameters,
                source_owned: !world.elaboration.generated.contains_key(name),
            },
        );
    }
    Some(facts)
}

/// The same program as this crate's fact types.
pub fn to_port(p: &seed::CheckedProgram) -> vesper_lint::CheckedProgram {
    vesper_lint::CheckedProgram {
        functions: p
            .functions
            .iter()
            .map(|(name, f)| {
                (
                    name.clone(),
                    vesper_lint::FunctionFacts {
                        declaration: f.declaration.clone(),
                        actions: f
                            .actions
                            .iter()
                            .map(|a| match a {
                                seed::Action::Perform(e) => vesper_lint::Action::Perform(e.clone()),
                                seed::Action::Call {
                                    callee,
                                    substitutions,
                                } => vesper_lint::Action::Call {
                                    callee: callee.clone(),
                                    substitutions: substitutions.clone(),
                                },
                                seed::Action::Access {
                                    root,
                                    path,
                                    pass,
                                    moves_noncopy,
                                } => vesper_lint::Action::Access {
                                    root: root.clone(),
                                    path: path.clone(),
                                    pass: *pass,
                                    moves_noncopy: *moves_noncopy,
                                },
                                seed::Action::Opaque(r) => vesper_lint::Action::Opaque(r.clone()),
                            })
                            .collect(),
                        parameters: f
                            .parameters
                            .iter()
                            .map(|p| vesper_lint::ParameterFacts {
                                parameter: p.parameter.clone(),
                                struct_fields: p.struct_fields.clone(),
                            })
                            .collect(),
                        source_owned: f.source_owned,
                    },
                )
            })
            .collect(),
    }
}

/// Every source program of the repository corpus: (path, facts) of each
/// `.ves` file up to `max_bytes` that checks as one module.
pub fn corpus(max_bytes: u64) -> Vec<(PathBuf, seed::CheckedProgram)> {
    let mut out = Vec::new();
    for path in files("ves") {
        if std::fs::metadata(&path).map_or(true, |m| m.len() > max_bytes) {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        // The checker may reject a source by panicking on shapes it does
        // not support; such a file has no facts.
        let result = std::panic::catch_unwind(|| facts(&source));
        if let Ok(Some(f)) = result {
            out.push((path, f));
        }
    }
    out
}

/// xorshift64*.
pub struct Rng(pub u64);
impl Rng {
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }
    pub fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len() as u64) as usize]
    }
}
