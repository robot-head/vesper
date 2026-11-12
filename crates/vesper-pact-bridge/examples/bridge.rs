//! Parses and checks real source ADTs, binds trait/impl/model declarations
//! from a versioned DTO, solves goals and consumes validated evidence.
//!
//! Run with `cargo run --example bridge`.

use vesper::ast::Item;
use vesper_pact_bridge::*;

const SOURCE: &str = include_str!("geometry.ves");
const BINDINGS: &str = include_str!("geometry.bindings.json");

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let pkg = PackageIdentity::new("@demo/geometry", "1.0.0");
    let checked = CheckedPackage::check(pkg.clone(), SOURCE)?;
    let bindings: BindingsDto = serde_json::from_str(BINDINGS)?;
    let session = SessionBuilder::new()
        .package(checked.clone(), bindings.clone())
        .build()?;
    println!("revision {}", session.revision());
    println!(
        "{} type constructors, {} traits, {} impls, {} models",
        session.tables().types().len(),
        session.program_dto().traits.len(),
        session.program_dto().impls.len(),
        session.program_dto().models.len()
    );

    // The goal's type comes from a checked source signature.
    let layer_ty = checked
        .module()
        .items
        .iter()
        .find_map(|i| match i {
            Item::Function(f) if f.name == "first_point" => Some(f.params[0].ty.clone()),
            _ => None,
        })
        .expect("first_point");
    let goal = GoalDto::Implements {
        self_ty: TypeDto::from_source(&layer_ty)?,
        trait_ref: DefRefDto::local("Eq"),
        args: vec![],
        brand: None,
    };
    let outcome = session.solve(&QueryBindingDto::new(
        pkg.clone(),
        EnvDto::default(),
        vec![],
        goal,
    ))?;
    let evidence = outcome.evidence().ok_or("expected a proof")?;
    let consumed = session.consume(evidence)?;
    println!("\n{layer_ty}: Eq selected {}", consumed.selection);
    print!("{}", consumed.explanation);

    // A substitution translated back to source names.
    let elem = GoalDto::ProjectionEq {
        projection: ProjectionDto {
            self_ty: TypeDto::from_source(&layer_ty)?,
            trait_ref: DefRefDto::local("Container"),
            args: vec![],
            brand: None,
            assoc: "Elem".into(),
        },
        ty: TypeDto::param("E"),
    };
    let q = QueryBindingDto::new(
        pkg.clone(),
        EnvDto::default(),
        vec![ParamDto::ty("E", None)],
        elem,
    );
    let consumed = session.consume(session.solve(&q)?.evidence().ok_or("expected a proof")?)?;
    println!(
        "\n<{layer_ty} as Container>::Elem = {}",
        consumed.substitution[0].1
    );

    // Named models are selected only explicitly or by `use model`.
    let ord = |brand| GoalDto::Implements {
        self_ty: TypeDto::named("Point", vec![]),
        trait_ref: DefRefDto::local("Ord"),
        args: vec![],
        brand,
    };
    for (label, env, brand) in [
        ("implicit", EnvDto::default(), None),
        (
            "use model ByY",
            EnvDto {
                use_models: vec![vec![DefRefDto::local("ByY")]],
                ..EnvDto::default()
            },
            None,
        ),
        (
            "via ByY",
            EnvDto::default(),
            Some(BrandDto::Model(DefRefDto::local("ByY"))),
        ),
    ] {
        let o = session.solve(&QueryBindingDto::new(pkg.clone(), env, vec![], ord(brand)))?;
        let c = session.consume(o.evidence().ok_or("expected a proof")?)?;
        println!("Point: Ord ({label}) -> {}", c.selection);
    }

    // Certificates round-trip and are re-validated.
    let cert = serde_json::to_string(&evidence.certificate())?;
    let accepted = session.accept_certificate(&serde_json::from_str(&cert)?)?;
    println!(
        "\ncertificate of {} bytes re-validated: {}",
        cert.len(),
        accepted == *evidence
    );

    // Every non-proof outcome stays a non-proof.
    let canvas = GoalDto::Implements {
        self_ty: TypeDto::named("Canvas", vec![]),
        trait_ref: DefRefDto::local("Eq"),
        args: vec![],
        brand: None,
    };
    let o = session.solve(&QueryBindingDto::new(
        pkg.clone(),
        EnvDto::default(),
        vec![],
        canvas,
    ))?;
    println!("Canvas: Eq -> {}", o.verdict());

    // A declaration change gives a new revision; old evidence is stale.
    let mut changed = bindings.clone();
    let mut dup = changed.impls[2].clone();
    dup.key = "eq.point.again".into();
    changed.impls.push(dup);
    match SessionBuilder::new()
        .package(checked.clone(), changed)
        .build()
    {
        Err(e) => println!("\noverlapping impl blocks the gate:\n{e}"),
        Ok(_) => return Err("incoherent declarations were accepted".into()),
    }
    let mut extended = bindings;
    extended.traits.push(TraitDto {
        name: "Draw".into(),
        params: vec![],
        assoc: vec![],
        coinductive: false,
    });
    let next = SessionBuilder::new().package(checked, extended).build()?;
    let stale = next.consume(evidence).unwrap_err();
    println!("\nafter a declaration change: {stale}");
    Ok(())
}
