//! Exporter public behavior; real Lean execution belongs to kernel acceptance.
use std::collections::BTreeSet;
use vesper::ast::{Expr, ExprKind, Item, Literal, TypeMap};
use vesper::numeric::IntegerKind;
use vesper::optimize::witness::{
    Equality, IntOp, Reason, RewriteCertificate, Substitution, Term, TermNode, WitnessError,
    WitnessLimits, verify_rewrite,
};
use vesper::optimize::witness_lean::{
    ExportError, ExportLimits, ReplayScope, export_rewrite, model_audit_roots, model_source,
    prepare_rewrite,
};
use vesper_lean_syntax::{Argument, Declaration, DefinitionKind, Module, Name, Term as LeanTerm};

// Independent walk of the public AST, without exporter-private Stats/template
// helpers. Literal storage and Vec/Box indirections are payload, not nodes.
fn name_shape(name: &Name) -> (usize, usize) {
    assert!(!name.components().is_empty());
    (1 + name.components().len(), 2)
}

fn term_shape(term: &LeanTerm) -> (usize, usize) {
    match term {
        LeanTerm::Ref(name) => {
            let (nodes, depth) = name_shape(name);
            (1 + nodes, 1 + depth)
        }
        LeanTerm::Nat(_) | LeanTerm::Rfl | LeanTerm::ByDecide => (1, 1),
        LeanTerm::App {
            function,
            arguments,
        } => {
            let (mut nodes, mut depth) = term_shape(function);
            nodes += 1;
            for argument in arguments {
                let (value, named) = match argument {
                    Argument::Positional(value) => (value, false),
                    Argument::Named { value, .. } => (value, true),
                };
                let (child_nodes, child_depth) = term_shape(value);
                nodes += 1 + child_nodes + usize::from(named);
                depth = depth.max(1 + child_depth.max(usize::from(named)));
            }
            (nodes, depth + 1)
        }
        LeanTerm::Ascribe { value, ty } => {
            let (a, ad) = term_shape(value);
            let (b, bd) = term_shape(ty);
            (1 + a + b, 1 + ad.max(bd))
        }
        LeanTerm::Binary { left, right, .. } => {
            let (a, ad) = term_shape(left);
            let (b, bd) = term_shape(right);
            (1 + a + b, 1 + ad.max(bd))
        }
        LeanTerm::Unary { value, .. } => {
            let (nodes, depth) = term_shape(value);
            (1 + nodes, 1 + depth)
        }
        _ => panic!("exporter emitted syntax outside its fixed subset"),
    }
}

fn declaration_shape(declaration: &Declaration) -> (usize, usize) {
    match declaration {
        Declaration::Definition(definition) => {
            assert!(definition.binders.is_empty());
            let (n, nd) = name_shape(&definition.name);
            let (a, ad) = term_shape(&definition.result);
            let (b, bd) = term_shape(&definition.body);
            (1 + n + a + b, 1 + nd.max(ad).max(bd))
        }
        Declaration::Theorem {
            name,
            binders,
            proposition,
            proof,
        } => {
            assert!(binders.is_empty());
            let (n, nd) = name_shape(name);
            let (a, ad) = term_shape(proposition);
            let (b, bd) = term_shape(proof);
            (1 + n + a + b, 1 + nd.max(ad).max(bd))
        }
        Declaration::Namespace { name, declarations } => {
            let (n, mut depth) = name_shape(name);
            let mut nodes = 1 + n;
            for declaration in declarations {
                let (child_nodes, child_depth) = declaration_shape(declaration);
                nodes += child_nodes;
                depth = depth.max(child_depth);
            }
            (nodes, depth + 1)
        }
        _ => panic!("exporter emitted declaration outside its fixed subset"),
    }
}

fn module_shape(module: &Module) -> (usize, usize) {
    let mut nodes = 1 + module.imports.len(); // ModuleName is one node per import.
    let mut depth = usize::from(!module.imports.is_empty());
    for declaration in &module.declarations {
        let (child_nodes, child_depth) = declaration_shape(declaration);
        nodes += child_nodes;
        depth = depth.max(child_depth);
    }
    (nodes, depth + 1)
}

fn fixture(name: &str) -> (Expr, Expr, TypeMap, RewriteCertificate) {
    let kind = IntegerKind::U8;
    let source = Expr {
        kind: ExprKind::Binary(
            "+".into(),
            Box::new(Expr {
                kind: ExprKind::Name(name.into()),
                span: 4..5,
            }),
            Box::new(Expr {
                kind: ExprKind::Literal(Literal::Int(0)),
                span: 6..7,
            }),
        ),
        span: 1..8,
    };
    let candidate = Expr {
        kind: ExprKind::Name(name.into()),
        span: 1..8,
    };
    let types = [
        ((1, 8), kind.ty()),
        ((4, 5), kind.ty()),
        ((6, 7), kind.ty()),
    ]
    .into_iter()
    .collect();
    let certificate = RewriteCertificate {
        schema_version: 1,
        arithmetic_version: 1,
        terms: vec![
            Term {
                kind,
                node: TermNode::InputVar(0),
            },
            Term {
                kind,
                node: TermNode::Const(0),
            },
            Term {
                kind,
                node: TermNode::Checked {
                    op: IntOp::Add,
                    children: [0, 1],
                },
            },
        ],
        proofs: vec![Equality {
            from: 2,
            to: 0,
            reason: Reason::Rule {
                rule: 1,
                substitution: Substitution {
                    bindings: [0; 3],
                    len: 1,
                },
                guard: None,
            },
        }],
        input: 2,
        output: 0,
        conclusion: 0,
    };
    (source, candidate, types, certificate)
}

#[test]
fn structured_definitions_proofs_and_manifest_preserve_topological_sharing() {
    let (source, candidate, types, certificate) = fixture("x");
    let prepared = export_rewrite(
        &source,
        &candidate,
        &types,
        &certificate,
        WitnessLimits::default(),
        ExportLimits::default(),
    )
    .unwrap();
    let manifest = *prepared.manifest();
    assert_eq!(manifest.scope, ReplayScope::MathModelEq);
    assert_eq!(manifest.module_name, "ReplayGenerated");
    assert_eq!(manifest.theorem, "Vesper.ReplayWitness.replay");
    assert_eq!(
        (
            manifest.term_count,
            manifest.proof_count,
            manifest.declaration_count
        ),
        (3, 1, 6)
    );
    assert_eq!(
        (manifest.input, manifest.output, manifest.conclusion),
        (2, 0, 0)
    );
    assert!(manifest.ast_node_count > manifest.declaration_count);
    assert!(manifest.ast_depth <= ExportLimits::default().max_ast_depth);
    let module = prepared.module();
    assert_eq!(
        module_shape(module),
        (manifest.ast_node_count, manifest.ast_depth)
    );
    assert_eq!(module.imports.len(), 1);
    let [Declaration::Namespace { name, declarations }] = module.declarations.as_slice() else {
        panic!("one namespace")
    };
    assert_eq!(
        name.components()
            .iter()
            .map(|part| part.as_str())
            .collect::<Vec<_>>(),
        ["Vesper", "ReplayWitness"]
    );
    assert_eq!(declarations.len(), 5);
    for (id, decl) in declarations[..3].iter().enumerate() {
        let Declaration::Definition(def) = decl else {
            panic!("named definition")
        };
        assert_eq!(def.kind, DefinitionKind::Def);
        assert_eq!(def.name.components()[0].as_str(), format!("n{id}"));
        assert!(def.binders.is_empty());
    }
    let Declaration::Theorem { name, proof, .. } = &declarations[4] else {
        panic!("final theorem")
    };
    assert_eq!(name.components()[0].as_str(), "replay");
    let LeanTerm::Ref(proof) = proof else {
        panic!("named prior proof")
    };
    assert_eq!(proof.components().last().unwrap().as_str(), "p0");
    let rendered = prepared.render().unwrap();
    assert!(rendered.source.len() <= manifest.source_bytes_upper_bound);
    assert_eq!(
        rendered.source_blake3,
        *blake3::hash(rendered.source.as_bytes()).as_bytes()
    );
    assert_eq!(rendered.manifest, manifest);
}

#[test]
fn canonical_certificate_digest_has_no_debug_or_capacity_encoding() {
    let (source, candidate, types, certificate) = fixture("x");
    let prepared = export_rewrite(
        &source,
        &candidate,
        &types,
        &certificate,
        WitnessLimits::default(),
        ExportLimits::default(),
    )
    .unwrap();
    let mut bytes = b"vesper.replay.certificate/1\0".to_vec();
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(3u64.to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    for id in [2u32, 0, 0] {
        bytes.extend(id.to_le_bytes());
    }
    bytes.extend([0, 1]);
    bytes.extend(0u32.to_le_bytes());
    bytes.extend([0, 0]);
    bytes.extend(0i128.to_le_bytes());
    bytes.extend([0, 2, 0]);
    bytes.extend(0u32.to_le_bytes());
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(2u32.to_le_bytes());
    bytes.extend(0u32.to_le_bytes());
    bytes.push(1);
    bytes.extend(1u16.to_le_bytes());
    bytes.push(1);
    for _ in 0..3 {
        bytes.extend(0u32.to_le_bytes());
    }
    bytes.push(0);
    assert_eq!(
        prepared.manifest().certificate_blake3,
        *blake3::hash(&bytes).as_bytes()
    );
    assert_eq!(
        prepared.manifest().model_blake3,
        *blake3::hash(model_source().as_bytes()).as_bytes()
    );
    let mut spare = certificate.clone();
    spare.terms.try_reserve_exact(16).unwrap();
    spare.proofs.try_reserve_exact(16).unwrap();
    let again = export_rewrite(
        &source,
        &candidate,
        &types,
        &spare,
        WitnessLimits::default(),
        ExportLimits::default(),
    )
    .unwrap();
    assert_eq!(
        again.manifest().certificate_blake3,
        prepared.manifest().certificate_blake3
    );
}

fn typed_node(bytes: &mut Vec<u8>, tag: u8, start: u64, end: u64) {
    bytes.push(tag);
    bytes.extend(start.to_le_bytes());
    bytes.extend(end.to_le_bytes());
    bytes.push(1);
}
fn identifier(bytes: &mut Vec<u8>, name: &str) {
    bytes.extend((name.len() as u64).to_le_bytes());
    bytes.extend(name.as_bytes());
}

#[test]
fn binding_digest_has_ordered_roles_actual_spans_and_only_used_typing() {
    let (source, candidate, mut types, certificate) = fixture("x");
    let prepared = export_rewrite(
        &source,
        &candidate,
        &types,
        &certificate,
        WitnessLimits::default(),
        ExportLimits::default(),
    )
    .unwrap();
    let mut bytes = b"vesper.replay.binding/1\0".to_vec();
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.push(0);
    bytes.extend(3u64.to_le_bytes());
    typed_node(&mut bytes, 2, 1, 8);
    typed_node(&mut bytes, 1, 4, 5);
    identifier(&mut bytes, "x");
    typed_node(&mut bytes, 0, 6, 7);
    bytes.extend(0i128.to_le_bytes());
    bytes.push(1);
    bytes.extend(1u64.to_le_bytes());
    typed_node(&mut bytes, 1, 1, 8);
    identifier(&mut bytes, "x");
    assert_eq!(
        prepared.manifest().binding_blake3,
        *blake3::hash(&bytes).as_bytes()
    );
    types.insert((999, 1000), IntegerKind::I64.ty());
    let unrelated = export_rewrite(
        &source,
        &candidate,
        &types,
        &certificate,
        WitnessLimits::default(),
        ExportLimits::default(),
    )
    .unwrap();
    assert_eq!(
        unrelated.manifest().binding_blake3,
        prepared.manifest().binding_blake3
    );
    let (renamed, renamed_candidate, renamed_types, renamed_certificate) =
        fixture("source «data» with spaces");
    let renamed = export_rewrite(
        &renamed,
        &renamed_candidate,
        &renamed_types,
        &renamed_certificate,
        WitnessLimits::default(),
        ExportLimits::default(),
    )
    .unwrap();
    assert_ne!(
        renamed.manifest().binding_blake3,
        prepared.manifest().binding_blake3
    );
    assert_eq!(
        renamed.manifest().certificate_blake3,
        prepared.manifest().certificate_blake3
    );
    assert_eq!(
        renamed.module(),
        prepared.module(),
        "source identifiers remain indexed model variables"
    );
}

#[test]
fn malformed_or_unbound_certificates_never_become_prepared_ast() {
    let (source, candidate, types, mut certificate) = fixture("x");
    let source_before = source.clone();
    let candidate_before = candidate.clone();
    let Reason::Rule { rule, .. } = &mut certificate.proofs[0].reason else {
        unreachable!()
    };
    *rule = 65535;
    assert_eq!(
        export_rewrite(
            &source,
            &candidate,
            &types,
            &certificate,
            WitnessLimits::default(),
            ExportLimits::default()
        )
        .unwrap_err(),
        ExportError::Witness(WitnessError::UnknownRule)
    );
    let (_, _, _, certificate) = fixture("x");
    let mut conflicting = candidate.clone();
    conflicting.span = 4..5;
    assert_eq!(
        export_rewrite(
            &source,
            &conflicting,
            &types,
            &certificate,
            WitnessLimits::default(),
            ExportLimits::default()
        )
        .unwrap_err(),
        ExportError::Witness(WitnessError::OutputMismatch)
    );
    assert_eq!(source, source_before);
    assert_eq!(candidate, candidate_before);
}

#[test]
fn all_export_limit_fields_are_positive_and_hard_bounded() {
    let (source, candidate, types, certificate) = fixture("x");
    macro_rules! limits {($($field:ident),+)=>{$(
        let mut limits=ExportLimits::default();limits.$field=0;
        assert_eq!(export_rewrite(&source,&candidate,&types,&certificate,WitnessLimits::default(),limits).unwrap_err(),ExportError::InvalidLimits);
        let mut limits=ExportLimits::default();limits.$field=ExportLimits::HARD.$field+1;
        assert_eq!(export_rewrite(&source,&candidate,&types,&certificate,WitnessLimits::default(),limits).unwrap_err(),ExportError::InvalidLimits);
        let mut limits=ExportLimits::default();limits.$field=1;
        assert_eq!(export_rewrite(&source,&candidate,&types,&certificate,WitnessLimits::default(),limits).unwrap_err(),ExportError::Limit);
    )+};}
    limits!(
        max_declarations,
        max_ast_nodes,
        max_ast_depth,
        max_name_bytes,
        max_source_bytes,
        max_metadata_bytes,
        max_work
    );
}

#[test]
fn exact_structural_limits_prepare_and_render_but_one_below_refuses() {
    let (source, candidate, types, certificate) = fixture("x");
    let prepared = export_rewrite(
        &source,
        &candidate,
        &types,
        &certificate,
        WitnessLimits::default(),
        ExportLimits::default(),
    )
    .unwrap();
    let m = *prepared.manifest();
    for (field, value) in [
        (0, m.declaration_count),
        (1, m.ast_node_count),
        (2, m.ast_depth),
        (3, m.name_bytes),
        (4, m.source_bytes_upper_bound),
    ] {
        let mut limits = ExportLimits::default();
        match field {
            0 => limits.max_declarations = value,
            1 => limits.max_ast_nodes = value,
            2 => limits.max_ast_depth = value,
            3 => limits.max_name_bytes = value,
            _ => limits.max_source_bytes = value,
        }
        export_rewrite(
            &source,
            &candidate,
            &types,
            &certificate,
            WitnessLimits::default(),
            limits,
        )
        .unwrap()
        .render()
        .unwrap();
        match field {
            0 => limits.max_declarations -= 1,
            1 => limits.max_ast_nodes -= 1,
            2 => limits.max_ast_depth -= 1,
            3 => limits.max_name_bytes -= 1,
            _ => limits.max_source_bytes -= 1,
        }
        assert_eq!(
            export_rewrite(
                &source,
                &candidate,
                &types,
                &certificate,
                WitnessLimits::default(),
                limits
            )
            .unwrap_err(),
            ExportError::Limit
        );
    }
}

fn checked_tail(source: &str) -> (Expr, TypeMap) {
    let parsed = vesper::parser::parse(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let checked = vesper::check::check(&parsed.module);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    let Item::Function(f) = &parsed.module.items[0] else {
        panic!("function")
    };
    let ExprKind::Block {
        tail: Some(tail), ..
    } = &f.body.as_ref().unwrap().kind
    else {
        panic!("tail")
    };
    ((**tail).clone(), checked.expression_types)
}

#[test]
fn public_source_helper_runs_actual_producer_full_gate_and_move_only_export() {
    for program in [
        "fn f(x: u64) -> u64 eff [Fail[ArithOverflow]] { x + 0 }",
        "fn f() -> u64 { 2 + 3 }",
        "fn f() -> u64 { wrapping_mul(18446744073709551615, 18446744073709551615) }",
    ] {
        let (source, types) = checked_tail(program);
        let before = source.clone();
        let replay = prepare_rewrite(
            &source,
            &types,
            WitnessLimits::default(),
            ExportLimits::default(),
        )
        .unwrap()
        .expect("genuine candidate");
        verify_rewrite(
            &source,
            &replay.candidate,
            &types,
            &replay.certificate,
            WitnessLimits::default(),
        )
        .unwrap();
        assert!(
            replay
                .certificate
                .proofs
                .iter()
                .any(|proof| matches!(proof.reason, Reason::Rule { .. }))
        );
        let rendered = replay.lean.render().unwrap();
        assert_eq!(
            rendered.source_blake3,
            *blake3::hash(rendered.source.as_bytes()).as_bytes()
        );
        assert_eq!(source, before);
    }
}

#[test]
fn source_helper_barriers_unchanged_and_refusal_never_return_partial_artifacts() {
    let (source, types) = checked_tail("fn f(x: u64) -> u64 { x }");
    assert!(
        prepare_rewrite(
            &source,
            &types,
            WitnessLimits::default(),
            ExportLimits::default()
        )
        .unwrap()
        .is_none()
    );
    let (source, types) = checked_tail("fn f(x: u64) -> u64 eff [Fail[RuntimeError]] { x / 2 }");
    assert!(
        prepare_rewrite(
            &source,
            &types,
            WitnessLimits::default(),
            ExportLimits::default()
        )
        .unwrap()
        .is_none()
    );
    let (source, types) = checked_tail("fn f() -> u64 { 2 + 3 }");
    let before = source.clone();
    let limits = ExportLimits {
        max_work: 1,
        ..ExportLimits::default()
    };
    assert!(matches!(
        prepare_rewrite(&source, &types, WitnessLimits::default(), limits),
        Err(ExportError::Limit)
    ));
    assert_eq!(source, before);
}

#[test]
fn static_model_audit_inventory_is_exact_and_not_kernel_acceptance_status() {
    let roots = model_audit_roots();
    assert_eq!(roots.len(), 33);
    assert_eq!(roots.iter().copied().collect::<BTreeSet<_>>().len(), 33);
    assert!(
        roots
            .iter()
            .all(|name| name.starts_with("Vesper.RewriteWitness."))
    );
    assert_eq!(roots[0], "Vesper.RewriteWitness.r01_add_zero");
    assert_eq!(
        roots[23],
        "Vesper.RewriteWitness.r24_wrapping_mul_constants"
    );
    assert_eq!(roots[32], "Vesper.RewriteWitness.congr_wrapping");
    assert!(!model_source().is_empty());
}

#[test]
fn actual_structural_depth_counts_names_arguments_namespace_and_module() {
    for program in [
        "fn f(x: u64) -> u64 eff [Fail[ArithOverflow]] { x + 0 }",
        "fn f() -> i64 { wrapping_add(9223372036854775807, 1) }",
        "fn f(x: u64) -> u64 eff [Fail[ArithOverflow]] { (x + 0) * 0 }",
    ] {
        let (source, types) = checked_tail(program);
        let prepared = prepare_rewrite(
            &source,
            &types,
            WitnessLimits::default(),
            ExportLimits::default(),
        )
        .unwrap()
        .expect("actual bounded candidate");
        let (nodes, depth) = module_shape(prepared.lean.module());
        assert_eq!(
            (nodes, depth),
            (
                prepared.lean.manifest().ast_node_count,
                prepared.lean.manifest().ast_depth
            )
        );
        assert!(depth >= 7, "Name/Ident and Argument levels must be counted");
        let exact = ExportLimits {
            max_ast_depth: depth,
            ..ExportLimits::default()
        };
        export_rewrite(
            &source,
            &prepared.candidate,
            &types,
            &prepared.certificate,
            WitnessLimits::default(),
            exact,
        )
        .unwrap()
        .render()
        .unwrap();
        let below = ExportLimits {
            max_ast_depth: depth - 1,
            ..ExportLimits::default()
        };
        assert_eq!(
            export_rewrite(
                &source,
                &prepared.candidate,
                &types,
                &prepared.certificate,
                WitnessLimits::default(),
                below
            )
            .unwrap_err(),
            ExportError::Limit
        );
    }
}
