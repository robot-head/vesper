#![allow(dead_code)]

use vesper::ast::Mode;
use vesper_pact_bridge::*;

pub fn std_pkg() -> PackageIdentity {
    PackageIdentity::new("@std/core", "1.0.0")
}
pub fn app_pkg() -> PackageIdentity {
    PackageIdentity::new("@app/main", "0.1.0")
}

pub const STD_SOURCE: &str = "
copy struct Uuid { hi: u64, lo: u64 }
enum List[T] { Nil, Cons(T, List[T]) }
struct Vec[T] { head: Option[T] }
struct CacheBTree[K, V] { key: Option[K], value: Option[V] }
linear struct WriteTxn { id: u64 }
effect Region { fn touch() -> Unit; }
";

pub fn check(pkg: PackageIdentity, src: &str) -> CheckedPackage {
    CheckedPackage::check(pkg, src).unwrap_or_else(|e| panic!("source rejected: {e}"))
}

pub fn ty(name: &str, args: Vec<TypeDto>) -> TypeDto {
    TypeDto::named(name, args)
}
pub fn qty(pkg: PackageIdentity, name: &str, args: Vec<TypeDto>) -> TypeDto {
    TypeDto::Def {
        def: DefRefDto::qualified(pkg, name),
        args: args.into_iter().map(ArgDto::Type).collect(),
    }
}
pub fn p(name: &str) -> TypeDto {
    TypeDto::param(name)
}
pub fn tr(name: &str) -> DefRefDto {
    DefRefDto::local(name)
}
pub fn implements(self_ty: TypeDto, trait_name: &str) -> GoalDto {
    GoalDto::Implements {
        self_ty,
        trait_ref: tr(trait_name),
        args: vec![],
        brand: None,
    }
}
pub fn implements_via(self_ty: TypeDto, trait_name: &str, brand: BrandDto) -> GoalDto {
    GoalDto::Implements {
        self_ty,
        trait_ref: tr(trait_name),
        args: vec![],
        brand: Some(brand),
    }
}
pub fn simple_trait(name: &str) -> TraitDto {
    TraitDto {
        name: name.into(),
        params: vec![],
        assoc: vec![],
        coinductive: false,
    }
}
pub fn conf(
    params: Vec<ParamDto>,
    trait_name: &str,
    self_ty: TypeDto,
    where_clauses: Vec<GoalDto>,
) -> ConformanceDto {
    ConformanceDto {
        params,
        trait_ref: tr(trait_name),
        self_ty,
        args: vec![],
        where_clauses,
        assoc: vec![],
    }
}
pub fn imp(key: &str, decl: ConformanceDto) -> ImplDto {
    ImplDto {
        key: key.into(),
        decl,
    }
}
pub fn model(name: &str, decl: ConformanceDto) -> ModelDto {
    ModelDto {
        name: name.into(),
        decl,
    }
}
pub fn tparam(name: &str) -> ParamDto {
    ParamDto::ty(name, None)
}
pub fn tparam_bound(name: &str, m: Mode) -> ParamDto {
    ParamDto::ty(name, Some(m))
}

/// The std fixture: Eq/Ord/Show with impls, a named reverse model and a
/// branded `CacheBTree[K, V, M: brand of Ord]`.
pub fn std_bindings() -> BindingsDto {
    let mut b = BindingsDto::empty(std_pkg());
    b.traits = vec![
        simple_trait("Eq"),
        simple_trait("Ord"),
        simple_trait("Show"),
    ];
    b.branded_types = vec![BrandedTypeDto {
        ty: "CacheBTree".into(),
        brands: vec![BrandParamDto {
            name: "M".into(),
            trait_ref: tr("Ord"),
        }],
    }];
    b.impls = vec![
        imp("eq.u64", conf(vec![], "Eq", ty("u64", vec![]), vec![])),
        imp("eq.uuid", conf(vec![], "Eq", ty("Uuid", vec![]), vec![])),
        imp(
            "eq.list",
            conf(
                vec![tparam("T")],
                "Eq",
                ty("List", vec![p("T")]),
                vec![implements(p("T"), "Eq")],
            ),
        ),
        imp(
            "eq.vec",
            conf(
                vec![tparam("T")],
                "Eq",
                ty("Vec", vec![p("T")]),
                vec![implements(p("T"), "Eq")],
            ),
        ),
        imp(
            "eq.pair",
            conf(
                vec![tparam("A"), tparam("B")],
                "Eq",
                TypeDto::Tuple(vec![p("A"), p("B")]),
                vec![implements(p("A"), "Eq"), implements(p("B"), "Eq")],
            ),
        ),
        imp("ord.uuid", conf(vec![], "Ord", ty("Uuid", vec![]), vec![])),
        imp(
            "show.cache",
            conf(
                vec![tparam("K"), tparam("V")],
                "Show",
                TypeDto::Def {
                    def: tr("CacheBTree"),
                    args: vec![
                        ArgDto::Type(p("K")),
                        ArgDto::Type(p("V")),
                        ArgDto::Brand(BrandDto::Canonical),
                    ],
                },
                vec![],
            ),
        ),
    ];
    b.models = vec![
        model(
            "ReverseUuidOrd",
            conf(vec![], "Ord", ty("Uuid", vec![]), vec![]),
        ),
        model(
            "OtherUuidOrd",
            conf(vec![], "Ord", ty("Uuid", vec![]), vec![]),
        ),
    ];
    b
}

pub fn std_session() -> CoherentSession {
    SessionBuilder::new()
        .package(check(std_pkg(), STD_SOURCE), std_bindings())
        .build()
        .unwrap_or_else(|e| panic!("{e}"))
}

pub fn query(pkg: PackageIdentity, goal: GoalDto) -> QueryBindingDto {
    QueryBindingDto::new(pkg, EnvDto::default(), vec![], goal)
}

pub fn proven(o: &BridgeOutcome) -> &Evidence {
    match o {
        BridgeOutcome::Proven(e) => e.as_ref(),
        other => panic!("expected proven, got {other:?}"),
    }
}
