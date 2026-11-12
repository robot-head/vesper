//! Source trait/impl/model declarations reach the bridge as bindings.
use vesper_pact_bridge::*;

const SRC: &str = "copy struct Point { x: i64, y: i64 }
trait Score { type Out: mod copy; fn score(in self) -> Self::Out; }
impl Score for Point { type Out = i64; fn score(in self) -> i64 { self.x } }
model ByY: Score for Point { type Out = bool; fn score(in self) -> bool { self.y > 0 } }
";

fn pkg() -> PackageIdentity {
    PackageIdentity::new("@demo/geo", "1.0.0")
}

fn goal(brand: Option<BrandDto>) -> GoalDto {
    GoalDto::Implements {
        self_ty: TypeDto::named("Point", vec![]),
        trait_ref: DefRefDto::local("Score"),
        args: vec![],
        brand,
    }
}

#[test]
fn source_declarations_lower_to_bindings() {
    let checked = CheckedPackage::check(pkg(), SRC).unwrap();
    let b = checked.source_bindings();
    assert_eq!(b.package, pkg());
    assert_eq!(b.traits.len(), 1);
    assert_eq!(
        b.traits[0].assoc[0].mode_bound,
        ModeDto::Const(vesper::ast::Mode::Copy)
    );
    assert_eq!(b.impls.len(), 1);
    assert_eq!(b.impls[0].key, "Score.Point");
    assert_eq!(b.models.len(), 1);
    assert_eq!(b.models[0].name, "ByY");
    // Generated bodies are private functions of the checked world.
    assert!(
        checked
            .world()
            .elaboration
            .generated
            .contains_key("model$ByY$score")
    );

    let session = SessionBuilder::new()
        .package(checked.clone(), b.clone())
        .build()
        .unwrap();
    let implicit = QueryBindingDto::new(pkg(), EnvDto::default(), vec![], goal(None));
    let c = session
        .consume(session.solve(&implicit).unwrap().evidence().unwrap())
        .unwrap();
    assert!(matches!(c.selection, Selection::Impl { ref path, .. } if path.name == "Score.Point"));
    let scoped = QueryBindingDto::new(
        pkg(),
        EnvDto {
            use_models: vec![vec![DefRefDto::local("ByY")]],
            ..EnvDto::default()
        },
        vec![],
        goal(None),
    );
    let c = session
        .consume(session.solve(&scoped).unwrap().evidence().unwrap())
        .unwrap();
    assert!(matches!(c.selection, Selection::Model { ref path, .. } if path.name == "ByY"));
    // The model's associated type is its own.
    let proj = QueryBindingDto::new(
        pkg(),
        EnvDto::default(),
        vec![ParamDto::ty("X", None)],
        GoalDto::ProjectionEq {
            projection: ProjectionDto {
                self_ty: TypeDto::named("Point", vec![]),
                trait_ref: DefRefDto::local("Score"),
                args: vec![],
                brand: Some(BrandDto::Model(DefRefDto::local("ByY"))),
                assoc: "Out".into(),
            },
            ty: TypeDto::param("X"),
        },
    );
    let c = session
        .consume(session.solve(&proj).unwrap().evidence().unwrap())
        .unwrap();
    assert!(
        c.substitution
            .iter()
            .any(|(n, v)| n == "X" && v.contains("bool")),
        "{c:?}"
    );
}

#[test]
fn source_and_binding_declarations_combine_and_stay_coherent() {
    let checked = CheckedPackage::check(pkg(), SRC).unwrap();
    // A tool-supplied overlapping impl next to the source impl is refused.
    let mut b = checked.source_bindings().clone();
    let mut dup = b.impls[0].clone();
    dup.key = "score.point.again".into();
    b.impls.push(dup);
    let e = SessionBuilder::new()
        .package(checked, b)
        .build()
        .unwrap_err();
    assert_eq!(e.kind, ErrorKind::Incoherent);
    assert!(e.mentions("PACT0102"), "{e}");
}

#[test]
fn rejected_trait_sources_never_reach_a_session() {
    let bad = SRC.replace(
        "fn score(in self) -> i64 { self.x }",
        "fn score(inout self) -> i64 { self.x }",
    );
    let e = CheckedPackage::check(pkg(), &bad).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Source);
    assert!(e.diagnostics.iter().any(|d| d.code == "E0603"), "{e}");
}

#[test]
fn source_fingerprints_bind_the_revision() {
    let a = CheckedPackage::check(pkg(), SRC).unwrap();
    let b = CheckedPackage::check(pkg(), &SRC.replace("\n", "\n\n  ")).unwrap();
    let ra = SessionBuilder::new()
        .package(a.clone(), a.source_bindings().clone())
        .build()
        .unwrap();
    let rb = SessionBuilder::new()
        .package(b.clone(), b.source_bindings().clone())
        .build()
        .unwrap();
    assert_eq!(ra.revision(), rb.revision(), "whitespace is not identity");
    let c = CheckedPackage::check(pkg(), &SRC.replace("y: i64 }", "y: i64, z: i64 }")).unwrap();
    let rc = SessionBuilder::new()
        .package(c.clone(), c.source_bindings().clone())
        .build()
        .unwrap();
    assert_ne!(
        ra.revision(),
        rc.revision(),
        "a field change is a new revision"
    );
}
