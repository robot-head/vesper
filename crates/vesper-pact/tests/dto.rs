mod common;

use common::*;
use vesper_pact::*;

const WORLD_FIXTURE: &str = "tests/fixtures/world.json";
const QUERY_FIXTURE: &str = "tests/fixtures/fold_source_query.json";

fn fold_source_query() -> QueryDto {
    let src = tr(SOURCE, ph(0), vec![open_row(vec![], Var::Placeholder(2))]);
    let env = Env {
        placeholders: vec![
            PlaceholderDecl::ty(None),
            PlaceholderDecl::ty(Some(Mode::Copy)),
            PlaceholderDecl::of(ParamKind::Row),
        ],
        assumptions: vec![
            Goal::Implements {
                trait_ref: src.clone(),
                brand: Brand::Canonical,
            },
            Goal::ProjectionEq {
                projection: Projection {
                    trait_ref: src.clone(),
                    brand: Brand::Canonical,
                    assoc: AssocIdx(0),
                },
                ty: ph(1),
            },
        ],
        model_scopes: vec![vec![BIG_ENDIAN_UUID]],
    };
    let item = Projection {
        trait_ref: src,
        brand: Brand::Canonical,
        assoc: AssocIdx(0),
    };
    query(
        env,
        vec![],
        mode_bound(t1(VEC, proj_ty(item)), Mode::Affine),
    )
}

fn check_fixture(path: &str, actual: &str) {
    if std::env::var_os("PACT_BLESS").is_some() {
        std::fs::write(path, actual).unwrap();
    }
    let expected = std::fs::read_to_string(path)
        .unwrap_or_else(|_| panic!("missing {path}; run with PACT_BLESS=1"));
    assert_eq!(
        actual, expected,
        "{path} drifted: the boundary schema changed (bump SCHEMA_VERSION and re-bless)"
    );
}

#[test]
fn program_dto_json_is_stable_and_round_trips() {
    let dto = world_dto();
    let json = serde_json::to_string_pretty(&dto).unwrap() + "\n";
    check_fixture(WORLD_FIXTURE, &json);
    let back: ProgramDto = serde_json::from_str(&json).unwrap();
    assert_eq!(back, dto);
    Program::new(back).unwrap();
}

#[test]
fn query_dto_from_fixture_solves() {
    let q = fold_source_query();
    let json = serde_json::to_string_pretty(&q).unwrap() + "\n";
    check_fixture(QUERY_FIXTURE, &json);
    let program: ProgramDto =
        serde_json::from_str(&std::fs::read_to_string(WORLD_FIXTURE).unwrap()).unwrap();
    let program = Program::new(program).unwrap();
    let q: QueryDto = serde_json::from_str(&json).unwrap();
    let o = solve(&program, &q);
    assert!(o.is_proven(), "{o:?}");
}

fn rejected(dto: ProgramDto) -> String {
    vesper_pact::diag::render(&Program::new(dto).expect_err("must be rejected"))
}

#[test]
fn schema_version_is_enforced() {
    let mut dto = world_dto();
    dto.schema = SchemaVersion { major: 1, minor: 0 };
    assert_eq!(
        rejected(dto),
        "PACT0001 [program] unsupported schema 1.0 (this solver accepts 0.0..=0.1)\n"
    );
    let mut dto = world_dto();
    dto.schema = SchemaVersion { major: 0, minor: 2 };
    assert!(rejected(dto).starts_with("PACT0001"));
    let mut q = fold_source_query();
    q.schema = SchemaVersion { major: 9, minor: 0 };
    assert!(Solver::new(&world()).solve(&q).is_err());
}

#[test]
fn malformed_declarations_are_rejected_with_sorted_diagnostics() {
    let mut dto = world_dto();
    // Arity: Vec with two arguments.
    dto.impls[0].decl.trait_ref.self_ty = t(VEC, vec![a(c(U64)), a(c(U64))]);
    // Kind: Source given a type where a row is expected.
    dto.impls[7].decl.trait_ref.args = vec![a(c(U64))];
    // Variable scope: inference variable in a template.
    dto.impls[1].decl.where_clauses = vec![implements(CLONE, inf(0), vec![])];
    // Unbound template parameter.
    dto.impls[3].decl.where_clauses = vec![implements(CLONE, bound(5), vec![])];
    // Unconstrained parameter: occurs only in a where-clause.
    dto.impls[4].decl.params = vec![ParamKind::Type];
    dto.impls[4].decl.where_clauses = vec![implements(CLONE, bound(0), vec![])];
    // Projection in an impl head.
    dto.impls[6].decl.trait_ref.self_ty = proj_ty(proj(COLLECT, c(BITS), vec![], 0));
    // Duplicate effect label.
    dto.impls[8].decl.trait_ref.args = vec![row(vec![eff(NET), eff(NET)])];
    // Wrong number of associated values.
    dto.impls[19].decl.assoc_values = vec![];
    // Ids must be dense.
    dto.models[2].id = ModelId(7);
    let text = rejected(dto.clone());
    let expected = "\
PACT0002 [program] model at 2 has id model#7
PACT0004 [impl#0] type#2 expects 1 arguments, got 2
PACT0004 [impl#19] 0 associated values supplied, trait#6 declares 1
PACT0005 [impl#7] trait#4 expects a Row argument, got type#0
PACT0006 [impl#1] variable ?0 is not legal here
PACT0006 [impl#3] variable ^5 is unbound
PACT0008 [impl#4] parameter ^0 does not occur in the head
PACT0009 [impl#6] associated-type projections in an impl/model head are outside this fragment
PACT0009 [impl#8] effect row repeats label Effect(EffectId(0)); duplicate labels are outside this fragment
";
    assert_eq!(text, expected);
    assert_eq!(rejected(dto), text, "deterministic");
}

#[test]
fn malformed_queries_are_rejected() {
    let p = world();
    let mut s = Solver::new(&p);
    let bad = |env: Env, binders: Vec<ParamKind>, goal: Goal| {
        let q = query(env, binders, goal);
        vesper_pact::diag::render(&Solver::new(&p).solve(&q).expect_err("rejected"))
    };
    // Unbound inference variable.
    assert!(bad(Env::default(), vec![], implements(CLONE, inf(0), vec![])).contains("PACT0006"));
    // Bound variable outside a template.
    assert!(bad(Env::default(), vec![], implements(CLONE, bound(0), vec![])).contains("PACT0006"));
    // Kind mismatch: a mode binder used as a type.
    assert!(
        bad(
            Env::default(),
            vec![ParamKind::Mode],
            implements(CLONE, inf(0), vec![])
        )
        .contains("PACT0005")
    );
    // Capability inference variables are outside the fragment.
    let cap_goal = implements(
        SOURCE,
        c(PULSE_TCP),
        vec![row(vec![EffectLabel::Capability(Var::Infer(0))])],
    );
    assert!(bad(Env::default(), vec![ParamKind::Capability], cap_goal).contains("PACT0009"));
    // Unknown model in a lexical scope.
    let env = Env {
        model_scopes: vec![vec![ModelId(99)]],
        ..Env::default()
    };
    assert!(bad(env, vec![], implements(CLONE, c(U64), vec![])).contains("PACT0003"));
    // Implicit brands are not legal in assumptions.
    let env = Env {
        placeholders: vec![PlaceholderDecl::ty(None)],
        assumptions: vec![via(ORD, ph(0), vec![], Brand::Var(Var::Infer(0)))],
        model_scopes: vec![],
    };
    assert!(bad(env, vec![], implements(CLONE, c(U64), vec![])).contains("PACT0006"));
    // The solver remains usable after rejecting queries.
    assert!(
        s.solve(&query(
            Env::default(),
            vec![],
            implements(CLONE, c(U64), vec![])
        ))
        .unwrap()
        .is_proven()
    );
}
