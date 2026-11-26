//! The checked world the Vesper driver holds (#2024): the session hands
//! over the world's declarations once (`WorldDecls`), and the driver's
//! checks and exports read them in Vesper (modules/driver/src/world.ves).
//!
//! The declarations cross as a byte stream until the checker builds the
//! world in Vesper itself (#2024 slice 3).

use super::*;

impl Session {
    pub(super) fn world_step(&mut self, step: Step) -> Result<i64, String> {
        match step {
            Step::WorldDecls => {
                self.text = world_words::encode(&self.checked()?.world);
                Ok(0)
            }
            _ => Err(format!("driver: step {} is not a world step", step as u32)),
        }
    }
}

/// The checked world's declarations, which the Vesper driver holds
/// (`world.ves` of modules/driver) and the interface export
/// (`interface.ves`), `build`'s main check and `run`'s entry check read,
/// as a byte stream: LEB128 numbers, and texts as their length and bytes.
///
/// ```text
/// index:  n adts (key, generic count, nominal mode)
///         n effects (key, generic count, linear)
/// adts:   key, G, is_enum, n variants (name, named, n fields (name, T))
/// effects: key, linear, G, n ops (name, local, F)
/// traits: n names
/// conformances: n (has_model, model, path, trait, self type)
/// functions: n (key, generated, G, F)
/// G: n (name, row, bound)
/// F: n params (pass, n projections (n parts), T), T result, E
/// E: n instances (labeled, name, n args T), has_tail, tail
/// T: 0 named (name, n T) | 1 tuple (n T) | 2 function (n (pass, T), T, E,
///    mode) | 3 row | 4 unresolved
/// ```
///
/// Modes are 0 copy, 1 affine, 2 linear; passes 0 in, 1 inout, 2 sink.
mod world_words {
    use std::collections::BTreeMap;
    use vesper::{ast, check::World};

    #[derive(Default)]
    struct Out(Vec<u8>);

    impl Out {
        fn n(&mut self, mut v: u64) {
            loop {
                let b = (v & 0x7f) as u8;
                v >>= 7;
                if v == 0 {
                    self.0.push(b);
                    return;
                }
                self.0.push(b | 0x80);
            }
        }
        fn len(&mut self, n: usize) {
            self.n(n as u64);
        }
        fn b(&mut self, v: bool) {
            self.0.push(u8::from(v));
        }
        fn s(&mut self, s: &str) {
            self.len(s.len());
            self.0.extend_from_slice(s.as_bytes());
        }
        fn mode(&mut self, m: ast::Mode) {
            self.n(m as u64);
        }
        fn pass(&mut self, p: ast::Pass) {
            self.n(p as u64);
        }
        fn generics(&mut self, gs: &[ast::Generic]) {
            self.len(gs.len());
            for g in gs {
                self.s(&g.name);
                self.b(g.row);
                self.mode(g.bound);
            }
        }
        fn effects(&mut self, e: &ast::Effects) {
            self.len(e.instances.len());
            for i in &e.instances {
                self.b(i.label.is_some());
                self.s(&i.name);
                self.len(i.args.len());
                for t in &i.args {
                    self.ty(t);
                }
            }
            self.b(e.tail.is_some());
            self.s(e.tail.as_deref().unwrap_or(""));
        }
        fn ty(&mut self, t: &ast::Type) {
            match t {
                ast::Type::Named(n, args) => {
                    self.n(0);
                    self.s(n);
                    self.len(args.len());
                    for a in args {
                        self.ty(a);
                    }
                }
                ast::Type::Tuple(ts) => {
                    self.n(1);
                    self.len(ts.len());
                    for a in ts {
                        self.ty(a);
                    }
                }
                ast::Type::Function {
                    params,
                    result,
                    effects,
                    mode,
                } => {
                    self.n(2);
                    self.len(params.len());
                    for (p, t) in params {
                        self.pass(*p);
                        self.ty(t);
                    }
                    self.ty(result);
                    self.effects(effects);
                    self.mode(*mode);
                }
                ast::Type::Row(_) => self.n(3),
                ast::Type::Infer | ast::Type::IntLiteral(_) => self.n(4),
            }
        }
        fn function(&mut self, f: &ast::Function) {
            self.len(f.params.len());
            for p in &f.params {
                self.pass(p.pass);
                self.len(p.projections.len());
                for path in &p.projections {
                    self.len(path.len());
                    for part in path {
                        self.s(part);
                    }
                }
                self.ty(&p.ty);
            }
            self.ty(&f.result);
            self.effects(&f.effects);
        }
    }

    // `Exporter::nominal_mode` of src/interface.rs.
    fn nominal_mode(world: &World, adt: &ast::Adt) -> ast::Mode {
        let bounds = adt
            .generics
            .iter()
            .map(|g| (g.name.clone(), ast::Mode::Copy))
            .collect::<BTreeMap<_, _>>();
        let ty = ast::Type::Named(
            adt.name.clone(),
            adt.generics
                .iter()
                .map(|g| ast::Type::named(&g.name))
                .collect(),
        );
        world.mode(&ty, &bounds)
    }

    pub(super) fn encode(world: &World) -> Vec<u8> {
        let mut o = Out::default();
        o.len(world.adts.len());
        for (name, adt) in &world.adts {
            o.s(name);
            o.len(adt.generics.len());
            o.mode(nominal_mode(world, adt));
        }
        o.len(world.effects.len());
        for (name, e) in &world.effects {
            o.s(name);
            o.len(e.generics.len());
            o.b(e.linear);
        }
        for (name, adt) in &world.adts {
            o.s(name);
            o.generics(&adt.generics);
            o.b(adt.is_enum);
            o.len(adt.variants.len());
            for v in &adt.variants {
                o.s(&v.name);
                o.b(v.named);
                o.len(v.fields.len());
                for f in &v.fields {
                    o.s(&f.name);
                    o.ty(&f.ty);
                }
            }
        }
        for (name, e) in &world.effects {
            o.s(name);
            o.b(e.linear);
            o.generics(&e.generics);
            o.len(e.ops.len());
            for op in &e.ops {
                o.s(&op.name);
                o.b(!op.generics.is_empty()
                    || op.body.is_some()
                    || !op.effects.instances.is_empty()
                    || op.effects.tail.is_some());
                o.function(op);
            }
        }
        let traits = &world.elaboration.traits;
        o.len(traits.len());
        for t in traits {
            o.s(t);
        }
        let conformances = &world.elaboration.conformances;
        o.len(conformances.len());
        for c in conformances {
            o.b(c.model.is_some());
            o.s(c.model.as_deref().unwrap_or(""));
            o.s(&c.path);
            o.s(&c.trait_name);
            o.s(&c.self_ty);
        }
        let generated = &world.elaboration.generated;
        o.len(world.functions.len());
        for (name, f) in &world.functions {
            o.s(name);
            o.b(generated.contains_key(name));
            o.generics(&f.generics);
            o.function(f);
        }
        o.0
    }
}
