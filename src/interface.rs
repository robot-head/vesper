//! Experimental source-contract export for the conservative SemVer checker.
//!
//! This is not canonical `.vhir`, an ABI description, an authority audit, or a
//! code/proof cache key. Callers must typecheck the module successfully first.
use crate::{ast, check::World};
use std::collections::{BTreeMap, BTreeSet};
use vesper_semver as api;

/// Export checked source declarations without bodies, locations or inferred
/// capability identities. Unsupported contracts remain explicit and therefore
/// cannot be certified compatible, including against themselves.
pub fn export(world: &World, package: &str) -> Result<api::Interface, String> {
    let package = vesper_loom_calculus::PackageName::parse(package)?;
    let exporter = Exporter {
        world,
        package: package.as_str(),
    };
    let mut interface = api::Interface::default();
    for (name, adt) in &world.adts {
        interface
            .exports
            .insert(exporter.id("type", name), exporter.adt(adt));
    }
    for (name, effect) in &world.effects {
        let declaration = exporter.effect(effect).unwrap_or_else(unsupported);
        interface
            .exports
            .insert(exporter.id("effect", name), declaration);
    }
    // Source traits, canonical impls and named models have no representation
    // in the current interface schema: export them as Unsupported so no
    // SemVer certificate can claim compatibility for them.
    for name in &world.elaboration.traits {
        interface.exports.insert(
            exporter.id("trait", name),
            unsupported(format!(
                "trait '{name}': trait contracts are not representable in interface schema {}",
                api::SCHEMA_VERSION
            )),
        );
    }
    for c in &world.elaboration.conformances {
        let (kind, name) = match &c.model {
            Some(m) => ("model", m.clone()),
            None => ("impl", format!("{}.{}", c.trait_name, c.self_ty)),
        };
        interface.exports.insert(
            exporter.id(kind, &name),
            unsupported(format!(
                "{kind} {} of trait '{}' for {}: conformances are not representable in this interface schema",
                c.path, c.trait_name, c.self_ty
            )),
        );
    }
    for (name, function) in &world.functions {
        // Generated trait/model implementation bodies are private.
        if world.elaboration.generated.contains_key(name) {
            continue;
        }
        let declaration = exporter
            .function(function, &function.generics)
            .map(|signature| api::Declaration::Function { signature })
            .unwrap_or_else(unsupported);
        interface
            .exports
            .insert(exporter.id("fn", name), declaration);
    }
    Ok(interface)
}

fn unsupported(description: String) -> api::Declaration {
    api::Declaration::Unsupported { description }
}
fn unknown(description: &str) -> api::Type {
    api::Type::Unsupported {
        description: description.into(),
    }
}
fn mode(mode: ast::Mode) -> api::Mode {
    match mode {
        ast::Mode::Copy => api::Mode::Copy,
        ast::Mode::Affine => api::Mode::Affine,
        ast::Mode::Linear => api::Mode::Linear,
    }
}
fn pass(pass: ast::Pass) -> api::Pass {
    match pass {
        ast::Pass::In => api::Pass::In,
        ast::Pass::Inout => api::Pass::Inout,
        ast::Pass::Sink => api::Pass::Sink,
    }
}
fn resumption(linear: bool) -> api::Resumption {
    if linear {
        api::Resumption::Linear
    } else {
        api::Resumption::Affine
    }
}
fn generics(gs: &[ast::Generic]) -> Vec<api::Generic> {
    gs.iter()
        .map(|g| {
            if g.row {
                // Source row binders admit abortable effects. Their numeric
                // `bound` field is not a continuation guarantee.
                api::Generic::Row {
                    minimum_resumption: api::Resumption::Affine,
                }
            } else {
                api::Generic::Type {
                    bound: mode(g.bound),
                }
            }
        })
        .collect()
}
fn primitive(name: &str) -> Option<api::Primitive> {
    Some(match name {
        "Unit" => api::Primitive::Unit,
        "Never" => api::Primitive::Never,
        "bool" => api::Primitive::Bool,
        "u8" => api::Primitive::U8,
        "u32" => api::Primitive::U32,
        "u64" => api::Primitive::U64,
        "i64" => api::Primitive::I64,
        "f64" => api::Primitive::F64,
        "String" => api::Primitive::String,
        _ => return None,
    })
}
fn refers_to(ty: &ast::Type, gs: &[ast::Generic]) -> bool {
    match ty {
        ast::Type::Named(n, args) => {
            gs.iter().any(|g| g.name == *n) || args.iter().any(|t| refers_to(t, gs))
        }
        ast::Type::Tuple(ts) => ts.iter().any(|t| refers_to(t, gs)),
        ast::Type::Function {
            params,
            result,
            effects,
            ..
        } => {
            params.iter().any(|(_, t)| refers_to(t, gs))
                || refers_to(result, gs)
                || effects
                    .tail
                    .as_ref()
                    .is_some_and(|n| gs.iter().any(|g| g.name == *n))
                || effects
                    .instances
                    .iter()
                    .any(|e| e.args.iter().any(|t| refers_to(t, gs)))
        }
        ast::Type::Row(_) | ast::Type::Infer | ast::Type::IntLiteral(_) => false,
    }
}
struct Exporter<'a> {
    world: &'a World,
    package: &'a str,
}
impl Exporter<'_> {
    fn id(&self, kind: &str, name: &str) -> String {
        // Builtins are compiler-provided declarations, not local definitions.
        let builtin = matches!(
            (kind, name),
            (
                "type",
                "Option" | "Result" | "RuntimeError" | "ArithOverflow"
            ) | ("effect", "Fail")
        );
        let package = if builtin { "std/core" } else { self.package };
        format!("{package}::{kind}::{name}")
    }

    // The DTO joins nominal kind with argument kinds, even for phantom type
    // parameters. Publish unconditional payload requirements here; publishing
    // the generic upper bound instead would make Option[u8] spuriously linear.
    fn nominal_mode(&self, adt: &ast::Adt) -> api::Mode {
        let bounds = adt
            .generics
            .iter()
            .map(|g| (g.name.clone(), ast::Mode::Copy))
            .collect();
        let ty = ast::Type::Named(
            adt.name.clone(),
            adt.generics
                .iter()
                .map(|g| ast::Type::named(&g.name))
                .collect(),
        );
        mode(self.world.mode(&ty, &bounds))
    }

    fn ty_mode(&self, ty: &ast::Type, gs: &[ast::Generic]) -> api::Mode {
        match ty {
            ast::Type::Named(n, args) => {
                let base = if let Some(g) = gs.iter().find(|g| g.name == *n) {
                    mode(g.bound)
                } else if n == "String" {
                    api::Mode::Affine
                } else if primitive(n).is_some() {
                    api::Mode::Copy
                } else if let Some(adt) = self.world.adts.get(n) {
                    self.nominal_mode(adt)
                } else {
                    api::Mode::Linear
                };
                args.iter().fold(base, |m, t| m.max(self.ty_mode(t, gs)))
            }
            ast::Type::Tuple(ts) => ts
                .iter()
                .fold(api::Mode::Copy, |m, t| m.max(self.ty_mode(t, gs))),
            ast::Type::Function { mode: m, .. } => mode(*m),
            _ => api::Mode::Linear,
        }
    }

    fn ty(&self, ty: &ast::Type, gs: &[ast::Generic]) -> api::Type {
        match ty {
            ast::Type::Named(n, args) => {
                if let Some(index) = gs.iter().position(|g| g.name == *n) {
                    return if args.is_empty() && !gs[index].row {
                        api::Type::Parameter { index }
                    } else {
                        unknown("row or applied generic in value type")
                    };
                }
                if let Some(name) = primitive(n) {
                    return if args.is_empty() {
                        api::Type::Primitive { name }
                    } else {
                        unknown("applied primitive type")
                    };
                }
                match self.world.adts.get(n) {
                    Some(adt) if adt.generics.len() == args.len() => api::Type::Named {
                        name: self.id("type", n),
                        arguments: args.iter().map(|t| self.ty(t, gs)).collect(),
                    },
                    _ => unknown("unresolved nominal type or captured arrow binder"),
                }
            }
            ast::Type::Tuple(ts) if ts.is_empty() => api::Type::Primitive {
                name: api::Primitive::Unit,
            },
            ast::Type::Tuple(ts) => api::Type::Tuple {
                elements: ts.iter().map(|t| self.ty(t, gs)).collect(),
            },
            ast::Type::Function {
                params,
                result,
                effects,
                mode: m,
            } => {
                if refers_to(ty, gs) {
                    return unknown(
                        "arrows capturing outer generic binders require a richer schema",
                    );
                }
                if !effects.instances.is_empty() || effects.tail.is_some() {
                    return unknown("effectful arrows require lexical capability binder scopes");
                }
                // Nested DTO functions have closed binder contexts. Captured
                // generic references become Unsupported under this empty scope.
                api::Type::Function {
                    signature: Box::new(api::Function {
                        generics: vec![],
                        mode: mode(*m),
                        parameters: params
                            .iter()
                            .map(|(p, t)| api::Parameter {
                                pass: pass(*p),
                                ty: self.ty(t, &[]),
                                projection: api::Projection::All,
                            })
                            .collect(),
                        result: self.ty(result, &[]),
                        result_mode: self.ty_mode(result, &[]),
                        effects: api::Effects::default(),
                    }),
                }
            }
            ast::Type::Row(_) => unknown("nominal row arguments are not representable"),
            ast::Type::Infer | ast::Type::IntLiteral(_) => unknown("unresolved source type"),
        }
    }

    fn effects(&self, effects: &ast::Effects, gs: &[ast::Generic]) -> Result<api::Effects, String> {
        let mut capabilities = BTreeMap::new();
        for effect in &effects.instances {
            if effect.label.is_some() {
                return Err("named capabilities require lexical binder scopes".into());
            }
            let decl = self
                .world
                .effects
                .get(&effect.name)
                .ok_or("unresolved effect declaration")?;
            if decl.generics.len() != effect.args.len() {
                return Err("effect argument arity mismatch".into());
            }
            let arguments: Vec<_> = effect.args.iter().map(|t| self.ty(t, gs)).collect();
            let name = self.id("effect", &effect.name);
            // Unnamed source capabilities resolve by effect plus type arguments.
            // Canonical DTO arguments remove generic names and source positions.
            let identity = format!("{name}:{}", serde_json::to_string(&arguments).unwrap());
            capabilities.insert(
                identity.clone(),
                api::Capability {
                    identity,
                    effect: name,
                    arguments,
                    resumption: resumption(decl.linear),
                },
            );
        }
        let mut row_variables = BTreeSet::new();
        if let Some(tail) = &effects.tail {
            let index = gs
                .iter()
                .position(|g| g.row && g.name == *tail)
                .ok_or("unresolved effect row binder")?;
            row_variables.insert(index);
        }
        Ok(api::Effects {
            capabilities: capabilities.into_values().collect(),
            row_variables,
        })
    }

    fn function(&self, f: &ast::Function, gs: &[ast::Generic]) -> Result<api::Function, String> {
        Ok(api::Function {
            generics: generics(gs),
            mode: api::Mode::Copy,
            parameters: f
                .params
                .iter()
                .map(|p| api::Parameter {
                    pass: pass(p.pass),
                    ty: self.ty(&p.ty, gs),
                    projection: if p.projections.is_empty() {
                        api::Projection::All
                    } else {
                        api::Projection::Paths {
                            paths: p.projections.iter().cloned().collect(),
                        }
                    },
                })
                .collect(),
            result: self.ty(&f.result, gs),
            result_mode: self.ty_mode(&f.result, gs),
            effects: self.effects(&f.effects, gs)?,
        })
    }

    fn adt(&self, adt: &ast::Adt) -> api::Declaration {
        if adt.is_enum && adt.variants.iter().any(|v| v.named) {
            return unsupported("named enum payloads require field-name contracts".into());
        }
        let gs = generics(&adt.generics);
        let m = self.nominal_mode(adt);
        if adt.is_enum {
            api::Declaration::Enum {
                generics: gs,
                mode: m,
                open: false,
                variants: adt
                    .variants
                    .iter()
                    .map(|v| {
                        (
                            v.name.clone(),
                            v.fields
                                .iter()
                                .map(|f| self.ty(&f.ty, &adt.generics))
                                .collect(),
                        )
                    })
                    .collect(),
            }
        } else {
            api::Declaration::Struct {
                generics: gs,
                mode: m,
                fields: api::Fields::Public {
                    fields: adt
                        .variants
                        .iter()
                        .flat_map(|v| &v.fields)
                        .map(|f| (f.name.clone(), self.ty(&f.ty, &adt.generics)))
                        .collect(),
                },
            }
        }
    }

    fn effect(&self, e: &ast::EffectDecl) -> Result<api::Declaration, String> {
        let mut operations = BTreeMap::new();
        for op in &e.ops {
            if !op.generics.is_empty()
                || op.body.is_some()
                || !op.effects.instances.is_empty()
                || op.effects.tail.is_some()
            {
                return Err(
                    "effect-local operation generics, bodies or outgoing effects are not supported"
                        .into(),
                );
            }
            operations.insert(op.name.clone(), self.function(op, &e.generics)?);
        }
        Ok(api::Declaration::Effect {
            generics: generics(&e.generics),
            sealed: false,
            resumption: resumption(e.linear),
            operations,
        })
    }
}
