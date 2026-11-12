mod common;

use common::*;
use vesper_pact::*;

#[test]
fn fixture_world_is_coherent() {
    let p = world();
    assert_eq!(
        check_coherence(&p),
        vec![],
        "{}",
        vesper_pact::diag::render(&check_coherence(&p))
    );
}

/// Adds conflicting declarations to the world and returns the new ids.
fn incoherent_world() -> (Program, Vec<ImplId>, Vec<ModelId>) {
    let mut dto = world_dto();
    let next_impl = |dto: &ProgramDto| ImplId(dto.impls.len() as u32);
    let mut added = vec![];
    let mut push = |dto: &mut ProgramDto, decl: ConformanceDecl| {
        let id = next_impl(dto);
        dto.impls.push(ImplDecl { id, decl });
        added.push(id);
    };
    // A: blanket impl[T] Clone for T overlaps every Clone impl.
    push(
        &mut dto,
        conf(
            STD,
            vec![ParamKind::Type],
            tr(CLONE, bound(0), vec![]),
            vec![],
            vec![],
        ),
    );
    // B: package APP implements foreign WireCodec for foreign Uuid: orphan.
    push(
        &mut dto,
        conf(APP, vec![], tr(WIRE_CODEC, c(UUID), vec![]), vec![], vec![]),
    );
    // C: impl[T] Merge[PulseTcp] for T in APP: uncovered T precedes the local type.
    push(
        &mut dto,
        conf(
            APP,
            vec![ParamKind::Type],
            tr(MERGE, bound(0), vec![a(c(PULSE_TCP))]),
            vec![],
            vec![],
        ),
    );
    // D: impl[T] Merge[T] for PulseTcp in APP: local type first, legal.
    push(
        &mut dto,
        conf(
            APP,
            vec![ParamKind::Type],
            tr(MERGE, c(PULSE_TCP), vec![a(bound(0))]),
            vec![],
            vec![],
        ),
    );
    // E: impl[T] Merge[PulseTcp] for Vec[T] in APP: T is covered by Vec, legal.
    push(
        &mut dto,
        conf(
            APP,
            vec![ParamKind::Type],
            tr(MERGE, t1(VEC, bound(0)), vec![a(c(PULSE_TCP))]),
            vec![],
            vec![],
        ),
    );
    // F: impl Container[mod copy] for Vec[WriteTxn]: overlaps impl#11? No: modal args differ (copy vs linear),
    //    but its Elem = WriteTxn violates `type Elem: mod copy`.
    push(
        &mut dto,
        conf(
            STD,
            vec![],
            tr(CONTAINER, t1(VEC, c(WRITE_TXN)), vec![mode(Mode::Copy)]),
            vec![],
            vec![c(WRITE_TXN)],
        ),
    );
    // G: impl Source[eff [Net]] for PulseTcp: effect rows differ from impl#8, no overlap.
    push(
        &mut dto,
        conf(
            APP,
            vec![],
            tr(SOURCE, c(PULSE_TCP), vec![row(vec![eff(NET)])]),
            vec![],
            vec![c(RECORD_BATCH)],
        ),
    );
    // H: impl[r] Source[r] for PulseTcp: open row overlaps impl#8 and G.
    push(
        &mut dto,
        conf(
            APP,
            vec![ParamKind::Row],
            tr(SOURCE, c(PULSE_TCP), vec![open_row(vec![], Var::Bound(0))]),
            vec![],
            vec![c(RECORD_BATCH)],
        ),
    );
    // Two more named models for Ord on Uuid in other packages: never a conflict.
    let mut models = vec![];
    for pkg in [WIRE, THIRD] {
        let id = ModelId(dto.models.len() as u32);
        dto.models.push(ModelDecl {
            id,
            decl: conf(pkg, vec![], tr(ORD, c(UUID), vec![]), vec![], vec![]),
        });
        models.push(id);
    }
    (Program::new(dto).expect("validates"), added, models)
}

#[test]
fn coherence_violations_are_reported_deterministically() {
    let (p, added, _) = incoherent_world();
    assert_eq!(added[0], ImplId(22));
    let text = vesper_pact::diag::render(&check_coherence(&p));
    let expected = "\
PACT0101 [impl#23] orphan canonical impl: neither trait#3 nor any type argument is defined in pkg#2; declare a named `model` instead
PACT0101 [impl#24] orphan canonical impl: uncovered parameter ^0 at type position 0 precedes any local type; declare a named `model` instead
PACT0102 [impl#0 & impl#22] canonical impls of trait#0 overlap; declare one as a named `model`
PACT0102 [impl#1 & impl#22] canonical impls of trait#0 overlap; declare one as a named `model`
PACT0102 [impl#2 & impl#22] canonical impls of trait#0 overlap; declare one as a named `model`
PACT0102 [impl#3 & impl#22] canonical impls of trait#0 overlap; declare one as a named `model`
PACT0102 [impl#8 & impl#29] canonical impls of trait#4 overlap; declare one as a named `model`
PACT0102 [impl#17 & impl#22] canonical impls of trait#0 overlap; declare one as a named `model`
PACT0102 [impl#21 & impl#22] canonical impls of trait#0 overlap; declare one as a named `model`
PACT0102 [impl#24 & impl#25] canonical impls of trait#8 overlap; declare one as a named `model`
PACT0102 [impl#24 & impl#26] canonical impls of trait#8 overlap; declare one as a named `model`
PACT0102 [impl#28 & impl#29] canonical impls of trait#4 overlap; declare one as a named `model`
PACT0103 [impl#27] associated value assoc#0 = type#4 must satisfy `mode(type#4) <= copy`: no-solution
`mode(type#4) <= copy`: mode linear is not below copy
";
    assert_eq!(text, expected);
    // Re-running is byte-identical.
    assert_eq!(vesper_pact::diag::render(&check_coherence(&p)), text);
    // D (local self type) and E (covered parameter) are legal orphans-wise.
    assert!(!text.contains("PACT0101 [impl#25]") && !text.contains("PACT0101 [impl#26]"));
    // Distinct effect rows and distinct modal arguments do not overlap.
    assert!(!text.contains("impl#8 & impl#28"));
    assert!(!text.contains("impl#11 & impl#27"));
}

#[test]
fn associated_value_must_satisfy_alias_bounds() {
    let mut dto = world_dto();
    let id = ImplId(dto.impls.len() as u32);
    // impl Collect for u64 { type Out = Rc[u64] }: `Out: Clone` fails.
    dto.impls.push(ImplDecl {
        id,
        decl: conf(
            STD,
            vec![],
            tr(COLLECT, c(U64), vec![]),
            vec![],
            vec![t1(RC, c(U64))],
        ),
    });
    let p = Program::new(dto).unwrap();
    let text = vesper_pact::diag::render(&check_coherence(&p));
    assert_eq!(
        text,
        "PACT0103 [impl#22] associated value assoc#0 = type#13[type#0] must satisfy `type#13[type#0]: trait#0 via canonical`: no-solution\n\
         `type#13[type#0]: trait#0 via canonical`: no matching impl, model or assumption\n"
    );
}

#[test]
fn named_models_are_exempt_from_orphan_and_overlap() {
    let (p, _, models) = incoherent_world();
    let text = vesper_pact::diag::render(&check_coherence(&p));
    for m in models
        .iter()
        .chain([REVERSE_UUID_ORD, BIG_ENDIAN_UUID, LITTLE_ENDIAN_UUID].iter())
    {
        assert!(!text.contains(&format!("[{m}]")), "{m} reported:\n{text}");
    }
    // Each named model remains separately addressable.
    for m in models {
        assert!(solve_goal(&p, via(ORD, c(UUID), vec![], Brand::Named(m))).is_proven());
    }
}

#[test]
fn overlap_makes_resolution_ambiguous_not_arbitrary() {
    // Even if an incoherent program is solved anyway, overlapping candidates
    // never produce an arbitrary choice.
    let (p, _, _) = incoherent_world();
    let o = solve_goal(&p, implements(CLONE, c(U64), vec![]));
    // Both impl#0 and the blanket impl#22 prove it with identical (empty)
    // answers, so the answer is unique even though evidence differs; with a
    // differing answer it must be ambiguous:
    assert!(o.is_proven());
    let q = query(
        Env::default(),
        vec![ParamKind::Row],
        implements(SOURCE, c(PULSE_TCP), vec![open_row(vec![], Var::Infer(0))]),
    );
    assert!(matches!(
        solve(&p, &q),
        Outcome::Ambiguous(Ambiguity::MultipleCandidates(_))
    ));
}

#[test]
fn orphan_rule_examples() {
    let p = world();
    // Trait-local impls are always legal.
    assert!(
        check_orphan(
            &p,
            &conf(
                WIRE,
                vec![],
                tr(WIRE_CODEC, c(UUID), vec![]),
                vec![],
                vec![]
            )
        )
        .is_ok()
    );
    // Local self type.
    assert!(
        check_orphan(
            &p,
            &conf(APP, vec![], tr(CLONE, c(WRITE_TXN), vec![]), vec![], vec![])
        )
        .is_ok()
    );
    // Foreign self, local later argument with no uncovered parameter before it.
    assert!(
        check_orphan(
            &p,
            &conf(
                APP,
                vec![],
                tr(MERGE, c(UUID), vec![a(c(PULSE_TCP))]),
                vec![],
                vec![]
            )
        )
        .is_ok()
    );
    // Foreign everything.
    assert!(
        check_orphan(
            &p,
            &conf(
                APP,
                vec![],
                tr(MERGE, c(UUID), vec![a(c(U64))]),
                vec![],
                vec![]
            )
        )
        .is_err()
    );
}
