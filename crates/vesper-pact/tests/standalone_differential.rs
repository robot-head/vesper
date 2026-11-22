//! Differential test of `check_orphan`, `conformance_env` and
//! `validate_query` (Rust-exit SH6 lane 12, #1407): the Vesper port against
//! the frozen Rust seed copy, over generated conformances and queries on the
//! fixture world. Values cross between the two packages as JSON (both
//! derive the same serde shapes) and results are compared by their `Debug`
//! text. Only the package vesper-pact runs it (the seed package lists its
//! tests and leaves this one out); on the seed it would compare the Rust
//! code with itself.
mod common;

use std::panic::{AssertUnwindSafe, catch_unwind};
use vesper_pact::*;

/// splitmix64, so the corpus is the same on every platform.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn chance(&mut self, n: u64) -> bool {
        self.below(n) == 0
    }
}

/// Generates terms over the fixture world's ids, sometimes one past the
/// end (unknown ids), with variables of every form.
struct Gen<'a> {
    rng: Rng,
    dto: &'a ProgramDto,
}

const KINDS: [ParamKind; 5] = [
    ParamKind::Type,
    ParamKind::Mode,
    ParamKind::Row,
    ParamKind::Brand,
    ParamKind::Capability,
];
const MODES: [Mode; 3] = [Mode::Copy, Mode::Affine, Mode::Linear];

impl Gen<'_> {
    fn id(&mut self, n: usize) -> u32 {
        // One in sixteen ids is out of range.
        if self.rng.chance(16) {
            n as u32 + self.rng.below(2) as u32
        } else {
            self.rng.below(n.max(1) as u64) as u32
        }
    }
    fn var(&mut self) -> Var {
        let i = self.rng.below(4) as u32;
        match self.rng.below(3) {
            0 => Var::Bound(i),
            1 => Var::Placeholder(i),
            _ => Var::Infer(i),
        }
    }
    fn kind(&mut self) -> ParamKind {
        KINDS[self.rng.below(5) as usize]
    }
    fn ty(&mut self, depth: u32) -> Ty {
        match self.rng.below(if depth == 0 { 2 } else { 6 }) {
            0 => Ty::Var(self.var()),
            1 => Ty::Ctor {
                ctor: TypeCtorId(self.id(self.dto.type_ctors.len())),
                args: vec![],
            },
            2 => Ty::Projection(Box::new(self.projection(depth - 1))),
            _ => {
                let ctor = TypeCtorId(self.id(self.dto.type_ctors.len()));
                let n = self
                    .dto
                    .type_ctors
                    .get(ctor.index())
                    .map_or(1, |c| c.params.len());
                // Mostly the declared arity, sometimes one off.
                let n = if self.rng.chance(8) { n + 1 } else { n };
                Ty::Ctor {
                    ctor,
                    args: (0..n).map(|_| self.arg(depth - 1)).collect(),
                }
            }
        }
    }
    fn mode(&mut self) -> ModeTerm {
        if self.rng.chance(3) {
            ModeTerm::Var(self.var())
        } else {
            ModeTerm::Const(MODES[self.rng.below(3) as usize])
        }
    }
    fn brand(&mut self) -> Brand {
        match self.rng.below(4) {
            0 | 1 => Brand::Canonical,
            2 => Brand::Named(ModelId(self.id(self.dto.models.len()))),
            _ => Brand::Var(self.var()),
        }
    }
    fn row(&mut self, depth: u32) -> EffectRow {
        let n = self.rng.below(3);
        let labels = (0..n)
            .map(|_| {
                if self.rng.chance(4) {
                    EffectLabel::Capability(self.var())
                } else {
                    let effect = EffectId(self.id(self.dto.effects.len()));
                    let k = self
                        .dto
                        .effects
                        .get(effect.index())
                        .map_or(0, |e| e.params.len());
                    EffectLabel::Effect {
                        effect,
                        args: (0..k).map(|_| self.arg(depth.saturating_sub(1))).collect(),
                    }
                }
            })
            .collect();
        let tail = self.rng.chance(2).then(|| self.var());
        EffectRow { labels, tail }
    }
    fn arg(&mut self, depth: u32) -> GenericArg {
        match self.rng.below(8) {
            0 => GenericArg::Mode(self.mode()),
            1 => GenericArg::Row(self.row(depth)),
            2 => GenericArg::Brand(self.brand()),
            3 => GenericArg::Capability(self.var()),
            _ => GenericArg::Ty(self.ty(depth)),
        }
    }
    fn trait_ref(&mut self, depth: u32) -> TraitRef {
        let trait_id = TraitId(self.id(self.dto.traits.len()));
        let n = self
            .dto
            .traits
            .get(trait_id.index())
            .map_or(0, |t| t.params.len());
        let n = if self.rng.chance(8) { n + 1 } else { n };
        TraitRef {
            trait_id,
            self_ty: self.ty(depth),
            args: (0..n).map(|_| self.arg(depth)).collect(),
        }
    }
    fn projection(&mut self, depth: u32) -> Projection {
        Projection {
            trait_ref: self.trait_ref(depth),
            brand: self.brand(),
            assoc: AssocIdx(self.rng.below(3) as u32),
        }
    }
    fn goal(&mut self, depth: u32) -> Goal {
        match self.rng.below(4) {
            0 => Goal::ProjectionEq {
                projection: self.projection(depth),
                ty: self.ty(depth),
            },
            1 => Goal::ModeBound {
                ty: self.ty(depth),
                bound: self.mode(),
            },
            _ => Goal::Implements {
                trait_ref: self.trait_ref(depth),
                brand: self.brand(),
            },
        }
    }
    fn env(&mut self) -> Env {
        let placeholders = (0..self.rng.below(4))
            .map(|_| PlaceholderDecl {
                kind: self.kind(),
                mode_bound: self
                    .rng
                    .chance(3)
                    .then(|| MODES[self.rng.below(3) as usize]),
            })
            .collect();
        let assumptions = (0..self.rng.below(3)).map(|_| self.goal(2)).collect();
        let model_scopes = (0..self.rng.below(3))
            .map(|_| {
                (0..self.rng.below(3))
                    .map(|_| ModelId(self.id(self.dto.models.len())))
                    .collect()
            })
            .collect();
        Env {
            placeholders,
            assumptions,
            model_scopes,
        }
    }
    fn query(&mut self) -> QueryDto {
        let mut q = QueryDto::new(
            self.env(),
            (0..self.rng.below(4)).map(|_| self.kind()).collect(),
            self.goal(3),
        );
        if self.rng.chance(32) {
            q.schema = SchemaVersion {
                major: self.rng.below(2) as u16,
                minor: self.rng.below(3) as u16,
            };
        }
        q
    }
    fn conformance(&mut self) -> ConformanceDecl {
        let params: Vec<ParamKind> = (0..self.rng.below(4)).map(|_| self.kind()).collect();
        let mut where_clauses: Vec<Goal> = (0..self.rng.below(4)).map(|_| self.goal(2)).collect();
        // Placeholder mode bounds, the form conformance_env assumes.
        for _ in 0..self.rng.below(3) {
            let i = self.rng.below(params.len().max(1) as u64) as u32;
            where_clauses.push(Goal::ModeBound {
                ty: Ty::Var(Var::Bound(i)),
                bound: ModeTerm::Const(MODES[self.rng.below(3) as usize]),
            });
        }
        ConformanceDecl {
            package: PackageId(self.id(self.dto.packages.len())),
            params,
            trait_ref: self.trait_ref(3),
            where_clauses,
            assoc_values: (0..self.rng.below(2)).map(|_| self.ty(2)).collect(),
        }
    }
}

fn to_seed<T: serde::Serialize, U: serde::de::DeserializeOwned>(x: &T) -> U {
    serde_json::from_str(&serde_json::to_string(x).unwrap()).unwrap()
}

/// The `Debug` text of a result, or `"panic"`.
fn run<T: std::fmt::Debug>(f: impl FnOnce() -> T) -> String {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => format!("{v:?}"),
        Err(_) => "panic".into(),
    }
}

const CASES: u64 = 3000;

#[test]
fn standalone_checks_match_the_rust_seed() {
    let dto = common::world_dto();
    let program = Program::new(dto.clone()).unwrap();
    let seed_program = vesper_pact_seed::Program::new(to_seed(&dto)).unwrap();
    let mut g = Gen {
        rng: Rng(0x5eed_1407),
        dto: &dto,
    };
    // Every declared impl and model, then generated conformances.
    let mut decls: Vec<ConformanceDecl> = dto.impls.iter().map(|i| i.decl.clone()).collect();
    decls.extend(dto.models.iter().map(|m| m.decl.clone()));
    decls.extend((0..CASES).map(|_| g.conformance()));
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let (mut orphans, mut errors, mut panics) = (0, 0, 0);
    for (n, d) in decls.iter().enumerate() {
        let sd: vesper_pact_seed::ConformanceDecl = to_seed(d);
        let ours = run(|| check_orphan(&program, d));
        let theirs = run(|| vesper_pact_seed::check_orphan(&seed_program, &sd));
        assert_eq!(ours, theirs, "check_orphan of case {n}: {d:?}");
        match ours.as_str() {
            "Ok(())" => {}
            "panic" => panics += 1,
            _ => orphans += 1,
        }
        let ours = run(|| conformance_env(d));
        let theirs = run(|| vesper_pact_seed::conformance_env(&sd));
        assert_eq!(ours, theirs, "conformance_env of case {n}: {d:?}");
    }
    for n in 0..CASES {
        let q = g.query();
        let sq: vesper_pact_seed::QueryDto = to_seed(&q);
        let ours = run(|| validate_query(&program, &q));
        let theirs = run(|| vesper_pact_seed::validate_query(&seed_program, &sq));
        assert_eq!(ours, theirs, "validate_query of case {n}: {q:?}");
        if ours != "Ok(())" {
            errors += 1;
        }
    }
    std::panic::set_hook(prev);
    // The corpus reaches every outcome.
    assert!(
        orphans > 100 && panics > 10,
        "{orphans} orphans, {panics} panics"
    );
    assert!(errors > 100 && errors < CASES, "{errors} invalid queries");
}
