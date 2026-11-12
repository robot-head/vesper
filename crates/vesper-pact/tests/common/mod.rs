//! A shared fixture world modelled on design §16.2–16.3.
#![allow(dead_code)]

use vesper_pact::*;

pub const STD: PackageId = PackageId(0);
pub const WIRE: PackageId = PackageId(1);
pub const APP: PackageId = PackageId(2);
pub const THIRD: PackageId = PackageId(3);

// Type constructors.
pub const U64: TypeCtorId = TypeCtorId(0);
pub const UUID: TypeCtorId = TypeCtorId(1);
pub const VEC: TypeCtorId = TypeCtorId(2);
pub const OPTION: TypeCtorId = TypeCtorId(3);
pub const WRITE_TXN: TypeCtorId = TypeCtorId(4);
pub const RECORD_BATCH: TypeCtorId = TypeCtorId(5);
pub const LIST: TypeCtorId = TypeCtorId(6);
pub const TILED_SEQ: TypeCtorId = TypeCtorId(7);
pub const PULSE_TCP: TypeCtorId = TypeCtorId(8);
pub const CACHE_BTREE: TypeCtorId = TypeCtorId(9);
pub const PAIR: TypeCtorId = TypeCtorId(10);
pub const FUSED: TypeCtorId = TypeCtorId(11);
pub const RETRY: TypeCtorId = TypeCtorId(12);
pub const RC: TypeCtorId = TypeCtorId(13);
pub const BITS: TypeCtorId = TypeCtorId(14);
pub const WRAP: TypeCtorId = TypeCtorId(15);
pub const IO_ERROR: TypeCtorId = TypeCtorId(16);
pub const DEEP: TypeCtorId = TypeCtorId(17);

// Effects.
pub const NET: EffectId = EffectId(0);
pub const CANCEL: EffectId = EffectId(1);
pub const FAIL: EffectId = EffectId(2);

// Traits.
pub const CLONE: TraitId = TraitId(0);
pub const SEND: TraitId = TraitId(1);
pub const ORD: TraitId = TraitId(2);
pub const WIRE_CODEC: TraitId = TraitId(3);
pub const SOURCE: TraitId = TraitId(4);
pub const CONTAINER: TraitId = TraitId(5);
pub const COLLECT: TraitId = TraitId(6);
pub const LOOP: TraitId = TraitId(7);
pub const MERGE: TraitId = TraitId(8);
pub const GROW: TraitId = TraitId(9);

// Named models.
pub const REVERSE_UUID_ORD: ModelId = ModelId(0);
pub const BIG_ENDIAN_UUID: ModelId = ModelId(1);
pub const LITTLE_ENDIAN_UUID: ModelId = ModelId(2);

pub fn t(ctor: TypeCtorId, args: Vec<GenericArg>) -> Ty {
    Ty::Ctor { ctor, args }
}
pub fn c(ctor: TypeCtorId) -> Ty {
    t(ctor, vec![])
}
pub fn a(ty: Ty) -> GenericArg {
    GenericArg::Ty(ty)
}
pub fn t1(ctor: TypeCtorId, x: Ty) -> Ty {
    t(ctor, vec![a(x)])
}
pub fn bound(i: u32) -> Ty {
    Ty::Var(Var::Bound(i))
}
pub fn ph(i: u32) -> Ty {
    Ty::Var(Var::Placeholder(i))
}
pub fn inf(i: u32) -> Ty {
    Ty::Var(Var::Infer(i))
}
pub fn tr(trait_id: TraitId, self_ty: Ty, args: Vec<GenericArg>) -> TraitRef {
    TraitRef {
        trait_id,
        self_ty,
        args,
    }
}
pub fn implements(trait_id: TraitId, self_ty: Ty, args: Vec<GenericArg>) -> Goal {
    Goal::Implements {
        trait_ref: tr(trait_id, self_ty, args),
        brand: Brand::Canonical,
    }
}
pub fn via(trait_id: TraitId, self_ty: Ty, args: Vec<GenericArg>, brand: Brand) -> Goal {
    Goal::Implements {
        trait_ref: tr(trait_id, self_ty, args),
        brand,
    }
}
pub fn proj(trait_id: TraitId, self_ty: Ty, args: Vec<GenericArg>, assoc: u32) -> Projection {
    Projection {
        trait_ref: tr(trait_id, self_ty, args),
        brand: Brand::Canonical,
        assoc: AssocIdx(assoc),
    }
}
pub fn proj_ty(p: Projection) -> Ty {
    Ty::Projection(Box::new(p))
}
pub fn eff(e: EffectId) -> EffectLabel {
    EffectLabel::Effect {
        effect: e,
        args: vec![],
    }
}
pub fn fail(e: Ty) -> EffectLabel {
    EffectLabel::Effect {
        effect: FAIL,
        args: vec![a(e)],
    }
}
pub fn row(labels: Vec<EffectLabel>) -> GenericArg {
    GenericArg::Row(EffectRow::closed(labels))
}
pub fn open_row(labels: Vec<EffectLabel>, tail: Var) -> GenericArg {
    GenericArg::Row(EffectRow::open(labels, tail))
}
pub fn mode(m: Mode) -> GenericArg {
    GenericArg::Mode(ModeTerm::Const(m))
}
pub fn mode_bound(ty: Ty, m: Mode) -> Goal {
    Goal::ModeBound {
        ty,
        bound: ModeTerm::Const(m),
    }
}

fn ctor(
    id: TypeCtorId,
    package: PackageId,
    params: Vec<ParamKind>,
    base: Mode,
    joins: Vec<u32>,
) -> TypeCtorDecl {
    TypeCtorDecl {
        id,
        package,
        params,
        mode: ModeScheme { base, joins },
    }
}

fn trait_decl(
    id: TraitId,
    package: PackageId,
    params: Vec<ParamKind>,
    assoc: Vec<AssocDecl>,
    coinductive: bool,
) -> TraitDecl {
    TraitDecl {
        id,
        package,
        params,
        assoc,
        coinductive,
    }
}

pub fn conf(
    package: PackageId,
    params: Vec<ParamKind>,
    trait_ref: TraitRef,
    where_clauses: Vec<Goal>,
    assoc_values: Vec<Ty>,
) -> ConformanceDecl {
    ConformanceDecl {
        package,
        params,
        trait_ref,
        where_clauses,
        assoc_values,
    }
}

use ParamKind::{Row, Type};

/// The §16 fixture world. Impl ids are positions in `impls`.
pub fn world_dto() -> ProgramDto {
    let mut p = ProgramDto::new();
    p.packages = (0..4).map(|i| PackageDecl { id: PackageId(i) }).collect();
    p.type_ctors = vec![
        ctor(U64, STD, vec![], Mode::Copy, vec![]),
        ctor(UUID, STD, vec![], Mode::Copy, vec![]),
        ctor(VEC, STD, vec![Type], Mode::Affine, vec![0]),
        ctor(OPTION, STD, vec![Type], Mode::Copy, vec![0]),
        ctor(WRITE_TXN, APP, vec![], Mode::Linear, vec![]),
        ctor(RECORD_BATCH, APP, vec![], Mode::Affine, vec![]),
        ctor(LIST, STD, vec![Type], Mode::Affine, vec![0]),
        ctor(TILED_SEQ, APP, vec![Type], Mode::Copy, vec![0]),
        ctor(PULSE_TCP, APP, vec![], Mode::Affine, vec![]),
        ctor(
            CACHE_BTREE,
            STD,
            vec![Type, Type, ParamKind::Brand],
            Mode::Affine,
            vec![0, 1],
        ),
        ctor(PAIR, STD, vec![Type, Type], Mode::Copy, vec![0, 1]),
        ctor(FUSED, STD, vec![Type], Mode::Affine, vec![0]),
        ctor(RETRY, STD, vec![Type], Mode::Affine, vec![0]),
        ctor(RC, STD, vec![Type], Mode::Affine, vec![0]),
        ctor(BITS, STD, vec![], Mode::Copy, vec![]),
        ctor(WRAP, STD, vec![Type], Mode::Affine, vec![0]),
        ctor(IO_ERROR, STD, vec![], Mode::Copy, vec![]),
        ctor(DEEP, STD, vec![Type], Mode::Copy, vec![0]),
    ];
    p.effects = vec![
        EffectDecl {
            id: NET,
            package: STD,
            params: vec![],
        },
        EffectDecl {
            id: CANCEL,
            package: STD,
            params: vec![],
        },
        EffectDecl {
            id: FAIL,
            package: STD,
            params: vec![Type],
        },
    ];
    p.traits = vec![
        trait_decl(CLONE, STD, vec![], vec![], false),
        trait_decl(SEND, STD, vec![], vec![], true),
        trait_decl(ORD, STD, vec![], vec![], false),
        trait_decl(WIRE_CODEC, WIRE, vec![], vec![], false),
        // trait Source[r: eff] { type Item: mod linear; }
        trait_decl(
            SOURCE,
            STD,
            vec![Row],
            vec![AssocDecl {
                mode_bound: ModeTerm::Const(Mode::Linear),
                bounds: vec![],
            }],
            false,
        ),
        // trait Container[m: mod] { type Elem: mod m; }
        trait_decl(
            CONTAINER,
            STD,
            vec![ParamKind::Mode],
            vec![AssocDecl {
                mode_bound: ModeTerm::Var(Var::Bound(1)),
                bounds: vec![],
            }],
            false,
        ),
        // trait Collect { type Out: mod affine + Clone; }
        trait_decl(
            COLLECT,
            STD,
            vec![],
            vec![AssocDecl {
                mode_bound: ModeTerm::Const(Mode::Affine),
                bounds: vec![AssocBound {
                    trait_id: CLONE,
                    args: vec![],
                    brand: Brand::Canonical,
                }],
            }],
            false,
        ),
        trait_decl(LOOP, STD, vec![], vec![], false),
        // trait Merge[Other]
        trait_decl(MERGE, STD, vec![Type], vec![], false),
        trait_decl(GROW, STD, vec![], vec![], false),
    ];
    let canonical = Brand::Canonical;
    let impls = vec![
        // 0: impl Clone for u64
        conf(STD, vec![], tr(CLONE, c(U64), vec![]), vec![], vec![]),
        // 1: impl Clone for Uuid
        conf(STD, vec![], tr(CLONE, c(UUID), vec![]), vec![], vec![]),
        // 2: impl[T] Clone for Vec[T] where T: Clone
        conf(
            STD,
            vec![Type],
            tr(CLONE, t1(VEC, bound(0)), vec![]),
            vec![implements(CLONE, bound(0), vec![])],
            vec![],
        ),
        // 3: impl[T] Clone for Option[T] where T: Clone
        conf(
            STD,
            vec![Type],
            tr(CLONE, t1(OPTION, bound(0)), vec![]),
            vec![implements(CLONE, bound(0), vec![])],
            vec![],
        ),
        // 4: impl Send for u64
        conf(STD, vec![], tr(SEND, c(U64), vec![]), vec![], vec![]),
        // 5: impl[T] Send for List[T] where T: Send, List[T]: Send  (structural auto trait)
        conf(
            STD,
            vec![Type],
            tr(SEND, t1(LIST, bound(0)), vec![]),
            vec![
                implements(SEND, bound(0), vec![]),
                implements(SEND, t1(LIST, bound(0)), vec![]),
            ],
            vec![],
        ),
        // 6: impl Ord for Uuid
        conf(STD, vec![], tr(ORD, c(UUID), vec![]), vec![], vec![]),
        // 7: impl[T] Source[eff []] for TiledSeq[T] { type Item = T }
        conf(
            APP,
            vec![Type],
            tr(SOURCE, t1(TILED_SEQ, bound(0)), vec![row(vec![])]),
            vec![],
            vec![bound(0)],
        ),
        // 8: impl Source[eff [Net, Cancel]] for PulseTcp { type Item = RecordBatch }
        conf(
            APP,
            vec![],
            tr(SOURCE, c(PULSE_TCP), vec![row(vec![eff(NET), eff(CANCEL)])]),
            vec![],
            vec![c(RECORD_BATCH)],
        ),
        // 9: impl[S, r] Source[r] for Fused[S] where S: Source[r] { type Item = <S as Source[r]>::Item }
        conf(
            STD,
            vec![Type, Row],
            tr(
                SOURCE,
                t1(FUSED, bound(0)),
                vec![open_row(vec![], Var::Bound(1))],
            ),
            vec![implements(
                SOURCE,
                bound(0),
                vec![open_row(vec![], Var::Bound(1))],
            )],
            vec![proj_ty(proj(
                SOURCE,
                bound(0),
                vec![open_row(vec![], Var::Bound(1))],
                0,
            ))],
        ),
        // 10: impl[S, r] Source[eff [Fail[IoError] | r]] for Retry[S] where S: Source[r]
        //     { type Item = <S as Source[r]>::Item }
        conf(
            STD,
            vec![Type, Row],
            tr(
                SOURCE,
                t1(RETRY, bound(0)),
                vec![open_row(vec![fail(c(IO_ERROR))], Var::Bound(1))],
            ),
            vec![implements(
                SOURCE,
                bound(0),
                vec![open_row(vec![], Var::Bound(1))],
            )],
            vec![proj_ty(proj(
                SOURCE,
                bound(0),
                vec![open_row(vec![], Var::Bound(1))],
                0,
            ))],
        ),
        // 11: impl[T] Container[mod linear] for Vec[T] { type Elem = T }
        conf(
            STD,
            vec![Type],
            tr(CONTAINER, t1(VEC, bound(0)), vec![mode(Mode::Linear)]),
            vec![],
            vec![bound(0)],
        ),
        // 12: impl Container[mod copy] for Bits { type Elem = u64 }
        conf(
            STD,
            vec![],
            tr(CONTAINER, c(BITS), vec![mode(Mode::Copy)]),
            vec![],
            vec![c(U64)],
        ),
        // 13: impl Container[mod affine] for Bits { type Elem = u64 }   (distinct modal argument)
        conf(
            STD,
            vec![],
            tr(CONTAINER, c(BITS), vec![mode(Mode::Affine)]),
            vec![],
            vec![c(U64)],
        ),
        // 14: impl[T] Loop for Wrap[T] where Wrap[T]: Loop   (inductive cycle, no base case)
        conf(
            STD,
            vec![Type],
            tr(LOOP, t1(WRAP, bound(0)), vec![]),
            vec![implements(LOOP, t1(WRAP, bound(0)), vec![])],
            vec![],
        ),
        // 15: impl[K, V, M] Merge[CacheBTree[K, V, M]] for CacheBTree[K, V, M] where K: Ord via M
        conf(
            STD,
            vec![Type, Type, ParamKind::Brand],
            tr(
                MERGE,
                t(
                    CACHE_BTREE,
                    vec![
                        a(bound(0)),
                        a(bound(1)),
                        GenericArg::Brand(Brand::Var(Var::Bound(2))),
                    ],
                ),
                vec![a(t(
                    CACHE_BTREE,
                    vec![
                        a(bound(0)),
                        a(bound(1)),
                        GenericArg::Brand(Brand::Var(Var::Bound(2))),
                    ],
                ))],
            ),
            vec![via(ORD, bound(0), vec![], Brand::Var(Var::Bound(2)))],
            vec![],
        ),
        // 16: impl[T] Send for Wrap[T] where Wrap[T]: Clone   (coinductive goal ...)
        conf(
            STD,
            vec![Type],
            tr(SEND, t1(WRAP, bound(0)), vec![]),
            vec![implements(CLONE, t1(WRAP, bound(0)), vec![])],
            vec![],
        ),
        // 17: impl[T] Clone for Wrap[T] where Wrap[T]: Send   (... cycling through an inductive one)
        conf(
            STD,
            vec![Type],
            tr(CLONE, t1(WRAP, bound(0)), vec![]),
            vec![implements(SEND, t1(WRAP, bound(0)), vec![])],
            vec![],
        ),
        // 18: impl[T] Grow for Deep[T] where Deep[Deep[T]]: Grow   (unbounded, non-cyclic)
        conf(
            STD,
            vec![Type],
            tr(GROW, t1(DEEP, bound(0)), vec![]),
            vec![implements(GROW, t1(DEEP, t1(DEEP, bound(0))), vec![])],
            vec![],
        ),
        // 19: impl Collect for Bits { type Out = Vec[u64] }
        conf(
            STD,
            vec![],
            tr(COLLECT, c(BITS), vec![]),
            vec![],
            vec![t1(VEC, c(U64))],
        ),
        // 20: impl Send for Uuid
        conf(STD, vec![], tr(SEND, c(UUID), vec![]), vec![], vec![]),
        // 21: impl[T] Clone for List[T] where T: Clone
        conf(
            STD,
            vec![Type],
            tr(CLONE, t1(LIST, bound(0)), vec![]),
            vec![implements(CLONE, bound(0), vec![])],
            vec![],
        ),
    ];
    p.impls = impls
        .into_iter()
        .enumerate()
        .map(|(i, decl)| ImplDecl {
            id: ImplId(i as u32),
            decl,
        })
        .collect();
    let _ = canonical;
    p.models = vec![
        // pub model ReverseUuidOrd: Ord for Uuid
        ModelDecl {
            id: REVERSE_UUID_ORD,
            decl: conf(APP, vec![], tr(ORD, c(UUID), vec![]), vec![], vec![]),
        },
        // pub model BigEndianUuidCodec: WireCodec for Uuid
        ModelDecl {
            id: BIG_ENDIAN_UUID,
            decl: conf(APP, vec![], tr(WIRE_CODEC, c(UUID), vec![]), vec![], vec![]),
        },
        // pub model LittleEndianUuidCodec: WireCodec for Uuid (another package)
        ModelDecl {
            id: LITTLE_ENDIAN_UUID,
            decl: conf(
                THIRD,
                vec![],
                tr(WIRE_CODEC, c(UUID), vec![]),
                vec![],
                vec![],
            ),
        },
    ];
    p
}

pub fn world() -> Program {
    Program::new(world_dto()).expect("fixture world validates")
}

pub fn query(env: Env, binders: Vec<ParamKind>, goal: Goal) -> QueryDto {
    QueryDto::new(env, binders, goal)
}

/// Solves and, for proofs, re-checks the derivation independently.
pub fn solve(program: &Program, q: &QueryDto) -> Outcome {
    let mut s = Solver::new(program);
    solve_with(&mut s, program, q)
}

pub fn solve_with(s: &mut Solver, program: &Program, q: &QueryDto) -> Outcome {
    let o = s.solve(q).expect("query validates");
    if let Outcome::Proven(sol) = &o {
        check_derivation(program, &q.env, &sol.derivation)
            .unwrap_or_else(|e| panic!("derivation rejected: {e}\n{}", sol.derivation.render()));
    }
    o
}

pub fn solve_goal(program: &Program, goal: Goal) -> Outcome {
    solve(program, &query(Env::default(), vec![], goal))
}

pub fn proven(o: &Outcome) -> &Solution {
    match o {
        Outcome::Proven(s) => s,
        other => panic!("expected proof, got {other:?}"),
    }
}

pub fn failure(o: &Outcome) -> &Failure {
    match o {
        Outcome::NoSolution(f) => f,
        other => panic!("expected no solution, got {other:?}"),
    }
}
